use super::*;
use infmon_common::ipc::types::{FlowCounters, FlowRuleCounters, FlowStats};

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
        let reasons: Vec<&str> = s
            .data_points
            .iter()
            .map(|dp| {
                dp.attributes
                    .iter()
                    .find(|a| a.key == "reason")
                    .unwrap()
                    .value
                    .as_ref()
                    .unwrap()
                    .value
                    .as_ref()
                    .map(|v| match v {
                        opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(
                            s,
                        ) => s.as_str(),
                        _ => "",
                    })
                    .unwrap_or("")
            })
            .collect();
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
