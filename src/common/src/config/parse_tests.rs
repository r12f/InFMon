use super::*;

const VALID_YAML: &str = r#"
flow-rules:
  - name: by_5tuple_l3
    fields: [mirror_src_ip, src_ip, dst_ip, ip_proto, dscp]
    max_keys: 1048576
    eviction_policy: lru_drop
  - name: by_dscp
    fields: [dscp]
    max_keys: 64
    eviction_policy: lru_drop
"#;

fn make_rule(name: &str, fields: Vec<Field>, max_keys: u32) -> FlowRule {
    FlowRule {
        name: name.to_string(),
        fields,
        max_keys,
        eviction_policy: EvictionPolicy::LruDrop,
    }
}

#[test]
fn parse_valid_yaml_two_rules() {
    let config = parse_yaml(VALID_YAML).unwrap();
    assert_eq!(config.flow_rules.len(), 2);
    assert_eq!(config.flow_rules[0].name, "by_5tuple_l3");
    assert_eq!(config.flow_rules[1].name, "by_dscp");
    assert_eq!(config.flow_rules[0].fields.len(), 5);
}

#[test]
fn parse_valid_yaml_one_rule() {
    let yaml = r#"
flow-rules:
  - name: simple
    fields: [src_ip]
    max_keys: 100
    eviction_policy: lru_drop
"#;
    let config = parse_yaml(yaml).unwrap();
    assert_eq!(config.flow_rules.len(), 1);
}

#[test]
fn roundtrip_parse_validate_crud() {
    let config = parse_yaml(VALID_YAML).unwrap();
    validate_config(&config).unwrap();
    let set = FlowRuleSet::from_config(&config).unwrap();
    assert_eq!(set.list().len(), 2);
    assert_eq!(set.show("by_dscp").unwrap().max_keys, 64);
}

#[test]
fn parse_yaml_file_and_load() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(VALID_YAML.as_bytes()).unwrap();
    drop(f);

    let config = parse_yaml_file(&path).unwrap();
    assert_eq!(config.flow_rules.len(), 2);

    let set = load_config(&path).unwrap();
    assert_eq!(set.list().len(), 2);
}

#[test]
fn parse_full_yaml_with_frontend_and_exporters() {
    let yaml = r#"
frontend:
  polling_interval_ms: 500
  backend_socket: "/run/infmon/backend.sock"
  control_socket: "/run/infmon/frontend.sock"
  vpp_stats_socket: "/run/vpp/stats.sock"
  startup_timeout: "10s"
  shutdown_grace_ms: 3000

flow-rules:
  - name: by-src
    fields: [src_ip]
    max_keys: 1000
    eviction_policy: lru_drop

exporters:
  - type: "otlp"
    name: "primary"
    endpoint: "http://collector.local:4317"
    export_timeout: "800ms"
    queue_depth: 2
    on_overflow: "drop_newest"
"#;
    let config = parse_yaml(yaml).unwrap();
    validate_config(&config).unwrap();
    assert!(config.frontend.is_some());
    assert_eq!(config.frontend.unwrap().polling_interval_ms, 500);
    assert_eq!(config.exporters.as_ref().unwrap().len(), 1);
    assert_eq!(config.exporters.as_ref().unwrap()[0].kind, "otlp");
    assert_eq!(
        config.exporters.as_ref().unwrap()[0]
            .extra
            .get("endpoint")
            .and_then(|v| v.as_str()),
        Some("http://collector.local:4317")
    );
}
