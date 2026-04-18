# Spec 005 — Frontend architecture (Rust)

## Version history

| Version | Date       | Author       | Changes |
| ------- | ---------- | ------------ | ------- |
| 0.1     | 2026-04-18 | Riff (r12f)  | Consolidated from v0.1–v0.6. Initial draft of `infmon-frontend` (Rust). Task model, `interval_ns` defined on tick 1, single-reader enforcement, reload-rollback failure handling, stop exit code, complete tracker→flow-rule rename (`FlowRuleStats`/`FlowRuleCounters`/`FlowRuleDef`/`FlowStatsSnapshot`), metric prefix cleanup, YAML config, `polling_interval_ms`, `InFMonStatsClient`/`InFMonControlClient`, control-plane `flow_rule_*` methods, and §3.0 mental-model paragraph. |

- **Parent epic:** `DPU-4` (EPIC: InFMon — flow telemetry service on BF-3)
- **Depends on:** [`000-overview`](000-overview.md), [`002-flow-tracking-model`](002-flow-tracking-model.md), [`004-backend-architecture`](004-backend-architecture.md)
- **Related:** [`006-exporter-otlp`](006-exporter-otlp.md), [`007-cli`](007-cli.md)

## 1. Purpose

Define the architecture of `infmon-frontend` — the Rust user-space
process that turns the backend's per-flow-rule flow state into exported
telemetry. The frontend is the single seam between

- *what the data plane has counted* (Spec 004, owned by the VPP
  plugin), and
- *what observability tooling consumes* (Spec 006, OTLP today; other
  exporters tomorrow).

It is intentionally a **thin** process: no flow tracking of its own, no
persistence, no dashboarding. Its job is to poll, fan out, and stay
out of the data path's way.

## 2. Scope

In scope:

- The 1 Hz polling loop that drives every export cycle.
- The exporter trait and plugin framework that lets new exporters land
  without touching the loop.
- The frontend lifecycle (`start`, `reload`, `stop`) and how it
  interacts with the backend's reload contract (Spec 004 §x).
- Backpressure and exporter-failure handling — the rules that keep one
  slow exporter from stalling the others or the backend.
- The IPC client to the backend: stats-segment reader plus control
  API, packaged as a reusable crate shared with `infmon-cli`.
- Process model, threading, and the crate layout under
  `src/infmon-frontend/`.

Out of scope (deferred or owned elsewhere):

- The wire format and content of OTLP metrics — Spec 006.
- The CLI command surface — Spec 007 (this spec only defines the
  shared IPC client crate).
- The backend's snapshot semantics, flow layout, and reload protocol
  — Spec 004.
- A second, non-OTLP exporter (Prometheus pull, file rotation, etc.).
  The plugin framework MUST allow it; v1 ships only OTLP.
- Long-term storage, query API, GUI.

## 3. Concepts

### 3.0 Mental model: flow-rules and flows

The frontend inherits its data model from Spec 002:

- A **flow-rule** is a configured matcher (key-set + limits). It does not
  carry counters itself.
- Each flow-rule **generates one flow per distinct key tuple** it observes
  in mirrored traffic; each flow owns its own packet/byte counters.

So a `FlowRuleStats` (§3.2) is named after the flow-rule that produced it
and contains the set of flows currently live for that flow-rule. The CLI
verb split (Spec 007) follows the same shape: `flow-rule {add,rm,list,show}`
configures matchers; `flow {list,show}` reads live flows out of the
frontend's most recent aggregate.

### 3.1 Tick

A **tick** is one iteration of the export loop. v1 fires at 1 Hz from a
monotonic timer. Each tick performs, in order:

1. `snapshot_and_clear` against the backend (per flow-rule).
2. Decode the snapshot into an in-memory `FlowStatsSnapshot`.
3. Fan out the `FlowStatsSnapshot` to every registered exporter.
4. Drop the `FlowStatsSnapshot`.

The aggregate is **never** retained across ticks. The frontend holds
no flow state of its own; everything it knows is what the backend
handed it on the current tick.

### 3.2 FlowStatsSnapshot

A `FlowStatsSnapshot` is the decoded form of one tick's worth of snapshots:

```text
FlowStatsSnapshot {
    tick_id:        u64,            // monotonic, starts at 1
    wall_clock_ns:  u64,            // CLOCK_REALTIME at snapshot
    monotonic_ns:   u64,            // CLOCK_MONOTONIC at snapshot
    interval_ns:    u64,            // monotonic_ns - prev monotonic_ns;
                                    // 0 on tick_id == 1 (no prior tick).
                                    // Exporters MUST treat interval_ns == 0
                                    // as "skip rate derivation for this tick".
    flow_rules:     Vec<FlowRuleStats>,
}

FlowRuleStats {
    name:           Arc<str>,
    fields:         Arc<[FieldId]>, // ordered, from the flow-rule def
    flows:          Vec<FlowStats>,      // already decoded from raw key bytes
    counters:       FlowRuleCounters // evictions, drops, packets, bytes
}
```

Flows carry the v1 counter set defined by Spec 002 §2.3 (`packets`,
`bytes`, `first_seen_ns`, `last_seen_ns`). The `FlowStats` struct is the
**decoded frontend representation** built by `frontend-ipc` from the
raw stats-segment record (whose binary layout is Spec 004's; the wire
shape is not the in-memory shape). Concretely, in this spec:

```text
FlowStats {
    key:        Vec<FieldValue>,    // decoded per-field from raw key bytes,
                                    // ordered to match FlowRuleStats.fields
    counters:   FlowCounters,       // packets, bytes, first_seen_ns, last_seen_ns
}
```

The aggregate is shared across exporters via `Arc<FlowStatsSnapshot>`;
exporters MUST treat it as read-only.

### 3.3 Exporter

An **exporter** is a plugin that consumes an `Arc<FlowStatsSnapshot>` and
pushes telemetry somewhere — OTLP/gRPC in v1. Exporters are loaded at
start time from configuration (§5), not dynamically discovered from the
filesystem. v1 plugins are statically linked Rust crates that register
themselves through the trait in §6; dynamic loading is a non-goal.

## 4. Process model

`infmon-frontend` is a single OS process with a fixed thread layout:

| Thread        | Count | Responsibility                                                 |
|---------------|-------|----------------------------------------------------------------|
| `main`        | 1     | Process startup/shutdown, signal handling, supervises others.  |
| `poller`      | 1     | Drives the 1 Hz tick (§3.1). Owns the IPC client.              |
| `exporter-N`  | N     | One **dedicated OS thread** per registered exporter, each running its own *single-threaded* `tokio` runtime (`tokio::runtime::Builder::new_current_thread`). Receives `Arc<FlowStatsSnapshot>` via a bounded channel. |
| `control`     | 1     | Listens on the local control socket; serves CLI/admin RPCs.    |

Rationale:

- Poller is single-threaded so the snapshot order across flow-rules is
  deterministic and the IPC client has one owner.
- Each exporter is a **dedicated OS thread**, not a task on a shared
  multi-thread runtime. This is deliberate: a blocking syscall or CPU
  spin inside one exporter's `export` future cannot starve the runtime
  workers that other exporters or the control thread depend on.
  Per-exporter backpressure (§7) is just per-channel capacity.
- `control` is separate so an admin RPC during shutdown still gets
  served. It runs on its own current-thread runtime as well.

I/O inside each runtime (OTLP/gRPC for `exporter-N`, control socket
for `control`, IPC reads for `poller`) uses `tokio` primitives. The
tick cadence comes from a monotonic timer, not from a
`tokio::time::interval`, so clock adjustments cannot bunch or skip
ticks.

## 5. Configuration

The frontend reads two pieces of config at start (and at reload):

1. **Flow definitions** — the same YAML schema defined in
   Spec 002 §5. The frontend forwards these to the backend via the
   control API (§8.2); it does not interpret flow semantics.
2. **Exporter config** — per-exporter blocks, keyed by exporter type:

```yaml
# /etc/infmon/frontend.yaml

frontend:
  polling_interval_ms: 1000           # v1: only 1000 is supported
  backend_socket: "/run/infmon/backend.sock"
  control_socket: "/run/infmon/frontend.sock"
  stats_segment: "/dev/shm/infmon-stats"

exporters:
  - type: "otlp"
    name: "primary"                    # unique within frontend
    endpoint: "http://collector.local:4317"
    timeout_ms: 800                    # exporter-side deadline; independent
                                       # of tick interval. See note below.
    queue_depth: 2                     # see §7
    on_overflow: "drop_oldest"         # drop_oldest | drop_newest
```

`polling_interval_ms` is fixed at `1000` in v1; the field exists so a future spec can
move to sub-second cadence without a config break. Validation is
all-or-nothing: a bad exporter block fails the whole reload (§9.2).

`timeout_ms` is the deadline applied to a single `Exporter::export`
call by the dispatcher and is **decoupled from the tick interval**:
each exporter runs on its own thread (§4) and a slow tick on one
exporter does not delay the poller's next `snapshot_and_clear`. The
only relationship between them is operational — if `timeout_ms`
routinely approaches or exceeds `tick_interval`, channels will fill
and `on_overflow` will fire, which shows up in `frontend_drops_total`.
Pick `timeout_ms` based on the exporter's expected p99, not the tick
period.

## 6. Exporter trait

```rust
/// Implemented by every exporter plugin.
///
/// Implementations MUST be `Send + Sync` and cheap to clone the
/// receiving end of (the framework wraps them in `Arc`).
pub trait Exporter: Send + Sync + 'static {
    /// Stable identifier for logs and metrics, e.g. "otlp".
    fn kind(&self) -> &'static str;

    /// Operator-assigned instance name, unique per frontend.
    fn name(&self) -> &str;

    /// Called once per tick with the shared aggregate.
    ///
    /// MUST return within the configured `timeout_ms`. A `Pending`
    /// future at deadline is cancelled and counted as a failure.
    fn export(&self, agg: Arc<FlowStatsSnapshot>)
        -> BoxFuture<'_, Result<(), ExporterError>>;

    /// Called on `reload` with the exporter's new config block.
    /// Returning `Err` aborts the reload (see §9.2).
    fn reload(&self, cfg: &ExporterConfig) -> Result<(), ConfigError>;

    /// Called once on shutdown. Implementations SHOULD flush.
    /// Bounded by `shutdown_grace_ms` (§9.3).
    fn shutdown(&self) -> BoxFuture<'_, ()>;
}

#[derive(Debug)]
pub enum ExporterError {
    Transient(anyhow::Error), // network blip, retryable next tick
    Permanent(anyhow::Error), // config-level wrongness; reported & dropped
    Timeout,                  // exceeded timeout_ms
}
```

### 6.1 Registration

Exporters are registered in a static inventory at compile time:

```rust
inventory::submit!(ExporterRegistration {
    kind: "otlp",
    factory: |cfg| Ok(Box::new(OtlpExporter::new(cfg)?)),
});
```

The `[[exporter]]` blocks in §5 are matched to registrations by
`type`. Unknown `type` is a config error.

### 6.2 Crate layout

```text
src/infmon-frontend/
├── frontend-core/        # tick loop, lifecycle, FlowStatsSnapshot
├── frontend-exporter/    # Exporter trait, registration, dispatch
├── frontend-ipc/         # shared with infmon-cli (Spec 007)
└── frontend-bin/         # main(), config loading, signal wiring
src/exporters/
└── otlp/                 # the v1 exporter (Spec 006)
```

Adding an exporter is: new crate under `src/exporters/`, depend on
`frontend-exporter`, `inventory::submit!` a registration, declare the
crate as a dep of `frontend-bin`. No changes to the loop or the trait.

## 7. Backpressure & failure handling

The hard rule: **a slow or failing exporter never blocks the poller
and never blocks another exporter.**

Mechanism:

- The poller produces one `Arc<FlowStatsSnapshot>` per tick and pushes it
  into each exporter's bounded channel with capacity `queue_depth`
  (default 2). The channel is a small ring buffer
  (`VecDeque<Arc<FlowStatsSnapshot>>` behind a `Mutex` plus a `Notify`); we
  cannot use `tokio::sync::mpsc` directly because it lacks a
  "pop-oldest from the sender side" operation, which `drop_oldest`
  needs. The wrapper keeps the same `Sender` / `Receiver` ergonomics.
- Each `exporter-N` thread consumes from its channel, awaits
  `export(agg)` with `timeout_ms`, and increments per-exporter
  counters (§10).
- When the channel is full at push time, the poller applies the
  exporter's `on_overflow` policy:

| Policy            | Behavior                                                                 | When to use                          |
|-------------------|--------------------------------------------------------------------------|--------------------------------------|
| `drop_oldest`     | Pop oldest queued tick, push new one. Bumps `frontend_drops_total{reason="overflow_old"}`.  | **Default.** Freshness wins.         |
| `drop_newest`     | Discard the just-produced tick. Bumps `..reason="overflow_new"`.         | Exporters that prefer monotonic seq. |

A `block_one_tick` policy was considered and rejected for v1: any
poller-side blocking starves *all* exporters of the next tick, which
contradicts the hard rule above. If we ever need bounded backpressure,
it will be implemented as a per-exporter retry on the exporter thread,
never as a poller block.

The poller's `snapshot_and_clear` against the backend always runs,
regardless of exporter health. If every exporter is overflowing, the
backend is still being drained — counters just don't reach a sink.
This is the "drop with metric" answer to the issue's question:
**buffer is one tick deep per exporter; everything beyond drops with
a labelled counter, never a process-wide queue.**

### 7.1 Exporter results

Per tick, per exporter, the dispatcher records exactly one of:

- `ok` — `export` returned `Ok(())` within `timeout_ms`.
- `transient` — `Err(Transient)` or `Err(Timeout)`. Logged at
  `WARN` with rate-limit; bumps `frontend_export_failures_total`
  with `reason` label. Next tick is attempted normally.
- `permanent` — `Err(Permanent)`. Logged at `ERROR`; the exporter
  is **disabled** until next `reload` (its channel is drained and
  closed). Bumps `frontend_export_disabled_total`.

A permanent error never crashes the process. Operators see it as
a `permanent` status in `infmon-cli exporter list`.

## 8. IPC to the backend

The frontend talks to the backend over two channels:

### 8.1 Stats segment (read-only, hot path)

A `mmap`'d shared-memory file (path in `frontend.stats_segment`,
default `/dev/shm/infmon-stats`) whose layout is owned by Spec 004.
The frontend reads it on every tick to obtain flow data. The
client crate (`frontend-ipc`) exposes:

```rust
pub struct InFMonStatsClient { /* ... */ }

impl InFMonStatsClient {
    pub fn open(path: &Path) -> Result<Self, IpcError>;
    /// Equivalent to backend's snapshot_and_clear: returns all
    /// flow-rules' flows as of the call, and clears the backend's
    /// counters atomically (per Spec 004 §x).
    pub fn snapshot_and_clear(&self) -> Result<RawSnapshot, IpcError>;
}
```

Concurrency: the segment is single-reader by contract — only the
poller thread calls `snapshot_and_clear`. To keep this from being
just a gentleman's agreement (two `infmon-frontend` instances, or a
restart that overlaps an old draining process, would otherwise both
race on the destructive clear half and each see partial data), the
frontend enforces single-writer-of-the-clear-side at startup:

1. On `start`, the frontend acquires an advisory `flock(LOCK_EX |
   LOCK_NB)` on `frontend.stats_segment` (or a sibling
   `<stats_segment>.lock` file if the kernel rejects locks on the
   shm node). Failure → refuse to start with `stats_segment_busy`
   in the closed error set.
2. The lock is held for the lifetime of the process and released on
   exit (kernel does this automatically on FD close, including
   abnormal termination).
3. A PID file at `<control_socket>.pid` is also written for human
   debugging, but the lock is the authoritative single-reader gate.

Symptom of a violation (e.g. someone bypasses the lock with a custom
binary): non-monotonic `tick_id` in observed exports and
`frontend_backend_disconnects_total` jumps that don't correlate with
backend restarts.

The CLI's read path uses the *control API* (§8.2) so it does not
race the poller on the clear half of the operation.

### 8.2 Control API (request/response)

A Unix-domain socket at `frontend.backend_socket`, length-prefixed
protobuf (transport owned by Spec 004). The client crate exposes:

```rust
pub struct InFMonControlClient { /* ... */ }

impl InFMonControlClient {
    pub async fn connect(path: &Path) -> Result<Self, IpcError>;

    // Flow-rule CRUD — wraps the backend ops from Spec 002 §7.
    pub async fn flow_rule_add(&self, def: FlowRuleDef)  -> Result<(), CtlError>;
    pub async fn flow_rule_rm (&self, name: &str)        -> Result<(), CtlError>;
    pub async fn flow_rule_list(&self)                   -> Result<Vec<FlowRuleDef>, CtlError>;
    pub async fn flow_rule_show(&self, name: &str)       -> Result<FlowRuleStats, CtlError>;

    // Frontend-only ops (served by the frontend's own control thread,
    // not forwarded to the backend).
    pub async fn reload(&self)                     -> Result<(), CtlError>;
    pub async fn exporter_list(&self)              -> Result<Vec<ExporterStatus>, CtlError>;
}
```

`infmon-cli` (Spec 007) is a thin wrapper over `InFMonControlClient`;
keeping the client in `frontend-ipc` means the CLI and frontend
cannot drift on the wire format.

### 8.3 Reconnection

If the stats segment disappears (backend restart) or the control
socket EOFs, the frontend:

1. Skips the current tick's snapshot (bumps
   `frontend_backend_disconnects_total`).
2. Retries `InFMonStatsClient::open` with exponential backoff capped at 5 s.
3. Continues exporter ticks with empty aggregates so the per-frontend
   liveness signal (`frontend_tick_total`) keeps incrementing.

The frontend never exits because the backend is down. Operators
restart it explicitly via `systemctl restart infmon-frontend`.

## 9. Lifecycle

### 9.1 `start`

1. Parse `frontend.yaml`. Refuse to start on any validation error
   (closed error set, mirrors Spec 002 §7.2: `invalid_spec`,
   `name_exists`, …).
2. Open `InFMonStatsClient` and `InFMonControlClient` to the backend. Refuse to
   start if the backend is not reachable within `startup_timeout_ms`
   (default 5000). The asymmetry with §8.3 (where runtime
   disconnects are tolerated indefinitely) is intentional: at start
   we have no exporters spawned yet and no useful work to do, so
   failing fast surfaces a misconfigured `backend_socket` /
   `stats_segment` to the operator immediately. Once we are running
   and have produced at least one tick, the cost of exiting (losing
   queued aggregates, restart storms under systemd `Restart=on-failure`)
   outweighs the diagnostic value, so we keep retrying instead.
3. Push the parsed flow definitions to the backend via
   `flow_rule_add` / `flow_rule_rm` so the backend's flow-rule set matches
   config. (Diff-based: pre-existing flow-rules with identical defs
   are left alone.)
4. Build each exporter from its registered factory. Any factory
   error aborts startup.
5. Spawn `poller`, `control`, and one `exporter-N` thread per
   exporter.
6. Begin ticking.

### 9.2 `reload`

Triggered by `SIGHUP` or `InFMonControlClient::reload()`.

1. Re-read `frontend.yaml`. Validate exporter and flow blocks
   independently. Any error → reload aborted, old config still
   running, error returned to the caller.
2. Diff flow-rule definitions and apply via `flow_rule_add` / `flow_rule_rm`
   (Spec 002 §7 covers the data-plane atomicity guarantee).
3. For each existing exporter still present in the new config: call
   `Exporter::reload(&new_cfg)`. Any `Err` → reload aborted, all
   previously-applied changes rolled back via the inverse ops.

   If a rollback step itself fails (e.g. a `flow_rule_rm` that
   should undo a step-2 `flow_rule_add` is rejected because the backend
   moved on, or the control socket EOFs mid-rollback), the frontend
   logs at `ERROR` with `frontend_reload_inconsistent_total`,
   marks the system as **degraded** (visible via
   `InFMonControlClient::exporter_list` and a `frontend_reload_status`
   gauge with values `ok | degraded`), and **continues running on a
   best-effort merged state** rather than crashing. Operators
   recover by issuing a fresh `reload` once the underlying cause is
   fixed, or by `systemctl restart infmon-frontend` if the merged
   state is unworkable. This is a deliberate choice: a half-applied
   reload is preferable to a process death that takes telemetry
   offline entirely.
4. For each exporter removed from config: drain its channel, await
   `Exporter::shutdown()` (bounded by `shutdown_grace_ms`), join its
   thread.
5. For each newly-added exporter: build, spawn its thread, register
   in the dispatcher.

Reload is **all-or-nothing from the operator's perspective**. The
poller continues ticking against the in-flight exporter set
throughout; ticks during a reload land on whichever set is currently
installed at the moment of fan-out.

### 9.3 `stop`

Triggered by `SIGINT` / `SIGTERM`.

1. Stop the poller after the current tick completes (do not begin a
   new snapshot).
2. Close exporter channels so each `exporter-N` drains queued ticks.
3. Await `Exporter::shutdown()` for every exporter, bounded by
   `shutdown_grace_ms` (default 2000 ms). Exceeders are abandoned;
   the process exits regardless.
4. Close the control socket.
5. Always exit `0` on operator-initiated stop. A slow exporter that
   exceeded `shutdown_grace_ms` is a graceful degradation worth a
   `WARN` log and a `frontend_shutdown_grace_exceeded_total` bump,
   not a process failure. Reserve non-zero exits for actual startup
   failures (§9.1) or unrecoverable runtime panics. This keeps the
   default systemd unit (`SuccessExitStatus=0`,
   `Restart=on-failure`) doing the right thing without forcing
   operators to remember `SuccessExitStatus=0 1`.

The frontend never holds backend state, so shutdown does not need
to flush to disk.

## 10. Observability

The frontend exports its own metrics through the same OTLP exporter
it serves to the backend's data, under the `frontend_*`
namespace. **A separate, non-disableable fallback** is intentionally
kept lightweight in v1: regardless of OTLP exporter health, the
frontend always logs the same metric set as a one-line structured
JSON record per tick at `INFO` (rate-limited and gated by
`frontend.metrics_log = on|off|on_failure`, default `on_failure`).
This means:

- In the steady state, metrics flow through OTLP and the log path
  is silent.
- When every exporter is `permanent`-failed (the §7.1 / Q2 case),
  the structured-log path is the operator's last-resort visibility
  channel without requiring a new write/serve path.
- Self-referential measurement effects (`export_duration_ns` for
  tick *N* observing the cost of emitting tick *N − 1*'s frontend
  metrics) are inherent to in-band emission and acceptable: the
  delta is a small constant relative to the per-flow-rule export
  payload, and the histogram exposes it.

Metrics emitted (under `frontend_*`):

- `frontend_tick_total` — counter, ticks attempted.
- `frontend_tick_skew_ns` — histogram, `actual - scheduled`
  for each tick. The 1 Hz cadence health signal.
- `frontend_snapshot_duration_ns` — histogram.
- `frontend_aggregate_flows` — histogram, flows per tick (renamed from `aggregate_buckets` in v0.4; no backcompat alias).
- `frontend_export_duration_ns{exporter}` — histogram.
- `frontend_export_failures_total{exporter,reason}` — counter;
  `reason` ∈ `{"transient","timeout","permanent"}`.
- `frontend_drops_total{exporter,reason}` — counter; `reason`
  ∈ `{"overflow_old","overflow_new"}`.
- `frontend_export_disabled_total{exporter}` — counter, bumped
  once when an exporter goes `permanent`.
- `frontend_backend_disconnects_total` — counter.

`tick_total`, `tick_skew_ns`, and `drops_total` together let an
operator answer "is the frontend keeping up?" without reading logs.

## 11. Testing

| Layer            | Test type      | What it covers                                      |
|------------------|----------------|-----------------------------------------------------|
| `frontend-ipc`   | unit (cargo)   | Snapshot decode, control RPC round-trips against an in-process fake backend. |
| `frontend-core`  | unit (cargo)   | Tick scheduling skew under load; aggregate decode. |
| `frontend-exporter` | unit        | Backpressure policies; permanent-error disables exporter; reload rollback. |
| `otlp` exporter  | unit + integ   | Wire format vs. an embedded OTLP receiver (Spec 006). |
| frontend bin     | integration    | `start → reload → stop` against a stub backend that mocks the stats segment and control socket. Lives in `tests/` (Spec 000), not in CI. |

CI (Spec 001) runs `cargo test --workspace` for the unit tests; the
end-to-end test against a real backend stays out of CI per the
project-wide policy.

## 12. Future extension hooks (non-normative)

- **Sub-second tick.** Move `polling_interval_ms` below 1000 once we have evidence
  the backend's `snapshot_and_clear` and the OTLP collector can keep
  up. No trait change required.
- **Dynamic exporter loading.** Replace the `inventory` registration
  with a `dlopen`-style loader. Trait stays the same; only
  registration changes.
- **Aggregate fan-in across multiple backends.** Make `InFMonStatsClient`
  multi-instance and merge per-flow-rule flows in the poller. Out of
  scope for v1 (single-DPU).
- **Exporter chaining / filters.** A `transform` plugin kind that
  sits between poller and exporter (e.g. relabel, drop low-volume
  flow-rules). Trait shape suggests itself; deferred.
- **At-least-once semantics.** Add a tick-id ack from each exporter
  and re-deliver on transient failures. Requires bounded retain
  buffer; explicitly off in v1 to keep memory predictable.

## 13. Open questions

- **Q1.** ~~Should `block_one_tick` be removed entirely from v1, or
  kept as a debug-only knob?~~ **Resolved (v0.3):** removed from v1
  per §7 — any poller-side block contradicts the hard rule.
- **Q2.** ~~Where does the frontend's own metrics emission sit when
  *every* exporter is permanent-failed?~~ **Resolved (v0.3):**
  always-on structured-log fallback per §10
  (`frontend.metrics_log = on_failure`).
- **Q3.** Do we need a `dry-run` mode that runs the poller and
  decodes aggregates but skips fan-out, for backend benchmarking?
  Cheap to add; defer to Spec 007 review.
