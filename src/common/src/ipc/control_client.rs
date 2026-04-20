use std::path::{Path, PathBuf};

use super::error::CtlError;
use super::types::{FlowRuleId, FlowRuleStats};

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
}

impl InFMonControlClient {
    /// Create a new control client for the given socket path.
    ///
    /// Does not open the socket — connection is deferred to the first RPC call,
    /// avoiding TOCTOU races on the path.
    pub fn new(path: &Path) -> Self {
        Self {
            socket_path: path.to_path_buf(),
        }
    }

    /// Add a flow rule to the backend.
    pub async fn flow_rule_add(&self, def: FlowRuleDef) -> Result<FlowRuleId, CtlError> {
        let _ = (&self.socket_path, &def);
        Err(CtlError::Request("not implemented (stub)".into()))
    }

    /// Remove a flow rule by name.
    pub async fn flow_rule_rm(&self, name: &str) -> Result<(), CtlError> {
        let _ = (&self.socket_path, name);
        Err(CtlError::Request("not implemented (stub)".into()))
    }

    /// List all flow rules.
    pub async fn flow_rule_list(&self) -> Result<Vec<FlowRuleDef>, CtlError> {
        let _ = &self.socket_path;
        Err(CtlError::Request("not implemented (stub)".into()))
    }

    /// Show detailed stats for one flow rule.
    pub async fn flow_rule_show(&self, name: &str) -> Result<FlowRuleStats, CtlError> {
        let _ = (&self.socket_path, name);
        Err(CtlError::Request("not implemented (stub)".into()))
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

#[cfg(test)]
#[path = "control_client_tests.rs"]
mod control_client_tests;
