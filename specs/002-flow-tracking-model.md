# Spec 002 — Flow tracking model

## Version history

| Version | Date       | Author      | Changes        |
| ------- | ---------- | ----------- | -------------- |
| 0.1     | 2026-04-18 | bf3 (agent) | Initial draft. |
| 0.2     | 2026-04-18 | bf3 (agent) | Rename tracker->flow-rule, bucket->flow. |
| 0.3     | 2026-04-18 | bf3 (agent) | Add `mirror_src_ip` to v1 field set as the only outer-header field allowed in a flow-rule key. |

Tracking issue: DPU-8 (project InFMon)
Parent epic: DPU-4 (EPIC: InFMon — flow telemetry service on BF-3)
Depends on: 000-overview (system overview), 003-erspan-and-packet-parsing
           (defines the parsed inner-packet record this spec keys off of)
Related: 004-backend-architecture (consumes this model in the VPP plugin),
         007-cli (exposes the CRUD surface as `flow {add,rm,list,show}`)

## 1. Purpose

Define the data model and lifecycle of a **flow flow-rule** in InFMon: the
named, configurable thing that turns parsed inner-packet records into
counter flows. The flow flow-rule is the single seam between
*"what the wire just gave us"* (Spec 003) and
*"what we export"* (Spec 006, OTLP).

A v1 deployment will typically run with a handful of flow-rules (≤ 16) and
millions of keys per flow-rule; this spec sets the contract that lets us
keep that bounded and predictable.

## 2. Concepts

### 2.1 Flow-rule

A **flow-rule** is:

```
flow-rule := (name, ordered_field_list, max_keys, eviction_policy)
```

- **name** — short identifier, `^[a-z0-9][a-z0-9_-]{0,30}$`. Used in
  exported metric labels and CLI references. Unique per backend instance.
- **ordered_field_list** — non-empty, ordered tuple of fields drawn from
  the v1 field set (§3). Order is significant: it defines the byte
  layout of the key (§4) and therefore the hash. Reordering produces a
  different flow-rule even with the same field set.
- **max_keys** — non-negative integer; upper bound on the number of
  distinct keys held simultaneously for this flow-rule. Required.
- **eviction_policy** — what to do when a new key arrives at a full
  flow-rule. v1 supports a single policy (§6); the field exists so future
  policies can be added without a config break.

A flow-rule is *static* in v1: its field list and max_keys are fixed at
configuration time. CRUD operations replace the whole flow-rule; partial
mutation (e.g. add a field) is explicitly out of scope to keep the key
layout immutable for the lifetime of any one flow-rule.

### 2.2 Key

A **key** is the concrete value emitted by a packet for a given flow-rule:
the tuple of field values, in the flow-rule's declared order. Two packets
produce the same key iff every listed field is bytewise equal after the
canonicalisation rules in §4.

### 2.3 Flow

A **flow** is the per-key counter state owned by the flow-rule. v1
flows carry the minimum needed by the OTLP exporter (Spec 006):
`packets`, `bytes`, `first_seen_ns`, `last_seen_ns`. The exact flow
layout is owned by Spec 004 (backend); this spec only requires that
*there is one flow per (flow-rule, key)*.

## 3. Field set v1 (L3, inner + mirror metadata)

All v1 fields are extracted from the **inner** packet headers — i.e.
after ERSPAN decapsulation per Spec 003 — with one named exception
(`mirror_src_ip`) covered below.

| Field            | Type / width            | Source                       | Notes                                                   |
|------------------|-------------------------|------------------------------|---------------------------------------------------------|
| `src_ip`         | 16 B (v4 mapped to v6)  | inner IPv4 / IPv6 SA         | IPv4 stored as `::ffff:a.b.c.d` for layout uniformity   |
| `dst_ip`         | 16 B (v4 mapped to v6)  | inner IPv4 / IPv6 DA         | same mapping rule                                       |
| `ip_proto`       | 1 B                     | inner IPv4.protocol / IPv6.next_header (after extension headers) | 0–255         |
| `dscp`           | 1 B (low 6 bits used)   | inner IPv4.tos>>2 / IPv6.tc>>2 | upper 2 bits zero                                     |
| `mirror_src_ip`  | 16 B (v4 mapped to v6)  | **outer** GRE/ERSPAN source IP, surfaced by Spec 003 as `mirror_src_ip` | The **only** outer-header field allowed in a flow-rule key. Identifies the mirroring device. Same v4-mapped-v6 layout rule. Opt-in: include it in a flow-rule's field list to break flows out per source device. |

A flow-rule MUST list at least one field. There is no implicit field; if
you want a flow-rule keyed only on `dscp`, configure it that way.

`mirror_src_ip` is the documented exception to the "inner only" rule: it
travels with the parser record (Spec 003 §4.5) so flow-rules can attribute
flows to the device that mirrored them. All other outer fields remain
unavailable.

### 3.1 Why IPv4-mapped-IPv6

So a single flow-rule can carry both address families with one fixed-width
key. A v4 packet and a v6 packet that happen to encode the same address
*will* collide; this is by design — operators who want them separated
should add `ip_proto` (or, post-v1, an explicit `ip_version` field).

### 3.2 Parser handoff

The packet parser (Spec 003) is responsible for delivering a normalised
record with each v1 field already extracted, validated, and in
host-endian where applicable. Flow-rules do **not** re-parse packets.
Malformed / truncated packets are dropped upstream and never reach the
flow-rule (they bump a parser counter, not a flow-rule counter).

## 4. Key layout & canonicalisation

For a flow-rule `T = [f1, f2, ..., fn]` the key is the concatenation of
each field's canonical encoding, in order, with no padding between
fields:

```
key(T, pkt) = enc(f1, pkt) || enc(f2, pkt) || ... || enc(fn, pkt)
```

Canonical encodings (v1):

- `src_ip`, `dst_ip`: 16 bytes, network byte order, IPv4 mapped as in §3.
- `ip_proto`: 1 byte.
- `dscp`: 1 byte, value in `0..=63`, upper bits zeroed.

Total key width is fixed per flow-rule and computable from the field list
alone. Implementations SHOULD reject configurations whose key width
exceeds 64 bytes (room for L4 fields in v2 without revisiting this cap).

The hash function used to index the flow store is owned by Spec 004;
this spec only fixes the *input* (the bytes above, in this order).

## 5. Configuration schema

Flow-rules are configured via a static file loaded at backend start.
TOML is the canonical format; YAML is accepted but converted to the same
internal representation. The CLI (Spec 007) is a thin wrapper that
mutates this same schema and asks the backend to reload.

### 5.1 TOML

```toml
# /etc/infmon/flows.toml

[[flow-rule]]
name             = "by_5tuple_l3"
fields           = ["src_ip", "dst_ip", "ip_proto", "dscp"]
max_keys         = 1_048_576           # 2^20
eviction_policy  = "lru_drop"          # only value supported in v1

[[flow-rule]]
name             = "by_dscp"
fields           = ["dscp"]
max_keys         = 64
eviction_policy  = "lru_drop"
```

### 5.2 YAML (equivalent)

```yaml
flow-rules:
  - name: by_5tuple_l3
    fields: [src_ip, dst_ip, ip_proto, dscp]
    max_keys: 1048576
    eviction_policy: lru_drop
  - name: by_dscp
    fields: [dscp]
    max_keys: 64
    eviction_policy: lru_drop
```

### 5.3 Validation rules

The backend MUST reject the configuration (and refuse to start, or
refuse the reload) if any of the following hold:

1. Two flow-rules share a name.
2. A flow-rule's `fields` list is empty, contains an unknown field, or
   contains duplicates.
3. `max_keys` is missing, negative, or zero.
4. `eviction_policy` is not in the supported set (v1: `{"lru_drop"}`).
5. The computed key width exceeds 64 bytes.
6. The total of all `max_keys` exceeds the backend's compile-time
   budget (set by Spec 004; default 16 Mi keys across all flow-rules).

Validation is **all-or-nothing** per reload. A failed reload leaves the
previously running flow-rule set untouched.

## 6. Eviction policy (v1)

v1 ships exactly one policy: **`lru_drop`**.

Behaviour, when a packet would create a new key in a flow-rule that is
already at `max_keys`:

1. Evict the least-recently-updated key from the flow-rule. Its flow
   contents are **dropped**, not flushed — Spec 006 (OTLP) flushes
   continuously, so the loss is bounded by the export interval.
2. Insert the new key with a fresh flow and apply the current packet.
3. Increment the per-flow-rule counter `infmon_tracker_evictions_total`
   (label: `flow-rule=<name>`).

If the eviction itself fails (e.g. data structure invariant violation),
the packet is dropped and `infmon_tracker_drops_total` is incremented
instead — the flow-rule never silently corrupts.

"Recently used" means *most recently updated by an incoming packet*.
Recency state lives with the flow and is maintained on every hit; the
exact data structure is owned by Spec 004 (a likely choice is a
segmented LRU keyed off the existing hash table, but that's an
implementation detail).

### 6.1 Why drop, not flush

A flush-on-evict design would couple the data plane to the exporter and
make eviction cost unbounded under load. Drop-with-counter keeps the
fast path predictable and surfaces the loss as a first-class metric so
operators can size `max_keys` against observed eviction rate.

## 7. CRUD API surface

The backend exposes a small management API consumed by `infmon-cli`
(Spec 007) and by the config loader. Transport is owned by Spec 004
(likely a Unix socket carrying length-prefixed protobuf or JSON); this
spec defines only the operations and their semantics.

| Op       | CLI                              | Input                                     | Output                              | Notes |
|----------|----------------------------------|-------------------------------------------|-------------------------------------|-------|
| `add`    | `infmon-cli flow add <spec>`     | full flow-rule definition (name, fields, max_keys, eviction_policy) | created flow-rule, or error           | Fails if name exists. |
| `rm`     | `infmon-cli flow rm <name>`      | flow-rule name                              | ok / `not_found`                    | Drops all flows for that flow-rule. |
| `list`   | `infmon-cli flow list`           | —                                         | array of flow-rule definitions        | Cheap; no flow data. |
| `show`   | `infmon-cli flow show <name>`    | flow-rule name                              | flow-rule definition + live stats: `bucket_count`, `evictions_total`, `drops_total`, `last_packet_ns` | Stats are best-effort snapshots. |

### 7.1 Semantics

- All operations are **synchronous** from the CLI's perspective: the
  call returns only after the backend has applied (or rejected) the
  change.
- `add` and `rm` are atomic with respect to the data plane: an
  in-flight packet either sees the old flow-rule set or the new one,
  never a partial state. Implementation note: Spec 004 will likely
  use RCU-style swap of the flow-rule table.
- `add` does **not** support replace-by-name. To change a flow-rule,
  `rm` then `add`. This preserves the rule from §2.1 that a flow-rule's
  field list is immutable for its lifetime, and forces operators to
  acknowledge that flows will be dropped.
- `list` and `show` are read-only and lock-free on the data plane.
- The static config file (§5) is applied via the same `add`/`rm`
  primitives at startup; there is no separate "bulk load" path.

### 7.2 Errors

A small, closed set:

- `name_exists` — `add` with a duplicate name.
- `not_found` — `rm` / `show` against an unknown name.
- `invalid_spec` — any §5.3 validation failure; carries a human-readable
  reason.
- `budget_exceeded` — would exceed the backend's total `max_keys`
  budget.
- `internal` — anything else; carries a correlation id for log lookup.

## 8. Observability

Per flow-rule, the backend exports (mechanism: Spec 006):

- `infmon_tracker_buckets{flow-rule}` — gauge, current key count.
- `infmon_tracker_evictions_total{flow-rule}` — counter (§6).
- `infmon_tracker_drops_total{flow-rule, reason}` — counter; `reason` ∈
  `{"eviction_failed", "budget_exceeded_runtime"}`.
- `infmon_tracker_packets_total{flow-rule}` — counter, packets accounted
  into this flow-rule (i.e. that successfully landed in a flow).
- `infmon_tracker_bytes_total{flow-rule}` — counter.

The first two are the load signal: a non-zero, growing
`evictions_total` means `max_keys` is under-provisioned for the
observed cardinality.

## 9. Future extension hooks (non-normative)

These are deliberately **not** in v1 but the v1 design must not
foreclose them.

### 9.1 L4 fields

`src_port`, `dst_port`, `tcp_flags`. These plug into the field-set
table (§3) with no schema change. The 64-byte key-width cap (§4) leaves
room: the v1 maximum 5-tuple-ish key is 16+16+1+1 = 34 bytes.

### 9.2 RoCEv2-specific fields

A RoCEv2 record exposes `bth.dest_qp` (24 bits), `bth.opcode` (8 bits),
and optionally `aeth.syndrome`. These will live behind a feature flag
on the parser (Spec 003) and appear as additional field names —
e.g. `roce_dest_qp`, `roce_opcode` — selectable per flow-rule. The
flow-rule model itself does not need to know they are RoCE-specific; it
just sees more entries in the field-set table.

### 9.3 Additional eviction policies

The `eviction_policy` field is a string precisely so we can add
policies (e.g. `"random_drop"`, `"reservoir"`, `"reject_new"`) without
a config break. Each new policy will land with its own sub-spec
covering correctness and counter semantics.

### 9.4 Per-flow-rule sampling

A `sample_rate` field on the flow-rule (1-in-N or token-flow) is the
natural next knob once cardinality control via `max_keys` proves
insufficient. Out of scope for v1 — call it out here so reviewers don't
try to bolt it onto §6.

### 9.5 Dynamic field reordering / field add

Explicitly **not** planned. The cost (rebuilding all flows, breaking
the immutability promise §2.1 makes to the exporter's label set)
outweighs the benefit. The supported migration is `rm` + `add`.

## 10. Open questions

- **Q1.** Should `show` include a top-N sample of live keys for
  debugging, or is that the job of a separate `flow dump` command? —
  Defer to Spec 007 (CLI) review.
- **Q2.** Do we need a "drain" semantic on `rm` (flush flows to OTLP
  before destroying)? Cheap to add later; v1 says no, to keep the
  backend simple.
- **Q3.** Hash collision behaviour — Spec 004 territory, mentioned
  here so the cross-link exists.
