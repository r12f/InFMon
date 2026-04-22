// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Riff
//
// Rust FFI bindings for the VAPI-based stats client.
// Only compiled when cfg(feature = "vapi") is set by build.rs.

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_int, c_void};

/// A single flow entry returned by snapshot_inline_dump.
#[repr(C)]
pub struct infmon_ffi_flow_entry_t {
    pub flow_rule_id_hi: u64,
    pub flow_rule_id_lo: u64,
    pub generation: u64,
    pub epoch_ns: u64,
    pub insert_failed: u64,
    pub table_full: u64,
    pub key_hash: u64,
    pub packets: u64,
    pub bytes: u64,
    pub last_update: u64,
    pub key_len: u16,
    pub key_data: *const u8,
}

// Compile-time check: ensure the FFI struct layout matches the C side.
// 10 × u64 (80) + 1 × u16 (2) + 6 padding + 1 × pointer (8) = 96 on 64-bit.
const _: () = assert!(std::mem::size_of::<infmon_ffi_flow_entry_t>() == 96);

/// Callback type for flow entries.
pub type infmon_ffi_entry_cb =
    Option<unsafe extern "C" fn(entry: *const infmon_ffi_flow_entry_t, ctx: *mut c_void) -> c_int>;

/// Callback type for flow rule list.
pub type infmon_ffi_list_cb = Option<unsafe extern "C" fn(hi: u64, lo: u64, ctx: *mut c_void)>;

extern "C" {
    pub fn infmon_vapi_connect(name: *const c_char) -> *mut c_void;
    pub fn infmon_vapi_disconnect(handle: *mut c_void);
    pub fn infmon_vapi_snapshot_inline(
        handle: *mut c_void,
        flow_rule_id_hi: u64,
        flow_rule_id_lo: u64,
        cb: infmon_ffi_entry_cb,
        cb_ctx: *mut c_void,
    ) -> c_int;
    pub fn infmon_vapi_list_flow_rules(
        handle: *mut c_void,
        cb: infmon_ffi_list_cb,
        ctx: *mut c_void,
    ) -> c_int;
}
