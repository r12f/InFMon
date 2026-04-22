//! Frontend lifecycle: start, reload, stop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::control::{self, ControlHandle, ControlState};
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
    control_handle: Option<ControlHandle>,
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
        tracing::info!("starting infmon-frontend");

        // Validate exporter registrations
        validate_registrations();

        // Parse config
        let config_text = std::fs::read_to_string(config_path).map_err(|e| {
            LifecycleError::ConfigError(format!("cannot read {}: {e}", config_path.display()))
        })?;
        let config: infmon_common::config::model::Config = serde_yaml::from_str(&config_text)
            .map_err(|e| LifecycleError::ConfigError(format!("YAML parse error: {e}")))?;

        // Validate flow rules
        infmon_common::config::validate_config(&config)
            .map_err(|e| LifecycleError::ConfigError(format!("validation error: {e}")))?;

        let frontend_cfg = config.frontend.clone().unwrap_or_default();
        let exporter_entries = config.exporters.clone().unwrap_or_default();

        // Fail-fast: check backend reachability via stats socket.
        // VPP's stats socket uses SOCK_SEQPACKET, so we verify it exists
        // and is connectable using a raw Unix seqpacket socket.
        let stats_socket = PathBuf::from(&frontend_cfg.vpp_stats_socket);
        let startup_timeout =
            parse_duration(&frontend_cfg.startup_timeout).unwrap_or(Duration::from_secs(5));

        let start_time = std::time::Instant::now();
        let mut connected = false;
        while start_time.elapsed() < startup_timeout {
            if vpp_stats_socket_reachable(&stats_socket) {
                tracing::info!(
                    "backend stats socket reachable at {}",
                    stats_socket.display()
                );
                connected = true;
                break;
            } else {
                tracing::debug!("backend not yet reachable at {}", stats_socket.display());
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        if !connected {
            return Err(LifecycleError::BackendUnreachable(format!(
                "stats socket at {} not reachable within {:?}",
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

        // Spawn control server for CLI RPCs
        let control_socket = PathBuf::from(&frontend_cfg.control_socket);
        let control_state = Arc::new(ControlState::new(config.flow_rules.clone()));
        let control_handle = match control::spawn(&control_socket, control_state) {
            Ok(h) => {
                tracing::info!("control server listening on {}", control_socket.display());
                Some(h)
            }
            Err(e) => {
                tracing::warn!("failed to start control server: {e}");
                None
            }
        };

        Ok(Frontend {
            poller_handle: Some(poller_handle),
            exporter_handles,
            exporter_senders,
            control_handle,
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
        tracing::info!(
            "reloading configuration from {}",
            self.config_path.display()
        );

        // Re-read and parse config
        let config_text = std::fs::read_to_string(&self.config_path)
            .map_err(|e| LifecycleError::ReloadFailed(format!("cannot read config: {e}")))?;
        let config: infmon_common::config::model::Config = serde_yaml::from_str(&config_text)
            .map_err(|e| LifecycleError::ReloadFailed(format!("YAML parse error: {e}")))?;

        infmon_common::config::validate_config(&config)
            .map_err(|e| LifecycleError::ReloadFailed(format!("validation error: {e}")))?;

        // TODO: diff flow rules, apply via control client
        // TODO: reload existing exporters, add/remove as needed
        tracing::warn!("reload: config validated but hot-swap not yet implemented — changes take effect on restart");
        Ok(())
    }

    /// Graceful shutdown. Always exits 0.
    pub fn stop(mut self) {
        tracing::info!("stopping infmon-frontend");
        self.shutdown.store(true, Ordering::Release);

        // 1. Stop the poller
        if let Some(handle) = self.poller_handle.take() {
            handle.stop();
            tracing::info!("poller stopped");
        }

        // 2. Close exporter channels (drop senders) so exporters drain
        self.exporter_senders.clear();

        // 3. Wait for exporter threads to finish
        let handles = std::mem::take(&mut self.exporter_handles);
        for handle in handles {
            handle.join();
        }

        // 4. Stop control server
        if let Some(h) = self.control_handle.take() {
            h.stop();
            tracing::info!("control server stopped");
        }

        tracing::info!("infmon-frontend stopped");
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
fn entry_to_exporter_config(entry: &infmon_common::config::model::ExporterEntry) -> ExporterConfig {
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
        tracing::warn!(
            "parse_duration: unrecognised format {:?}, returning None",
            s
        );
        None
    }
}

/// Check if the VPP stats socket is reachable by attempting a
/// SOCK_SEQPACKET connection (VPP uses seqpacket, not stream).
fn vpp_stats_socket_reachable(path: &Path) -> bool {
    use std::os::unix::io::FromRawFd;

    // socket(AF_UNIX, SOCK_SEQPACKET, 0)
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0) };
    if fd < 0 {
        return false;
    }

    let path_bytes = path.as_os_str().as_encoded_bytes();

    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;

    if path_bytes.len() >= std::mem::size_of_val(&addr.sun_path) {
        // Safety: we own the fd, close it before returning
        let _ = unsafe { std::fs::File::from_raw_fd(fd) };
        return false; // sun_path overflow
    }
    // Copy path bytes into sun_path
    unsafe {
        std::ptr::copy_nonoverlapping(
            path_bytes.as_ptr(),
            addr.sun_path.as_mut_ptr() as *mut u8,
            path_bytes.len(),
        );
    }

    let len =
        (&addr.sun_path as *const _ as usize - &addr as *const _ as usize) + path_bytes.len() + 1;
    let rc = unsafe {
        libc::connect(
            fd,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            len as libc::socklen_t,
        )
    };
    // Safety: take ownership of fd after connect() to ensure cleanup.
    // Deferring from_raw_fd until after connect avoids fragility if the
    // guard were accidentally dropped/moved before the connect call.
    let _ = unsafe { std::fs::File::from_raw_fd(fd) }; // auto-close on drop
    rc == 0
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
