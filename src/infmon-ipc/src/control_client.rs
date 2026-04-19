use std::path::{Path, PathBuf};

use crate::error::CtlError;
use crate::types::*;

// Re-export FlowRule from infmon-config so CLI/frontend use one type
pub use infmon_config::model::{EvictionPolicy, Field, FlowRule as FlowRuleDef};

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
    /// Connect to the backend control socket.
    pub async fn connect(path: &Path) -> Result<Self, CtlError> {
        if !path.exists() {
            return Err(CtlError::Connect(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("control socket not found: {}", path.display()),
            )));
        }

        Ok(Self {
            socket_path: path.to_path_buf(),
        })
    }

    /// Add a flow rule to the backend.
    pub async fn flow_rule_add(&self, def: FlowRuleDef) -> Result<FlowRuleId, CtlError> {
        let _ = (&self.socket_path, &def);
        Err(CtlError::Connect(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "not connected to backend (stub)",
        )))
    }

    /// Remove a flow rule by name.
    pub async fn flow_rule_rm(&self, name: &str) -> Result<(), CtlError> {
        let _ = (&self.socket_path, name);
        Err(CtlError::Connect(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "not connected to backend (stub)",
        )))
    }

    /// List all flow rules.
    pub async fn flow_rule_list(&self) -> Result<Vec<FlowRuleDef>, CtlError> {
        let _ = &self.socket_path;
        Err(CtlError::Connect(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "not connected to backend (stub)",
        )))
    }

    /// Show detailed stats for one flow rule.
    pub async fn flow_rule_show(&self, name: &str) -> Result<FlowRuleStats, CtlError> {
        let _ = (&self.socket_path, name);
        Err(CtlError::Connect(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "not connected to backend (stub)",
        )))
    }

    /// Trigger a config reload on the frontend.
    pub async fn reload(&self) -> Result<(), CtlError> {
        let _ = &self.socket_path;
        Err(CtlError::Connect(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "not connected to backend (stub)",
        )))
    }

    /// List all exporters and their status.
    pub async fn exporter_list(&self) -> Result<Vec<ExporterStatus>, CtlError> {
        let _ = &self.socket_path;
        Err(CtlError::Connect(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "not connected to backend (stub)",
        )))
    }

    /// Path to the control socket.
    pub fn path(&self) -> &Path {
        &self.socket_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_nonexistent_socket() {
        let result = InFMonControlClient::connect(Path::new("/tmp/nonexistent.sock")).await;
        assert!(matches!(result, Err(CtlError::Connect(_))));
    }
}
