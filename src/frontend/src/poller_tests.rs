use super::*;
use std::sync::mpsc;

#[test]
fn decode_empty_raw_snapshot() {
    let raw = infmon_common::ipc::stats_client::RawSnapshot {
        descriptors: Vec::new(),
    };
    let snap = decode_snapshot(raw, 1, 1000, 2000, 0);
    assert_eq!(snap.tick_id, 1);
    assert_eq!(snap.interval_ns, 0); // first tick
    assert!(snap.flow_rules.is_empty());
}

#[test]
fn decode_second_tick_has_interval() {
    let raw = infmon_common::ipc::stats_client::RawSnapshot {
        descriptors: Vec::new(),
    };
    let snap = decode_snapshot(raw, 2, 1000, 3_000_000_000, 2_000_000_000);
    assert_eq!(snap.tick_id, 2);
    assert_eq!(snap.interval_ns, 1_000_000_000);
}

#[test]
fn poller_stops_immediately() {
    // No real stats socket — poller should try to connect, fail,
    // then stop when we signal it.
    let config = PollerConfig {
        stats_socket: PathBuf::from("/tmp/nonexistent-infmon-test.sock"),
        interval: Duration::from_millis(100),
    };
    let (tx, _rx) = mpsc::sync_channel::<Arc<FlowStatsSnapshot>>(2);
    let handle = spawn(config, vec![tx]);

    // Give it a moment to start, then stop.
    thread::sleep(Duration::from_millis(150));
    handle.stop();
}

#[test]
#[ignore = "requires a real VPP stats segment; InFMonStatsClient::open fails on empty files"]
fn poller_sends_snapshots_on_real_segment() {
    // This test needs a real VPP shared-memory stats segment.
    // An empty file is not a valid segment, so InFMonStatsClient::open
    // will fail and the poller enters reconnect backoff.
    let dir = tempfile::TempDir::new().unwrap();
    let sock = dir.path().join("stats.sock");
    std::fs::write(&sock, b"").unwrap();

    let config = PollerConfig {
        stats_socket: sock,
        interval: Duration::from_millis(50),
    };
    let (tx, rx) = mpsc::sync_channel::<Arc<FlowStatsSnapshot>>(8);
    let handle = spawn(config, vec![tx]);

    // Wait for a few ticks.
    thread::sleep(Duration::from_millis(200));
    handle.stop();

    // Should have received at least one snapshot.
    let mut count = 0;
    while let Ok(snap) = rx.try_recv() {
        count += 1;
        assert!(snap.tick_id > 0);
        if snap.tick_id > 1 {
            assert!(snap.interval_ns > 0);
        }
    }
    assert!(count >= 1, "expected at least 1 snapshot, got {}", count);
}

#[test]
#[ignore = "requires a real VPP stats segment; InFMonStatsClient::open fails on empty files"]
fn backpressure_drops_snapshots() {
    // Channel with capacity 1. If poller ticks faster than we consume,
    // it should drop snapshots without blocking.
    let dir = tempfile::TempDir::new().unwrap();
    let sock = dir.path().join("stats.sock");
    std::fs::write(&sock, b"").unwrap();

    let config = PollerConfig {
        stats_socket: sock,
        interval: Duration::from_millis(20),
    };
    let (tx, rx) = mpsc::sync_channel::<Arc<FlowStatsSnapshot>>(1);
    let handle = spawn(config, vec![tx]);

    // Don't consume — let the channel fill up.
    thread::sleep(Duration::from_millis(200));
    handle.stop();

    // We should have exactly 1 in the channel (capacity) — others were dropped.
    let snap = rx.try_recv().unwrap();
    assert_eq!(snap.tick_id, 1);
}

#[test]
fn monotonic_and_wall_clocks_work() {
    let m = monotonic_ns();
    let w = wall_clock_ns();
    assert!(m > 0);
    assert!(w > 0);
}

#[test]
fn poller_config_default() {
    let cfg = PollerConfig::default();
    assert_eq!(cfg.stats_socket, PathBuf::from("/run/vpp/stats.sock"));
    assert_eq!(cfg.interval, Duration::from_millis(1000));
}

#[test]
fn poller_handle_drop_stops_thread() {
    // spawn() defers the actual socket connection to the poller loop,
    // so a nonexistent path won't panic here — it just retries internally.
    let config = PollerConfig {
        stats_socket: PathBuf::from("/tmp/nonexistent-infmon-poller-drop-test.sock"),
        interval: Duration::from_millis(100),
    };
    let (tx, _rx) = mpsc::sync_channel::<Arc<FlowStatsSnapshot>>(2);
    // Dropping handle should stop the thread cleanly
    let handle = spawn(config, vec![tx]);
    drop(handle);
}

#[test]
fn poller_with_no_senders() {
    // spawn() defers connection — nonexistent path retries internally without panic.
    let config = PollerConfig {
        stats_socket: PathBuf::from("/tmp/nonexistent-infmon-no-senders-test.sock"),
        interval: Duration::from_millis(100),
    };
    let handle = spawn(config, vec![]);
    thread::sleep(Duration::from_millis(150));
    handle.stop();
}

#[test]
fn poller_with_multiple_senders() {
    // spawn() defers connection — nonexistent path retries internally without panic.
    let config = PollerConfig {
        stats_socket: PathBuf::from("/tmp/nonexistent-infmon-multi-sender-test.sock"),
        interval: Duration::from_millis(100),
    };
    let (tx1, _rx1) = mpsc::sync_channel::<Arc<FlowStatsSnapshot>>(2);
    let (tx2, _rx2) = mpsc::sync_channel::<Arc<FlowStatsSnapshot>>(2);
    let handle = spawn(config, vec![tx1, tx2]);
    thread::sleep(Duration::from_millis(150));
    handle.stop();
}

#[test]
fn decode_snapshot_with_descriptor_but_no_slots() {
    use infmon_common::ipc::types::FlowRuleId;
    let raw = infmon_common::ipc::stats_client::RawSnapshot {
        descriptors: vec![infmon_common::ipc::stats_client::RawDescriptor {
            flow_rule_id: FlowRuleId { hi: 1, lo: 2 },
            flow_rule_index: 0,
            generation: 1,
            epoch_ns: 0,
            slots: vec![],
            key_arena: vec![],
            insert_failed: 5,
            table_full: 0,
        }],
    };
    // decode_snapshot(raw, tick_id, interval_ns, monotonic_ns, wall_clock_ns)
    let snap = decode_snapshot(raw, 3, 1000, 3000, 2000);
    assert_eq!(snap.tick_id, 3);
    assert_eq!(snap.interval_ns, 1000); // 3000 - 2000
    assert_eq!(snap.flow_rules.len(), 1);
    assert!(snap.flow_rules[0].flows.is_empty());
    assert_eq!(snap.flow_rules[0].counters.drops, 5);
}

#[test]
fn decode_snapshot_skips_zero_key_len_slots() {
    use infmon_common::ipc::types::FlowRuleId;
    let raw = infmon_common::ipc::stats_client::RawSnapshot {
        descriptors: vec![infmon_common::ipc::stats_client::RawDescriptor {
            flow_rule_id: FlowRuleId { hi: 0, lo: 1 },
            flow_rule_index: 0,
            generation: 1,
            epoch_ns: 0,
            slots: vec![infmon_common::ipc::stats_client::RawSlot {
                key_hash: 0,
                packets: 100,
                bytes: 5000,
                key_offset: 0,
                key_len: 0, // should be filtered out
                flags: 0,
                last_update: 0,
            }],
            key_arena: vec![],
            insert_failed: 0,
            table_full: 0,
        }],
    };
    let snap = decode_snapshot(raw, 1, 0, 0, 0);
    assert!(snap.flow_rules[0].flows.is_empty());
}

#[test]
fn decode_snapshot_skips_out_of_bounds_key() {
    use infmon_common::ipc::types::FlowRuleId;
    let raw = infmon_common::ipc::stats_client::RawSnapshot {
        descriptors: vec![infmon_common::ipc::stats_client::RawDescriptor {
            flow_rule_id: FlowRuleId { hi: 0, lo: 1 },
            flow_rule_index: 0,
            generation: 1,
            epoch_ns: 0,
            slots: vec![infmon_common::ipc::stats_client::RawSlot {
                key_hash: 123,
                packets: 10,
                bytes: 500,
                key_offset: 100, // out of bounds
                key_len: 10,
                flags: 0,
                last_update: 0,
            }],
            key_arena: vec![0u8; 4], // too small
            insert_failed: 0,
            table_full: 0,
        }],
    };
    let snap = decode_snapshot(raw, 1, 0, 0, 0);
    assert!(snap.flow_rules[0].flows.is_empty());
}

#[test]
fn sleep_interruptible_stops_early() {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let start = std::time::Instant::now();

    // Set stop after 50ms from another thread
    let stop2 = Arc::clone(&stop);
    let t = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        stop2.store(true, std::sync::atomic::Ordering::Release);
    });

    sleep_interruptible(Duration::from_secs(10), &stop);
    let elapsed = start.elapsed();
    t.join().unwrap();

    // Should have stopped well before 10 seconds
    assert!(elapsed < Duration::from_secs(1));
}
