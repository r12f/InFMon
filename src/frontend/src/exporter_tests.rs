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
    fn export(&self, _snap: Arc<FlowStatsSnapshot>) -> BoxFuture<'_, Result<(), ExporterError>> {
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
    fn export(&self, _snap: Arc<FlowStatsSnapshot>) -> BoxFuture<'_, Result<(), ExporterError>> {
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
    fn export(&self, _snap: Arc<FlowStatsSnapshot>) -> BoxFuture<'_, Result<(), ExporterError>> {
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
