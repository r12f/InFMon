pub mod crud;
pub mod model;
pub mod parse;
pub mod validate;

pub use crud::{CrudError, FlowRuleSet, FLOW_RULE_SET_MAX};
pub use model::{Config, EvictionPolicy, Field, FlowRule};
pub use parse::{load_config, parse_yaml, parse_yaml_file, ConfigError, ParseError};
pub use validate::{
    validate_config, validate_rule, ValidationError, MAX_KEYS_BUDGET, MAX_KEY_WIDTH,
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
            flow_rules: vec![
                make_rule("ab", vec![Field::SrcIp], 10),
                make_rule("ab", vec![Field::DstIp], 10),
            ],
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
            flow_rules: vec![
                make_rule("rule-a", vec![Field::SrcIp], MAX_KEYS_BUDGET),
                make_rule("rule-b", vec![Field::DstIp], 1),
            ],
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
