use super::*;

#[test]
fn flow_rule_id_display_parse_roundtrip() {
    let id = FlowRuleId {
        hi: 0x0123456789abcdef,
        lo: 0xfedcba9876543210,
    };
    let s = id.to_string();
    assert_eq!(s, "0123456789abcdef-fedcba9876543210");
    let parsed: FlowRuleId = s.parse().unwrap();
    assert_eq!(parsed, id);
}

#[test]
fn flow_rule_id_parse_invalid() {
    assert!("not-a-valid-id".parse::<FlowRuleId>().is_err());
    assert!("nodashatall".parse::<FlowRuleId>().is_err());
}
