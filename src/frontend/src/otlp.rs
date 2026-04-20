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
use rand::Rng;
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint};

use infmon_common::ipc::types::{FieldId, FieldValue, FlowRuleStats, FlowStatsSnapshot};

use crate::exporter::{
    BoxFuture, ConfigError, Exporter, ExporterConfig, ExporterError, ExporterMetrics,
    ExporterRegistration,
};

// ── Constants ──────────────────────────────────────────────────────

/// Default per-tick point cap (spec §8.2).
const DEFAULT_MAX_EXPORT_POINTS_PER_TICK: u64 = 2_000_000;

/// Default max data points per batch (spec §7.1 step 4).
const DEFAULT_MAX_BATCH_POINTS: usize = 8192;

/// Max length for string attribute values (spec §8.2).
const ATTRIBUTE_LENGTH_CAP: usize = 256;

/// Number of per-flow data points (packets, bytes, last_seen).
const POINTS_PER_FLOW: u64 = 3;

/// Number of per-flow-rule data points (flows, evictions, drops, packets, bytes).
const POINTS_PER_FLOW_RULE: u64 = 5;

/// Maximum retries per batch (spec §7.3).
const MAX_RETRIES: u32 = 3;

/// Base delay for exponential backoff (spec §7.3).
const RETRY_BASE: Duration = Duration::from_secs(1);

/// Cap for exponential backoff (spec §7.3).
const RETRY_CAP: Duration = Duration::from_secs(30);

// ── OTLP Exporter ──────────────────────────────────────────────────

/// OTLP/gRPC metrics exporter.
pub struct OtlpExporter {
    name: String,
    endpoint: String,
    #[allow(dead_code)] // stored for future reload support
    instance_id_path: Option<String>,
    max_export_points_per_tick: u64,
    max_batch_points: usize,
    export_timeout: Duration,
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

        let max_batch_points = cfg
            .extra
            .get("max_batch_points")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_MAX_BATCH_POINTS);

        let export_timeout = cfg.export_timeout;

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
            max_batch_points,
            export_timeout,
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
    /// Slice a single OTLP request into multiple batches, each containing
    /// at most `max_batch_points` data points (spec §7.1 step 4).
    fn slice_into_batches(
        &self,
        request: ExportMetricsServiceRequest,
    ) -> Vec<ExportMetricsServiceRequest> {
        if self.max_batch_points == 0 {
            log::warn!(
                "max_batch_points is 0 (batching disabled) — this is likely a misconfiguration"
            );
            return vec![request];
        }

        // Guard against empty request
        if request.resource_metrics.is_empty() {
            return vec![request];
        }

        if request.resource_metrics[0].scope_metrics.is_empty() {
            return vec![request];
        }

        // Count total data points
        let total_points: usize = request
            .resource_metrics
            .iter()
            .flat_map(|rm| &rm.scope_metrics)
            .flat_map(|sm| &sm.metrics)
            .map(count_metric_data_points)
            .sum();

        if total_points <= self.max_batch_points {
            return vec![request];
        }

        // NOTE: InFMon always produces a single ResourceMetrics with a single
        // ScopeMetrics. If multiple resources/scopes are ever needed, this
        // flattening must be updated to preserve per-resource/scope grouping.
        debug_assert!(
            request.resource_metrics.len() == 1
                && request.resource_metrics[0].scope_metrics.len() == 1,
            "slice_into_batches assumes single resource + single scope"
        );

        // Flatten all metrics, then partition into batches
        let resource = request.resource_metrics[0].resource.clone();
        let scope = request.resource_metrics[0].scope_metrics[0].scope.clone();
        let schema_url_rm = request.resource_metrics[0].schema_url.clone();
        let schema_url_sm = request.resource_metrics[0].scope_metrics[0]
            .schema_url
            .clone();

        let all_metrics: Vec<Metric> = request
            .resource_metrics
            .into_iter()
            .flat_map(|rm| rm.scope_metrics)
            .flat_map(|sm| sm.metrics)
            .collect();

        let mut batches = Vec::new();
        let mut current_metrics: Vec<Metric> = Vec::new();
        let mut current_points: usize = 0;

        for metric in all_metrics {
            let pts = count_metric_data_points(&metric);
            if pts == 0 {
                current_metrics.push(metric);
                continue;
            }

            // If this metric fits in the current batch, add it
            if current_points + pts <= self.max_batch_points {
                current_points += pts;
                current_metrics.push(metric);
            } else if pts <= self.max_batch_points && current_points > 0 {
                // Flush current batch, start new one with this metric
                batches.push(self.wrap_batch(
                    std::mem::take(&mut current_metrics),
                    resource.clone(),
                    scope.clone(),
                    schema_url_rm.clone(),
                    schema_url_sm.clone(),
                ));
                current_points = pts;
                current_metrics.push(metric);
            } else {
                // This single metric exceeds batch size — split its data points
                if !current_metrics.is_empty() {
                    batches.push(self.wrap_batch(
                        std::mem::take(&mut current_metrics),
                        resource.clone(),
                        scope.clone(),
                        schema_url_rm.clone(),
                        schema_url_sm.clone(),
                    ));
                    current_points = 0;
                }
                // Split the metric's data points across batches
                let split = split_metric_data_points(metric, self.max_batch_points);
                for (m, count) in split {
                    if current_points + count <= self.max_batch_points {
                        current_points += count;
                        current_metrics.push(m);
                    } else {
                        if !current_metrics.is_empty() {
                            batches.push(self.wrap_batch(
                                std::mem::take(&mut current_metrics),
                                resource.clone(),
                                scope.clone(),
                                schema_url_rm.clone(),
                                schema_url_sm.clone(),
                            ));
                        }
                        current_points = count;
                        current_metrics.push(m);
                    }
                }
            }
        }

        if !current_metrics.is_empty() {
            batches.push(self.wrap_batch(
                current_metrics,
                resource.clone(),
                scope.clone(),
                schema_url_rm,
                schema_url_sm,
            ));
        }

        if batches.is_empty() {
            batches.push(ExportMetricsServiceRequest {
                resource_metrics: vec![],
            });
        }

        batches
    }

    /// Wrap metrics into a full OTLP request.
    fn wrap_batch(
        &self,
        metrics: Vec<Metric>,
        resource: Option<Resource>,
        scope: Option<InstrumentationScope>,
        schema_url_rm: String,
        schema_url_sm: String,
    ) -> ExportMetricsServiceRequest {
        ExportMetricsServiceRequest {
            resource_metrics: vec![ResourceMetrics {
                resource,
                scope_metrics: vec![ScopeMetrics {
                    scope,
                    metrics,
                    schema_url: schema_url_sm,
                }],
                schema_url: schema_url_rm,
            }],
        }
    }

    /// Export a single batch with retry and exponential backoff (spec §7.3).
    ///
    /// Retries up to `MAX_RETRIES` times on transient/retryable errors.
    /// Uses exponential backoff with full jitter: `min(cap, base * 2^attempt) * rand(0,1)`.
    async fn export_batch_with_retry(
        &self,
        request: ExportMetricsServiceRequest,
    ) -> Result<(), ExporterError> {
        let mut last_err = None;

        for attempt in 0..=MAX_RETRIES {
            if attempt > 0 {
                // Exponential backoff with full jitter
                let backoff_max = RETRY_CAP.min(RETRY_BASE * 2u32.saturating_pow(attempt));
                let jitter = rand::thread_rng().gen_range(0.0..1.0);
                let delay = backoff_max.mul_f64(jitter);
                log::warn!(
                    "OTLP export retry {}/{} after {:.1}s backoff",
                    attempt,
                    MAX_RETRIES,
                    delay.as_secs_f64()
                );
                tokio::time::sleep(delay).await;
            }

            let client_result = self.get_client().await;
            let mut client = match client_result {
                Ok(c) => c,
                Err(e) => {
                    last_err = Some(ExporterError::Transient(e));
                    continue;
                }
            };

            let result =
                tokio::time::timeout(self.export_timeout, client.export(request.clone())).await;

            match result {
                Ok(Ok(_)) => {
                    return Ok(());
                }
                Ok(Err(e)) => {
                    let is_retryable = matches!(
                        e.code(),
                        tonic::Code::Unavailable
                            | tonic::Code::DeadlineExceeded
                            | tonic::Code::ResourceExhausted
                            | tonic::Code::Aborted
                    );
                    if is_retryable {
                        log::warn!(
                            "OTLP export attempt {}/{} failed with retryable gRPC code {:?}: {}",
                            attempt,
                            MAX_RETRIES,
                            e.code(),
                            e.message()
                        );
                        self.clear_client().await;
                        last_err = Some(ExporterError::Transient(Box::new(e)));
                        continue;
                    } else {
                        // Non-retryable: drop immediately
                        log::error!(
                            "OTLP export failed with permanent gRPC code {:?}: {}",
                            e.code(),
                            e.message()
                        );
                        return Err(ExporterError::Permanent(Box::new(e)));
                    }
                }
                Err(_elapsed) => {
                    // Timeout
                    self.clear_client().await;
                    last_err = Some(ExporterError::Timeout);
                    continue;
                }
            }
        }

        // All retries exhausted
        Err(last_err.unwrap_or(ExporterError::Timeout))
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

            // Slice into batches (spec §7.1 step 4)
            let batches = self.slice_into_batches(request);

            // Track points per batch for accurate partial-success accounting
            let batch_points: Vec<u64> = batches
                .iter()
                .map(|b| {
                    b.resource_metrics
                        .iter()
                        .flat_map(|rm| &rm.scope_metrics)
                        .flat_map(|sm| &sm.metrics)
                        .map(count_metric_data_points)
                        .sum::<usize>() as u64
                })
                .collect();

            let start = std::time::Instant::now();
            let mut failed = false;
            let mut points_emitted = 0u64;

            for (i, batch) in batches.into_iter().enumerate() {
                match self.export_batch_with_retry(batch).await {
                    Ok(()) => {
                        self.metrics.batches_sent.fetch_add(1, Ordering::Relaxed);
                        points_emitted += batch_points[i];
                    }
                    Err(ExporterError::Permanent(e)) => {
                        self.metrics
                            .batches_failed_non_retryable
                            .fetch_add(1, Ordering::Relaxed);
                        if points_emitted > 0 {
                            self.metrics
                                .points_emitted
                                .fetch_add(points_emitted, Ordering::Relaxed);
                        }
                        let elapsed = start.elapsed().as_secs_f64();
                        self.metrics.set_export_duration(elapsed);
                        return Err(ExporterError::Permanent(e));
                    }
                    Err(_) => {
                        self.metrics
                            .batches_failed_transient
                            .fetch_add(1, Ordering::Relaxed);
                        failed = true;
                        // Continue trying remaining batches — don't poison-pill
                    }
                }
            }

            let elapsed = start.elapsed().as_secs_f64();
            self.metrics.set_export_duration(elapsed);

            if !failed {
                self.metrics
                    .points_emitted
                    .fetch_add(points_built, Ordering::Relaxed);
                Ok(())
            } else {
                // Some batches failed — emit exact count for successfully sent batches.
                if points_emitted > 0 {
                    self.metrics
                        .points_emitted
                        .fetch_add(points_emitted, Ordering::Relaxed);
                }
                Err(ExporterError::Transient(
                    "some batches failed after retries".into(),
                ))
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

/// Count the number of data points in a single metric.
fn count_metric_data_points(m: &Metric) -> usize {
    match &m.data {
        Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(s)) => s.data_points.len(),
        Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Gauge(g)) => {
            g.data_points.len()
        }
        _ => 0,
    }
}

/// Split a metric's data points into chunks of at most `max_points`.
/// Returns a list of `(metric, point_count)` pairs.
fn split_metric_data_points(m: Metric, max_points: usize) -> Vec<(Metric, usize)> {
    let max_points = max_points.max(1);
    match m.data {
        Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(sum)) => {
            let chunks: Vec<Vec<NumberDataPoint>> = sum
                .data_points
                .chunks(max_points)
                .map(|c| c.to_vec())
                .collect();
            chunks
                .into_iter()
                .map(|dp| {
                    let count = dp.len();
                    (
                        Metric {
                            name: m.name.clone(),
                            description: m.description.clone(),
                            unit: m.unit.clone(),
                            data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                                Sum {
                                    data_points: dp,
                                    aggregation_temporality: sum.aggregation_temporality,
                                    is_monotonic: sum.is_monotonic,
                                },
                            )),
                            metadata: m.metadata.clone(),
                        },
                        count,
                    )
                })
                .collect()
        }
        Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Gauge(gauge)) => {
            let chunks: Vec<Vec<NumberDataPoint>> = gauge
                .data_points
                .chunks(max_points)
                .map(|c| c.to_vec())
                .collect();
            chunks
                .into_iter()
                .map(|dp| {
                    let count = dp.len();
                    (
                        Metric {
                            name: m.name.clone(),
                            description: m.description.clone(),
                            unit: m.unit.clone(),
                            data: Some(
                                opentelemetry_proto::tonic::metrics::v1::metric::Data::Gauge(
                                    Gauge { data_points: dp },
                                ),
                            ),
                            metadata: m.metadata.clone(),
                        },
                        count,
                    )
                })
                .collect()
        }
        _ => vec![(m, 0)],
    }
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
    flow: &infmon_common::ipc::types::FlowStats,
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

#[cfg(test)]
#[path = "otlp_tests.rs"]
mod otlp_tests;
