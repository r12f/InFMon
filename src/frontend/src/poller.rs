//! Poller thread: drives the 1 Hz tick loop.
//!
//! The poller owns the [`InFMonStatsClient`] and on each tick:
//! 1. Reads and clears the stats segment via `snapshot_and_clear`.
//! 2. Decodes the raw snapshot into a [`FlowStatsSnapshot`].
//! 3. Wraps it in `Arc` and fans it out to exporter channels.
//! 4. Drops the snapshot — nothing is retained across ticks.
//!
//! On disconnect the poller skips the tick and reconnects with
//! exponential backoff.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use infmon_ipc::stats_client::InFMonStatsClient;
use infmon_ipc::types::FlowStatsSnapshot;

/// Sender half that exporter threads receive snapshots through.
pub type SnapshotSender = std::sync::mpsc::SyncSender<Arc<FlowStatsSnapshot>>;

/// Configuration for the poller thread.
#[derive(Debug, Clone)]
pub struct PollerConfig {
    /// Path to the VPP stats segment socket.
    pub stats_socket: PathBuf,
    /// Polling interval (default: 1000 ms).
    pub interval: Duration,
}

impl Default for PollerConfig {
    fn default() -> Self {
        Self {
            stats_socket: PathBuf::from("/run/vpp/stats.sock"),
            interval: Duration::from_millis(1000),
        }
    }
}

/// Handle to a running poller thread.
#[must_use = "dropping the handle immediately stops the poller"]
pub struct PollerHandle {
    join: Option<thread::JoinHandle<()>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl PollerHandle {
    /// Signal the poller to stop and wait for it to finish.
    pub fn stop(mut self) {
        self.shutdown();
    }

    /// Internal: signal stop and join the thread.
    fn shutdown(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

impl Drop for PollerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawn the poller thread. Returns a handle to stop it.
///
/// `senders` are bounded channels to each registered exporter. The
/// poller will try to send to every exporter; if a channel is full the
/// snapshot is dropped for that exporter (backpressure, §7 of spec).
pub fn spawn(config: PollerConfig, senders: Vec<SnapshotSender>) -> PollerHandle {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();

    let join = thread::Builder::new()
        .name("poller".into())
        .spawn(move || {
            run_loop(&config, &senders, &stop2);
        })
        .expect("failed to spawn poller thread");

    PollerHandle {
        join: Some(join),
        stop,
    }
}

// ── internals ──────────────────────────────────────────────────────

/// Read monotonic clock in nanoseconds.
fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: CLOCK_MONOTONIC is always valid.
    let ret = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    if ret != 0 {
        log::error!("clock_gettime(CLOCK_MONOTONIC) failed: {}", ret);
        return 0;
    }
    debug_assert!(ts.tv_sec >= 0, "CLOCK_MONOTONIC returned negative tv_sec");
    debug_assert!(ts.tv_nsec >= 0, "CLOCK_MONOTONIC returned negative tv_nsec");
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Read wall-clock (CLOCK_REALTIME) in nanoseconds.
fn wall_clock_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let ret = unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) };
    if ret != 0 {
        log::error!("clock_gettime(CLOCK_REALTIME) failed: {}", ret);
        return 0;
    }
    debug_assert!(ts.tv_sec >= 0, "CLOCK_REALTIME returned negative tv_sec");
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Try to open the stats client, returning None on failure.
fn try_connect(path: &Path) -> Option<InFMonStatsClient> {
    match InFMonStatsClient::open(path) {
        Ok(c) => {
            log::info!("connected to stats segment at {}", path.display());
            Some(c)
        }
        Err(e) => {
            log::warn!(
                "failed to connect to stats segment at {}: {}",
                path.display(),
                e
            );
            None
        }
    }
}

/// Decode a `RawSnapshot` into a `FlowStatsSnapshot`.
fn decode_snapshot(
    raw: infmon_ipc::stats_client::RawSnapshot,
    tick_id: u64,
    wall: u64,
    mono: u64,
    prev_mono: u64,
) -> FlowStatsSnapshot {
    use infmon_ipc::decode::decode_key;
    use infmon_ipc::types::*;

    let interval_ns = if tick_id == 1 || prev_mono == 0 {
        0
    } else {
        mono.saturating_sub(prev_mono)
    };

    let flow_rules = raw
        .descriptors
        .into_iter()
        .map(|desc| {
            // TODO: field metadata is not yet encoded in the raw descriptor.
            // Once the backend includes it, populate this from `desc` so that
            // `decode_key` can produce meaningful flow keys. Until then, all
            // keys are decoded with an empty schema.
            let fields: Vec<FieldId> = Vec::new();

            let flows = desc
                .slots
                .into_iter()
                .filter(|slot| slot.key_len > 0)
                .filter_map(|slot| {
                    let start = slot.key_offset as usize;
                    let end = start + slot.key_len as usize;
                    if end > desc.key_arena.len() {
                        log::warn!("slot key extends past arena, skipping");
                        return None;
                    }
                    let key_bytes = &desc.key_arena[start..end];
                    let key = match decode_key(&fields, key_bytes) {
                        Ok(k) => k,
                        Err(e) => {
                            log::warn!("failed to decode flow key: {}", e);
                            return None;
                        }
                    };
                    Some(FlowStats {
                        key,
                        counters: FlowCounters {
                            packets: slot.packets,
                            bytes: slot.bytes,
                            // TODO: first_seen_ns is not available in the raw
                            // slot data. Wire it in once the backend tracks it.
                            first_seen_ns: 0,
                            last_seen_ns: slot.last_update,
                        },
                    })
                })
                .collect();

            FlowRuleStats {
                name: desc.flow_rule_id.to_string(),
                fields,
                flows,
                counters: FlowRuleCounters {
                    // TODO: aggregate packet/byte counters are not yet
                    // available in RawDescriptor. Wire them in once the
                    // backend exposes per-rule aggregate stats.
                    evictions: 0,
                    drops: desc.insert_failed,
                    packets: 0,
                    bytes: 0,
                },
            }
        })
        .collect();

    FlowStatsSnapshot {
        tick_id,
        wall_clock_ns: wall,
        monotonic_ns: mono,
        interval_ns,
        flow_rules,
    }
}

/// Main poller loop.
fn run_loop(
    config: &PollerConfig,
    senders: &[SnapshotSender],
    stop: &std::sync::atomic::AtomicBool,
) {
    let mut client: Option<InFMonStatsClient> = None;
    let mut tick_id: u64 = 0;
    let mut prev_mono: u64 = 0;
    let mut backoff = Duration::from_millis(100);
    let max_backoff = Duration::from_secs(30);

    while !stop.load(std::sync::atomic::Ordering::Acquire) {
        let tick_start = monotonic_ns();

        // Ensure connected.
        if client.is_none() {
            client = try_connect(&config.stats_socket);
            if client.is_none() {
                // Exponential backoff on reconnection.
                log::debug!("reconnect backoff: {:?}", backoff);
                sleep_interruptible(backoff, stop);
                backoff = (backoff * 2).min(max_backoff);
                continue;
            }
            backoff = Duration::from_millis(100);
            // Reset prev_mono so the first tick after reconnect doesn't
            // include the entire disconnect duration in interval_ns.
            prev_mono = 0;
        }

        // Perform snapshot.
        if let Some(c) = client.as_ref() {
            match c.snapshot_and_clear() {
                Ok(raw) => {
                    tick_id += 1;
                    let wall = wall_clock_ns();
                    let mono = monotonic_ns();
                    let snapshot = decode_snapshot(raw, tick_id, wall, mono, prev_mono);
                    prev_mono = mono;

                    let snap = Arc::new(snapshot);

                    // Fan out to exporters.
                    for (i, tx) in senders.iter().enumerate() {
                        if tx.try_send(snap.clone()).is_err() {
                            log::warn!(
                                "exporter {} channel full, dropping snapshot (tick {})",
                                i,
                                tick_id
                            );
                        }
                    }
                }
                Err(e) => {
                    log::warn!("snapshot_and_clear failed: {} — disconnecting", e);
                    client = None;
                    continue;
                }
            }
        }

        // Sleep until next tick, accounting for time spent in this tick.
        let elapsed = Duration::from_nanos(monotonic_ns().saturating_sub(tick_start));
        if let Some(remaining) = config.interval.checked_sub(elapsed) {
            sleep_interruptible(remaining, stop);
        }
    }

    log::info!("poller stopped after {} ticks", tick_id);
}

/// Sleep for `dur`, but wake early if `stop` is set.
/// Checks every 50 ms.
fn sleep_interruptible(dur: Duration, stop: &std::sync::atomic::AtomicBool) {
    let check = Duration::from_millis(50);
    let mut remaining = dur;
    while remaining > Duration::ZERO {
        if stop.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let sleep_time = remaining.min(check);
        thread::sleep(sleep_time);
        remaining = remaining.saturating_sub(sleep_time);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn decode_empty_raw_snapshot() {
        let raw = infmon_ipc::stats_client::RawSnapshot {
            descriptors: Vec::new(),
        };
        let snap = decode_snapshot(raw, 1, 1000, 2000, 0);
        assert_eq!(snap.tick_id, 1);
        assert_eq!(snap.interval_ns, 0); // first tick
        assert!(snap.flow_rules.is_empty());
    }

    #[test]
    fn decode_second_tick_has_interval() {
        let raw = infmon_ipc::stats_client::RawSnapshot {
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
        // Poller should work even with empty sender list
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
        use infmon_ipc::types::FlowRuleId;
        let raw = infmon_ipc::stats_client::RawSnapshot {
            descriptors: vec![infmon_ipc::stats_client::RawDescriptor {
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
        let snap = decode_snapshot(raw, 3, 1000, 3000, 2000);
        assert_eq!(snap.tick_id, 3);
        assert_eq!(snap.interval_ns, 1000); // 3000 - 2000
        assert_eq!(snap.flow_rules.len(), 1);
        assert!(snap.flow_rules[0].flows.is_empty());
        assert_eq!(snap.flow_rules[0].counters.drops, 5);
    }

    #[test]
    fn decode_snapshot_skips_zero_key_len_slots() {
        use infmon_ipc::types::FlowRuleId;
        let raw = infmon_ipc::stats_client::RawSnapshot {
            descriptors: vec![infmon_ipc::stats_client::RawDescriptor {
                flow_rule_id: FlowRuleId { hi: 0, lo: 1 },
                flow_rule_index: 0,
                generation: 1,
                epoch_ns: 0,
                slots: vec![infmon_ipc::stats_client::RawSlot {
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
        use infmon_ipc::types::FlowRuleId;
        let raw = infmon_ipc::stats_client::RawSnapshot {
            descriptors: vec![infmon_ipc::stats_client::RawDescriptor {
                flow_rule_id: FlowRuleId { hi: 0, lo: 1 },
                flow_rule_index: 0,
                generation: 1,
                epoch_ns: 0,
                slots: vec![infmon_ipc::stats_client::RawSlot {
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
        let stop = std::sync::atomic::AtomicBool::new(false);
        let start = std::time::Instant::now();

        // Set stop after 50ms from another thread
        let stop_ref = &stop as *const _ as usize;
        let t = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let stop = unsafe { &*(stop_ref as *const std::sync::atomic::AtomicBool) };
            stop.store(true, std::sync::atomic::Ordering::Release);
        });

        sleep_interruptible(Duration::from_secs(10), &stop);
        let elapsed = start.elapsed();
        t.join().unwrap();

        // Should have stopped well before 10 seconds
        assert!(elapsed < Duration::from_secs(1));
    }
}
