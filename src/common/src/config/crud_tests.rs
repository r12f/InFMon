use super::test_helpers::make_rule;
use super::*;

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
