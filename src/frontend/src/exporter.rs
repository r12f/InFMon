//! Exporter trait, registration, and per-exporter dispatch.
//!
//! Each exporter runs on a **dedicated OS thread** with its own single-threaded
//! `tokio` runtime. Snapshots are delivered via a bounded channel; when the
//! channel is full the newest snapshot is dropped (`drop_newest` policy).

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use infmon_ipc::types::FlowStatsSnapshot;

// ── Error types ────────────────────────────────────────────────────

/// Errors returned by [`Exporter::export`].
#[derive(Debug)]
pub enum ExporterError {
    /// Network blip or similar transient issue; retryable next tick.
    Transient(Box<dyn std::error::Error + Send + Sync>),
    /// Config-level wrongness; exporter will be disabled until next reload.
    Permanent(Box<dyn std::error::Error + Send + Sync>),
    /// The export call exceeded `export_timeout`.
    Timeout,
}

impl fmt::Display for ExporterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExporterError::Transient(e) => write!(f, "transient: {e}"),
            ExporterError::Permanent(e) => write!(f, "permanent: {e}"),
            ExporterError::Timeout => write!(f, "timeout"),
        }
    }
}

impl std::error::Error for ExporterError {}

/// Errors returned by [`Exporter::reload`].
#[derive(Debug)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "config error: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

// ── Exporter config (per-instance) ─────────────────────────────────

/// Per-exporter configuration block from `config.yaml`.
#[derive(Debug, Clone)]
pub struct ExporterConfig {
    /// Exporter type key, e.g. `"otlp"`.
    pub kind: String,
    /// Operator-assigned instance name, unique within the frontend.
    pub name: String,
    /// Bounded channel capacity (default 2).
    pub queue_depth: usize,
    /// Deadline for a single `export()` call.
    pub export_timeout: Duration,
    /// Arbitrary key-value pairs for the exporter plugin.
    pub extra: std::collections::HashMap<String, String>,
}

impl Default for ExporterConfig {
    fn default() -> Self {
        Self {
            kind: String::new(),
            name: String::new(),
            queue_depth: 2,
            export_timeout: Duration::from_millis(800),
            extra: Default::default(),
        }
    }
}

// ── BoxFuture alias ────────────────────────────────────────────────

/// A boxed, `Send` future — the return type for async trait methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// ── Exporter trait ─────────────────────────────────────────────────

/// Implemented by every exporter plugin.
///
/// Implementations MUST be `Send + Sync` and are wrapped in `Arc` by
/// the framework.
pub trait Exporter: Send + Sync + 'static {
    /// Stable identifier for logs and metrics, e.g. `"otlp"`.
    fn kind(&self) -> &'static str;

    /// Operator-assigned instance name, unique per frontend.
    fn name(&self) -> &str;

    /// Called once per tick with the shared snapshot.
    ///
    /// MUST return within the configured `export_timeout`.  A pending
    /// future at deadline is cancelled and counted as a failure.
    fn export(&self, snap: Arc<FlowStatsSnapshot>) -> BoxFuture<'_, Result<(), ExporterError>>;

    /// Called on `reload` with the exporter's new config block.
    /// Returning `Err` aborts the reload (spec §9.2).
    fn reload(&self, cfg: &ExporterConfig) -> Result<(), ConfigError>;

    /// Called once on shutdown. Implementations SHOULD flush.
    /// Bounded by `shutdown_grace_ms` (spec §9.3).
    fn shutdown(&self) -> BoxFuture<'_, ()>;
}

// ── inventory-based registration ───────────────────────────────────

/// Factory function type: build an exporter from its config.
pub type ExporterFactory =
    fn(&ExporterConfig) -> Result<Box<dyn Exporter>, Box<dyn std::error::Error + Send + Sync>>;

/// A compile-time registration entry for an exporter plugin.
///
/// Plugins register via:
/// ```ignore
/// inventory::submit!(ExporterRegistration {
///     kind: "otlp",
///     factory: |cfg| Ok(Box::new(OtlpExporter::new(cfg)?)),
/// });
/// ```
pub struct ExporterRegistration {
    /// The type key matched against `exporters[].type` in config.
    pub kind: &'static str,
    /// Factory that builds an instance from config.
    pub factory: ExporterFactory,
}

inventory::collect!(ExporterRegistration);

/// Look up a registered exporter factory by kind.
pub fn find_factory(kind: &str) -> Option<ExporterFactory> {
    for reg in inventory::iter::<ExporterRegistration> {
        if reg.kind == kind {
            return Some(reg.factory);
        }
    }
    None
}

// ── Bounded snapshot channel (drop_newest) ─────────────────────────

/// Sender half of a bounded snapshot channel with `drop_newest` overflow.
#[derive(Clone)]
pub struct SnapshotSender {
    inner: std::sync::mpsc::SyncSender<Arc<FlowStatsSnapshot>>,
}

/// Receiver half of a bounded snapshot channel.
pub struct SnapshotReceiver {
    inner: std::sync::mpsc::Receiver<Arc<FlowStatsSnapshot>>,
}

/// Create a bounded channel pair with the given capacity.
pub fn snapshot_channel(capacity: usize) -> (SnapshotSender, SnapshotReceiver) {
    let cap = capacity.max(1);
    let (tx, rx) = std::sync::mpsc::sync_channel(cap);
    (SnapshotSender { inner: tx }, SnapshotReceiver { inner: rx })
}

impl SnapshotSender {
    /// Try to send a snapshot. If the channel is full, the snapshot is
    /// dropped (drop_newest policy) and `Err(())` is returned so the
    /// caller can bump a metric.
    #[allow(clippy::result_unit_err)]
    pub fn try_send(&self, snap: Arc<FlowStatsSnapshot>) -> Result<(), ()> {
        match self.inner.try_send(snap) {
            Ok(()) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(_)) => Err(()),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => Err(()),
        }
    }
}

impl SnapshotReceiver {
    /// Blocking receive. Returns `None` when all senders are dropped.
    pub fn recv(&self) -> Option<Arc<FlowStatsSnapshot>> {
        self.inner.recv().ok()
    }

    /// Non-blocking receive.
    pub fn try_recv(&self) -> Option<Arc<FlowStatsSnapshot>> {
        self.inner.try_recv().ok()
    }
}

// ── Per-exporter dispatch thread ───────────────────────────────────

/// Handle to a running exporter thread.
pub struct ExporterHandle {
    join: Option<thread::JoinHandle<()>>,
    _name: String,
}

impl ExporterHandle {
    /// Wait for the exporter thread to finish (call after closing the channel).
    pub fn join(mut self) {
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ExporterHandle {
    fn drop(&mut self) {
        // Thread will exit once the sender is dropped and the channel drains.
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

/// Spawn a dedicated OS thread for an exporter.
///
/// The thread runs a single-threaded tokio runtime and consumes snapshots
/// from `rx`, calling `exporter.export()` with the configured timeout.
pub fn spawn_exporter_thread(
    exporter: Arc<dyn Exporter>,
    rx: SnapshotReceiver,
    export_timeout: Duration,
) -> ExporterHandle {
    let name = format!("exporter-{}", exporter.name());
    let thread_name = name.clone();

    let join = thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio current-thread runtime for exporter");

            rt.block_on(async {
                while let Some(snap) = rx.recv() {
                    let result = tokio::time::timeout(export_timeout, exporter.export(snap)).await;

                    match result {
                        Ok(Ok(())) => {}
                        Ok(Err(ExporterError::Timeout)) | Err(_) => {
                            log::warn!(
                                "exporter '{}' timed out (limit {:?})",
                                exporter.name(),
                                export_timeout,
                            );
                        }
                        Ok(Err(ExporterError::Transient(e))) => {
                            log::warn!("exporter '{}' transient error: {}", exporter.name(), e,);
                        }
                        Ok(Err(ExporterError::Permanent(e))) => {
                            log::error!(
                                "exporter '{}' permanent error: {} — disabling",
                                exporter.name(),
                                e,
                            );
                            break;
                        }
                    }
                }
                log::info!("exporter '{}' thread exiting", exporter.name());
            });
        })
        .expect("failed to spawn exporter thread");

    ExporterHandle {
        join: Some(join),
        _name: name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A minimal test exporter that counts export calls.
    struct CountingExporter {
        count: AtomicU64,
        instance_name: String,
    }

    impl CountingExporter {
        fn new(name: &str) -> Self {
            Self {
                count: AtomicU64::new(0),
                instance_name: name.to_string(),
            }
        }
    }

    impl Exporter for CountingExporter {
        fn kind(&self) -> &'static str {
            "test"
        }
        fn name(&self) -> &str {
            &self.instance_name
        }
        fn export(
            &self,
            _snap: Arc<FlowStatsSnapshot>,
        ) -> BoxFuture<'_, Result<(), ExporterError>> {
            self.count.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Ok(()) })
        }
        fn reload(&self, _cfg: &ExporterConfig) -> Result<(), ConfigError> {
            Ok(())
        }
        fn shutdown(&self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    #[test]
    fn drop_newest_overflow() {
        let (tx, rx) = snapshot_channel(1);

        let snap = Arc::new(FlowStatsSnapshot {
            tick_id: 1,
            wall_clock_ns: 0,
            monotonic_ns: 0,
            interval_ns: 0,
            flow_rules: vec![],
        });

        // First send succeeds.
        assert!(tx.try_send(snap.clone()).is_ok());
        // Second send should fail (channel full, drop_newest).
        assert!(tx.try_send(snap.clone()).is_err());

        // Consume and verify.
        let received = rx.recv().unwrap();
        assert_eq!(received.tick_id, 1);
    }

    #[test]
    fn exporter_thread_processes_snapshots() {
        let exporter = Arc::new(CountingExporter::new("test-1"));
        let (tx, rx) = snapshot_channel(4);

        let exp_clone = exporter.clone();
        let handle = spawn_exporter_thread(exp_clone, rx, Duration::from_secs(5));

        // Send 3 snapshots.
        for i in 1..=3 {
            let snap = Arc::new(FlowStatsSnapshot {
                tick_id: i,
                wall_clock_ns: 0,
                monotonic_ns: 0,
                interval_ns: 0,
                flow_rules: vec![],
            });
            tx.try_send(snap).unwrap();
        }

        // Drop sender to signal the thread to exit.
        drop(tx);
        handle.join();

        assert_eq!(exporter.count.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn find_factory_returns_none_for_unknown() {
        assert!(find_factory("nonexistent_exporter_type").is_none());
    }
}
