use super::types::*;

/// Raw snapshot data before decoding into FlowStatsSnapshot
#[derive(Debug)]
pub struct RawSnapshot {
    pub descriptors: Vec<RawDescriptor>,
}

/// Mirrors infmon_stats_descriptor_t from the C backend (96 bytes)
#[derive(Debug, Clone)]
pub struct RawDescriptor {
    pub flow_rule_id: FlowRuleId,
    pub flow_rule_index: u32,
    pub generation: u64,
    pub epoch_ns: u64,
    pub slots: Vec<RawSlot>,
    pub key_arena: Vec<u8>,
    pub insert_failed: u64,
    pub table_full: u64,
}

/// Mirrors infmon_slot_t (64 bytes)
#[derive(Debug, Clone)]
pub struct RawSlot {
    pub key_hash: u64,
    pub packets: u64,
    pub bytes: u64,
    pub key_offset: u32,
    pub key_len: u16,
    pub flags: u16,
    pub last_update: u64,
}

