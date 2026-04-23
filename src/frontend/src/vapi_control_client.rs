// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Riff
//
// VPP VAPI-based control client for flow-rule CRUD.
// Forwards add/delete operations to the VPP backend.

use std::ffi::CString;
use std::os::raw::c_void;

use infmon_common::config::model::{EvictionPolicy, Field};
use infmon_common::ipc::types::FlowRuleId;

use super::vapi_ffi;
use super::vapi_stats_client::VapiError;

/// VAPI-based control client for flow-rule CRUD operations.
///
/// Holds its own VAPI connection (separate from the stats client used
/// by the poller) because the control thread and poller thread run
/// concurrently and VAPI connections are not thread-safe.
pub struct VapiControlClient {
    handle: *mut c_void,
}

// SAFETY: VapiControlClient wraps a raw VAPI handle. The handle
// is not thread-affine — VAPI's shared-memory transport is
// process-global and any thread may call into it provided calls
// are serialised. We implement Send (not Sync) so the handle can
// move between threads. When wrapped in Mutex<VapiControlClient>,
// Sync is auto-derived because VapiControlClient: Send. This is
// sound: the Mutex serialises all access, satisfying VAPI's
// single-caller requirement regardless of which thread holds the
// lock.
unsafe impl Send for VapiControlClient {}

impl VapiControlClient {
    /// Connect to VPP API for control operations.
    pub fn connect(name: &str) -> Result<Self, VapiError> {
        let c_name = CString::new(name).map_err(|_| VapiError::ConnectFailed)?;
        let handle = unsafe { vapi_ffi::infmon_vapi_connect(c_name.as_ptr()) };
        if handle.is_null() {
            return Err(VapiError::ConnectFailed);
        }
        Ok(Self { handle })
    }

    /// Add a flow rule to the VPP backend.
    ///
    /// Returns the backend-assigned flow rule ID on success.
    pub fn flow_rule_add(
        &self,
        name: &str,
        fields: &[Field],
        max_keys: u32,
        eviction_policy: EvictionPolicy,
    ) -> Result<FlowRuleId, VapiError> {
        let c_name =
            CString::new(name).map_err(|_| VapiError::FlowRuleFailed("invalid name".into()))?;

        // Convert Field enums to u8 values matching the backend's infmon_field_t
        let field_bytes: Vec<u8> = fields.iter().map(|f| field_to_u8(*f)).collect();

        let eviction = match eviction_policy {
            EvictionPolicy::LruDrop => 0u8,
            _ => 0u8, // Default to LruDrop for unknown policies
        };

        let mut id_hi: u64 = 0;
        let mut id_lo: u64 = 0;

        let rv = unsafe {
            vapi_ffi::infmon_vapi_flow_rule_add(
                self.handle,
                c_name.as_ptr(),
                field_bytes.as_ptr(),
                field_bytes.len() as u32,
                max_keys,
                eviction,
                &mut id_hi,
                &mut id_lo,
            )
        };

        if rv != 0 {
            return Err(VapiError::FlowRuleFailed(format!(
                "flow_rule_add failed (rv={rv})"
            )));
        }

        Ok(FlowRuleId {
            hi: id_hi,
            lo: id_lo,
        })
    }

    /// Delete a flow rule from the VPP backend by its ID.
    pub fn flow_rule_del(&self, id: &FlowRuleId) -> Result<(), VapiError> {
        let rv = unsafe { vapi_ffi::infmon_vapi_flow_rule_del(self.handle, id.hi, id.lo) };

        if rv != 0 {
            return Err(VapiError::FlowRuleFailed(format!(
                "flow_rule_del failed (rv={rv})"
            )));
        }

        Ok(())
    }
}

impl Drop for VapiControlClient {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                vapi_ffi::infmon_vapi_disconnect(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

/// Convert a config `Field` to its backend `infmon_field_t` u8 value.
fn field_to_u8(f: Field) -> u8 {
    match f {
        Field::SrcIp => 0,
        Field::DstIp => 1,
        Field::IpProto => 2,
        Field::Dscp => 3,
        Field::MirrorSrcIp => 4,
        Field::SrcPort => 5,
        Field::DstPort => 6,
        // Field is #[non_exhaustive]; panic on unknown variants so
        // new additions surface as a compile-test failure rather than
        // silently mapping to SrcIp.
        _ => panic!("unsupported Field variant for VAPI mapping"),
    }
}
