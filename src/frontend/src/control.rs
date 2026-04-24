//! Control socket server for CLI ↔ frontend RPC.
//!
//! Listens on a Unix domain socket and dispatches JSON-line requests
//! from `infmonctl` to the frontend's in-memory state.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

use infmon_common::config::model::FlowRule;
use infmon_common::ipc::protocol::*;
use infmon_common::ipc::types::FlowStatsSnapshot;

#[cfg(feature = "vapi")]
use crate::vapi_control_client::VapiControlClient;
#[cfg(feature = "vapi")]
use infmon_common::ipc::types::FlowRuleId;
#[cfg(feature = "vapi")]
use std::collections::HashMap;

/// Shared state accessible to the control server.
///
/// **Lock ordering invariant**: always acquire `flow_rules` before
/// `latest_snapshot`. Never reverse to avoid deadlocks.
pub struct ControlState {
    /// Flow rules configured via the CLI (or loaded from config).
    pub flow_rules: RwLock<Vec<FlowRule>>,
    /// Latest stats snapshot from the poller (if any).
    pub latest_snapshot: Mutex<Option<Arc<FlowStatsSnapshot>>>,
    /// Monotonic counter for generating unique flow rule IDs.
    next_rule_id: AtomicU64,
    /// Optional VAPI control client for forwarding CRUD to the VPP backend.
    #[cfg(feature = "vapi")]
    pub vapi_control: Option<Mutex<VapiControlClient>>,
    /// Maps flow rule name → backend-assigned ID (populated when VAPI is used).
    #[cfg(feature = "vapi")]
    pub rule_id_map: Mutex<HashMap<String, FlowRuleId>>,
}

impl ControlState {
    pub fn new(initial_rules: Vec<FlowRule>) -> Self {
        let initial_count = initial_rules.len() as u64;
        Self {
            flow_rules: RwLock::new(initial_rules),
            latest_snapshot: Mutex::new(None),
            next_rule_id: AtomicU64::new(initial_count + 1),
            #[cfg(feature = "vapi")]
            vapi_control: None,
            #[cfg(feature = "vapi")]
            rule_id_map: Mutex::new(HashMap::new()),
        }
    }

    /// Replace the latest snapshot (called by the snapshot‐forwarding thread).
    ///
    /// Intentionally recovers from mutex poison (`into_inner()`) — losing a
    /// snapshot is preferable to panicking on the stats path.
    pub fn update_snapshot(&self, snap: Arc<FlowStatsSnapshot>) {
        *self
            .latest_snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(snap);
    }

    /// Create a ControlState with a VAPI control client for backend forwarding.
    #[cfg(feature = "vapi")]
    pub fn with_vapi(initial_rules: Vec<FlowRule>, client: VapiControlClient) -> Self {
        let initial_count = initial_rules.len() as u64;
        Self {
            flow_rules: RwLock::new(initial_rules),
            latest_snapshot: Mutex::new(None),
            next_rule_id: AtomicU64::new(initial_count + 1),
            vapi_control: Some(Mutex::new(client)),
            rule_id_map: Mutex::new(HashMap::new()),
        }
    }
}

/// Handle to a running control server thread.
pub struct ControlHandle {
    join: Option<thread::JoinHandle<()>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    socket_path: PathBuf,
}

impl ControlHandle {
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        // Connect to self to unblock accept()
        let _ = std::os::unix::net::UnixStream::connect(&self.socket_path);
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ControlHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawn the control server thread.
pub fn spawn(socket_path: &Path, state: Arc<ControlState>) -> std::io::Result<ControlHandle> {
    use std::os::unix::net::UnixListener;

    // Remove stale socket file
    let _ = std::fs::remove_file(socket_path);

    // Create parent directory if needed
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(socket_path)?;
    listener.set_nonblocking(false)?;

    // Restrict socket to owner-only (0o600) so unprivileged local users
    // cannot connect and mutate flow rules.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    }

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let path = socket_path.to_path_buf();
    let path2 = socket_path.to_path_buf();

    let join = thread::Builder::new()
        .name("control".into())
        .spawn(move || {
            run_server(listener, state, &stop2);
            // Clean up socket file
            let _ = std::fs::remove_file(&path);
        })?;

    Ok(ControlHandle {
        join: Some(join),
        stop,
        socket_path: path2,
    })
}

fn run_server(
    listener: std::os::unix::net::UnixListener,
    state: Arc<ControlState>,
    stop: &std::sync::atomic::AtomicBool,
) {
    use std::io::{BufRead, BufReader, Read, Write};

    // Set a timeout so we periodically check the stop flag
    listener
        .set_nonblocking(false)
        .expect("set_nonblocking failed");

    for stream in listener.incoming() {
        if stop.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }

        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                if stop.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                tracing::warn!("control: accept error: {e}");
                continue;
            }
        };

        // Set a per-connection read/write timeout
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(10)));
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(10)));

        // Cap read at 64 KiB during I/O (via Take adapter) to prevent OOM
        // from malicious clients sending multi-GB lines without newlines.
        const MAX_LINE: usize = 64 * 1024;
        let limited = (&stream).take(MAX_LINE as u64);
        let mut reader = BufReader::new(limited);
        let mut line = String::new();

        match reader.read_line(&mut line) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("control: read error: {e}");
                continue;
            }
        }

        // If we hit the limit without a newline, the request is too large.
        if line.len() >= MAX_LINE && !line.ends_with('\n') {
            let resp = Response::err(-1, "request too large");
            let mut resp_line = serde_json::to_string(&resp).unwrap_or_default();
            resp_line.push('\n');
            let mut writer = &stream;
            let _ = writer.write_all(resp_line.as_bytes());
            continue;
        }

        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle_request(&req, &state),
            Err(e) => Response::err(-1, format!("invalid request: {e}")),
        };

        let mut resp_line = match serde_json::to_string(&response) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("control: failed to serialize response: {e}");
                continue;
            }
        };
        resp_line.push('\n');

        // Need mutable ref to write
        let mut writer = &stream;
        if let Err(e) = writer.write_all(resp_line.as_bytes()) {
            tracing::warn!("control: write error: {e}");
        }
    }
}

fn handle_request(req: &Request, state: &ControlState) -> Response {
    match req {
        Request::FlowRuleAdd(params) => handle_flow_rule_add(params, state),
        Request::FlowRuleRm(params) => handle_flow_rule_rm(params, state),
        Request::FlowRuleList => handle_flow_rule_list(state),
        Request::FlowRuleShow(params) => handle_flow_rule_show(params, state),
        Request::StatsShow(params) => handle_stats_show(params, state),
    }
}

fn handle_flow_rule_add(params: &FlowRuleAddParams, state: &ControlState) -> Response {
    let rule = FlowRule {
        name: params.name.clone(),
        fields: params.fields.clone(),
        max_keys: params.max_keys,
        eviction_policy: params.eviction_policy,
    };

    let mut rules = state.flow_rules.write().unwrap_or_else(|e| e.into_inner());

    if rules.iter().any(|r| r.name == rule.name) {
        return Response::err(6, format!("flow rule '{}' already exists", rule.name));
    }

    // Forward to VPP backend via VAPI if available
    #[cfg(feature = "vapi")]
    let id_str = {
        if let Some(ref vapi_mutex) = state.vapi_control {
            let vapi = vapi_mutex.lock().unwrap_or_else(|e| e.into_inner());
            match vapi.flow_rule_add(
                &rule.name,
                &rule.fields,
                rule.max_keys,
                rule.eviction_policy,
            ) {
                Ok(id) => {
                    tracing::info!(
                        "flow rule '{}' added to backend (id={:016x}-{:016x})",
                        rule.name,
                        id.hi,
                        id.lo
                    );
                    let id_str = format!("{:016x}-{:016x}", id.hi, id.lo);
                    // Store name → ID mapping for later deletion
                    let mut map = state.rule_id_map.lock().unwrap_or_else(|e| e.into_inner());
                    map.insert(rule.name.clone(), id);
                    rules.push(rule);
                    id_str
                }
                Err(e) => {
                    return Response::err(
                        7,
                        format!("backend rejected flow rule '{}': {e}", params.name),
                    );
                }
            }
        } else {
            // No VAPI client — local-only mode
            rules.push(rule);
            let seq = state
                .next_rule_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            format!("{:016x}-{:016x}", seq, 0u64)
        }
    };

    #[cfg(not(feature = "vapi"))]
    let id_str = {
        rules.push(rule);
        let seq = state
            .next_rule_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("{:016x}-{:016x}", seq, 0u64)
    };

    Response::ok(ResponseData::FlowRuleId(FlowRuleIdData { id: id_str }))
}

fn handle_flow_rule_rm(params: &FlowRuleRmParams, state: &ControlState) -> Response {
    // Check local existence before dispatching backend delete to avoid
    // removing a rule from the backend when it doesn't exist locally.
    {
        let rules = state.flow_rules.read().unwrap_or_else(|e| e.into_inner());
        if !rules.iter().any(|r| r.name == params.name) {
            return Response::err(3, format!("flow rule '{}' not found", params.name));
        }
    }

    // Forward delete to VPP backend via VAPI if available
    #[cfg(feature = "vapi")]
    {
        if let Some(ref vapi_mutex) = state.vapi_control {
            let id = {
                let map = state.rule_id_map.lock().unwrap_or_else(|e| e.into_inner());
                map.get(&params.name).cloned()
            };
            match id {
                Some(rule_id) => {
                    let vapi = vapi_mutex.lock().unwrap_or_else(|e| e.into_inner());
                    if let Err(e) = vapi.flow_rule_del(&rule_id) {
                        return Response::err(
                            7,
                            format!("backend failed to delete flow rule '{}': {e}", params.name),
                        );
                    }
                    tracing::info!(
                        "flow rule '{}' deleted from backend (id={:016x}-{:016x})",
                        params.name,
                        rule_id.hi,
                        rule_id.lo
                    );
                }
                None => {
                    // Rule not in ID map — may not have been added via VAPI
                    tracing::warn!(
                        "flow rule '{}' has no backend ID, removing locally only",
                        params.name
                    );
                }
            }
        }
    }

    let mut rules = state.flow_rules.write().unwrap_or_else(|e| e.into_inner());
    rules.retain(|r| r.name != params.name);

    // Remove from ID map after successful local removal
    #[cfg(feature = "vapi")]
    {
        state
            .rule_id_map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&params.name);
    }

    Response::ok_empty()
}

fn handle_flow_rule_list(state: &ControlState) -> Response {
    let rules = state.flow_rules.read().unwrap_or_else(|e| e.into_inner());
    let data: Vec<FlowRuleData> = rules.iter().map(FlowRuleData::from).collect();
    Response::ok(ResponseData::FlowRuleList(FlowRuleListData { rules: data }))
}

fn handle_flow_rule_show(params: &FlowRuleShowParams, state: &ControlState) -> Response {
    let rules = state.flow_rules.read().unwrap_or_else(|e| e.into_inner());
    let rule = match rules.iter().find(|r| r.name == params.name) {
        Some(r) => r,
        None => return Response::err(3, format!("flow rule '{}' not found", params.name)),
    };

    // Try to get flow stats from the latest snapshot
    let snapshot = state
        .latest_snapshot
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (counters, flows) = if let Some(snap) = snapshot.as_ref() {
        if let Some(frs) = snap.flow_rules.iter().find(|fr| fr.name == params.name) {
            let counters = FlowRuleCountersData {
                packets: frs.counters.packets,
                bytes: frs.counters.bytes,
                evictions: frs.counters.evictions,
                drops: frs.counters.drops,
            };
            let flows: Vec<FlowEntryData> = frs
                .flows
                .iter()
                .map(|f| FlowEntryData {
                    key: f.key.iter().map(|v| format!("{:?}", v)).collect(),
                    packets: f.counters.packets,
                    bytes: f.counters.bytes,
                    first_seen_ns: f.counters.first_seen_ns,
                    last_seen_ns: f.counters.last_seen_ns,
                })
                .collect();
            (counters, flows)
        } else {
            (
                FlowRuleCountersData {
                    packets: 0,
                    bytes: 0,
                    evictions: 0,
                    drops: 0,
                },
                vec![],
            )
        }
    } else {
        (
            FlowRuleCountersData {
                packets: 0,
                bytes: 0,
                evictions: 0,
                drops: 0,
            },
            vec![],
        )
    };

    Response::ok(ResponseData::FlowRuleDetail(FlowRuleDetailData {
        name: rule.name.clone(),
        fields: rule.fields.clone(),
        max_keys: rule.max_keys,
        eviction_policy: rule.eviction_policy,
        counters,
        flows,
    }))
}

fn handle_stats_show(params: &StatsShowParams, state: &ControlState) -> Response {
    let rules = state.flow_rules.read().unwrap_or_else(|e| e.into_inner());
    let snapshot = state
        .latest_snapshot
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let mut flow_rule_stats: Vec<FlowRuleStatsData> = Vec::new();

    for rule in rules.iter() {
        if let Some(ref name_filter) = params.name {
            if !rule.name.eq(name_filter) {
                continue;
            }
        }

        let (packets, bytes, evictions, drops, active_flows) = if let Some(snap) = snapshot.as_ref()
        {
            if let Some(frs) = snap.flow_rules.iter().find(|fr| fr.name == rule.name) {
                (
                    frs.counters.packets,
                    frs.counters.bytes,
                    frs.counters.evictions,
                    frs.counters.drops,
                    frs.flows.len() as u64,
                )
            } else {
                (0, 0, 0, 0, 0)
            }
        } else {
            (0, 0, 0, 0, 0)
        };

        flow_rule_stats.push(FlowRuleStatsData {
            name: rule.name.clone(),
            packets,
            bytes,
            evictions,
            drops,
            active_flows,
        });
    }

    Response::ok(ResponseData::StatsShow(StatsShowData {
        flow_rules: flow_rule_stats,
    }))
}
