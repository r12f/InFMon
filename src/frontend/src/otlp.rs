//! OTLP/gRPC metrics exporter.
//!
//! Implements the [`Exporter`] trait and translates [`FlowStatsSnapshot`]
//! data into OTLP metrics, shipping them over gRPC (or HTTP/protobuf).
//!
//! Spec reference: `specs/006-exporter-otlp.md`

use std::net::IpAddr;
use std::sync::Arc;

use opentelemetry_proto::tonic::collector::metrics::v1::{
    metrics_service_client::MetricsServiceClient, ExportMetricsServiceRequest,
};
use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue};
use opentelemetry_proto::tonic::metrics::v1::{
    AggregationTemporality, Gauge, Metric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use tokio::sync::Mutex;
use tonic::transport::Channel;

use infmon_ipc::types::{FieldId, FieldValue, FlowRuleStats, FlowStatsSnapshot};

use crate::exporter::{
    BoxFuture, ConfigError, Exporter, ExporterConfig, ExporterError, ExporterRegistration,
};

// ── Constants ──────────────────────────────────────────────────────

/// Default per-tick point cap (spec §8.2).
const DEFAULT_MAX_EXPORT_POINTS_PER_TICK: u64 = 2_000_000;

/// Max length for string attribute values (spec §8.2).
const ATTRIBUTE_LENGTH_CAP: usize = 256;

/// Number of per-flow data points (packets, bytes, last_seen).
const POINTS_PER_FLOW: u64 = 3;

/// Number of per-flow-rule data points (flows, evictions, drops, packets, bytes).
#[allow(dead_code)]
const POINTS_PER_FLOW_RULE: u64 = 5;

// ── OTLP Exporter ──────────────────────────────────────────────────

/// OTLP/gRPC metrics exporter.
pub struct OtlpExporter {
    name: String,
    endpoint: String,
    max_export_points_per_tick: u64,
    resource_attrs: Vec<KeyValue>,
    client: Mutex<Option<MetricsServiceClient<Channel>>>,
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
        let instance_id = load_or_create_instance_id();
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
            max_export_points_per_tick,
            resource_attrs,
            client: Mutex::new(None),
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
        let endpoint =
            if self.endpoint.starts_with("http://") || self.endpoint.starts_with("https://") {
                self.endpoint.clone()
            } else {
                format!("http://{}", self.endpoint)
            };

        let channel = Channel::from_shared(endpoint)?.connect().await?;
        let client = MetricsServiceClient::new(channel);
        *guard = Some(client.clone());
        Ok(client)
    }

    /// Build the full OTLP request from a snapshot.
    fn build_request(&self, snap: &FlowStatsSnapshot) -> ExportMetricsServiceRequest {
        let wall_ns = snap.wall_clock_ns;

        // Calculate total flows and apply cardinality cap (spec §8.2).
        let total_flows: u64 = snap.flow_rules.iter().map(|fr| fr.flows.len() as u64).sum();

        let effective_cap = self.max_export_points_per_tick / POINTS_PER_FLOW.max(1);

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

        for (fr_idx, fr) in snap.flow_rules.iter().enumerate() {
            let budget = flow_budgets.get(fr_idx).copied().unwrap_or(0) as usize;
            let flows_to_emit = budget.min(fr.flows.len());

            // Per-flow data points
            for flow in fr.flows.iter().take(flows_to_emit) {
                let attrs = build_flow_attributes(fr, flow, &fr.fields);

                // infmon.flow.packets (Sum, cumulative, monotonic)
                metrics.push(Metric {
                    name: "infmon.flow.packets".into(),
                    description: "Total packets attributed to this flow".into(),
                    unit: "{packets}".into(),
                    data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                        Sum {
                            data_points: vec![NumberDataPoint {
                                attributes: attrs.clone(),
                                start_time_unix_nano: flow.counters.first_seen_ns,
                                time_unix_nano: wall_ns,
                                value: Some(
                                    opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                                        flow.counters.packets as i64,
                                    ),
                                ),
                                ..Default::default()
                            }],
                            aggregation_temporality:
                                AggregationTemporality::Cumulative as i32,
                            is_monotonic: true,
                        },
                    )),
                    metadata: Vec::new(),
                });

                // infmon.flow.bytes (Sum, cumulative, monotonic)
                metrics.push(Metric {
                    name: "infmon.flow.bytes".into(),
                    description: "Total bytes attributed to this flow".into(),
                    unit: "By".into(),
                    data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                        Sum {
                            data_points: vec![NumberDataPoint {
                                attributes: attrs.clone(),
                                start_time_unix_nano: flow.counters.first_seen_ns,
                                time_unix_nano: wall_ns,
                                value: Some(
                                    opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                                        flow.counters.bytes as i64,
                                    ),
                                ),
                                ..Default::default()
                            }],
                            aggregation_temporality:
                                AggregationTemporality::Cumulative as i32,
                            is_monotonic: true,
                        },
                    )),
                    metadata: Vec::new(),
                });

                // infmon.flow.last_seen (Gauge, ns)
                metrics.push(Metric {
                    name: "infmon.flow.last_seen".into(),
                    description: "Wall-clock ns of the most recent packet".into(),
                    unit: "ns".into(),
                    data: Some(
                        opentelemetry_proto::tonic::metrics::v1::metric::Data::Gauge(Gauge {
                            data_points: vec![NumberDataPoint {
                                attributes: attrs,
                                time_unix_nano: wall_ns,
                                value: Some(
                                    opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                                        flow.counters.last_seen_ns as i64,
                                    ),
                                ),
                                ..Default::default()
                            }],
                        }),
                    ),
                    metadata: Vec::new(),
                });
            }

            // Per-flow-rule observability points (spec §3.4)
            let fr_attrs = vec![kv_string("flow-rule", &fr.name)];

            // infmon.flow-rule.flows (Gauge)
            metrics.push(Metric {
                name: "infmon.flow-rule.flows".into(),
                description: "Number of live flows in this flow-rule".into(),
                unit: "{flows}".into(),
                data: Some(
                    opentelemetry_proto::tonic::metrics::v1::metric::Data::Gauge(Gauge {
                        data_points: vec![NumberDataPoint {
                            attributes: fr_attrs.clone(),
                            time_unix_nano: wall_ns,
                            value: Some(
                                opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                                    fr.flows.len() as i64,
                                ),
                            ),
                            ..Default::default()
                        }],
                    }),
                ),
                metadata: Vec::new(),
            });

            // infmon.flow-rule.evictions (Sum, cumulative)
            metrics.push(Metric {
                name: "infmon.flow-rule.evictions".into(),
                description: "Total evictions for this flow-rule".into(),
                unit: "{evictions}".into(),
                data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                    Sum {
                        data_points: vec![NumberDataPoint {
                            attributes: fr_attrs.clone(),
                            time_unix_nano: wall_ns,
                            value: Some(
                                opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                                    fr.counters.evictions as i64,
                                ),
                            ),
                            ..Default::default()
                        }],
                        aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        is_monotonic: true,
                    },
                )),
                metadata: Vec::new(),
            });

            // infmon.flow-rule.drops (Sum, cumulative) with reason attr
            let mut drops_attrs = fr_attrs.clone();
            drops_attrs.push(kv_string("reason", "eviction_failed"));
            metrics.push(Metric {
                name: "infmon.flow-rule.drops".into(),
                description: "Total drops for this flow-rule".into(),
                unit: "{drops}".into(),
                data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                    Sum {
                        data_points: vec![NumberDataPoint {
                            attributes: drops_attrs,
                            time_unix_nano: wall_ns,
                            value: Some(
                                opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                                    fr.counters.drops as i64,
                                ),
                            ),
                            ..Default::default()
                        }],
                        aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        is_monotonic: true,
                    },
                )),
                metadata: Vec::new(),
            });

            // infmon.flow-rule.packets (Sum, cumulative)
            metrics.push(Metric {
                name: "infmon.flow-rule.packets".into(),
                description: "Total packets for this flow-rule".into(),
                unit: "{packets}".into(),
                data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                    Sum {
                        data_points: vec![NumberDataPoint {
                            attributes: fr_attrs.clone(),
                            time_unix_nano: wall_ns,
                            value: Some(
                                opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                                    fr.counters.packets as i64,
                                ),
                            ),
                            ..Default::default()
                        }],
                        aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        is_monotonic: true,
                    },
                )),
                metadata: Vec::new(),
            });

            // infmon.flow-rule.bytes (Sum, cumulative)
            metrics.push(Metric {
                name: "infmon.flow-rule.bytes".into(),
                description: "Total bytes for this flow-rule".into(),
                unit: "By".into(),
                data: Some(opentelemetry_proto::tonic::metrics::v1::metric::Data::Sum(
                    Sum {
                        data_points: vec![NumberDataPoint {
                            attributes: fr_attrs,
                            time_unix_nano: wall_ns,
                            value: Some(
                                opentelemetry_proto::tonic::metrics::v1::number_data_point::Value::AsInt(
                                    fr.counters.bytes as i64,
                                ),
                            ),
                            ..Default::default()
                        }],
                        aggregation_temporality: AggregationTemporality::Cumulative as i32,
                        is_monotonic: true,
                    },
                )),
                metadata: Vec::new(),
            });
        }

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
        }
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
            let request = self.build_request(&snap);

            let mut client = self.get_client().await.map_err(ExporterError::Transient)?;

            client.export(request).await.map_err(|e| {
                // Classify gRPC status codes
                match e.code() {
                    tonic::Code::Unavailable
                    | tonic::Code::DeadlineExceeded
                    | tonic::Code::ResourceExhausted
                    | tonic::Code::Aborted => ExporterError::Transient(Box::new(e)),
                    tonic::Code::Unauthenticated
                    | tonic::Code::PermissionDenied
                    | tonic::Code::InvalidArgument
                    | tonic::Code::Unimplemented => ExporterError::Permanent(Box::new(e)),
                    _ => ExporterError::Transient(Box::new(e)),
                }
            })?;

            Ok(())
        })
    }

    fn reload(&self, _cfg: &ExporterConfig) -> Result<(), ConfigError> {
        // Reload is not yet supported for OTLP; a restart is required.
        Ok(())
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {
            // Drop the client to close the gRPC channel.
            let mut guard = self.client.lock().await;
            *guard = None;
            log::info!("OTLP exporter '{}' shut down", self.name);
        })
    }
}

// ── Registration ───────────────────────────────────────────────────

inventory::submit!(ExporterRegistration {
    kind: "otlp",
    factory: |cfg| Ok(Box::new(OtlpExporter::new(cfg)?)),
});

// ── Helpers ────────────────────────────────────────────────────────

/// Build a `KeyValue` with a string value.
fn kv_string(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(
                opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(
                    truncate_utf8(value, ATTRIBUTE_LENGTH_CAP).to_string(),
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

/// Truncate a string to at most `max_bytes` bytes, respecting UTF-8 char
/// boundaries, appending `…` if truncated (spec §8.2).
fn truncate_utf8(s: &str, max_bytes: usize) -> std::borrow::Cow<'_, str> {
    if s.len() <= max_bytes {
        return std::borrow::Cow::Borrowed(s);
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
) -> Vec<KeyValue> {
    let mut attrs = Vec::with_capacity(fields.len() + 1);

    // Always include flow-rule name
    attrs.push(kv_string("flow-rule", &fr.name));

    // Map each field to its attribute, matching field position to key position
    for (i, field_id) in fields.iter().enumerate() {
        if let Some(value) = flow.key.get(i) {
            match (field_id, value) {
                (FieldId::SrcIp, FieldValue::Ip(addr)) => {
                    attrs.push(kv_string("flow.src_ip", &render_ip(addr)));
                }
                (FieldId::DstIp, FieldValue::Ip(addr)) => {
                    attrs.push(kv_string("flow.dst_ip", &render_ip(addr)));
                }
                (FieldId::MirrorSrcIp, FieldValue::Ip(addr)) => {
                    attrs.push(kv_string("flow.mirror_src_ip", &render_ip(addr)));
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
fn load_or_create_instance_id() -> String {
    let path = "/var/lib/infmon/instance_id";
    if let Ok(id) = std::fs::read_to_string(path) {
        let id = id.trim().to_string();
        if !id.is_empty() {
            return id;
        }
    }
    // Generate a new UUID v4
    let id = uuid::Uuid::new_v4().to_string();
    // Try to persist it (best-effort; may fail if dir doesn't exist or no perms)
    let _ = std::fs::create_dir_all("/var/lib/infmon");
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
        assert_eq!(truncate_utf8(s, 256).as_ref(), "hello");
    }

    #[test]
    fn truncate_utf8_truncates() {
        let s = "a".repeat(300);
        let result = truncate_utf8(&s, 256);
        assert!(result.len() <= 256);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn truncate_utf8_respects_char_boundaries() {
        // '€' is 3 bytes in UTF-8
        let s = "€".repeat(100); // 300 bytes
        let result = truncate_utf8(&s, 10);
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
        let req = exporter.build_request(&snap);
        assert_eq!(req.resource_metrics.len(), 1);
        let rm = &req.resource_metrics[0];
        assert!(rm.resource.is_some());
        assert_eq!(rm.scope_metrics.len(), 1);
        assert_eq!(rm.scope_metrics[0].metrics.len(), 0);
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
        let req = exporter.build_request(&snap);
        let metrics = &req.resource_metrics[0].scope_metrics[0].metrics;

        // 3 per-flow + 5 per-flow-rule = 8
        assert_eq!(metrics.len(), 8);

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
        // Cap at 6 points = 2 flows (3 points each)
        extra.insert("max_export_points_per_tick".to_string(), "6".to_string());
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

        let req = exporter.build_request(&snap);
        let metrics = &req.resource_metrics[0].scope_metrics[0].metrics;

        // Count per-flow metrics (should be capped)
        let flow_metric_count = metrics
            .iter()
            .filter(|m| m.name.starts_with("infmon.flow."))
            .count();
        // 2 flows * 3 metrics = 6 per-flow metrics
        assert_eq!(flow_metric_count, 6);

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
        let attrs = build_flow_attributes(&fr, &flow, &fr.fields);

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
        let attrs = build_flow_attributes(&fr, &flow, &fr.fields);
        let keys: Vec<&str> = attrs.iter().map(|kv| kv.key.as_str()).collect();

        // Should be sorted
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }
}
