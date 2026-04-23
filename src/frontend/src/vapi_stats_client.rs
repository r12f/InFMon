// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Riff
//
// VPP VAPI-based stats client.
// Stats client using VPP binary API calls.

use std::ffi::CString;
use std::os::raw::{c_int, c_void};

use infmon_common::ipc::stats_client::{RawDescriptor, RawSlot, RawSnapshot};
use infmon_common::ipc::types::FlowRuleId;

use super::vapi_ffi;

/// Error type for VAPI operations.
#[derive(Debug)]
pub enum VapiError {
    ConnectFailed,
    SnapshotFailed(String),
    ListFailed,
    FlowRuleFailed(String),
}

impl std::fmt::Display for VapiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VapiError::ConnectFailed => write!(f, "failed to connect to VPP API"),
            VapiError::SnapshotFailed(s) => write!(f, "snapshot_inline_dump failed: {}", s),
            VapiError::ListFailed => write!(f, "list_flow_rules failed"),
            VapiError::FlowRuleFailed(s) => write!(f, "flow rule operation failed: {}", s),
        }
    }
}

impl std::error::Error for VapiError {}

/// VAPI-based stats client.
/// Connects to VPP and retrieves flow stats via infmon_snapshot_inline_dump.
pub struct VapiStatsClient {
    handle: *mut c_void,
}

// SAFETY: VapiStatsClient wraps a raw VAPI handle that is not inherently
// thread-safe. We guarantee safety by confining all usage to the single
// poller thread — no concurrent access occurs.
unsafe impl Send for VapiStatsClient {}

impl VapiStatsClient {
    /// Connect to VPP API.
    pub fn connect(name: &str) -> Result<Self, VapiError> {
        let c_name = CString::new(name).map_err(|_| VapiError::ConnectFailed)?;
        let handle = unsafe { vapi_ffi::infmon_vapi_connect(c_name.as_ptr()) };
        if handle.is_null() {
            return Err(VapiError::ConnectFailed);
        }
        Ok(Self { handle })
    }

    /// List all flow rule IDs known to VPP.
    pub fn list_flow_rules(&self) -> Result<Vec<FlowRuleId>, VapiError> {
        let mut ids: Vec<FlowRuleId> = Vec::new();

        unsafe extern "C" fn list_cb(hi: u64, lo: u64, ctx: *mut c_void) {
            let ids = &mut *(ctx as *mut Vec<FlowRuleId>);
            ids.push(FlowRuleId { hi, lo });
        }

        let rv = unsafe {
            vapi_ffi::infmon_vapi_list_flow_rules(
                self.handle,
                Some(list_cb),
                &mut ids as *mut Vec<FlowRuleId> as *mut c_void,
            )
        };

        if rv != 0 {
            return Err(VapiError::ListFailed);
        }

        Ok(ids)
    }

    /// Perform snapshot_and_clear via VPP binary API for all known flow rules.
    /// Returns a RawSnapshot compatible with the existing decode pipeline.
    pub fn snapshot_and_clear(&self) -> Result<RawSnapshot, VapiError> {
        // First list all flow rules
        let flow_rule_ids = self.list_flow_rules()?;

        let mut descriptors = Vec::new();

        for frid in &flow_rule_ids {
            let hi = frid.hi;
            let lo = frid.lo;

            let mut entries: Vec<CollectedEntry> = Vec::new();

            unsafe extern "C" fn entry_cb(
                entry: *const vapi_ffi::infmon_ffi_flow_entry_t,
                ctx: *mut c_void,
            ) -> c_int {
                let entries = &mut *(ctx as *mut Vec<CollectedEntry>);
                let e = &*entry;

                let key_bytes = if e.key_len > 0 && !e.key_data.is_null() {
                    std::slice::from_raw_parts(e.key_data, e.key_len as usize).to_vec()
                } else {
                    Vec::new()
                };

                entries.push(CollectedEntry {
                    _flow_rule_id_hi: e.flow_rule_id_hi,
                    _flow_rule_id_lo: e.flow_rule_id_lo,
                    generation: e.generation,
                    epoch_ns: e.epoch_ns,
                    insert_failed: e.insert_failed,
                    table_full: e.table_full,
                    key_hash: e.key_hash,
                    packets: e.packets,
                    bytes: e.bytes,
                    last_update: e.last_update,
                    key_bytes,
                });
                0 // continue
            }

            let rv = unsafe {
                vapi_ffi::infmon_vapi_snapshot_inline(
                    self.handle,
                    hi,
                    lo,
                    Some(entry_cb),
                    &mut entries as *mut Vec<CollectedEntry> as *mut c_void,
                )
            };

            if rv != 0 {
                tracing::warn!("snapshot_inline for flow_rule {frid} failed (rv={rv})");
                continue;
            }

            if entries.is_empty() {
                // No occupied slots for this flow rule — skip
                continue;
            }

            // Merge entries that share the same flow key across workers.
            // With per-worker counter tables, the same key can appear once per
            // worker. We merge: sum packets/bytes, max last_update.
            // `first` captures snapshot-level metadata (generation, epoch_ns,
            // insert_failed, table_full) from entries[0] — these fields are
            // identical across workers for the same flow rule, so any entry
            // would do.
            let first = &entries[0];
            let merged = merge_worker_entries(&entries);

            // Build a RawDescriptor from the merged entries.
            let mut key_arena = Vec::new();
            let mut slots = Vec::new();

            for e in &merged {
                let key_offset = key_arena.len() as u32;
                key_arena.extend_from_slice(&e.key_bytes);
                slots.push(RawSlot {
                    key_hash: e.key_hash,
                    packets: e.packets,
                    bytes: e.bytes,
                    key_offset,
                    key_len: e.key_bytes.len() as u16,
                    flags: 1, // OCCUPIED
                    last_update: e.last_update,
                });
            }

            descriptors.push(RawDescriptor {
                flow_rule_id: *frid,
                flow_rule_index: 0, // not critical for decode
                generation: first.generation,
                epoch_ns: first.epoch_ns,
                slots,
                key_arena,
                insert_failed: merged
                    .iter()
                    .map(|e| e.insert_failed)
                    .fold(0u64, u64::saturating_add),
                table_full: merged
                    .iter()
                    .map(|e| e.table_full)
                    .fold(0u64, u64::saturating_add),
            });
        }

        Ok(RawSnapshot { descriptors })
    }
}

impl Drop for VapiStatsClient {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                vapi_ffi::infmon_vapi_disconnect(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

/// Merge entries from multiple workers that share the same flow key.
///
/// Identity: `(key_hash, key_bytes)`. For matching entries we sum
/// `packets` and `bytes`, and take `max(last_update)`.
fn merge_worker_entries(entries: &[CollectedEntry]) -> Vec<CollectedEntry> {
    use std::collections::HashMap;

    // Map from (key_hash, key_bytes) → index into `merged`.
    let mut index_map: HashMap<(u64, &[u8]), usize> = HashMap::with_capacity(entries.len());
    let mut merged: Vec<CollectedEntry> = Vec::with_capacity(entries.len());

    for e in entries {
        let identity = (e.key_hash, e.key_bytes.as_slice());
        if let Some(&idx) = index_map.get(&identity) {
            let m = &mut merged[idx];
            m.packets = m.packets.saturating_add(e.packets);
            m.bytes = m.bytes.saturating_add(e.bytes);
            if e.last_update > m.last_update {
                m.last_update = e.last_update;
            }
            // generation is identical across workers for the same flow rule
            // (set at rule-add time), so no merge needed.
            if e.epoch_ns > m.epoch_ns {
                m.epoch_ns = e.epoch_ns;
            }
            m.insert_failed = m.insert_failed.saturating_add(e.insert_failed);
            m.table_full = m.table_full.saturating_add(e.table_full);
        } else {
            let idx = merged.len();
            index_map.insert(identity, idx);
            merged.push(CollectedEntry {
                _flow_rule_id_hi: e._flow_rule_id_hi,
                _flow_rule_id_lo: e._flow_rule_id_lo,
                generation: e.generation,
                epoch_ns: e.epoch_ns,
                insert_failed: e.insert_failed,
                table_full: e.table_full,
                key_hash: e.key_hash,
                packets: e.packets,
                bytes: e.bytes,
                last_update: e.last_update,
                key_bytes: e.key_bytes.clone(),
            });
        }
    }

    merged
}

/// Internal struct for collecting entries from the FFI callback.
struct CollectedEntry {
    _flow_rule_id_hi: u64,
    _flow_rule_id_lo: u64,
    generation: u64,
    epoch_ns: u64,
    insert_failed: u64,
    table_full: u64,
    key_hash: u64,
    packets: u64,
    bytes: u64,
    last_update: u64,
    key_bytes: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(
        key: &[u8],
        key_hash: u64,
        packets: u64,
        bytes: u64,
        last_update: u64,
    ) -> CollectedEntry {
        CollectedEntry {
            _flow_rule_id_hi: 1,
            _flow_rule_id_lo: 2,
            generation: 1,
            epoch_ns: 0,
            insert_failed: 0,
            table_full: 0,
            key_hash,
            packets,
            bytes,
            last_update,
            key_bytes: key.to_vec(),
        }
    }

    #[test]
    fn merge_no_duplicates() {
        let entries = vec![
            make_entry(&[1, 2, 3, 4], 0xAABB, 100, 5000, 1000),
            make_entry(&[5, 6, 7, 8], 0xCCDD, 200, 8000, 2000),
        ];
        let merged = merge_worker_entries(&entries);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].packets, 100);
        assert_eq!(merged[1].packets, 200);
    }

    #[test]
    fn merge_two_workers_same_key() {
        let entries = vec![
            make_entry(&[1, 2, 3, 4], 0xAABB, 100, 5000, 1000),
            make_entry(&[1, 2, 3, 4], 0xAABB, 50, 2500, 2000),
        ];
        let merged = merge_worker_entries(&entries);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].packets, 150);
        assert_eq!(merged[0].bytes, 7500);
        assert_eq!(merged[0].last_update, 2000);
    }

    #[test]
    fn merge_three_workers_same_key() {
        let entries = vec![
            make_entry(&[10], 0x11, 10, 100, 500),
            make_entry(&[10], 0x11, 20, 200, 300),
            make_entry(&[10], 0x11, 30, 300, 800),
        ];
        let merged = merge_worker_entries(&entries);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].packets, 60);
        assert_eq!(merged[0].bytes, 600);
        assert_eq!(merged[0].last_update, 800);
    }

    #[test]
    fn merge_mixed_unique_and_duplicate() {
        let entries = vec![
            make_entry(&[1], 0xAA, 10, 100, 100),
            make_entry(&[2], 0xBB, 20, 200, 200),
            make_entry(&[1], 0xAA, 30, 300, 300), // dup of first
        ];
        let merged = merge_worker_entries(&entries);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].packets, 40); // 10 + 30
        assert_eq!(merged[0].bytes, 400);
        assert_eq!(merged[0].last_update, 300);
        assert_eq!(merged[1].packets, 20);
    }

    #[test]
    fn merge_same_hash_different_key_not_merged() {
        // Hash collision: same hash but different key bytes
        let entries = vec![
            make_entry(&[1, 2], 0xAAAA, 10, 100, 100),
            make_entry(&[3, 4], 0xAAAA, 20, 200, 200),
        ];
        let merged = merge_worker_entries(&entries);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_empty() {
        let merged = merge_worker_entries(&[]);
        assert!(merged.is_empty());
    }

    #[test]
    fn merge_saturating_add() {
        let entries = vec![
            make_entry(&[1], 0xAA, u64::MAX - 10, u64::MAX, 100),
            make_entry(&[1], 0xAA, 20, 500, 200),
        ];
        let merged = merge_worker_entries(&entries);
        assert_eq!(merged[0].packets, u64::MAX); // saturating
        assert_eq!(merged[0].bytes, u64::MAX);
    }
}
