//! JSON-line protocol for CLI ↔ frontend control socket.
//!
//! Each message is a single JSON object terminated by `\n`.
//! The client sends a [`Request`], the server replies with a [`Response`].

use serde::{Deserialize, Serialize};

use crate::config::model::{EvictionPolicy, Field, FlowRule};

// ── Requests ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    FlowRuleAdd(FlowRuleAddParams),
    FlowRuleRm(FlowRuleRmParams),
    FlowRuleList,
    FlowRuleShow(FlowRuleShowParams),
    StatsShow(StatsShowParams),
    StatsPull,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlowRuleAddParams {
    pub name: String,
    pub fields: Vec<Field>,
    pub max_keys: u32,
    pub eviction_policy: EvictionPolicy,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlowRuleRmParams {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlowRuleShowParams {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatsShowParams {
    pub name: Option<String>,
}

// ── Responses ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<ResponseData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorData>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorData {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponseData {
    FlowRuleId(FlowRuleIdData),
    FlowRuleList(FlowRuleListData),
    FlowRuleDetail(FlowRuleDetailData),
    StatsShow(StatsShowData),
    StatsPull(StatsPullData),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlowRuleListData {
    pub rules: Vec<FlowRuleData>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlowRuleIdData {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowRuleData {
    pub name: String,
    pub fields: Vec<Field>,
    pub max_keys: u32,
    pub eviction_policy: EvictionPolicy,
}

impl From<&FlowRule> for FlowRuleData {
    fn from(r: &FlowRule) -> Self {
        Self {
            name: r.name.clone(),
            fields: r.fields.clone(),
            max_keys: r.max_keys,
            eviction_policy: r.eviction_policy,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlowRuleDetailData {
    pub name: String,
    pub fields: Vec<Field>,
    pub max_keys: u32,
    pub eviction_policy: EvictionPolicy,
    pub counters: FlowRuleCountersData,
    pub flows: Vec<FlowEntryData>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlowRuleCountersData {
    pub packets: u64,
    pub bytes: u64,
    pub evictions: u64,
    pub drops: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlowEntryData {
    pub key: Vec<String>,
    pub packets: u64,
    pub bytes: u64,
    pub first_seen_ns: u64,
    pub last_seen_ns: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatsShowData {
    pub flow_rules: Vec<FlowRuleStatsData>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlowRuleStatsData {
    pub name: String,
    pub packets: u64,
    pub bytes: u64,
    pub evictions: u64,
    pub drops: u64,
    pub active_flows: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatsPullData {
    pub tick_id: u64,
    pub wall_clock_ns: u64,
    #[serde(flatten)]
    pub stats: StatsShowData,
}

// ── Helpers ───────────────────────────────────────────────────────────

impl Response {
    pub fn ok(data: ResponseData) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn ok_empty() -> Self {
        Self {
            ok: true,
            data: None,
            error: None,
        }
    }

    pub fn err(code: i32, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(ErrorData {
                code,
                message: message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flow_rule_list_roundtrip() {
        let req = Request::FlowRuleList;
        let json = serde_json::to_string(&req).unwrap();
        eprintln!("Serialized FlowRuleList: {json}");
        let _back: Request = serde_json::from_str(&json).unwrap();
    }
}
