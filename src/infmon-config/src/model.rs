use serde::{Deserialize, Serialize};

/// Flow field identifiers (v1 field set)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum Field {
    /// Source IP address (16 bytes — IPv6-sized; IPv4 addresses are mapped to IPv6)
    SrcIp,
    /// Destination IP address (16 bytes — IPv6-sized; IPv4 addresses are mapped to IPv6)
    DstIp,
    /// IP protocol number (1 byte)
    IpProto,
    /// DSCP value (1 byte)
    Dscp,
    /// Mirror source IP address (16 bytes — IPv6-sized)
    MirrorSrcIp,
}

impl Field {
    /// Byte width of this field in a key
    pub fn width(self) -> u32 {
        match self {
            Field::SrcIp | Field::DstIp | Field::MirrorSrcIp => 16,
            Field::IpProto | Field::Dscp => 1,
        }
    }
}

/// Eviction policy (v1: only lru_drop)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum EvictionPolicy {
    LruDrop,
}

/// A single flow-rule definition
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowRule {
    pub name: String,
    pub fields: Vec<Field>,
    pub max_keys: u32,
    pub eviction_policy: EvictionPolicy,
}

/// Top-level config (flow-rules section only for now)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    pub flow_rules: Vec<FlowRule>,
}
