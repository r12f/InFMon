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

use infmon_common::ipc::stats_client::InFMonStatsClient;
use infmon_common::ipc::types::FlowStatsSnapshot;

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
        tracing::error!("clock_gettime(CLOCK_MONOTONIC) failed: {}", ret);
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
        tracing::error!("clock_gettime(CLOCK_REALTIME) failed: {}", ret);
        return 0;
    }
    debug_assert!(ts.tv_sec >= 0, "CLOCK_REALTIME returned negative tv_sec");
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Try to open the stats client, returning None on failure.
fn try_connect(path: &Path) -> Option<InFMonStatsClient> {
    match InFMonStatsClient::open(path) {
        Ok(c) => {
            tracing::info!("connected to stats segment at {}", path.display());
            Some(c)
        }
        Err(e) => {
            tracing::warn!(
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
    raw: infmon_common::ipc::stats_client::RawSnapshot,
    tick_id: u64,
    wall: u64,
    mono: u64,
    prev_mono: u64,
) -> FlowStatsSnapshot {
    use infmon_common::ipc::decode::decode_key;
    use infmon_common::ipc::types::*;

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
                        tracing::warn!("slot key extends past arena, skipping");
                        return None;
                    }
                    let key_bytes = &desc.key_arena[start..end];
                    let key = match decode_key(&fields, key_bytes) {
                        Ok(k) => k,
                        Err(e) => {
                            tracing::warn!("failed to decode flow key: {}", e);
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
                tracing::debug!("reconnect backoff: {:?}", backoff);
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
                            tracing::warn!(
                                "exporter {} channel full, dropping snapshot (tick {})",
                                i,
                                tick_id
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("snapshot_and_clear failed: {} — disconnecting", e);
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

    tracing::info!("poller stopped after {} ticks", tick_id);
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
#[path = "poller_tests.rs"]
mod poller_tests;
