use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

/// Identifies a flow rule (128-bit UUID from the backend)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FlowRuleId {
    pub hi: u64,
    pub lo: u64,
}

impl fmt::Display for FlowRuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}-{:016x}", self.hi, self.lo)
    }
}

impl FromStr for FlowRuleId {
    type Err = String;

    /// Parse a `FlowRuleId` from a "{hi:016x}-{lo:016x}" string.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (hi_str, lo_str) = s
            .split_once('-')
            .ok_or_else(|| format!("invalid FlowRuleId: missing '-' in '{s}'"))?;
        let hi =
            u64::from_str_radix(hi_str, 16).map_err(|e| format!("invalid FlowRuleId hi: {e}"))?;
        let lo =
            u64::from_str_radix(lo_str, 16).map_err(|e| format!("invalid FlowRuleId lo: {e}"))?;
        Ok(Self { hi, lo })
    }
}

/// Field identifiers matching the backend's infmon_field_type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FieldId {
    SrcIp = 0,
    DstIp = 1,
    IpProto = 2,
    Dscp = 3,
    MirrorSrcIp = 4,
}

/// A decoded field value from raw key bytes
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    Ip(IpAddr),
    Proto(u8),
    Dscp(u8),
}

/// Per-flow counters
#[derive(Debug, Clone, Copy, Default)]
pub struct FlowCounters {
    pub packets: u64,
    pub bytes: u64,
    /// Monotonic nanoseconds (same clock as `FlowStatsSnapshot::monotonic_ns`)
    pub first_seen_ns: u64,
    /// Monotonic nanoseconds (same clock as `FlowStatsSnapshot::monotonic_ns`)
    pub last_seen_ns: u64,
}

/// A single decoded flow
#[derive(Debug, Clone)]
pub struct FlowStats {
    pub key: Vec<FieldValue>,
    pub counters: FlowCounters,
}

/// Per-flow-rule aggregate counters
#[derive(Debug, Clone, Copy, Default)]
pub struct FlowRuleCounters {
    pub evictions: u64,
    pub drops: u64,
    pub packets: u64,
    pub bytes: u64,
}

/// Stats for one flow rule in a snapshot.
///
/// Uses owned `String`/`Vec` for simplicity. If profiling shows allocation
/// overhead from repeated snapshots, consider `Arc<str>`/`Arc<[FieldId]>` for
/// metadata that stays constant across ticks.
#[derive(Debug, Clone)]
pub struct FlowRuleStats {
    pub name: String,
    pub fields: Vec<FieldId>,
    pub flows: Vec<FlowStats>,
    pub counters: FlowRuleCounters,
}

#[cfg(test)]
#[path = "types_tests.rs"]
mod types_tests;

/// One tick's worth of snapshot data
#[derive(Debug, Clone)]
pub struct FlowStatsSnapshot {
    pub tick_id: u64,
    pub wall_clock_ns: u64,
    pub monotonic_ns: u64,
    pub interval_ns: u64,
    pub flow_rules: Vec<FlowRuleStats>,
}
