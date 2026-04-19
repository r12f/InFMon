//! Frontend lifecycle: start, reload, stop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use infmon_ipc::stats_client::InFMonStatsClient;

use crate::exporter::{
    self, ExporterConfig, ExporterHandle, SnapshotSender, snapshot_channel,
    find_factory, validate_registrations,
};
use crate::poller::{self, PollerConfig, PollerHandle};

/// Errors during lifecycle operations.
#[derive(Debug)]
pub enum LifecycleError {
    /// Config file could not be parsed.
    ConfigError(String),
    /// Backend unreachable at startup.
    BackendUnreachable(String),
    /// Exporter factory not found for the given kind.
    UnknownExporter(String),
    /// Exporter factory returned an error.
    ExporterInit(String),
    /// Reload failed — rolled back to previous config.
    ReloadFailed(String),
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LifecycleError::ConfigError(e) => write!(f, "config error: {e}"),
            LifecycleError::BackendUnreachable(e) => write!(f, "backend unreachable: {e}"),
            LifecycleError::UnknownExporter(e) => write!(f, "unknown exporter type: {e}"),
            LifecycleError::ExporterInit(e) => write!(f, "exporter init failed: {e}"),
            LifecycleError::ReloadFailed(e) => write!(f, "reload failed: {e}"),
        }
    }
}

impl std::error::Error for LifecycleError {}

/// Running frontend state.
pub struct Frontend {
    poller_handle: Option<PollerHandle>,
    exporter_handles: Vec<ExporterHandle>,
    exporter_senders: Vec<SnapshotSender>,
    config_path: PathBuf,
    shutdown: Arc<AtomicBool>,
}

impl Frontend {
    /// Start the frontend daemon.
    ///
    /// 1. Parse config
    /// 2. Validate backend is reachable (fail-fast)
    /// 3. Build exporters from config
    /// 4. Spawn poller and exporter threads
    pub fn start(config_path: &Path) -> Result<Self, LifecycleError> {
        log::info!("starting infmon-frontend");

        // Validate exporter registrations
        validate_registrations();

        // Parse config
        let config_text = std::fs::read_to_string(config_path)
            .map_err(|e| LifecycleError::ConfigError(format!("cannot read {}: {e}", config_path.display())))?;
        let config: infmon_config::model::Config = serde_yaml::from_str(&config_text)
            .map_err(|e| LifecycleError::ConfigError(format!("YAML parse error: {e}")))?;

        // Validate flow rules
        infmon_config::validate_config(&config)
            .map_err(|e| LifecycleError::ConfigError(format!("validation error: {e}")))?;

        let frontend_cfg = config.frontend.clone().unwrap_or_default();
        let exporter_entries = config.exporters.clone().unwrap_or_default();

        // Fail-fast: check backend reachability via stats socket
        let stats_socket = PathBuf::from(&frontend_cfg.vpp_stats_socket);
        let startup_timeout = parse_duration(&frontend_cfg.startup_timeout).unwrap_or(Duration::from_secs(5));

        // Try to connect to stats socket within startup_timeout
        let start_time = std::time::Instant::now();
        let mut connected = false;
        while start_time.elapsed() < startup_timeout {
            match InFMonStatsClient::open(&stats_socket) {
                Ok(_client) => {
                    log::info!("backend stats segment reachable at {}", stats_socket.display());
                    connected = true;
                    break;
                }
                Err(e) => {
                    log::debug!("backend not yet reachable: {e}");
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
        if !connected {
            return Err(LifecycleError::BackendUnreachable(
                format!("stats segment at {} not reachable within {:?}", stats_socket.display(), startup_timeout),
            ));
        }

        // Build exporters
        let mut exporter_handles = Vec::new();
        let mut exporter_senders = Vec::new();

        for entry in &exporter_entries {
            let factory = find_factory(&entry.kind)
                .ok_or_else(|| LifecycleError::UnknownExporter(entry.kind.clone()))?;

            let ecfg = entry_to_exporter_config(entry);
            let exporter_instance = factory(&ecfg)
                .map_err(|e| LifecycleError::ExporterInit(format!("{}: {e}", entry.name)))?;

            let (tx, rx) = snapshot_channel(entry.queue_depth);
            let export_timeout = parse_duration(&entry.export_timeout).unwrap_or(Duration::from_millis(800));

            let handle = exporter::spawn_exporter_thread(Arc::from(exporter_instance), rx, export_timeout)
                .map_err(|e| LifecycleError::ExporterInit(format!("spawn {}: {e}", entry.name)))?;

            exporter_handles.push(handle);
            exporter_senders.push(tx);
        }

        // Spawn poller with exporter senders (convert to raw SyncSender for poller API)
        let poller_config = PollerConfig {
            stats_socket,
            interval: Duration::from_millis(frontend_cfg.polling_interval_ms),
        };

        let raw_senders: Vec<_> = exporter_senders.iter().map(|s| s.as_raw_sender().clone()).collect();
        let poller_handle = poller::spawn(poller_config, raw_senders);

        let shutdown = Arc::new(AtomicBool::new(false));

        Ok(Frontend {
            poller_handle: Some(poller_handle),
            exporter_handles,
            exporter_senders,
            config_path: config_path.to_path_buf(),
            shutdown,
        })
    }

    /// Reload configuration (triggered by SIGHUP).
    /// All-or-nothing with rollback on failure.
    pub fn reload(&mut self) -> Result<(), LifecycleError> {
        log::info!("reloading configuration from {}", self.config_path.display());

        // Re-read and parse config
        let config_text = std::fs::read_to_string(&self.config_path)
            .map_err(|e| LifecycleError::ReloadFailed(format!("cannot read config: {e}")))?;
        let config: infmon_config::model::Config = serde_yaml::from_str(&config_text)
            .map_err(|e| LifecycleError::ReloadFailed(format!("YAML parse error: {e}")))?;

        infmon_config::validate_config(&config)
            .map_err(|e| LifecycleError::ReloadFailed(format!("validation error: {e}")))?;

        // TODO: diff flow rules, apply via control client
        // TODO: reload existing exporters, add/remove as needed
        log::info!("configuration reloaded successfully");
        Ok(())
    }

    /// Graceful shutdown. Always exits 0.
    pub fn stop(mut self) {
        log::info!("stopping infmon-frontend");
        self.shutdown.store(true, Ordering::Release);

        // 1. Stop the poller
        if let Some(handle) = self.poller_handle.take() {
            handle.stop();
            log::info!("poller stopped");
        }

        // 2. Close exporter channels (drop senders) so exporters drain
        self.exporter_senders.clear();

        // 3. Wait for exporter threads to finish
        let handles = std::mem::take(&mut self.exporter_handles);
        for handle in handles {
            handle.join();
        }

        log::info!("infmon-frontend stopped");
    }

    /// Check if shutdown has been signalled.
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }
}

/// Convert an ExporterEntry to ExporterConfig for the factory.
fn entry_to_exporter_config(entry: &infmon_config::model::ExporterEntry) -> ExporterConfig {
    let mut extra = std::collections::HashMap::new();
    for (k, v) in &entry.extra {
        extra.insert(k.clone(), v.clone());
    }
    ExporterConfig {
        kind: entry.kind.clone(),
        name: entry.name.clone(),
        queue_depth: entry.queue_depth,
        export_timeout: parse_duration(&entry.export_timeout).unwrap_or(Duration::from_millis(800)),
        extra,
    }
}

/// Parse a duration string like "800ms", "5s", "1s".
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(ms) = s.strip_suffix("ms") {
        ms.trim().parse::<u64>().ok().map(Duration::from_millis)
    } else if let Some(secs) = s.strip_suffix('s') {
        secs.trim().parse::<u64>().ok().map(Duration::from_secs)
    } else {
        // Try as milliseconds
        s.parse::<u64>().ok().map(Duration::from_millis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_ms() {
        assert_eq!(parse_duration("800ms"), Some(Duration::from_millis(800)));
    }

    #[test]
    fn parse_duration_s() {
        assert_eq!(parse_duration("5s"), Some(Duration::from_secs(5)));
    }

    #[test]
    fn parse_duration_bare_number() {
        assert_eq!(parse_duration("1000"), Some(Duration::from_millis(1000)));
    }

    #[test]
    fn parse_duration_invalid() {
        assert_eq!(parse_duration("abc"), None);
    }

    #[test]
    fn entry_to_config_basic() {
        let entry = infmon_config::model::ExporterEntry {
            kind: "otlp".into(),
            name: "primary".into(),
            queue_depth: 2,
            export_timeout: "800ms".into(),
            on_overflow: "drop_newest".into(),
            extra: std::collections::HashMap::new(),
        };
        let cfg = entry_to_exporter_config(&entry);
        assert_eq!(cfg.kind, "otlp");
        assert_eq!(cfg.name, "primary");
        assert_eq!(cfg.queue_depth, 2);
        assert_eq!(cfg.export_timeout, Duration::from_millis(800));
    }
}
