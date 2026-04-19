pub mod crud;
pub mod model;
pub mod parse;
pub mod validate;

pub use crud::{CrudError, FlowRuleSet, FLOW_RULE_SET_MAX};
pub use model::{Config, EvictionPolicy, ExporterEntry, Field, FlowRule, FrontendConfig};
pub use parse::{load_config, parse_yaml, parse_yaml_file, ConfigError, ParseError};
pub use validate::{
    validate_config, validate_exporter, validate_frontend, validate_rule, ValidationError,
    KNOWN_EXPORTER_TYPES, MAX_KEYS_BUDGET, MAX_KEY_WIDTH, VALID_OVERFLOW_POLICIES,
};

#[cfg(test)]
mod tests {
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
    fn reject_duplicate_names() {
        let config = Config {
            frontend: None,
            flow_rules: vec![
                make_rule("ab", vec![Field::SrcIp], 10),
                make_rule("ab", vec![Field::DstIp], 10),
            ],
            exporters: None,
        };
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::DuplicateName("ab".into()))
        );
    }

    #[test]
    fn reject_empty_fields() {
        let rule = make_rule("ab", vec![], 10);
        assert_eq!(
            validate_rule(&rule),
            Err(ValidationError::EmptyFields { name: "ab".into() })
        );
    }

    #[test]
    fn reject_duplicate_fields() {
        let rule = make_rule("ab", vec![Field::SrcIp, Field::SrcIp], 10);
        assert_eq!(
            validate_rule(&rule),
            Err(ValidationError::DuplicateField {
                name: "ab".into(),
                field: Field::SrcIp,
            })
        );
    }

    #[test]
    fn reject_max_keys_zero() {
        let rule = make_rule("ab", vec![Field::SrcIp], 0);
        assert_eq!(
            validate_rule(&rule),
            Err(ValidationError::ZeroMaxKeys { name: "ab".into() })
        );
    }

    #[test]
    fn accept_within_key_width_limit() {
        // 16+16+16+1+1 = 50 bytes, within the 64-byte limit.
        // v1 fields cannot exceed 64 bytes (max is 3×16+1+1=50).
        let rule = make_rule(
            "ab",
            vec![
                Field::SrcIp,
                Field::DstIp,
                Field::MirrorSrcIp,
                Field::IpProto,
                Field::Dscp,
            ],
            10,
        );
        assert!(validate_rule(&rule).is_ok());
    }

    #[test]
    fn reject_total_budget_exceeded() {
        let config = Config {
            frontend: None,
            flow_rules: vec![
                make_rule("rule-a", vec![Field::SrcIp], MAX_KEYS_BUDGET),
                make_rule("rule-b", vec![Field::DstIp], 1),
            ],
            exporters: None,
        };
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::BudgetExceeded {
                total: MAX_KEYS_BUDGET as u64 + 1,
            })
        );
    }

    #[test]
    fn reject_invalid_name_too_short() {
        let rule = make_rule("a", vec![Field::SrcIp], 10);
        assert!(matches!(
            validate_rule(&rule),
            Err(ValidationError::InvalidName { .. })
        ));
    }

    #[test]
    fn reject_invalid_name_too_long() {
        let name = "a".repeat(32);
        let rule = make_rule(&name, vec![Field::SrcIp], 10);
        assert!(matches!(
            validate_rule(&rule),
            Err(ValidationError::InvalidName { .. })
        ));
    }

    #[test]
    fn reject_invalid_name_uppercase() {
        let rule = make_rule("AB", vec![Field::SrcIp], 10);
        assert!(matches!(
            validate_rule(&rule),
            Err(ValidationError::InvalidName { .. })
        ));
    }

    #[test]
    fn reject_invalid_name_special_chars() {
        let rule = make_rule("a.b", vec![Field::SrcIp], 10);
        assert!(matches!(
            validate_rule(&rule),
            Err(ValidationError::InvalidName { .. })
        ));
    }

    #[test]
    fn crud_add_list_show() {
        let mut set = FlowRuleSet::new(MAX_KEYS_BUDGET);
        let rule = make_rule("ab", vec![Field::SrcIp], 10);
        set.add(rule.clone()).unwrap();
        assert_eq!(set.list().len(), 1);
        assert_eq!(set.show("ab").unwrap(), &rule);
    }

    #[test]
    fn crud_rm() {
        let mut set = FlowRuleSet::new(MAX_KEYS_BUDGET);
        set.add(make_rule("ab", vec![Field::SrcIp], 10)).unwrap();
        let removed = set.rm("ab").unwrap();
        assert_eq!(removed.name, "ab");
        assert!(set.list().is_empty());
    }

    #[test]
    fn crud_add_duplicate_fails() {
        let mut set = FlowRuleSet::new(MAX_KEYS_BUDGET);
        set.add(make_rule("ab", vec![Field::SrcIp], 10)).unwrap();
        assert!(matches!(
            set.add(make_rule("ab", vec![Field::DstIp], 10)),
            Err(CrudError::NameExists(_))
        ));
    }

    #[test]
    fn crud_rm_not_found() {
        let mut set = FlowRuleSet::new(MAX_KEYS_BUDGET);
        assert!(matches!(set.rm("nope"), Err(CrudError::NotFound(_))));
    }

    #[test]
    fn crud_budget_enforcement() {
        let mut set = FlowRuleSet::new(100);
        set.add(make_rule("ab", vec![Field::SrcIp], 90)).unwrap();
        assert!(matches!(
            set.add(make_rule("cd", vec![Field::DstIp], 20)),
            Err(CrudError::BudgetExceeded { .. })
        ));
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
    fn crud_set_full() {
        let mut set = FlowRuleSet::new(MAX_KEYS_BUDGET);
        for i in 0..FLOW_RULE_SET_MAX {
            set.add(make_rule(&format!("rule-{i:02}"), vec![Field::SrcIp], 1))
                .unwrap();
        }
        assert!(matches!(
            set.add(make_rule("overflow", vec![Field::SrcIp], 1)),
            Err(CrudError::SetFull { .. })
        ));
    }
}

#[cfg(test)]
mod frontend_config_tests {
    use super::*;
    use std::collections::HashMap;

    fn make_valid_config() -> Config {
        Config {
            frontend: Some(FrontendConfig::default()),
            flow_rules: vec![FlowRule {
                name: "test-rule".to_string(),
                fields: vec![Field::SrcIp],
                max_keys: 100,
                eviction_policy: EvictionPolicy::LruDrop,
            }],
            exporters: Some(vec![ExporterEntry {
                kind: "otlp".into(),
                name: "primary".into(),
                queue_depth: 2,
                export_timeout: "800ms".into(),
                on_overflow: "drop_newest".into(),
                extra: HashMap::from([(
                    "endpoint".to_string(),
                    serde_yaml::Value::String("http://collector:4317".into()),
                )]),
            }]),
        }
    }

    #[test]
    fn valid_full_config() {
        let config = make_valid_config();
        validate_config(&config).unwrap();
    }

    #[test]
    fn reject_zero_polling_interval() {
        let mut config = make_valid_config();
        config.frontend.as_mut().unwrap().polling_interval_ms = 0;
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::ZeroPollingInterval)
        );
    }

    #[test]
    fn reject_zero_shutdown_grace() {
        let mut config = make_valid_config();
        config.frontend.as_mut().unwrap().shutdown_grace_ms = 0;
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::ZeroShutdownGrace)
        );
    }

    #[test]
    fn reject_invalid_startup_timeout() {
        let mut config = make_valid_config();
        config.frontend.as_mut().unwrap().startup_timeout = "abc".into();
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::InvalidStartupTimeout {
                value: "abc".into()
            })
        );
    }

    #[test]
    fn reject_unknown_exporter_type() {
        let mut config = make_valid_config();
        config.exporters.as_mut().unwrap()[0].kind = "prometheus".into();
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::UnknownExporterType {
                name: "primary".into(),
                kind: "prometheus".into(),
                known: "otlp".into(),
            })
        );
    }

    #[test]
    fn reject_empty_exporter_name() {
        let mut config = make_valid_config();
        config.exporters.as_mut().unwrap()[0].name = "".into();
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::EmptyExporterName { index: 0 })
        );
    }

    #[test]
    fn reject_empty_exporter_type() {
        let mut config = make_valid_config();
        config.exporters.as_mut().unwrap()[0].kind = "".into();
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::EmptyExporterType {
                name: "primary".into()
            })
        );
    }

    #[test]
    fn reject_duplicate_exporter_names() {
        let mut config = make_valid_config();
        let dup = config.exporters.as_ref().unwrap()[0].clone();
        config.exporters.as_mut().unwrap().push(dup);
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::DuplicateExporterName("primary".into()))
        );
    }

    #[test]
    fn reject_zero_queue_depth() {
        let mut config = make_valid_config();
        config.exporters.as_mut().unwrap()[0].queue_depth = 0;
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::ZeroQueueDepth {
                name: "primary".into()
            })
        );
    }

    #[test]
    fn reject_queue_depth_too_large() {
        let mut config = make_valid_config();
        config.exporters.as_mut().unwrap()[0].queue_depth = 10_001;
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::QueueDepthTooLarge {
                name: "primary".into(),
                depth: 10_001,
                max: 10_000,
            })
        );
    }

    #[test]
    fn reject_invalid_export_timeout() {
        let mut config = make_valid_config();
        config.exporters.as_mut().unwrap()[0].export_timeout = "forever".into();
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::InvalidExportTimeout {
                name: "primary".into(),
                value: "forever".into(),
            })
        );
    }

    #[test]
    fn reject_invalid_overflow_policy() {
        let mut config = make_valid_config();
        config.exporters.as_mut().unwrap()[0].on_overflow = "drop_oldest".into();
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::InvalidOverflowPolicy {
                name: "primary".into(),
                policy: "drop_oldest".into(),
                valid: "drop_newest".into(),
            })
        );
    }

    #[test]
    fn reject_zero_startup_timeout() {
        let mut config = make_valid_config();
        config.frontend.as_mut().unwrap().startup_timeout = "0s".into();
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::ZeroStartupTimeout)
        );
    }

    #[test]
    fn reject_zero_export_timeout() {
        let mut config = make_valid_config();
        config.exporters.as_mut().unwrap()[0].export_timeout = "0ms".into();
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::ZeroExportTimeout {
                name: "primary".into()
            })
        );
    }

    #[test]
    fn reject_invalid_exporter_name() {
        let mut config = make_valid_config();
        config.exporters.as_mut().unwrap()[0].name = "A".into();
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::InvalidExporterName { name: "A".into() })
        );
    }

    #[test]
    fn reject_otlp_missing_endpoint() {
        let mut config = make_valid_config();
        config.exporters.as_mut().unwrap()[0]
            .extra
            .remove("endpoint");
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::MissingOtlpEndpoint {
                name: "primary".into()
            })
        );
    }

    #[test]
    fn reject_otlp_empty_endpoint() {
        let mut config = make_valid_config();
        config.exporters.as_mut().unwrap()[0]
            .extra
            .insert("endpoint".to_string(), serde_yaml::Value::String("".into()));
        assert_eq!(
            validate_config(&config),
            Err(ValidationError::MissingOtlpEndpoint {
                name: "primary".into()
            })
        );
    }

    #[test]
    fn accept_no_frontend_section() {
        let mut config = make_valid_config();
        config.frontend = None;
        validate_config(&config).unwrap();
    }

    #[test]
    fn accept_no_exporters_section() {
        let mut config = make_valid_config();
        config.exporters = None;
        validate_config(&config).unwrap();
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
}
