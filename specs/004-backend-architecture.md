# Spec 004 — Backend Architecture (VPP plugin)

| Field    | Value                                                         |
| -------- | ------------------------------------------------------------- |
| Status   | Draft                                                         |
| Owner    | bf3 (agent)                                                   |
| Reviewer | @banidoru                                                     |
| Tracking | DPU-10 (parent: DPU-4 — EPIC InFMon)                          |
| Repo     | https://github.com/r12f/InFMon                                |
| Depends  | spec 000 (overview), spec 002 (flow tracking model), spec 003 (ERSPAN parsing) |

## 1. Motivation

`infmon-backend` is the data-plane half of InFMon. It runs as a VPP plugin on
the BlueField-3 ARM cores, consumes ERSPAN III mirrored packets, and turns
them into per-flow counters that the Rust frontend reads, aggregates, and
exports.

Because it is the only component that touches every packet, it dominates the
entire system's performance envelope. Every choice in this spec — the graph
node layout, the counter representation, the control surface, the
snapshot semantics, the threading model — is driven by one rule:

> **The packet-processing path must never block, never allocate, and never
> wait on the control plane.**

This spec defines the backend's internal architecture so that:

- spec 005 (frontend) can rely on a stable shared-memory + control contract,
- spec 006 (OTLP exporter) can reason about counter freshness and reset
  semantics,
- spec 007 (CLI) has a documented control surface to drive,
- and reviewers can evaluate the design before any C/C++ code is written.

## 2. Scope

In-scope:

- The VPP graph node layout the plugin installs.
- The in-memory counter table layout, atomicity guarantees, and lookup model.
- How counters are exposed to userspace consumers (the frontend).
- The control surface used to manage flow definitions and trigger snapshots.
- The semantics of the `snapshot_and_clear` operation that backs every export
  cycle.
- The threading model and per-core throughput targets the implementation
  must hit.

Out of scope (deferred to later specs):

- Wire format of OTLP / IPFIX exports — spec 006.
- Frontend aggregation logic, REST surface, auth — spec 005.
- CLI UX — spec 007.
- Persistence of flow definitions across reboots — v2.
- Sampling policies (every packet is counted in v1).
- Any inline action on production traffic; InFMon is observe-only by
  construction.

## 3. Terminology

| Term              | Meaning                                                          |
| ----------------- | ---------------------------------------------------------------- |
| **flow_def**      | Operator-supplied definition of *what* counts as a flow — the key fields and an optional pre-filter. See spec 002. |
| **key**           | Concrete tuple value derived from one packet under a `flow_def`. |
| **counter row**   | The pair `(packets, bytes)` (each 64-bit) for one `(flow_def, key)`. |
| **counter table** | Per-`flow_def` hash table mapping `key → counter row`.           |
| **snapshot**      | Atomic capture of every counter row in every table at one instant. |
| **stats segment** | VPP's existing shared-memory region that the frontend mmaps read-only. |
| **batch**         | A vector of packet buffer indices VPP delivers to a graph node in one call (≤ `VLIB_FRAME_SIZE`, currently 256). |

## 4. VPP graph node layout

The plugin registers four nodes wired in a single linear path. All four run
in the worker thread that owns the input device's RX queue; no inter-thread
hand-off occurs on the data path.

```
  dpdk-input  ──►  infmon-erspan-decap  ──►  infmon-flow-match  ──►  infmon-counter  ──►  drop
                       │  (no-decap)              │  (no-match)              │  (counted)
                       └──►  drop                 └──►  drop                 └──►  drop
```

Per-node responsibilities:

- **`infmon-erspan-decap`** — Validates outer L2/L3, recognises GRE proto
  `0x88BE` (ERSPAN II) / `0x22EB` (ERSPAN III), strips the outer headers
  according to spec 003, and rewrites `vlib_buffer_t.current_data` /
  `current_length` so the inner Ethernet frame is at the head of the buffer.
  Non-ERSPAN packets and malformed encapsulations go to `drop` with a
  per-reason error counter (so the frontend can surface ingress health).

- **`infmon-flow-match`** — For each active `flow_def`, parses just the
  fields required by that definition's key + filter expression, evaluates
  the filter, and emits one `(flow_def_id, key_hash, key_blob)` triple per
  matching `(packet, flow_def)` pair into a stack-allocated scratch vector.
  A packet that matches no `flow_def` exits via `drop` with no work done.
  See spec 002 for the key/filter language.

- **`infmon-counter`** — Walks the scratch vector and, for each entry, issues
  one update against the corresponding counter table (§5). This is the only
  node that mutates shared state.

- **`drop`** — VPP's built-in. We do not free buffers ourselves.

Vector handoff is avoided: the entire pipeline executes on the RX worker
thread for the queue. This keeps cache lines hot and removes the only
place where the data path could plausibly block.

## 5. Counter table

### 5.1 Layout

For each `flow_def` the plugin owns one **counter table**:

- A bounded-size Robin-Hood hash table sized at plugin init from a CLI
  argument (`max_keys_per_flow_def`, default `2^20 = 1,048,576`).
- Slot layout (cache-line aligned, 64 B):

```
struct infmon_slot {
    u64  key_hash;          // 0  full 64-bit hash of key_blob
    u64  packets;           // 8  atomic, monotonic
    u64  bytes;             // 16 atomic, monotonic
    u32  key_offset;        // 24 offset into per-table key arena
    u16  key_len;           // 28
    u16  flags;             // 30  occupied / tombstone / overflow
    u8   _pad[32];          // 32  pad to 64 B
};
```

- Key blobs live in a separate **key arena** (a flat `u8[]` with a bump
  allocator) so that the slot itself stays one cache line and lookups touch
  exactly one cache line in the common case.

### 5.2 Atomicity

- `packets` and `bytes` are updated with `__atomic_fetch_add(..., RELAXED)`
  on the data path. Relaxed is sufficient because the frontend never
  correlates the two fields with anything outside the same row, and the
  snapshot mechanism (§7) provides the only consistency boundary it needs.
- Slot occupancy transitions (`free → occupied`, `occupied → tombstone`)
  are gated by a per-bucket-group seqlock so that the snapshot reader can
  detect a torn read of `(key_hash, key_offset, key_len)` and retry. A
  bucket group is 8 contiguous slots (one cache line of metadata per
  group).
- Insertions on the data path use compare-and-swap on `flags`. On
  contention we retry up to `INFMON_INSERT_RETRY` (default 4) times; on
  exhaustion we increment a per-table `insert_failed` counter and drop
  the contribution from this packet (the packet itself still goes to
  `drop`, like every other packet — InFMon never forwards).
- Table full → contribution dropped, `table_full` counter incremented.
  We deliberately do not evict; eviction policy is a v2 conversation.

### 5.3 Width

64-bit counters are mandatory. At 100 Gbps line-rate of 64-byte frames
(~148.8 Mpps) and even with all packets hitting one row, a 32-bit packet
counter would wrap in ≈29 s, which is shorter than any realistic export
interval. 64-bit gives ~3.9k years of headroom on the same workload.

## 6. Stats-segment exposure

The frontend MUST NOT call into the plugin on the hot path to read
counters. Instead, the plugin publishes its tables through VPP's existing
**stats segment** (the same shared-memory region that powers `vpp_get_stats`).
This gives us:

- A read-only mmap from the frontend (no syscalls per read once mapped).
- A directory mechanism (`/stat_dir`) we register table descriptors under,
  so the frontend discovers tables by enumeration instead of by hard-coded
  paths.

Per-table descriptor layout (registered under
`/infmon/<flow_def_id>/<generation>`):

| Field                  | Type     | Notes                                       |
| ---------------------- | -------- | ------------------------------------------- |
| `flow_def_id`          | `u128`   | UUID of the flow_def.                       |
| `generation`           | `u64`    | Bumped on every snapshot_and_clear (§7).    |
| `epoch_ns`             | `u64`    | Wall-clock at table creation.               |
| `slots_ptr`            | `void*`  | Pointer into stats segment to slot array.   |
| `slots_len`            | `u32`    | Number of slots.                            |
| `key_arena_ptr`        | `void*`  | Pointer into stats segment to key arena.    |
| `key_arena_len`        | `u32`    | Bytes in arena.                             |
| `insert_failed`        | `u64`    | Cumulative.                                 |
| `table_full`           | `u64`    | Cumulative.                                 |

Frontends iterate the directory, follow the latest generation pointer per
`flow_def_id`, and walk slots using the seqlock protocol from §5.2.

## 7. Control surface

The control surface is **not** on the data path. It is exposed as a VPP
**binary API plugin** (the same mechanism `vpp_api_test` and Go bindings
already speak), with a Unix-socket transport. We deliberately reuse VPP's
binary API rather than invent a side-channel because:

- The frontend already needs to talk to VPP for interface state; one
  transport is simpler than two.
- The binary API is request/response with built-in serialisation, ACL,
  and back-pressure — none of which we want to reinvent.
- VPP already has Rust bindings (`vpp-api-client`) that the frontend
  (spec 005) can consume.

### 7.1 Messages (v1)

| Message                          | Direction        | Purpose                                        |
| -------------------------------- | ---------------- | ---------------------------------------------- |
| `infmon_flow_def_add`            | client → plugin  | Register a new `flow_def`. Returns `flow_def_id`. |
| `infmon_flow_def_del`            | client → plugin  | Tear down a `flow_def` and free its tables.    |
| `infmon_flow_def_list`           | client → plugin  | Enumerate active flow_defs.                    |
| `infmon_flow_def_get`            | client → plugin  | Return the full definition for one id.         |
| `infmon_snapshot_and_clear`      | client → plugin  | Atomic table swap (§7.2). Returns the descriptor of the *retired* table. |
| `infmon_status`                  | client → plugin  | Per-worker counters (packets seen, drops by reason, table fullness). |

The wire schema (`*.api` file) lives in
`infmon-backend/api/infmon.api` and is consumed by `vppapigen` to produce
both C headers and Rust bindings during build.

### 7.2 `snapshot_and_clear` semantics

This is the export primitive. Frontends call it once per export interval
(spec 005 sets the cadence; the backend is indifferent).

**Contract:**

1. Caller invokes `infmon_snapshot_and_clear(flow_def_id)`.
2. Plugin allocates a new, empty counter table (`generation = G+1`) for
   this `flow_def_id`. Allocation happens off the worker thread; the
   table is zeroed by a control thread before installation.
3. Plugin atomically swaps the table pointer published in the
   `infmon-counter` node's per-flow_def context. The swap is a single
   `__atomic_store_n(..., RELEASE)` on a pointer — no worker thread
   stalls, no lock is taken, no packet is dropped.
4. From the next packet onward, the worker counts into the new table
   (`G+1`).
5. The retired table (`G`) remains live in the stats segment under its
   old directory entry. The reply to `snapshot_and_clear` contains the
   descriptor of `G`; the caller may walk it at leisure.
6. The retired table is freed by a control-thread RCU-style grace period:
   we wait until every worker has executed at least one node dispatch
   (using VPP's existing `vlib_worker_thread_barrier_*` machinery, **not**
   on the data path) plus an additional grace window of
   `INFMON_RETIRE_GRACE_NS` (default 5 s) to give in-flight readers time
   to finish, then unregister the directory entry and free the slot
   array + key arena.

The crucial properties:

- **No reset bit, no zeroing of live counters.** The new table starts at
  zero because it is freshly allocated, not because anyone wrote zeros to
  shared memory. This eliminates the classic "counter went backwards"
  race seen in IPFIX implementations.
- **The backend keeps counting on the new table from the very next
  packet.** No export interval ever loses a packet to a snapshot.
- **Each snapshot is internally consistent.** Because the swap is atomic
  and the retired table is immutable from that instant, the frontend can
  walk it without seqlocks (the §5.2 seqlock retry exists only for the
  *live* table).

## 8. Threading model

- The plugin runs entirely inside VPP's existing worker threads. We do
  not spawn additional pthreads on the data path.
- One **control thread** (VPP's main thread) owns:
  - binary-API request handling,
  - new-table allocation and zeroing,
  - retired-table free after grace period,
  - stats-directory registration / unregistration.
- Each worker thread processes its own RX queues end-to-end (decap →
  match → count). All counter updates are atomic, so multiple workers
  may legally hit the same row concurrently; the relaxed atomics ensure
  this stays cheap.
- `flow_def` definitions are read-mostly. They are stored in an
  RCU-protected vector indexed by `flow_def_id`. Workers acquire a
  pointer to the current vector once per frame (not per packet) and use
  it for the whole batch. Definition updates publish a new vector and
  retire the old one through the same grace-period machinery as table
  retirement.
- We require RX-queue → worker pinning (set via VPP's standard
  `dpdk { dev … { workers <list> } }` config). Without pinning the
  cache-line story in §5 collapses; the plugin will refuse to start if
  any input interface lands on the main thread.

## 9. Batch sizes

- The data-plane nodes process whole VPP frames (`VLIB_FRAME_SIZE`,
  currently 256). All three plugin nodes are written as **dual-loop**
  nodes (process two packets per inner iteration with software prefetch
  of `+2` and `+3`), which is the standard VPP pattern.
- The match-emit scratch vector is sized
  `VLIB_FRAME_SIZE × max_active_flow_defs` and lives in per-thread TLS;
  no allocation occurs per frame.
- Counter updates are issued one row at a time. We do not batch CAS
  attempts across packets — the contention rate at expected workloads
  (millions of distinct flows, sparse hot keys) does not justify the
  complexity, and measurement (§10) takes precedence over micro-opt
  guesses.

## 10. Performance targets

These are the numbers the implementation must hit on a single
BlueField-3 ARM core (Cortex-A78AE @ 2.75 GHz, 64 B cache lines, DPDK
25.11, VPP 24.10) with one RX queue, one `flow_def`, and a key set
small enough to fit in L2 (~1024 keys):

| Workload                                  | Target               | Stretch              |
| ----------------------------------------- | -------------------- | -------------------- |
| 64 B ERSPAN-encapsulated frames, all hit  | **≥ 12 Mpps / core** | ≥ 18 Mpps / core     |
| 1500 B frames, all hit                    | **line rate (8.2 Mpps @ 100 Gb)** | line rate |
| Distinct keys = 256 k (working set spills L2) | **≥ 6 Mpps / core**  | ≥ 9 Mpps / core      |
| `snapshot_and_clear` end-to-end latency   | **≤ 5 ms**           | ≤ 1 ms               |
| Worker thread CPU stall during snapshot   | **0 cycles**         | 0 cycles             |

Targets are validated by the offline benchmark harness defined in spec
008 (TBD); they are not enforced by CI because the CI runners are x86 and
do not have BF-3 hardware. Every PR that touches the data path MUST
report the harness output in the PR description.

## 11. Failure modes & observability

The plugin exposes the following error counters via the standard VPP
`show errors` mechanism (and, transitively, via `infmon_status`):

| Counter                       | Meaning                                              |
| ----------------------------- | ---------------------------------------------------- |
| `erspan_unknown_proto`        | Outer header parsed but ERSPAN type unrecognised.    |
| `erspan_truncated`            | Buffer too short for declared ERSPAN header.         |
| `inner_parse_failed`          | Inner L2/L3/L4 parse error after decap.              |
| `flow_def_no_match`           | Packet matched zero flow_defs (informational).       |
| `counter_insert_retry_exhausted` | CAS retries exceeded `INFMON_INSERT_RETRY`.        |
| `counter_table_full`          | Table reached `max_keys_per_flow_def`.               |
| `snapshot_alloc_failed`       | Could not allocate replacement table (OOM in stats segment). |

These are per-worker, summed by VPP's existing stats infrastructure;
the frontend reports them as gauges so operators can wire alerts.

## 12. Open questions

1. **Hash function.** Default plan is `xxh3_64` over the key blob.
   Alternative: VPP's built-in `clib_xxhash`. Decide during impl PR with
   a microbench in the harness.
2. **Stats segment sizing.** Default VPP stats segment is 32 MiB. With
   `1M slots × 64 B = 64 MiB` per table, we exceed it instantly. The
   plugin will require operators to bump `statseg { size }` and will
   refuse to start otherwise; whether to ship a recommended value as a
   tunable in spec 005 is open.
3. **Per-CPU sharded tables.** A sharded design (one sub-table per
   worker, merged at snapshot) avoids cross-core CAS entirely but
   complicates §7.2. Defer to v2 unless the §10 targets miss.

## 13. Acceptance

This spec is accepted (per the project's spec-first process — see spec
000) when it is merged to `main` with @banidoru's sign-off. After
acceptance, the implementation PR for `infmon-backend` may begin and
must conform to §4–§9 or amend this spec first.
