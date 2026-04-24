use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::error::CtlError;
use super::protocol::*;
use super::types::{
    FieldId, FieldValue, FlowCounters, FlowRuleCounters, FlowRuleId, FlowRuleStats, FlowStats,
};

// Re-export FlowRule from infmon-config so CLI/frontend use one type
pub use crate::config::model::{EvictionPolicy, Field, FlowRule as FlowRuleDef};

/// Status of a single exporter
#[derive(Debug, Clone)]
pub struct ExporterStatus {
    pub name: String,
    pub kind: String,
    pub healthy: bool,
    pub ticks_exported: u64,
    pub ticks_dropped: u64,
}

pub struct InFMonControlClient {
    socket_path: PathBuf,
    timeout: Duration,
}

impl InFMonControlClient {
    /// Create a new control client for the given socket path.
    ///
    /// Does not open the socket — connection is deferred to the first RPC call,
    /// avoiding TOCTOU races on the path.
    pub fn new(path: &Path) -> Self {
        Self {
            socket_path: path.to_path_buf(),
            timeout: Duration::from_secs(5),
        }
    }

    /// Create a new control client with a custom timeout.
    pub fn with_timeout(path: &Path, timeout: Duration) -> Self {
        Self {
            socket_path: path.to_path_buf(),
            timeout,
        }
    }

    /// Send a request, receive a response, and check for errors.
    /// Returns the optional response data on success, or a `CtlError` on failure.
    async fn rpc_ok(&self, request: &Request) -> Result<Option<ResponseData>, CtlError> {
        let resp = self.rpc(request).await?;
        if !resp.ok {
            let err = resp.error.unwrap_or(ErrorData {
                code: -1,
                message: "unknown error".into(),
            });
            return Err(CtlError::Backend {
                code: err.code,
                message: err.message,
            });
        }
        Ok(resp.data)
    }

    /// Send a request and receive a response over the Unix socket.
    async fn rpc(&self, request: &Request) -> Result<Response, CtlError> {
        let stream = tokio::time::timeout(self.timeout, UnixStream::connect(&self.socket_path))
            .await
            .map_err(|_| {
                CtlError::Connect(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connection timed out",
                ))
            })?
            .map_err(CtlError::Connect)?;

        let (reader, mut writer) = stream.into_split();

        let mut line = serde_json::to_string(request)
            .map_err(|e| CtlError::Protocol(format!("serialize request: {e}")))?;
        line.push('\n');

        tokio::time::timeout(self.timeout, writer.write_all(line.as_bytes()))
            .await
            .map_err(|_| {
                CtlError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "write timed out",
                ))
            })?
            .map_err(CtlError::Io)?;

        // Cap response read at 64 KiB to prevent OOM from a buggy/compromised server.
        const MAX_RESPONSE: u64 = 64 * 1024;
        let limited_reader = reader.take(MAX_RESPONSE);
        let mut buf_reader = BufReader::new(limited_reader);
        let mut response_line = String::new();
        tokio::time::timeout(self.timeout, buf_reader.read_line(&mut response_line))
            .await
            .map_err(|_| {
                CtlError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "read timed out",
                ))
            })?
            .map_err(CtlError::Io)?;

        let response: Response = serde_json::from_str(&response_line)
            .map_err(|e| CtlError::Protocol(format!("deserialize response: {e}")))?;

        Ok(response)
    }

    /// Add a flow rule to the backend.
    pub async fn flow_rule_add(&self, def: FlowRuleDef) -> Result<FlowRuleId, CtlError> {
        let request = Request::FlowRuleAdd(FlowRuleAddParams {
            name: def.name,
            fields: def.fields,
            max_keys: def.max_keys,
            eviction_policy: def.eviction_policy,
        });
        match self.rpc_ok(&request).await? {
            Some(ResponseData::FlowRuleId(id_data)) => id_data
                .id
                .parse::<FlowRuleId>()
                .map_err(|e| CtlError::Protocol(format!("invalid FlowRuleId: {e}"))),
            _ => Err(CtlError::Protocol("unexpected response data".into())),
        }
    }

    /// Remove a flow rule by name.
    pub async fn flow_rule_rm(&self, name: &str) -> Result<(), CtlError> {
        let request = Request::FlowRuleRm(FlowRuleRmParams {
            name: name.to_string(),
        });
        self.rpc_ok(&request).await?;
        Ok(())
    }

    /// List all flow rules.
    pub async fn flow_rule_list(&self) -> Result<Vec<FlowRuleDef>, CtlError> {
        let request = Request::FlowRuleList;
        match self.rpc_ok(&request).await? {
            Some(ResponseData::FlowRuleList(list)) => Ok(list
                .rules
                .into_iter()
                .map(|r| FlowRuleDef {
                    name: r.name,
                    fields: r.fields,
                    max_keys: r.max_keys,
                    eviction_policy: r.eviction_policy,
                })
                .collect()),
            _ => Err(CtlError::Protocol("unexpected response data".into())),
        }
    }

    /// Show detailed stats for one flow rule.
    pub async fn flow_rule_show(&self, name: &str) -> Result<FlowRuleStats, CtlError> {
        let request = Request::FlowRuleShow(FlowRuleShowParams {
            name: name.to_string(),
        });
        match self.rpc_ok(&request).await? {
            Some(ResponseData::FlowRuleDetail(detail)) => Ok(FlowRuleStats {
                name: detail.name,
                fields: detail.fields.iter().filter_map(field_to_field_id).collect(),
                max_keys: detail.max_keys,
                eviction_policy: detail.eviction_policy,
                flows: detail
                    .flows
                    .into_iter()
                    .map(|f| FlowStats {
                        key: f.key.iter().map(|_k| FieldValue::Proto(0)).collect(), // simplified: wire keys are strings, lossless round-trip deferred
                        counters: FlowCounters {
                            packets: f.packets,
                            bytes: f.bytes,
                            first_seen_ns: f.first_seen_ns,
                            last_seen_ns: f.last_seen_ns,
                        },
                    })
                    .collect(),
                counters: FlowRuleCounters {
                    packets: detail.counters.packets,
                    bytes: detail.counters.bytes,
                    evictions: detail.counters.evictions,
                    drops: detail.counters.drops,
                },
            }),
            _ => Err(CtlError::Protocol("unexpected response data".into())),
        }
    }

    /// Show aggregate stats (optionally filtered by name).
    pub async fn stats_show(&self, name: Option<&str>) -> Result<StatsShowData, CtlError> {
        let request = Request::StatsShow(StatsShowParams {
            name: name.map(|s| s.to_string()),
        });
        match self.rpc_ok(&request).await? {
            Some(ResponseData::StatsShow(data)) => Ok(data),
            _ => Err(CtlError::Protocol("unexpected response data".into())),
        }
    }

    /// Trigger an immediate snapshot pull from the poller.
    pub async fn stats_pull(&self) -> Result<StatsPullData, CtlError> {
        let request = Request::StatsPull;
        match self.rpc_ok(&request).await? {
            Some(ResponseData::StatsPull(data)) => Ok(data),
            _ => Err(CtlError::Protocol("unexpected response data".into())),
        }
    }

    /// Trigger a config reload on the frontend.
    pub async fn reload(&self) -> Result<(), CtlError> {
        let _ = &self.socket_path;
        Err(CtlError::Request("not implemented (stub)".into()))
    }

    /// List all exporters and their status.
    pub async fn exporter_list(&self) -> Result<Vec<ExporterStatus>, CtlError> {
        let _ = &self.socket_path;
        Err(CtlError::Request("not implemented (stub)".into()))
    }

    /// Path to the control socket.
    pub fn path(&self) -> &Path {
        &self.socket_path
    }
}

fn field_to_field_id(f: &Field) -> Option<FieldId> {
    match f {
        Field::SrcIp => Some(FieldId::SrcIp),
        Field::DstIp => Some(FieldId::DstIp),
        Field::IpProto => Some(FieldId::IpProto),
        Field::Dscp => Some(FieldId::Dscp),
        Field::MirrorSrcIp => Some(FieldId::MirrorSrcIp),
        Field::SrcPort => Some(FieldId::SrcPort),
        Field::DstPort => Some(FieldId::DstPort),
    }
}

#[cfg(test)]
#[path = "control_client_tests.rs"]
mod control_client_tests;
