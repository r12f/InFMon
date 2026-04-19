//! Frontend lifecycle: start, reload, stop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use infmon_ipc::stats_client::InFMonStatsClient;

use crate::exporter::{
    self, find_factory, snapshot_channel, validate_registrations, ExporterConfig, ExporterHandle,
    SnapshotSender,
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
    /// The caller-provided `shutdown` flag is shared with the signal handler
    /// in `main()`, so `is_shutting_down()` reflects external signals.
    ///
    /// 1. Parse config
    /// 2. Validate backend is reachable (fail-fast)
    /// 3. Build exporters from config
    /// 4. Spawn poller and exporter threads
    pub fn start(config_path: &Path, shutdown: Arc<AtomicBool>) -> Result<Self, LifecycleError> {
        log::info!("starting infmon-frontend");

        // Validate exporter registrations
        validate_registrations();

        // Parse config
        let config_text = std::fs::read_to_string(config_path).map_err(|e| {
            LifecycleError::ConfigError(format!("cannot read {}: {e}", config_path.display()))
        })?;
        let config: infmon_config::model::Config = serde_yaml::from_str(&config_text)
            .map_err(|e| LifecycleError::ConfigError(format!("YAML parse error: {e}")))?;

        // Validate flow rules
        infmon_config::validate_config(&config)
            .map_err(|e| LifecycleError::ConfigError(format!("validation error: {e}")))?;

        let frontend_cfg = config.frontend.clone().unwrap_or_default();
        let exporter_entries = config.exporters.clone().unwrap_or_default();

        // Fail-fast: check backend reachability via stats socket
        let stats_socket = PathBuf::from(&frontend_cfg.vpp_stats_socket);
        let startup_timeout =
            parse_duration(&frontend_cfg.startup_timeout).unwrap_or(Duration::from_secs(5));

        // Try to connect to stats socket within startup_timeout.
        // The client is intentionally opened and dropped — we only need to
        // verify that the stats segment is mmappable, not hold it open.
        // The poller will open its own long-lived connection.
        let start_time = std::time::Instant::now();
        let mut connected = false;
        while start_time.elapsed() < startup_timeout {
            match InFMonStatsClient::open(&stats_socket) {
                Ok(_client) => {
                    log::info!(
                        "backend stats segment reachable at {}",
                        stats_socket.display()
                    );
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
            return Err(LifecycleError::BackendUnreachable(format!(
                "stats segment at {} not reachable within {:?}",
                stats_socket.display(),
                startup_timeout
            )));
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

            let (tx, rx) = snapshot_channel(ecfg.queue_depth);
            let export_timeout = ecfg.export_timeout;

            let handle = exporter::spawn_exporter_thread(
                Arc::from(exporter_instance),
                rx,
                export_timeout,
            )
            .map_err(|e| LifecycleError::ExporterInit(format!("spawn {}: {e}", entry.name)))?;

            exporter_handles.push(handle);
            exporter_senders.push(tx);
        }

        // Spawn poller with exporter senders (convert to raw SyncSender for poller API)
        let poller_config = PollerConfig {
            stats_socket,
            interval: Duration::from_millis(frontend_cfg.polling_interval_ms),
        };

        let raw_senders: Vec<_> = exporter_senders
            .iter()
            .map(|s| s.as_raw_sender().clone())
            .collect();
        let poller_handle = poller::spawn(poller_config, raw_senders);

        Ok(Frontend {
            poller_handle: Some(poller_handle),
            exporter_handles,
            exporter_senders,
            config_path: config_path.to_path_buf(),
            shutdown,
        })
    }

    /// Reload configuration (triggered by SIGHUP).
    ///
    /// **Current limitation:** this is a stub that validates the new config
    /// but does not yet apply changes to running exporters or flow rules.
    /// A future iteration will diff the old and new configs and hot-swap
    /// exporters / update flow rules via the control client.
    pub fn reload(&mut self) -> Result<(), LifecycleError> {
        log::info!(
            "reloading configuration from {}",
            self.config_path.display()
        );

        // Re-read and parse config
        let config_text = std::fs::read_to_string(&self.config_path)
            .map_err(|e| LifecycleError::ReloadFailed(format!("cannot read config: {e}")))?;
        let config: infmon_config::model::Config = serde_yaml::from_str(&config_text)
            .map_err(|e| LifecycleError::ReloadFailed(format!("YAML parse error: {e}")))?;

        infmon_config::validate_config(&config)
            .map_err(|e| LifecycleError::ReloadFailed(format!("validation error: {e}")))?;

        // TODO: diff flow rules, apply via control client
        // TODO: reload existing exporters, add/remove as needed
        log::warn!("reload: config validated but hot-swap not yet implemented — changes take effect on restart");
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
///
/// `queue_depth` and `export_timeout` are read exclusively from the returned
/// `ExporterConfig` by `start()`, avoiding a second source of truth.
fn entry_to_exporter_config(entry: &infmon_config::model::ExporterEntry) -> ExporterConfig {
    let mut extra = std::collections::HashMap::new();
    for (k, v) in &entry.extra {
        // Convert serde_yaml::Value to a string representation for the
        // exporter plugin's key-value interface.
        let s = match v {
            serde_yaml::Value::String(s) => s.clone(),
            other => serde_yaml::to_string(other)
                .unwrap_or_default()
                .trim()
                .to_string(),
        };
        extra.insert(k.clone(), s);
    }
    ExporterConfig {
        kind: entry.kind.clone(),
        name: entry.name.clone(),
        queue_depth: entry.queue_depth,
        export_timeout: parse_duration(&entry.export_timeout).unwrap_or(Duration::from_millis(800)),
        extra,
    }
}

/// Parse a duration string like `"800ms"`, `"5s"`, `"2m"`, `"1h"`.
///
/// Supported suffixes: `ms`, `s`, `m`, `h`. Bare integers are treated as
/// milliseconds. Unrecognised suffixes return `None` and log a warning so
/// the caller can fall back to a default without silent surprises.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(ms) = s.strip_suffix("ms") {
        ms.trim().parse::<u64>().ok().map(Duration::from_millis)
    } else if let Some(h) = s.strip_suffix('h') {
        h.trim()
            .parse::<u64>()
            .ok()
            .map(|v| Duration::from_secs(v * 3600))
    } else if let Some(m) = s.strip_suffix('m') {
        m.trim()
            .parse::<u64>()
            .ok()
            .map(|v| Duration::from_secs(v * 60))
    } else if let Some(secs) = s.strip_suffix('s') {
        secs.trim().parse::<u64>().ok().map(Duration::from_secs)
    } else if s.chars().all(|c| c.is_ascii_digit()) {
        // Bare integer → milliseconds
        s.parse::<u64>().ok().map(Duration::from_millis)
    } else {
        log::warn!(
            "parse_duration: unrecognised format {:?}, returning None",
            s
        );
        None
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
    fn parse_duration_m() {
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
    }

    #[test]
    fn parse_duration_h() {
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
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

    // ── parse_duration edge cases ───────────────────────────────────

    #[test]
    fn parse_duration_zero_ms() {
        assert_eq!(parse_duration("0ms"), Some(Duration::from_millis(0)));
    }

    #[test]
    fn parse_duration_zero_bare() {
        assert_eq!(parse_duration("0"), Some(Duration::from_millis(0)));
    }

    #[test]
    fn parse_duration_whitespace_trimmed() {
        assert_eq!(parse_duration("  5s  "), Some(Duration::from_secs(5)));
    }

    #[test]
    fn parse_duration_large_value() {
        assert_eq!(parse_duration("86400s"), Some(Duration::from_secs(86400)));
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration("24h"), Some(Duration::from_secs(24 * 3600)));
    }

    #[test]
    fn parse_duration_empty_string() {
        // Empty string after trim has no digits, treated as invalid
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn parse_duration_negative_is_invalid() {
        assert_eq!(parse_duration("-5s"), None);
    }

    #[test]
    fn parse_duration_float_is_invalid() {
        assert_eq!(parse_duration("1.5s"), None);
    }

    #[test]
    fn parse_duration_unknown_suffix() {
        assert_eq!(parse_duration("100x"), None);
    }

    // ── entry_to_exporter_config edge cases ─────────────────────────

    #[test]
    fn entry_to_config_with_extra_fields() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "endpoint".to_string(),
            serde_yaml::Value::String("http://localhost:4317".into()),
        );
        extra.insert(
            "timeout".to_string(),
            serde_yaml::Value::Number(serde_yaml::Number::from(30)),
        );

        let entry = infmon_config::model::ExporterEntry {
            kind: "otlp".into(),
            name: "with-extras".into(),
            queue_depth: 4,
            export_timeout: "5s".into(),
            on_overflow: "drop_newest".into(),
            extra,
        };
        let cfg = entry_to_exporter_config(&entry);
        assert_eq!(cfg.kind, "otlp");
        assert_eq!(cfg.name, "with-extras");
        assert_eq!(cfg.queue_depth, 4);
        assert_eq!(cfg.export_timeout, Duration::from_secs(5));
        assert_eq!(cfg.extra.get("endpoint").unwrap(), "http://localhost:4317");
        assert_eq!(cfg.extra.get("timeout").unwrap(), "30");
    }

    #[test]
    fn entry_to_config_invalid_timeout_uses_default() {
        let entry = infmon_config::model::ExporterEntry {
            kind: "test".into(),
            name: "bad-timeout".into(),
            queue_depth: 2,
            export_timeout: "invalid".into(),
            on_overflow: "drop_newest".into(),
            extra: std::collections::HashMap::new(),
        };
        let cfg = entry_to_exporter_config(&entry);
        // Should fallback to 800ms
        assert_eq!(cfg.export_timeout, Duration::from_millis(800));
    }

    // ── LifecycleError Display ──────────────────────────────────────

    #[test]
    fn lifecycle_error_display() {
        assert_eq!(
            format!("{}", LifecycleError::ConfigError("bad yaml".into())),
            "config error: bad yaml"
        );
        assert_eq!(
            format!("{}", LifecycleError::BackendUnreachable("timeout".into())),
            "backend unreachable: timeout"
        );
        assert_eq!(
            format!("{}", LifecycleError::UnknownExporter("foo".into())),
            "unknown exporter type: foo"
        );
        assert_eq!(
            format!("{}", LifecycleError::ExporterInit("fail".into())),
            "exporter init failed: fail"
        );
        assert_eq!(
            format!("{}", LifecycleError::ReloadFailed("oops".into())),
            "reload failed: oops"
        );
    }

    // ── Frontend::start error paths ─────────────────────────────────

    #[test]
    fn start_fails_on_missing_config() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let result = Frontend::start(
            Path::new("/tmp/nonexistent-infmon-test-config.yaml"),
            shutdown,
        );
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(matches!(err, LifecycleError::ConfigError(_)));
    }

    #[test]
    fn start_fails_on_invalid_yaml() {
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join("bad.yaml");
        std::fs::write(&cfg_path, "{{{{not yaml").unwrap();

        let shutdown = Arc::new(AtomicBool::new(false));
        let result = Frontend::start(&cfg_path, shutdown);
        assert!(result.is_err());
        assert!(matches!(
            result.err().unwrap(),
            LifecycleError::ConfigError(_)
        ));
    }

    #[test]
    fn start_fails_on_unreachable_backend() {
        // Valid YAML with a non-existent stats socket path
        let dir = tempfile::TempDir::new().unwrap();
        let cfg_path = dir.path().join("config.yaml");
        let yaml = r#"
frontend:
  vpp_stats_socket: /tmp/nonexistent-infmon-test-stats.sock
  startup_timeout: "100ms"
  polling_interval_ms: 100
flow-rules: []
"#;
        std::fs::write(&cfg_path, yaml).unwrap();

        let shutdown = Arc::new(AtomicBool::new(false));
        let result = Frontend::start(&cfg_path, shutdown);
        assert!(result.is_err());
        assert!(matches!(
            result.err().unwrap(),
            LifecycleError::BackendUnreachable(_)
        ));
    }

    #[test]
    fn reload_failed_error_contains_reason() {
        // Verifies that ReloadFailed's Display output includes the underlying message.
        let err = LifecycleError::ReloadFailed("cannot read config: No such file".into());
        assert!(format!("{err}").contains("cannot read config"));
    }
}
