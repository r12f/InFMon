//! OTLP/gRPC metrics exporter.
//!
//! Implements the [`Exporter`] trait and translates [`FlowStatsSnapshot`]
//! data into OTLP metrics, shipping them over gRPC (or HTTP/protobuf).
//!
//! Spec reference: `specs/006-exporter-otlp.md`

use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use opentelemetry_proto::tonic::collector::metrics::v1::{
    metrics_service_client::MetricsServiceClient, ExportMetricsServiceRequest,
};
use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue};
use opentelemetry_proto::tonic::metrics::v1::{
    AggregationTemporality, Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint};

use infmon_ipc::types::{FieldId, FieldValue, FlowRuleStats, FlowStatsSnapshot};

use crate::exporter::{
    BoxFuture, ConfigError, Exporter, ExporterConfig, ExporterError, ExporterMetrics,
    ExporterRegistration,
};

// ── Constants ──────────────────────────────────────────────────────

/// Default per-tick point cap (spec §8.2).
const DEFAULT_MAX_EXPORT_POINTS_PER_TICK: u64 = 2_000_000;

/// Max length for string attribute values (spec §8.2).
const ATTRIBUTE_LENGTH_CAP: usize = 256;

/// Number of per-flow data points (packets, bytes, last_seen).
const POINTS_PER_FLOW: u64 = 3;

/// Number of per-flow-rule data points (flows, evictions, drops, packets, bytes).
const POINTS_PER_FLOW_RULE: u64 = 5;

// ── OTLP Exporter ──────────────────────────────────────────────────

/// OTLP/gRPC metrics exporter.
pub struct OtlpExporter {
    name: String,
    endpoint: String,
    #[allow(dead_code)] // stored for future reload support
    instance_id_path: Option<String>,
    max_export_points_per_tick: u64,
    resource_attrs: Vec<KeyValue>,
    client: Mutex<Option<MetricsServiceClient<Channel>>>,
    metrics: Arc<ExporterMetrics>,
}

impl OtlpExporter {
    /// Build a new exporter from its config block.
    pub fn new(cfg: &ExporterConfig) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let endpoint = cfg.extra.get("endpoint").cloned().unwrap_or_default();

        if endpoint.is_empty() {
            return Err("OTLP exporter requires a non-empty 'endpoint'".into());
        }

        let max_export_points_per_tick = cfg
            .extra
            .get("max_export_points_per_tick")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MAX_EXPORT_POINTS_PER_TICK);

        // Optional configurable instance_id path (default: /var/lib/infmon/instance_id)
        let instance_id_path = cfg.extra.get("instance_id_path").cloned();

        // Build resource attributes (spec §5).
        let mut resource_attrs = vec![
            kv_string("service.name", "infmon-frontend"),
            kv_string("service.namespace", "infmon"),
            kv_string(
                "service.version",
                option_env!("CARGO_PKG_VERSION").unwrap_or("0.0.0"),
            ),
        ];

        // host.name
        if let Ok(hostname) = hostname::get() {
            resource_attrs.push(kv_string("host.name", &hostname.to_string_lossy()));
        }

        // host.arch (runtime, never hard-coded)
        resource_attrs.push(kv_string("host.arch", std::env::consts::ARCH));

        // host.id from /etc/machine-id (silent omit if unavailable)
        if let Ok(machine_id) = std::fs::read_to_string("/etc/machine-id") {
            let id = machine_id.trim();
            if !id.is_empty() {
                resource_attrs.push(kv_string("host.id", id));
            }
        }

        // service.instance.id — read or create persistent instance ID
        let id_path = instance_id_path
            .as_deref()
            .unwrap_or("/var/lib/infmon/instance_id");
        let instance_id = load_or_create_instance_id(id_path);
        resource_attrs.push(kv_string("service.instance.id", &instance_id));

        // Operator-supplied resource attributes from extra config
        // (keys prefixed with "resource." are treated as resource attrs)
        for (k, v) in &cfg.extra {
            if let Some(attr_key) = k.strip_prefix("resource.") {
                if !attr_key.is_empty() {
                    resource_attrs.push(kv_string(attr_key, v));
                }
            }
        }

        Ok(Self {
            name: cfg.name.clone(),
            endpoint,
            instance_id_path,
            max_export_points_per_tick,
            resource_attrs,
            client: Mutex::new(None),
            metrics: Arc::new(ExporterMetrics::default()),
        })
    }

    /// Lazily connect to the gRPC endpoint.
    async fn get_client(
        &self,
    ) -> Result<MetricsServiceClient<Channel>, Box<dyn std::error::Error + Send + Sync>> {
        let mut guard = self.client.lock().await;
        if let Some(ref client) = *guard {
            return Ok(client.clone());
        }

        // Ensure endpoint has scheme
        let endpoint_uri =
            if self.endpoint.starts_with("http://") || self.endpoint.starts_with("https://") {
                self.endpoint.clone()
            } else {
                format!("http://{}", self.endpoint)
            };

        let channel = Endpoint::from_shared(endpoint_uri)?
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .connect()
            .await?;
        let client = MetricsServiceClient::new(channel);
        *guard = Some(client.clone());
        Ok(client)
    }

    /// Clear the cached client so the next call reconnects.
    async fn clear_client(&self) {
        let mut guard = self.client.lock().await;
        *guard = None;
    }

    /// Build the full OTLP request from a snapshot.
    /// Returns `(request, points_built)` — the caller increments `points_emitted`
    /// only after a successful export.
    fn build_request(&self, snap: &FlowStatsSnapshot) -> (ExportMetricsServiceRequest, u64) {
        let wall_ns = snap.wall_clock_ns;
        let trunc_counter = &self.metrics.attrs_truncated;

        // Calculate total flows and apply cardinality cap (spec §8.2).
        let total_flows: u64 = snap.flow_rules.iter().map(|fr| fr.flows.len() as u64).sum();

        // Reserve budget for per-flow-rule points before computing per-flow allocations.
        let flow_rule_budget = snap.flow_rules.len() as u64 * POINTS_PER_FLOW_RULE;
        let remaining_budget = self
            .max_export_points_per_tick
            .saturating_sub(flow_rule_budget);
        let effective_cap = remaining_budget / POINTS_PER_FLOW.max(1);

        // Determine per-flow-rule budget
        let flow_budgets: Vec<u64> = if total_flows == 0 || total_flows <= effective_cap {
            snap.flow_rules
                .iter()
                .map(|fr| fr.flows.len() as u64)
                .collect()
        } else {
            // Proportional allocation
            let mut budgets: Vec<u64> = snap
                .flow_rules
                .iter()
                .map(|fr| {
                    (effective_cap as u128 * fr.flows.len() as u128 / total_flows as u128) as u64
                })
                .collect();
            // Distribute remainder
            let allocated: u64 = budgets.iter().sum();
            let mut remainder = effective_cap.saturating_sub(allocated);
            for b in budgets.iter_mut() {
                if remainder == 0 {
                    break;
                }
                *b += 1;
                remainder -= 1;
            }
            budgets
        };

        let mut metrics: Vec<Metric> = Vec::new();
        let mut points_dropped_count: u64 = 0;

        // Collect data points grouped by metric name to produce idiomatic OTLP
        // (one Metric with N data points, rather than N Metrics with 1 each).
        let mut flow_packets_points: Vec<NumberDataPoint> = Vec::new();
        let mut flow_bytes_points: Vec<NumberDataPoint> = Vec::new();
        let mut flow_last_seen_points: Vec<NumberDataPoint> = Vec::new();
        let mut fr_flows_points: Vec<NumberDataPoint> = Vec::new();
        let mut fr_evictions_points: Vec<NumberDataPoint> = Vec::new();
        let mut fr_drops_points: Vec<NumberDataPoint> = Vec::new();
        let mut fr_packets_points: Vec<NumberDataPoint> = Vec::new();
        let mut fr_bytes_points: Vec<NumberDataPoint> = Vec::new();

        for (fr_idx, fr) in snap.flow_rules.iter().enumerate() {
            let budget = flow_budgets.get(fr_idx).copied().unwrap_or(0) as usize;
            let flows_to_emit = budget.min(fr.flows.len());
            let flows_dropped = fr.flows.len().saturating_sub(flows_to_emit);
            points_dropped_count += flows_dropped as u64 * POINTS_PER_FLOW;

            // Per-flow data points
            for flow in fr.flows.iter().take(flows_to_emit) {
                let attrs = build_flow_attributes(fr, flow, &fr.fields, Some(trunc_counter));

                // infmon.flow.packets
                flow_packets_points.push(NumberDataPoint {
                    attributes: attrs.clone(),
                    start_time_unix_nano: flow.counters.first_seen_ns,
                    time_unix_nano: wall_ns,
                    value: Some(
                        opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                            saturating_i64(flow.counters.packets),
                        ),
                    ),
                    ..Default::default()
                });

                // infmon.flow.bytes
                flow_bytes_points.push(NumberDataPoint {
                    attributes: attrs.clone(),
                    start_time_unix_nano: flow.counters.first_seen_ns,
                    time_unix_nano: wall_ns,
                    value: Some(
                        opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                            saturating_i64(flow.counters.bytes),
                        ),
                    ),
                    ..Default::default()
                });

                // infmon.flow.last_seen
                flow_last_seen_points.push(NumberDataPoint {
                    attributes: attrs,
                    time_unix_nano: wall_ns,
                    value: Some(
                        opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                            saturating_i64(flow.counters.last_seen_ns),
                        ),
                    ),
                    ..Default::default()
                });
            }

            // Per-flow-rule observability points (spec §3.4)
            let fr_attrs = vec![kv_string("flow-rule", &fr.name)];

            // infmon.flow-rule.flows (Gauge)
            fr_flows_points.push(NumberDataPoint {
                attributes: fr_attrs.clone(),
                time_unix_nano: wall_ns,
                value: Some(
                    opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                        saturating_i64(fr.flows.len() as u64),
                    ),
                ),
                ..Default::default()
            });

            // infmon.flow-rule.evictions (Sum, cumulative)
            fr_evictions_points.push(NumberDataPoint {
                attributes: fr_attrs.clone(),
                time_unix_nano: wall_ns,
                value: Some(
                    opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                        saturating_i64(fr.counters.evictions),
                    ),
                ),
                ..Default::default()
            });

            // infmon.flow-rule.drops (Sum, cumulative) with reason attr
            // TODO(extensibility): derive `reason` from data when additional
            // drop reasons are added (currently only eviction_failed exists).
            let mut drops_attrs = fr_attrs.clone();
            drops_attrs.push(kv_string("reason", "eviction_failed"));
            fr_drops_points.push(NumberDataPoint {
                attributes: drops_attrs,
                time_unix_nano: wall_ns,
                value: Some(
                    opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                        saturating_i64(fr.counters.drops),
                    ),
                ),
                ..Default::default()
            });

            // infmon.flow-rule.packets (Sum, cumulative)
            fr_packets_points.push(NumberDataPoint {
                attributes: fr_attrs.clone(),
                time_unix_nano: wall_ns,
                value: Some(
                    opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                        saturating_i64(fr.counters.packets),
                    ),
                ),
                ..Default::default()
            });

            // infmon.flow-rule.bytes (Sum, cumulative)
            fr_bytes_points.push(NumberDataPoint {
                attributes: fr_attrs,
                time_unix_nano: wall_ns,
                value: Some(
                    opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                        saturating_i64(fr.counters.bytes),
                    ),
                ),
                ..Default::default()
            });
        }

        // ── Track and append self-observability metrics (spec §8.3) ─────

        // Count emitted data points (flow + flow-rule) before vecs are moved
        let points_emitted_count: u64 = flow_packets_points.len() as u64
            + flow_bytes_points.len() as u64
            + flow_last_seen_points.len() as u64
            + fr_flows_points.len() as u64
            + fr_evictions_points.len() as u64
            + fr_drops_points.len() as u64
            + fr_packets_points.len() as u64
            + fr_bytes_points.len() as u64;

        // Build grouped Metric objects (one per metric name, N data points each)
        if !flow_packets_points.is_empty() {
            metrics.push(Metric {
                name: "infmon.flow.packets".into(),
                description: "Total packets attributed to this flow".into(),
                unit: "{packets}".into(),
                data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                    Sum {
                        data_points: flow_packets_points,
                        aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        is_monotonic: true,
                    },
                )),
                metadata: Vec::new(),
            });
        }

        if !flow_bytes_points.is_empty() {
            metrics.push(Metric {
                name: "infmon.flow.bytes".into(),
                description: "Total bytes attributed to this flow".into(),
                unit: "By".into(),
                data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                    Sum {
                        data_points: flow_bytes_points,
                        aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        is_monotonic: true,
                    },
                )),
                metadata: Vec::new(),
            });
        }

        if !flow_last_seen_points.is_empty() {
            metrics.push(Metric {
                name: "infmon.flow.last_seen".into(),
                description: "Wall-clock ns of the most recent packet".into(),
                unit: "ns".into(),
                data: Some(
                    opentelemetry_proto::tonic::metrics::v1::metric::Data::Gauge(Gauge {
                        data_points: flow_last_seen_points,
                    }),
                ),
                metadata: Vec::new(),
            });
        }

        if !fr_flows_points.is_empty() {
            metrics.push(Metric {
                name: "infmon.flow-rule.flows".into(),
                description: "Number of live flows in this flow-rule".into(),
                unit: "{flows}".into(),
                data: Some(
                    opentelemetry_proto::tonic::metrics::v1::metric::Data::Gauge(Gauge {
                        data_points: fr_flows_points,
                    }),
                ),
                metadata: Vec::new(),
            });
        }

        if !fr_evictions_points.is_empty() {
            metrics.push(Metric {
                name: "infmon.flow-rule.evictions".into(),
                description: "Total evictions for this flow-rule".into(),
                unit: "{evictions}".into(),
                data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                    Sum {
                        data_points: fr_evictions_points,
                        aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        is_monotonic: true,
                    },
                )),
                metadata: Vec::new(),
            });
        }

        if !fr_drops_points.is_empty() {
            metrics.push(Metric {
                name: "infmon.flow-rule.drops".into(),
                description: "Total drops for this flow-rule".into(),
                unit: "{drops}".into(),
                data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                    Sum {
                        data_points: fr_drops_points,
                        aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        is_monotonic: true,
                    },
                )),
                metadata: Vec::new(),
            });
        }

        if !fr_packets_points.is_empty() {
            metrics.push(Metric {
                name: "infmon.flow-rule.packets".into(),
                description: "Total packets for this flow-rule".into(),
                unit: "{packets}".into(),
                data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                    Sum {
                        data_points: fr_packets_points,
                        aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        is_monotonic: true,
                    },
                )),
                metadata: Vec::new(),
            });
        }

        if !fr_bytes_points.is_empty() {
            metrics.push(Metric {
                name: "infmon.flow-rule.bytes".into(),
                description: "Total bytes for this flow-rule".into(),
                unit: "By".into(),
                data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                    Sum {
                        data_points: fr_bytes_points,
                        aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        is_monotonic: true,
                    },
                )),
                metadata: Vec::new(),
            });
        }

        // ── Append self-observability metric objects ──────────────────────

        // Update atomic counters (points_dropped tracked here; points_emitted
        // is deferred to the caller so it only counts successfully exported points)
        self.metrics
            .points_dropped
            .fetch_add(points_dropped_count, Ordering::Relaxed);

        let m = &self.metrics;

        // Cumulative counters
        metrics.push(make_sum_metric(
            "infmon.exporter.ticks_dropped",
            "{ticks}",
            m.ticks_dropped.load(Ordering::Relaxed),
            vec![],
            wall_ns,
        ));
        metrics.push(make_sum_metric(
            "infmon.exporter.batches_sent",
            "{batches}",
            m.batches_sent.load(Ordering::Relaxed),
            vec![],
            wall_ns,
        ));
        metrics.push(make_sum_metric(
            "infmon.exporter.batches_dropped",
            "{batches}",
            m.batches_dropped.load(Ordering::Relaxed),
            vec![],
            wall_ns,
        ));

        // batches_failed: emit two data points with reason attribute
        let failed_non_retryable = m.batches_failed_non_retryable.load(Ordering::Relaxed);
        let failed_transient = m.batches_failed_transient.load(Ordering::Relaxed);
        metrics.push(Metric {
            name: "infmon.exporter.batches_failed".into(),
            description: String::new(),
            unit: "{batches}".into(),
            data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                Sum {
                    data_points: vec![
                        NumberDataPoint {
                            attributes: vec![kv_string("reason", "non_retryable")],
                            time_unix_nano: wall_ns,
                            value: Some(
                                opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                                    saturating_i64(failed_non_retryable),
                                ),
                            ),
                            ..Default::default()
                        },
                        NumberDataPoint {
                            attributes: vec![kv_string("reason", "transient")],
                            time_unix_nano: wall_ns,
                            value: Some(
                                opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                                    saturating_i64(failed_transient),
                                ),
                            ),
                            ..Default::default()
                        },
                    ],
                    aggregation_temporality: AggregationTemporality::Cumulative as i32,
                    is_monotonic: true,
                },
            )),
            metadata: Vec::new(),
        });

        metrics.push(make_sum_metric(
            "infmon.exporter.points_emitted",
            "{points}",
            m.points_emitted.load(Ordering::Relaxed),
            vec![],
            wall_ns,
        ));

        // points_dropped with reason
        metrics.push(make_sum_metric(
            "infmon.exporter.points_dropped",
            "{points}",
            m.points_dropped.load(Ordering::Relaxed),
            vec![kv_string("reason", "export_cap")],
            wall_ns,
        ));

        metrics.push(make_sum_metric(
            "infmon.exporter.attrs_truncated",
            "{attrs}",
            m.attrs_truncated.load(Ordering::Relaxed),
            vec![],
            wall_ns,
        ));

        // Gauges
        metrics.push(make_gauge_metric(
            "infmon.exporter.export_duration",
            "s",
            m.get_export_duration(),
            wall_ns,
        ));
        metrics.push(make_gauge_metric(
            "infmon.exporter.queue_depth",
            "{batches}",
            m.queue_depth.load(Ordering::Relaxed) as f64,
            wall_ns,
        ));

        (
            ExportMetricsServiceRequest {
                resource_metrics: vec![ResourceMetrics {
                    resource: Some(Resource {
                        attributes: self.resource_attrs.clone(),
                        dropped_attributes_count: 0,
                    }),
                    scope_metrics: vec![ScopeMetrics {
                        scope: Some(InstrumentationScope {
                            name: "infmon".into(),
                            version: option_env!("CARGO_PKG_VERSION").unwrap_or("0.0.0").into(),
                            attributes: Vec::new(),
                            dropped_attributes_count: 0,
                        }),
                        metrics,
                        schema_url: String::new(),
                    }],
                    schema_url: String::new(),
                }],
            },
            points_emitted_count,
        )
    }
}

impl Exporter for OtlpExporter {
    fn kind(&self) -> &'static str {
        "otlp"
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn export(&self, snap: Arc<FlowStatsSnapshot>) -> BoxFuture<'_, Result<(), ExporterError>> {
        Box::pin(async move {
            let (request, points_built) = self.build_request(&snap);

            let mut client = self.get_client().await.map_err(ExporterError::Transient)?;

            let start = std::time::Instant::now();
            match client.export(request).await {
                Ok(_) => {
                    let elapsed = start.elapsed().as_secs_f64();
                    self.metrics.set_export_duration(elapsed);
                    self.metrics.batches_sent.fetch_add(1, Ordering::Relaxed);
                    self.metrics
                        .points_emitted
                        .fetch_add(points_built, Ordering::Relaxed);
                    Ok(())
                }
                Err(e) => {
                    let elapsed = start.elapsed().as_secs_f64();
                    self.metrics.set_export_duration(elapsed);
                    // Classify gRPC status codes
                    let err = match e.code() {
                        tonic::Code::Unavailable
                        | tonic::Code::DeadlineExceeded
                        | tonic::Code::ResourceExhausted
                        | tonic::Code::Aborted => {
                            // Clear cached client on transient errors so next tick reconnects
                            self.clear_client().await;
                            ExporterError::Transient(Box::new(e))
                        }
                        tonic::Code::Unauthenticated
                        | tonic::Code::PermissionDenied
                        | tonic::Code::InvalidArgument
                        | tonic::Code::Unimplemented => ExporterError::Permanent(Box::new(e)),
                        _ => {
                            self.clear_client().await;
                            ExporterError::Transient(Box::new(e))
                        }
                    };
                    Err(err)
                }
            }
        })
    }

    fn reload(&self, _cfg: &ExporterConfig) -> Result<(), ConfigError> {
        // OTLP exporter doesn't support live reload — endpoint, resource attrs,
        // and caps are immutable after construction. Return an error so callers
        // know a restart is required.
        Err(ConfigError(
            "OTLP exporter requires restart to apply config changes".into(),
        ))
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            // Drop the client to close the gRPC channel.
            let mut guard = self.client.lock().await;
            *guard = None;
            log::info!("OTLP exporter '{}' shut down", self.name);
        })
    }

    fn metrics(&self) -> Option<Arc<ExporterMetrics>> {
        Some(self.metrics.clone())
    }
}

// ── Registration ───────────────────────────────────────────────────

inventory::submit!(ExporterRegistration {
    kind: "otlp",
    factory: |cfg| Ok(Box::new(OtlpExporter::new(cfg)?)),
});

// ── Helpers ────────────────────────────────────────────────────────

/// Saturating u64 → i64 conversion: clamps at i64::MAX instead of wrapping.
fn saturating_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Build a cumulative monotonic `Sum` metric.
fn make_sum_metric(
    name: &str,
    unit: &str,
    value: u64,
    attrs: Vec<KeyValue>,
    time_unix_nano: u64,
) -> Metric {
    Metric {
        name: name.into(),
        description: String::new(),
        unit: unit.into(),
        data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
            Sum {
                data_points: vec![NumberDataPoint {
                    attributes: attrs,
                    time_unix_nano,
                    value: Some(
                        opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                            saturating_i64(value),
                        ),
                    ),
                    ..Default::default()
                }],
                aggregation_temporality: AggregationTemporality::Cumulative as i32,
                is_monotonic: true,
            },
        )),
        metadata: Vec::new(),
    }
}

/// Build a `Gauge` metric with a double value.
fn make_gauge_metric(name: &str, unit: &str, value: f64, time_unix_nano: u64) -> Metric {
    Metric {
        name: name.into(),
        description: String::new(),
        unit: unit.into(),
        data: Some(
            opentelemetry_proto::tonic::metrics::v1::metric::Data::Gauge(Gauge {
                data_points: vec![NumberDataPoint {
                    attributes: vec![],
                    time_unix_nano,
                    value: Some(
                        opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsDouble(
                            value,
                        ),
                    ),
                    ..Default::default()
                }],
            }),
        ),
        metadata: Vec::new(),
    }
}

/// Build a `KeyValue` with a string value.
fn kv_string(key: &str, value: &str) -> KeyValue {
    kv_string_counted(key, value, None)
}

/// Build a `KeyValue` with a string value, optionally counting truncations.
fn kv_string_counted(key: &str, value: &str, counter: Option<&AtomicU64>) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(
                opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(
                    truncate_utf8_counted(value, ATTRIBUTE_LENGTH_CAP, counter).to_string(),
                ),
            ),
        }),
    }
}

/// Build a `KeyValue` with an integer value.
fn kv_int(key: &str, value: i64) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(opentelemetry_proto::tonic::common::v1::any_value::Value::IntValue(value)),
        }),
    }
}

/// Like [`truncate_utf8`] but optionally bumps an atomic counter on truncation.
fn truncate_utf8_counted<'a>(
    s: &'a str,
    max_bytes: usize,
    counter: Option<&AtomicU64>,
) -> std::borrow::Cow<'a, str> {
    if s.len() <= max_bytes {
        return std::borrow::Cow::Borrowed(s);
    }
    if let Some(c) = counter {
        c.fetch_add(1, Ordering::Relaxed);
    }
    // Leave room for the ellipsis (3 bytes for '…')
    let limit = max_bytes.saturating_sub(3);
    let mut end = limit;
    // Back up to a char boundary
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = s[..end].to_string();
    truncated.push('…');
    std::borrow::Cow::Owned(truncated)
}

/// Render an IP address in canonical text form.
/// IPv4-mapped IPv6 addresses (::ffff:a.b.c.d) are rendered as IPv4.
fn render_ip(addr: &IpAddr) -> String {
    match addr {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => {
            // Check for IPv4-mapped: ::ffff:x.x.x.x
            if let Some(v4) = v6.to_ipv4_mapped() {
                v4.to_string()
            } else {
                v6.to_string()
            }
        }
    }
}

/// Build per-flow attributes from the flow-rule's field list and the flow's key.
fn build_flow_attributes(
    fr: &FlowRuleStats,
    flow: &infmon_ipc::types::FlowStats,
    fields: &[FieldId],
    trunc_counter: Option<&AtomicU64>,
) -> Vec<KeyValue> {
    let mut attrs = Vec::with_capacity(fields.len() + 1);

    // Always include flow-rule name
    attrs.push(kv_string_counted("flow-rule", &fr.name, trunc_counter));

    // Map each field to its attribute, matching field position to key position
    for (i, field_id) in fields.iter().enumerate() {
        if let Some(value) = flow.key.get(i) {
            match (field_id, value) {
                (FieldId::SrcIp, FieldValue::Ip(addr)) => {
                    attrs.push(kv_string_counted(
                        "flow.src_ip",
                        &render_ip(addr),
                        trunc_counter,
                    ));
                }
                (FieldId::DstIp, FieldValue::Ip(addr)) => {
                    attrs.push(kv_string_counted(
                        "flow.dst_ip",
                        &render_ip(addr),
                        trunc_counter,
                    ));
                }
                (FieldId::MirrorSrcIp, FieldValue::Ip(addr)) => {
                    attrs.push(kv_string_counted(
                        "flow.mirror_src_ip",
                        &render_ip(addr),
                        trunc_counter,
                    ));
                }
                (FieldId::IpProto, FieldValue::Proto(p)) => {
                    attrs.push(kv_int("flow.ip_proto", *p as i64));
                }
                (FieldId::Dscp, FieldValue::Dscp(d)) => {
                    attrs.push(kv_int("flow.dscp", *d as i64));
                }
                _ => {} // field/value type mismatch — skip
            }
        }
    }

    // Sort by key for stable wire representation (spec §4.2)
    attrs.sort_by(|a, b| a.key.cmp(&b.key));
    attrs
}

/// Load or create a persistent instance ID (spec §5: service.instance.id).
/// The path is configurable via the `instance_id_path` extra config key;
/// defaults to `/var/lib/infmon/instance_id`.
fn load_or_create_instance_id(path: &str) -> String {
    if let Ok(id) = std::fs::read_to_string(path) {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return id;
        }
    }
    // Generate a new UUID v4
    let id = uuid::Uuid::new_v4().to_string();
    // Try to persist it (best-effort; may fail if dir doesn't exist or no perms)
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, &id);
    id
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use infmon_ipc::types::{FlowCounters, FlowRuleCounters, FlowStats};

    fn make_config() -> ExporterConfig {
        let mut extra = std::collections::HashMap::new();
        extra.insert("endpoint".to_string(), "localhost:4317".to_string());
        ExporterConfig {
            kind: "otlp".into(),
            name: "test-otlp".into(),
            extra,
            ..Default::default()
        }
    }

    #[test]
    fn new_requires_endpoint() {
        let cfg = ExporterConfig {
            kind: "otlp".into(),
            name: "test".into(),
            ..Default::default()
        };
        assert!(OtlpExporter::new(&cfg).is_err());
    }

    #[test]
    fn new_succeeds_with_endpoint() {
        let cfg = make_config();
        let exporter = OtlpExporter::new(&cfg).unwrap();
        assert_eq!(exporter.kind(), "otlp");
        assert_eq!(exporter.name(), "test-otlp");
    }

    #[test]
    fn truncate_utf8_no_truncation() {
        let s = "hello";
        assert_eq!(truncate_utf8_counted(s, 256, None).as_ref(), "hello");
    }

    #[test]
    fn truncate_utf8_truncates() {
        let s = "a".repeat(300);
        let result = truncate_utf8_counted(&s, 256, None);
        assert!(result.len() <= 256);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn truncate_utf8_respects_char_boundaries() {
        // '€' is 3 bytes in UTF-8
        let s = "€".repeat(100); // 300 bytes
        let result = truncate_utf8_counted(&s, 10, None);
        // Should truncate to a valid UTF-8 string
        assert!(result.len() <= 10);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn render_ip_v4() {
        let addr: IpAddr = "192.168.1.1".parse().unwrap();
        assert_eq!(render_ip(&addr), "192.168.1.1");
    }

    #[test]
    fn render_ip_v6() {
        let addr: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(render_ip(&addr), "2001:db8::1");
    }

    #[test]
    fn render_ip_v4_mapped_v6() {
        let v6: std::net::Ipv6Addr = "::ffff:192.168.1.1".parse().unwrap();
        let addr = IpAddr::V6(v6);
        assert_eq!(render_ip(&addr), "192.168.1.1");
    }

    #[test]
    fn build_request_empty_snapshot() {
        let cfg = make_config();
        let exporter = OtlpExporter::new(&cfg).unwrap();
        let snap = FlowStatsSnapshot {
            tick_id: 1,
            wall_clock_ns: 1_000_000_000,
            monotonic_ns: 1_000_000_000,
            interval_ns: 1_000_000_000,
            flow_rules: vec![],
        };
        let req = exporter.build_request(&snap).0;
        assert_eq!(req.resource_metrics.len(), 1);
        let rm = &req.resource_metrics[0];
        assert!(rm.resource.is_some());
        assert_eq!(rm.scope_metrics.len(), 1);
        assert_eq!(rm.scope_metrics[0].metrics.len(), 9); // 9 self-observability metrics only
    }

    #[test]
    fn build_request_with_flows() {
        let cfg = make_config();
        let exporter = OtlpExporter::new(&cfg).unwrap();
        let snap = FlowStatsSnapshot {
            tick_id: 1,
            wall_clock_ns: 1_000_000_000,
            monotonic_ns: 1_000_000_000,
            interval_ns: 1_000_000_000,
            flow_rules: vec![FlowRuleStats {
                name: "test-rule".into(),
                fields: vec![FieldId::SrcIp, FieldId::DstIp],
                flows: vec![FlowStats {
                    key: vec![
                        FieldValue::Ip("10.0.0.1".parse().unwrap()),
                        FieldValue::Ip("10.0.0.2".parse().unwrap()),
                    ],
                    counters: FlowCounters {
                        packets: 100,
                        bytes: 5000,
                        first_seen_ns: 500_000_000,
                        last_seen_ns: 900_000_000,
                    },
                }],
                counters: FlowRuleCounters {
                    evictions: 0,
                    drops: 0,
                    packets: 100,
                    bytes: 5000,
                },
            }],
        };
        let req = exporter.build_request(&snap).0;
        let metrics = &req.resource_metrics[0].scope_metrics[0].metrics;

        // 3 per-flow + 5 per-flow-rule + 9 self-observability = 17
        assert_eq!(metrics.len(), 17);

        // Check metric names
        let names: Vec<&str> = metrics.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"infmon.flow.packets"));
        assert!(names.contains(&"infmon.flow.bytes"));
        assert!(names.contains(&"infmon.flow.last_seen"));
        assert!(names.contains(&"infmon.flow-rule.flows"));
        assert!(names.contains(&"infmon.flow-rule.evictions"));
        assert!(names.contains(&"infmon.flow-rule.drops"));
        assert!(names.contains(&"infmon.flow-rule.packets"));
        assert!(names.contains(&"infmon.flow-rule.bytes"));
    }

    #[test]
    fn build_request_cardinality_cap() {
        let mut extra = std::collections::HashMap::new();
        extra.insert("endpoint".to_string(), "localhost:4317".to_string());
        // Cap at 21 points: 5 per-flow-rule + 16 remaining = 5 flows (3 pts each = 15, cap 5)
        extra.insert("max_export_points_per_tick".to_string(), "21".to_string());
        let cfg = ExporterConfig {
            kind: "otlp".into(),
            name: "test-cap".into(),
            extra,
            ..Default::default()
        };
        let exporter = OtlpExporter::new(&cfg).unwrap();

        let mut flows = Vec::new();
        for i in 0..10 {
            flows.push(FlowStats {
                key: vec![FieldValue::Ip(format!("10.0.0.{}", i).parse().unwrap())],
                counters: FlowCounters {
                    packets: 10,
                    bytes: 100,
                    first_seen_ns: 0,
                    last_seen_ns: 0,
                },
            });
        }

        let snap = FlowStatsSnapshot {
            tick_id: 1,
            wall_clock_ns: 1_000_000_000,
            monotonic_ns: 1_000_000_000,
            interval_ns: 1_000_000_000,
            flow_rules: vec![FlowRuleStats {
                name: "big-rule".into(),
                fields: vec![FieldId::SrcIp],
                flows,
                counters: FlowRuleCounters::default(),
            }],
        };

        let req = exporter.build_request(&snap).0;
        let metrics = &req.resource_metrics[0].scope_metrics[0].metrics;

        // Count per-flow metrics (should be capped to 2 flows)
        let flow_metric_count = metrics
            .iter()
            .filter(|m| m.name.starts_with("infmon.flow."))
            .count();
        // 3 grouped Metric objects (packets, bytes, last_seen) each with 5 data points
        assert_eq!(flow_metric_count, 3);

        // Verify capped data point count: 5 flows × 3 metrics = 15 data points total
        let flow_data_points: usize = metrics
            .iter()
            .filter(|m| m.name.starts_with("infmon.flow."))
            .map(|m| match &m.data {
                Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(s)) => {
                    s.data_points.len()
                }
                Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Gauge(g)) => {
                    g.data_points.len()
                }
                _ => 0,
            })
            .sum();
        assert_eq!(flow_data_points, 15);

        // Plus 5 per-flow-rule metrics
        let fr_metric_count = metrics
            .iter()
            .filter(|m| m.name.starts_with("infmon.flow-rule."))
            .count();
        assert_eq!(fr_metric_count, 5);
    }

    #[test]
    fn resource_attributes_include_required() {
        let cfg = make_config();
        let exporter = OtlpExporter::new(&cfg).unwrap();
        let attr_keys: Vec<&str> = exporter
            .resource_attrs
            .iter()
            .map(|kv| kv.key.as_str())
            .collect();

        assert!(attr_keys.contains(&"service.name"));
        assert!(attr_keys.contains(&"service.namespace"));
        assert!(attr_keys.contains(&"service.version"));
        assert!(attr_keys.contains(&"host.arch"));
        assert!(attr_keys.contains(&"service.instance.id"));
    }

    #[test]
    fn resource_attrs_from_extra_config() {
        let mut extra = std::collections::HashMap::new();
        extra.insert("endpoint".to_string(), "localhost:4317".to_string());
        extra.insert(
            "resource.infmon.dpu.id".to_string(),
            "bf3-rack17".to_string(),
        );
        let cfg = ExporterConfig {
            kind: "otlp".into(),
            name: "test-res".into(),
            extra,
            ..Default::default()
        };
        let exporter = OtlpExporter::new(&cfg).unwrap();
        let has_dpu_id = exporter
            .resource_attrs
            .iter()
            .any(|kv| kv.key == "infmon.dpu.id");
        assert!(has_dpu_id);
    }

    #[test]
    fn flow_attributes_only_for_declared_fields() {
        // A flow-rule with only Dscp should produce only flow-rule + flow.dscp attrs
        let fr = FlowRuleStats {
            name: "dscp-only".into(),
            fields: vec![FieldId::Dscp],
            flows: vec![],
            counters: FlowRuleCounters::default(),
        };
        let flow = FlowStats {
            key: vec![FieldValue::Dscp(46)],
            counters: FlowCounters::default(),
        };
        let attrs = build_flow_attributes(&fr, &flow, &fr.fields, None);

        let keys: Vec<&str> = attrs.iter().map(|kv| kv.key.as_str()).collect();
        assert!(keys.contains(&"flow-rule"));
        assert!(keys.contains(&"flow.dscp"));
        assert!(!keys.contains(&"flow.src_ip"));
        assert!(!keys.contains(&"flow.dst_ip"));
    }

    #[test]
    fn flow_attributes_sorted_by_key() {
        let fr = FlowRuleStats {
            name: "multi".into(),
            fields: vec![
                FieldId::SrcIp,
                FieldId::DstIp,
                FieldId::IpProto,
                FieldId::Dscp,
            ],
            flows: vec![],
            counters: FlowRuleCounters::default(),
        };
        let flow = FlowStats {
            key: vec![
                FieldValue::Ip("10.0.0.1".parse().unwrap()),
                FieldValue::Ip("10.0.0.2".parse().unwrap()),
                FieldValue::Proto(6),
                FieldValue::Dscp(0),
            ],
            counters: FlowCounters::default(),
        };
        let attrs = build_flow_attributes(&fr, &flow, &fr.fields, None);
        let keys: Vec<&str> = attrs.iter().map(|kv| kv.key.as_str()).collect();

        // Should be sorted
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn self_observability_metrics_present_in_empty_snapshot() {
        let cfg = make_config();
        let exporter = OtlpExporter::new(&cfg).unwrap();
        let snap = FlowStatsSnapshot {
            tick_id: 1,
            wall_clock_ns: 1_000_000_000,
            monotonic_ns: 1_000_000_000,
            interval_ns: 1_000_000_000,
            flow_rules: vec![],
        };
        let req = exporter.build_request(&snap).0;
        let metrics = &req.resource_metrics[0].scope_metrics[0].metrics;
        let names: Vec<&str> = metrics.iter().map(|m| m.name.as_str()).collect();

        // All 9 self-observability metrics must be present
        assert!(names.contains(&"infmon.exporter.ticks_dropped"));
        assert!(names.contains(&"infmon.exporter.batches_sent"));
        assert!(names.contains(&"infmon.exporter.batches_dropped"));
        assert!(names.contains(&"infmon.exporter.batches_failed"));
        assert!(names.contains(&"infmon.exporter.points_emitted"));
        assert!(names.contains(&"infmon.exporter.points_dropped"));
        assert!(names.contains(&"infmon.exporter.attrs_truncated"));
        assert!(names.contains(&"infmon.exporter.export_duration"));
        assert!(names.contains(&"infmon.exporter.queue_depth"));
    }

    #[test]
    fn self_observability_no_flow_attributes() {
        let cfg = make_config();
        let exporter = OtlpExporter::new(&cfg).unwrap();
        let snap = FlowStatsSnapshot {
            tick_id: 1,
            wall_clock_ns: 1_000_000_000,
            monotonic_ns: 1_000_000_000,
            interval_ns: 1_000_000_000,
            flow_rules: vec![],
        };
        let req = exporter.build_request(&snap).0;
        let metrics = &req.resource_metrics[0].scope_metrics[0].metrics;

        // Self-observability metrics must NOT have flow-rule or flow attributes
        for m in metrics {
            if m.name.starts_with("infmon.exporter.") {
                let points = match &m.data {
                    Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(s)) => {
                        &s.data_points
                    }
                    Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Gauge(g)) => {
                        &g.data_points
                    }
                    _ => continue,
                };
                for dp in points {
                    for attr in &dp.attributes {
                        assert_ne!(
                            attr.key, "flow-rule",
                            "self-obs metric {} has flow-rule attr",
                            m.name
                        );
                        assert!(
                            !attr.key.starts_with("flow."),
                            "self-obs metric {} has flow attr {}",
                            m.name,
                            attr.key
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn self_observability_points_emitted_tracks_correctly() {
        let cfg = make_config();
        let exporter = OtlpExporter::new(&cfg).unwrap();
        let snap = FlowStatsSnapshot {
            tick_id: 1,
            wall_clock_ns: 1_000_000_000,
            monotonic_ns: 1_000_000_000,
            interval_ns: 1_000_000_000,
            flow_rules: vec![FlowRuleStats {
                name: "test-rule".into(),
                fields: vec![FieldId::SrcIp],
                flows: vec![FlowStats {
                    key: vec![FieldValue::Ip("10.0.0.1".parse().unwrap())],
                    counters: FlowCounters {
                        packets: 100,
                        bytes: 5000,
                        first_seen_ns: 500_000_000,
                        last_seen_ns: 900_000_000,
                    },
                }],
                counters: FlowRuleCounters::default(),
            }],
        };

        // First build: 1 flow × 3 per-flow + 5 per-flow-rule = 8 data points
        let (_, points1) = exporter.build_request(&snap);
        assert_eq!(points1, 8);
        // points_emitted is NOT incremented by build_request (only on successful export)
        assert_eq!(
            exporter
                .metrics
                .points_emitted
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );

        // Second build: also 8 points
        let (_, points2) = exporter.build_request(&snap);
        assert_eq!(points2, 8);
    }

    #[test]
    fn self_observability_points_dropped_on_cap() {
        let mut extra = std::collections::HashMap::new();
        extra.insert("endpoint".to_string(), "localhost:4317".to_string());
        // Cap at 8 points: 5 flow-rule + 3 remaining = 1 flow max
        extra.insert("max_export_points_per_tick".to_string(), "8".to_string());
        let cfg = ExporterConfig {
            kind: "otlp".into(),
            name: "test-drop".into(),
            extra,
            ..Default::default()
        };
        let exporter = OtlpExporter::new(&cfg).unwrap();

        let snap = FlowStatsSnapshot {
            tick_id: 1,
            wall_clock_ns: 1_000_000_000,
            monotonic_ns: 1_000_000_000,
            interval_ns: 1_000_000_000,
            flow_rules: vec![FlowRuleStats {
                name: "test-rule".into(),
                fields: vec![FieldId::SrcIp],
                flows: vec![
                    FlowStats {
                        key: vec![FieldValue::Ip("10.0.0.1".parse().unwrap())],
                        counters: FlowCounters::default(),
                    },
                    FlowStats {
                        key: vec![FieldValue::Ip("10.0.0.2".parse().unwrap())],
                        counters: FlowCounters::default(),
                    },
                    FlowStats {
                        key: vec![FieldValue::Ip("10.0.0.3".parse().unwrap())],
                        counters: FlowCounters::default(),
                    },
                ],
                counters: FlowRuleCounters::default(),
            }],
        };

        let _ = exporter.build_request(&snap);
        // 3 flows, but cap allows only 1 → 2 dropped × 3 points_per_flow = 6
        assert_eq!(
            exporter
                .metrics
                .points_dropped
                .load(std::sync::atomic::Ordering::Relaxed),
            6
        );
    }

    #[test]
    fn self_observability_batches_failed_has_reason() {
        let cfg = make_config();
        let exporter = OtlpExporter::new(&cfg).unwrap();
        let snap = FlowStatsSnapshot {
            tick_id: 1,
            wall_clock_ns: 1_000_000_000,
            monotonic_ns: 1_000_000_000,
            interval_ns: 1_000_000_000,
            flow_rules: vec![],
        };
        let req = exporter.build_request(&snap).0;
        let metrics = &req.resource_metrics[0].scope_metrics[0].metrics;
        let bf = metrics
            .iter()
            .find(|m| m.name == "infmon.exporter.batches_failed")
            .unwrap();
        if let Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(s)) = &bf.data {
            assert_eq!(s.data_points.len(), 2);
            let reasons: Vec<&str> = s.data_points.iter().map(|dp| {
                dp.attributes.iter().find(|a| a.key == "reason").unwrap()
                    .value.as_ref().unwrap()
                    .value.as_ref().map(|v| match v {
                        opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s) => s.as_str(),
                        _ => "",
                    }).unwrap_or("")
            }).collect();
            assert!(reasons.contains(&"non_retryable"));
            assert!(reasons.contains(&"transient"));
        } else {
            panic!("batches_failed should be a Sum");
        }
    }

    #[test]
    fn self_observability_metrics_returns_some() {
        let cfg = make_config();
        let exporter = OtlpExporter::new(&cfg).unwrap();
        assert!(exporter.metrics().is_some());
    }
}
