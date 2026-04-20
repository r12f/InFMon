//! Control socket server for CLI ↔ frontend RPC.
//!
//! Listens on a Unix domain socket and dispatches JSON-line requests
//! from `infmonctl` to the frontend's in-memory state.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

use infmon_common::config::model::FlowRule;
use infmon_common::ipc::protocol::*;
use infmon_common::ipc::types::FlowStatsSnapshot;

/// Shared state accessible to the control server.
pub struct ControlState {
    /// Flow rules configured via the CLI (or loaded from config).
    pub flow_rules: RwLock<Vec<FlowRule>>,
    /// Latest stats snapshot from the poller (if any).
    pub latest_snapshot: Mutex<Option<Arc<FlowStatsSnapshot>>>,
}

impl ControlState {
    pub fn new(initial_rules: Vec<FlowRule>) -> Self {
        Self {
            flow_rules: RwLock::new(initial_rules),
            latest_snapshot: Mutex::new(None),
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
    use std::io::{BufRead, BufReader, Write};

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

        let mut reader = BufReader::new(&stream);
        let mut line = String::new();

        match reader.read_line(&mut line) {
            Ok(0) => continue, // EOF
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("control: read error: {e}");
                continue;
            }
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

    let mut rules = state.flow_rules.write().unwrap();

    // Check for duplicate name
    if rules.iter().any(|r| r.name == rule.name) {
        return Response::err(6, format!("flow rule '{}' already exists", rule.name));
    }

    rules.push(rule);

    // Generate a synthetic ID
    let id = format!("{:016x}-{:016x}", rules.len() as u64, 0u64);
    Response::ok(ResponseData::FlowRuleId(FlowRuleIdData { id }))
}

fn handle_flow_rule_rm(params: &FlowRuleRmParams, state: &ControlState) -> Response {
    let mut rules = state.flow_rules.write().unwrap();
    let before = rules.len();
    rules.retain(|r| r.name != params.name);
    if rules.len() == before {
        return Response::err(3, format!("flow rule '{}' not found", params.name));
    }
    Response::ok_empty()
}

fn handle_flow_rule_list(state: &ControlState) -> Response {
    let rules = state.flow_rules.read().unwrap();
    let data: Vec<FlowRuleData> = rules.iter().map(FlowRuleData::from).collect();
    Response::ok(ResponseData::FlowRuleList(data))
}

fn handle_flow_rule_show(params: &FlowRuleShowParams, state: &ControlState) -> Response {
    let rules = state.flow_rules.read().unwrap();
    let rule = match rules.iter().find(|r| r.name == params.name) {
        Some(r) => r,
        None => return Response::err(3, format!("flow rule '{}' not found", params.name)),
    };

    // Try to get flow stats from the latest snapshot
    let snapshot = state.latest_snapshot.lock().unwrap();
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
    let rules = state.flow_rules.read().unwrap();
    let snapshot = state.latest_snapshot.lock().unwrap();

    let mut flow_rule_stats: Vec<FlowRuleStatsData> = Vec::new();

    for rule in rules.iter() {
        if let Some(ref name_filter) = params.name {
            if !rule.name.contains(name_filter) {
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
