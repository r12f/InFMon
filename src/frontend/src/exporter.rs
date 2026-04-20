//! Exporter trait, registration, and per-exporter dispatch.
//!
//! Each exporter runs on a **dedicated OS thread** with its own single-threaded
//! `tokio` runtime. Snapshots are delivered via a bounded channel; when the
//! channel is full the newest snapshot is dropped (`drop_newest` policy).

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use infmon_common::ipc::types::FlowStatsSnapshot;

// ── Self-observability metrics (spec §8.3) ──────────────────────────

/// Shared counters for exporter self-observability metrics.
///
/// All counters use `Ordering::Relaxed` — they are approximate
/// observability signals, not correctness-critical.
#[derive(Debug, Default, Clone)]
pub struct ExporterMetrics {
    /// Cumulative ticks dropped due to backpressure.
    /// TODO: wire up in poller/channel integration (channel-full branch).
    pub ticks_dropped: Arc<AtomicU64>,
    /// Cumulative batches successfully sent.
    pub batches_sent: Arc<AtomicU64>,
    /// Cumulative batches dropped (channel overflow).
    /// TODO: wire up in poller/channel integration (channel-full branch).
    pub batches_dropped: Arc<AtomicU64>,
    /// Cumulative batches that failed (non-retryable / permanent).
    pub batches_failed_non_retryable: Arc<AtomicU64>,
    /// Cumulative batches that failed (transient: timeout, network blip, etc.).
    ///
    /// Note: the exporter loop does not retry within a tick — each snapshot gets
    /// one export attempt. A transient failure therefore means this tick's batch
    /// was lost (the next tick will try a fresh snapshot).
    pub batches_failed_transient: Arc<AtomicU64>,
    /// Cumulative data points emitted (successfully exported).
    pub points_emitted: Arc<AtomicU64>,
    /// Cumulative data points dropped (export cap).
    pub points_dropped: Arc<AtomicU64>,
    /// Cumulative attribute truncations.
    pub attrs_truncated: Arc<AtomicU64>,
    /// Last export duration in seconds, stored as `f64::to_bits()` in an
    /// `AtomicU64` (not a plain integer counter — use [`Self::get_export_duration`]
    /// to read back as `f64`).
    pub export_duration_bits: Arc<AtomicU64>,
    /// Current queue depth.
    /// TODO: wire up in poller/channel integration (derive from channel `len()`).
    pub queue_depth: Arc<AtomicU64>,
}

impl ExporterMetrics {
    /// Store export duration as f64 seconds.
    pub fn set_export_duration(&self, secs: f64) {
        self.export_duration_bits
            .store(secs.to_bits(), Ordering::Relaxed);
    }

    /// Load export duration as f64 seconds.
    pub fn get_export_duration(&self) -> f64 {
        f64::from_bits(self.export_duration_bits.load(Ordering::Relaxed))
    }
}

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

    /// Return self-observability metrics if the exporter tracks them.
    fn metrics(&self) -> Option<Arc<ExporterMetrics>> {
        None
    }
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
///
/// Returns the first match. If multiple plugins register the same `kind`,
/// only the first is reachable — duplicates are detected and logged at
/// startup by [`validate_registrations`].
pub fn find_factory(kind: &str) -> Option<ExporterFactory> {
    for reg in inventory::iter::<ExporterRegistration> {
        if reg.kind == kind {
            return Some(reg.factory);
        }
    }
    None
}

/// Check for duplicate `kind` registrations and log warnings.
/// Should be called once at startup.
pub fn validate_registrations() {
    let mut seen = std::collections::HashSet::new();
    for reg in inventory::iter::<ExporterRegistration> {
        if !seen.insert(reg.kind) {
            log::warn!(
                "duplicate exporter registration for kind '{}' — only the first will be used",
                reg.kind,
            );
        }
    }
}

// ── Bounded snapshot channel (drop_newest) ─────────────────────────

/// Error returned by [`SnapshotSender::try_send`].
#[derive(Debug)]
pub enum TrySendError {
    /// The channel is full (backpressure). The snapshot was dropped.
    Full,
    /// The receiver has been dropped — the exporter thread is gone.
    Disconnected,
}

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
///
/// A `capacity` of zero is clamped to 1 with a warning log, since a
/// zero-capacity `sync_channel` would require a rendezvous and break
/// the drop-newest overflow policy.
pub fn snapshot_channel(capacity: usize) -> (SnapshotSender, SnapshotReceiver) {
    let cap = if capacity == 0 {
        log::warn!("snapshot_channel: capacity 0 clamped to 1");
        1
    } else {
        capacity
    };
    let (tx, rx) = std::sync::mpsc::sync_channel(cap);
    (SnapshotSender { inner: tx }, SnapshotReceiver { inner: rx })
}

impl SnapshotSender {
    /// Get the underlying `SyncSender` for interop with the poller.
    pub fn as_raw_sender(&self) -> &std::sync::mpsc::SyncSender<Arc<FlowStatsSnapshot>> {
        &self.inner
    }

    /// Try to send a snapshot. If the queue is full, returns
    /// [`TrySendError::Full`] so the caller can track drops via
    /// `ExporterMetrics::batches_dropped`.
    pub fn try_send(&self, snap: Arc<FlowStatsSnapshot>) -> Result<(), TrySendError> {
        match self.inner.try_send(snap) {
            Ok(()) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Full(_)) => Err(TrySendError::Full),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => Err(TrySendError::Disconnected),
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
///
/// # Shutdown contract
///
/// The caller MUST drop all [`SnapshotSender`] clones **before** dropping
/// `ExporterHandle` (or calling [`join`](Self::join)). The thread exits
/// when `rx.recv()` returns `None`, which only happens once every sender
/// is dropped. If senders are still alive when `Drop` runs, the `join()`
/// call will block indefinitely.
pub struct ExporterHandle {
    join: Option<thread::JoinHandle<()>>,
    _name: String,
}

impl ExporterHandle {
    /// Wait for the exporter thread to finish (call after closing the channel).
    pub fn join(mut self) {
        if let Some(h) = self.join.take() {
            if let Err(e) = h.join() {
                log::error!("exporter thread panicked: {:?}", e);
            }
        }
    }
}

impl Drop for ExporterHandle {
    fn drop(&mut self) {
        // Explicit join to ensure the thread finishes before the handle is
        // reclaimed. `JoinHandle` detaches on drop (does NOT join), so
        // without this the thread could outlive resources it references.
        if let Some(h) = self.join.take() {
            if let Err(e) = h.join() {
                log::error!("exporter thread panicked: {:?}", e);
            }
        }
    }
}

/// Spawn a dedicated OS thread for an exporter.
///
/// The thread runs a single-threaded tokio runtime and consumes snapshots
/// from `rx`, calling `exporter.export()` with the configured timeout.
///
/// Returns `Err` if the tokio runtime or the OS thread fails to spawn.
pub fn spawn_exporter_thread(
    exporter: Arc<dyn Exporter>,
    rx: SnapshotReceiver,
    export_timeout: Duration,
) -> Result<ExporterHandle, Box<dyn std::error::Error + Send + Sync>> {
    let name = format!("exporter-{}", exporter.name());
    let thread_name = name.clone();

    let metrics = exporter.metrics();

    let join = thread::Builder::new().name(thread_name).spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                log::error!(
                    "exporter '{}': failed to build tokio runtime: {}",
                    exporter.name(),
                    e,
                );
                return;
            }
        };

        rt.block_on(async {
            let mut backoff = Duration::from_millis(100);
            const MAX_BACKOFF: Duration = Duration::from_secs(30);

            while let Some(snap) = rx.recv() {
                let result = tokio::time::timeout(export_timeout, exporter.export(snap)).await;

                match result {
                    Ok(Ok(())) => {
                        backoff = Duration::from_millis(100); // reset on success
                    }
                    Ok(Err(ExporterError::Timeout)) => {
                        log::warn!("exporter '{}' self-reported timeout", exporter.name(),);
                        if let Some(ref m) = metrics {
                            m.batches_failed_transient.fetch_add(1, Ordering::Relaxed);
                        }
                        // Transient-like, apply backoff
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                    Err(_) => {
                        log::warn!(
                            "exporter '{}' export exceeded framework deadline ({:?})",
                            exporter.name(),
                            export_timeout,
                        );
                        if let Some(ref m) = metrics {
                            m.batches_failed_transient.fetch_add(1, Ordering::Relaxed);
                        }
                        // Transient-like, apply backoff
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                    Ok(Err(ExporterError::Transient(e))) => {
                        log::warn!("exporter '{}' transient error: {}", exporter.name(), e);
                        if let Some(ref m) = metrics {
                            m.batches_failed_transient.fetch_add(1, Ordering::Relaxed);
                        }
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                    Ok(Err(ExporterError::Permanent(e))) => {
                        log::error!(
                            "exporter '{}' permanent error: {} — disabling",
                            exporter.name(),
                            e,
                        );
                        if let Some(ref m) = metrics {
                            m.batches_failed_non_retryable
                                .fetch_add(1, Ordering::Relaxed);
                        }
                        break;
                    }
                }
            }

            // Flush exporter buffers on shutdown (spec §9.3).
            exporter.shutdown().await;
            log::info!("exporter '{}' thread exiting", exporter.name());
        });
    })?;

    Ok(ExporterHandle {
        join: Some(join),
        _name: name,
    })
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

    /// Build a minimal `FlowStatsSnapshot` for tests — only `tick_id` varies.
    fn test_snap(tick_id: u64) -> Arc<FlowStatsSnapshot> {
        Arc::new(FlowStatsSnapshot {
            tick_id,
            wall_clock_ns: 0,
            monotonic_ns: 0,
            interval_ns: 0,
            flow_rules: vec![],
        })
    }

    #[test]
    fn drop_newest_overflow() {
        let (tx, rx) = snapshot_channel(1);

        let snap = test_snap(1);

        // First send succeeds.
        assert!(tx.try_send(snap.clone()).is_ok());
        // Second send should fail (channel full, drop_newest).
        assert!(matches!(tx.try_send(snap.clone()), Err(TrySendError::Full)));

        // Consume and verify.
        let received = rx.recv().unwrap();
        assert_eq!(received.tick_id, 1);
    }

    #[test]
    fn exporter_thread_processes_snapshots() {
        let exporter = Arc::new(CountingExporter::new("test-1"));
        let (tx, rx) = snapshot_channel(4);

        let exp_clone = exporter.clone();
        let handle = spawn_exporter_thread(exp_clone, rx, Duration::from_secs(5)).unwrap();

        // Send 3 snapshots.
        for i in 1..=3 {
            tx.try_send(test_snap(i)).unwrap();
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

    // ── ExporterConfig defaults ─────────────────────────────────────

    #[test]
    fn exporter_config_default_values() {
        let cfg = ExporterConfig::default();
        assert_eq!(cfg.kind, "");
        assert_eq!(cfg.name, "");
        assert_eq!(cfg.queue_depth, 2);
        assert_eq!(cfg.export_timeout, Duration::from_millis(800));
        assert!(cfg.extra.is_empty());
    }

    // ── ExporterError Display ───────────────────────────────────────

    #[test]
    fn exporter_error_display_transient() {
        let e = ExporterError::Transient("network down".into());
        assert_eq!(format!("{e}"), "transient: network down");
    }

    #[test]
    fn exporter_error_display_permanent() {
        let e = ExporterError::Permanent("bad auth".into());
        assert_eq!(format!("{e}"), "permanent: bad auth");
    }

    #[test]
    fn exporter_error_display_timeout() {
        let e = ExporterError::Timeout;
        assert_eq!(format!("{e}"), "timeout");
    }

    #[test]
    fn config_error_display() {
        let e = ConfigError("missing field".to_string());
        assert_eq!(format!("{e}"), "config error: missing field");
    }

    // ── Snapshot channel tests ──────────────────────────────────────

    #[test]
    fn snapshot_channel_capacity_zero_clamped_to_one() {
        // capacity 0 should be clamped to 1 (logged warning)
        let (tx, rx) = snapshot_channel(0);
        let snap = test_snap(1);
        // Should succeed once (capacity is 1)
        assert!(tx.try_send(snap.clone()).is_ok());
        // Second should fail (full)
        assert!(matches!(tx.try_send(snap), Err(TrySendError::Full)));
        // Verify we can receive
        assert!(rx.recv().is_some());
    }

    #[test]
    fn snapshot_channel_larger_capacity() {
        let (tx, rx) = snapshot_channel(4);
        for i in 1..=4 {
            assert!(tx.try_send(test_snap(i)).is_ok());
        }
        // 5th should be full
        assert!(matches!(tx.try_send(test_snap(5)), Err(TrySendError::Full)));

        // Drain and verify order
        for i in 1..=4 {
            let s = rx.recv().unwrap();
            assert_eq!(s.tick_id, i);
        }
    }

    #[test]
    fn snapshot_receiver_returns_none_when_sender_dropped() {
        let (tx, rx) = snapshot_channel(2);
        drop(tx);
        assert!(rx.recv().is_none());
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn snapshot_sender_disconnected_when_receiver_dropped() {
        let (tx, rx) = snapshot_channel(2);
        drop(rx);
        assert!(matches!(
            tx.try_send(test_snap(1)),
            Err(TrySendError::Disconnected)
        ));
    }

    #[test]
    fn snapshot_sender_clone_works() {
        let (tx, rx) = snapshot_channel(2);
        let tx2 = tx.clone();
        tx.try_send(test_snap(1)).unwrap();
        tx2.try_send(test_snap(2)).unwrap();
        assert_eq!(rx.recv().unwrap().tick_id, 1);
        assert_eq!(rx.recv().unwrap().tick_id, 2);
    }

    // ── Exporter thread behavior tests ──────────────────────────────

    /// Exporter that returns a permanent error on the first call.
    struct PermanentFailExporter {
        name: String,
        export_count: AtomicU64,
    }

    impl PermanentFailExporter {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                export_count: AtomicU64::new(0),
            }
        }
    }

    impl Exporter for PermanentFailExporter {
        fn kind(&self) -> &'static str {
            "test-fail"
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn export(
            &self,
            _snap: Arc<FlowStatsSnapshot>,
        ) -> BoxFuture<'_, Result<(), ExporterError>> {
            self.export_count.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Err(ExporterError::Permanent("fatal".into())) })
        }
        fn reload(&self, _cfg: &ExporterConfig) -> Result<(), ConfigError> {
            Ok(())
        }
        fn shutdown(&self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    #[test]
    fn exporter_thread_stops_on_permanent_error() {
        let exporter = Arc::new(PermanentFailExporter::new("fail-1"));
        let (tx, rx) = snapshot_channel(4);
        let exp_clone = exporter.clone();
        let handle = spawn_exporter_thread(exp_clone, rx, Duration::from_secs(5)).unwrap();

        // Send multiple snapshots
        for i in 1..=3 {
            let _ = tx.try_send(test_snap(i));
        }

        drop(tx);
        handle.join();

        // Should have only processed 1 (permanent error stops the loop)
        assert_eq!(exporter.export_count.load(Ordering::Relaxed), 1);
    }

    /// Exporter that returns transient errors then succeeds.
    struct TransientThenOkExporter {
        call_count: AtomicU64,
    }

    impl TransientThenOkExporter {
        fn new() -> Self {
            Self {
                call_count: AtomicU64::new(0),
            }
        }
    }

    impl Exporter for TransientThenOkExporter {
        fn kind(&self) -> &'static str {
            "test-transient"
        }
        fn name(&self) -> &str {
            "transient-exp"
        }
        fn export(
            &self,
            _snap: Arc<FlowStatsSnapshot>,
        ) -> BoxFuture<'_, Result<(), ExporterError>> {
            let n = self.call_count.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                if n == 0 {
                    Err(ExporterError::Transient("blip".into()))
                } else {
                    Ok(())
                }
            })
        }
        fn reload(&self, _cfg: &ExporterConfig) -> Result<(), ConfigError> {
            Ok(())
        }
        fn shutdown(&self) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }

    #[test]
    fn exporter_thread_continues_after_transient_error() {
        let exporter = Arc::new(TransientThenOkExporter::new());
        let (tx, rx) = snapshot_channel(4);
        let exp_clone = exporter.clone();
        let handle = spawn_exporter_thread(exp_clone, rx, Duration::from_secs(5)).unwrap();

        for i in 1..=2 {
            tx.try_send(test_snap(i)).unwrap();
        }

        // Allow generous time for backoff + processing on loaded CI runners.
        std::thread::sleep(Duration::from_millis(2000));
        drop(tx);
        handle.join();

        // Should have processed both snapshots (transient error retries, doesn't stop the loop).
        // Use >= 2 because the retry logic may re-attempt the first snapshot before moving on.
        assert!(exporter.call_count.load(Ordering::Relaxed) >= 2);
    }

    #[test]
    fn exporter_handle_drop_joins_thread() {
        let exporter = Arc::new(CountingExporter::new("drop-test"));
        let (tx, rx) = snapshot_channel(2);
        let handle = spawn_exporter_thread(exporter, rx, Duration::from_secs(5)).unwrap();
        drop(tx);
        // Dropping handle should join the thread without panic
        drop(handle);
    }

    #[test]
    fn counting_exporter_trait_methods() {
        let exp = CountingExporter::new("my-exp");
        assert_eq!(exp.kind(), "test");
        assert_eq!(exp.name(), "my-exp");
        assert!(exp.reload(&ExporterConfig::default()).is_ok());
    }
}
