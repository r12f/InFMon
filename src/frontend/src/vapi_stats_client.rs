// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Riff
//
// VPP VAPI-based stats client.
// Replaces the shared-memory InFMonStatsClient with VPP binary API calls.

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
}

impl std::fmt::Display for VapiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VapiError::ConnectFailed => write!(f, "failed to connect to VPP API"),
            VapiError::SnapshotFailed(s) => write!(f, "snapshot_inline_dump failed: {}", s),
            VapiError::ListFailed => write!(f, "list_flow_rules failed"),
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

            // Build a RawDescriptor from the collected entries.
            // We assemble a key_arena by concatenating all key bytes.
            let first = &entries[0];
            let mut key_arena = Vec::new();
            let mut slots = Vec::new();

            // Table-level metadata (generation, epoch, counters) comes from the
            // first entry in the dump — all entries share the same snapshot context.

            for e in &entries {
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
                insert_failed: first.insert_failed,
                table_full: first.table_full,
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
