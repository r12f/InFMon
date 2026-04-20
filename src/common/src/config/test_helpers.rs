use super::*;

pub fn make_rule(name: &str, fields: Vec<Field>, max_keys: u32) -> FlowRule {
    FlowRule {
        name: name.to_string(),
        fields,
        max_keys,
        eviction_policy: EvictionPolicy::LruDrop,
    }
}
