use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Frontend daemon configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FrontendConfig {
    /// Polling interval in milliseconds (default: 1000)
    #[serde(default = "default_polling_interval_ms")]
    pub polling_interval_ms: u64,
    /// Path to backend control socket
    #[serde(default = "default_backend_socket")]
    pub backend_socket: String,
    /// Path to frontend control socket
    #[serde(default = "default_control_socket")]
    pub control_socket: String,
    /// Path to VPP stats segment socket
    #[serde(default = "default_vpp_stats_socket")]
    pub vpp_stats_socket: String,
    /// Startup timeout for backend connectivity (default: 5s)
    #[serde(default = "default_startup_timeout")]
    pub startup_timeout: String,
    /// Shutdown grace period in ms (default: 2000)
    #[serde(default = "default_shutdown_grace_ms")]
    pub shutdown_grace_ms: u64,
}

fn default_polling_interval_ms() -> u64 {
    1000
}
fn default_backend_socket() -> String {
    "/run/infmon/backend.sock".into()
}
fn default_control_socket() -> String {
    "/run/infmon/frontend.sock".into()
}
fn default_vpp_stats_socket() -> String {
    "/run/vpp/stats.sock".into()
}
fn default_startup_timeout() -> String {
    "5s".into()
}
fn default_shutdown_grace_ms() -> u64 {
    2000
}

impl Default for FrontendConfig {
    fn default() -> Self {
        Self {
            polling_interval_ms: 1000,
            backend_socket: default_backend_socket(),
            control_socket: default_control_socket(),
            vpp_stats_socket: default_vpp_stats_socket(),
            startup_timeout: default_startup_timeout(),
            shutdown_grace_ms: default_shutdown_grace_ms(),
        }
    }
}

/// Per-exporter configuration block
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExporterEntry {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(default = "default_queue_depth")]
    pub queue_depth: usize,
    #[serde(default = "default_export_timeout")]
    pub export_timeout: String,
    #[serde(default = "default_on_overflow")]
    pub on_overflow: String,
    /// Extra exporter-specific key-value pairs
    #[serde(flatten)]
    pub extra: HashMap<String, String>,
}

fn default_queue_depth() -> usize {
    2
}
fn default_export_timeout() -> String {
    "800ms".into()
}
fn default_on_overflow() -> String {
    "drop_newest".into()
}

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

/// Top-level config
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    #[serde(default)]
    pub frontend: Option<FrontendConfig>,
    pub flow_rules: Vec<FlowRule>,
    #[serde(default)]
    pub exporters: Option<Vec<ExporterEntry>>,
}
