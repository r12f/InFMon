# Spec 006 — OTLP exporter (v1)

## Version history

| Version | Date       | Author       | Changes |
| ------- | ---------- | ------------ | ------- |
| 0.1     | 2026-04-18 | Riff (r12f)  | Initial draft of the v1 OpenTelemetry (OTLP) exporter. Defines OTLP metric mapping for flow-rule and flow state from Spec 002, runtime cap, jitter, naming, drop-reason enum, dynamic `host.arch`, §1.1 mental-model paragraph, and the `flow.mirror_src_ip` per-flow attribute (opt-in, sourced from Spec 003 §4.2.1). Identifier `distinct_data_points_per_flow`. |

- **Parent epic:** `DPU-4` (EPIC: InFMon — flow telemetry service on BF-3)
- **Depends on:** [`000-overview`](000-overview.md), [`002-flow-tracking-model`](002-flow-tracking-model.md)
- **Related:** [`004-backend-architecture`](004-backend-architecture.md),
  [`005-frontend-architecture`](005-frontend-architecture.md),
  [`007-cli`](007-cli.md)

## 1. Purpose

Define the **v1 OpenTelemetry (OTLP) exporter** for InFMon: how flow-rule
state held by the backend (Spec 002) is turned into OTLP metrics and shipped
off-DPU to a collector.

The OTLP exporter is the **only** standard-format exporter in v1. It is the
sole production sink for flow data; everything else (CLI `flow show`, ad-hoc
JSON dumps) is for debugging. Getting this contract right means downstream
collectors, dashboards, and alerting can be built without waiting for InFMon
to stabilise further.

### 1.1 Mental model

A **flow-rule** is a configured matcher (key field set + limits) declared by
the operator. At runtime each flow-rule generates one **flow** per distinct
key tuple it observes, and that flow owns the packet/byte counters. The
exporter walks every live flow under every flow-rule each tick (§3.2) and
emits per-flow OTLP data points carrying the flow's key as attributes plus
per-flow-rule observability points (§3.4) scoped to the flow-rule itself.

## 2. Concepts

### 2.1 Exporter instance

A single exporter instance is configured per `infmon-frontend` process.
Multiple destinations are not supported in v1 — operators who need fan-out
should run an OpenTelemetry Collector adjacent to the DPU.

### 2.2 Source of truth

The exporter never touches the data plane. It consumes the **snapshot**
surface owned by the backend (Spec 004) on a fixed cadence and translates
each flow-rule's live flows into OTLP data points.

### 2.3 Wire shape

OTLP / metrics. v1 ships the gRPC transport by default and accepts
http/protobuf as a configuration switch. JSON-over-HTTP is **not** supported
(noisier on the wire, no observed collector that requires it).

## 3. Signal type and metric model

### 3.1 Signal: metrics

InFMon exports **metrics** only in v1. Logs and traces are out of scope —
flow data is intrinsically aggregate counter state, and shoehorning it
through `Logs` would lose the collector's native aggregation pipeline.

### 3.2 Per-flow data points

Each live flow in each flow-rule becomes a fixed set of OTLP data points,
all sharing the flow's attribute set (§4):

| OTLP metric name             | Instrument | Unit  | Source field      | Notes                                     |
|------------------------------|------------|-------|-------------------|-------------------------------------------|
| `infmon.flow.packets`        | Sum (cumulative, monotonic) | `{packets}` | `flow.packets` | Total packets attributed to this flow. |
| `infmon.flow.bytes`          | Sum (cumulative, monotonic) | `By`        | `flow.bytes`   | Total bytes (L3 length per Spec 003).    |
| `infmon.flow.last_seen`      | Gauge      | `ns`  | `flow.last_seen_ns` | Wall-clock ns of the most recent packet that updated this flow. See note below on float64 precision. |

> **Precision note for `last_seen`.** The unit is wall-clock nanoseconds since
> the Unix epoch (~19-digit integer). Backends that store gauge values as
> `float64` (Prometheus, many Mimir/Cortex setups) will lose sub-microsecond
> precision (float64 has 53 bits of mantissa, < 16 decimal digits). This is
> acceptable for `last_seen` — operators use it for staleness/age, not for
> sub-microsecond ordering. Backends that need finer resolution can read the
> source `start_time_unix_nano` / `time_unix_nano` fields directly from the
> OTLP record. The unit stays `ns` to remain consistent with the OTel
> timestamp conventions used elsewhere in the record.

`first_seen_ns` is **not** exported as its own data point. It is folded into
the data-point `start_time_unix_nano` of the two `Sum` metrics, which is the
OTLP-native way to express "this counter started accumulating at time T".

### 3.3 Aggregation temporality

`Cumulative` for both `Sum` metrics. Rationale:

- Cumulative is the OTLP default and what most collector pipelines (Prometheus
  remote-write, Mimir, Cortex) prefer.
- The backend owns the flow lifetime; the exporter is stateless. Cumulative
  - a stable `start_time_unix_nano` lets a collector compute deltas without
  the exporter having to track previous-export state.
- On flow eviction (Spec 002 §6) the counter is gone. Receivers that
  compute deltas will see a counter reset; OTLP Sum semantics handle that
  correctly when `start_time_unix_nano` advances.
- If the **same flow key reappears** after eviction, the backend allocates
  a brand-new flow with a fresh `first_seen_ns`. The exporter MUST therefore
  treat it as a new time series: the cumulative counters restart at zero and
  the new `start_time_unix_nano` advances. Receivers detect the reset via
  the `start_time_unix_nano` change — the standard OTLP Sum reset signal —
  and MUST NOT splice the old and new sequences into one continuous series.

### 3.4 Per-flow-rule observability points

The counters defined in Spec 002 §8 are exported alongside the per-flow
data points. They are scoped to the flow-rule, not the key:

| OTLP metric name                  | Instrument | Unit         | Source                                    |
|-----------------------------------|------------|--------------|-------------------------------------------|
| `infmon.flow-rule.flows`          | Gauge      | `{flows}`  | `infmon_flow_rule_flows`                  |
| `infmon.flow-rule.evictions`        | Sum (cum.) | `{evictions}`| `infmon_flow_rule_evictions_total`        |
| `infmon.flow-rule.drops`            | Sum (cum.) | `{drops}`    | `infmon_flow_rule_drops_total{reason}`    |
| `infmon.flow-rule.packets`          | Sum (cum.) | `{packets}`  | `infmon_flow_rule_packets_total`          |
| `infmon.flow-rule.bytes`            | Sum (cum.) | `By`         | `infmon_flow_rule_bytes_total`            |

Their attribute set is `{flow-rule}` (plus `reason` for `drops`), nothing else.

The `reason` attribute on `infmon.flow-rule.drops` is a closed enum. v1 values:

| `reason`        | Meaning                                                          |
|-----------------|------------------------------------------------------------------|
| `table_full`    | Flow table at `max_keys`, no room to admit a new key.            |
| `parse_error`   | Upstream packet failed parser invariants (Spec 003).             |
| `rate_limit`    | Per-flow-rule admission rate limit (Spec 002 §8).                |
| `key_rejected`  | Key violated a flow-rule field constraint (e.g. invalid `dscp`). |

The authoritative list lives in Spec 002 §8; this table is a mirror for
implementer convenience. Any new value MUST be added there first.

## 4. Attribute mapping (per-flow points)

Each per-flow data point carries a fixed attribute set derived from
the flow-rule's field list (Spec 002 §3) plus a `flow-rule` label:

| Attribute         | Type        | Source                                  |
|-------------------|-------------|-----------------------------------------|
| `flow-rule`         | string      | flow-rule `name` (Spec 002 §2.1)          |
| `flow.src_ip`     | string      | `enc(src_ip)` rendered as canonical text (IPv4 if v4-mapped, else v6) |
| `flow.dst_ip`     | string      | same rule for `dst_ip`                  |
| `flow.ip_proto`   | int         | numeric `ip_proto`, 0–255               |
| `flow.dscp`       | int         | numeric `dscp`, 0–63                    |
| `flow.mirror_src_ip` | string   | canonical text form of the ERSPAN outer source IP (Spec 003 §4.2.1); only emitted when the flow-rule opts into `mirror_src_ip` (Spec 002 v1 field set). |

Rules:

1. The exporter MUST emit **only** attributes that correspond to fields
   actually present in the flow-rule's `ordered_field_list`. A flow-rule keyed
   on `[dscp]` produces points with attributes `{flow-rule, flow.dscp}` —
   nothing else.
2. The mapping is fixed (§9 reserves the `flow.*` namespace for future
   fields). Attribute names MUST NOT vary between exports.
3. IP addresses are rendered as text (canonical IPv4 dotted-quad if the
   16-byte key is v4-mapped, RFC 5952 lowercase v6 otherwise), not as raw
   bytes. Text form is what every viable collector / TSDB indexes natively.
4. `dscp` and `ip_proto` are emitted as integers, not numeric strings.

### 4.1 Why prefix with `flow.`

Reserves a namespace so future signals (e.g. `link.*` for interface stats,
`parser.*` for the upstream parser) do not collide. Keeps the collector-side
schema obvious at a glance.

### 4.2 Attribute order

OTLP attribute sets are unordered by spec; the exporter MAY emit them in
any order. Implementations SHOULD sort them by attribute key for stable
on-the-wire bytes (helps gRPC compression and snapshot diffs in tests).

## 5. Resource attributes

Every export carries a `Resource` describing the producer. v1 set:

| Attribute                       | Source                                        | Notes |
|---------------------------------|-----------------------------------------------|-------|
| `service.name`                  | constant `"infmon-frontend"`                  | OTel-mandated. |
| `service.namespace`             | constant `"infmon"`                           | Lets a multi-tenant collector group all InFMon producers. |
| `service.version`               | InFMon build version (`CARGO_PKG_VERSION` of `infmon-frontend`, or the package version baked in at build time) | The single "infmon version" mentioned in the issue. |
| `service.instance.id`           | stable per-process UUID, generated at frontend startup, persisted in `/var/lib/infmon/instance_id` | Distinguishes restarts from re-deploys. |
| `host.name`                     | `gethostname(2)` at startup                   | Human-readable host identity. |
| `host.id`                       | `/etc/machine-id` if readable, else absent    | Stable across reboots; omit silently if unavailable. |
| `host.arch`                     | runtime value of `std::env::consts::ARCH` (e.g. `"aarch64"` on the v1 BF-3 .deb build, `"x86_64"` on a dev box) | Reflects the actual binary; never hard-coded so unofficial dev/test builds aren't mis-labelled. Per OTel host conventions. |
| `infmon.dpu.id`                 | configured value (see §6); falls back to `host.name` if unset | The "dpu" attribute the issue calls for. Free-form short string operators set per-DPU. |
| `infmon.dpu.platform`           | configured value (e.g. `"bluefield-3"`)        | Optional; omitted if unset. |

`host.*` attributes follow OpenTelemetry [semantic conventions for host].
`service.*` likewise. `infmon.*` is our own namespace and is documented here.

Resource attributes are computed once at frontend startup and reused on
every export; they MUST NOT vary between exports of a single process
lifetime.

## 6. Endpoint and transport configuration

The exporter is configured in the same `infmon-frontend` config file
(format owned by Spec 005). The OTLP block:

### 6.1 TOML

```toml
[exporter.otlp]
enabled            = true
protocol           = "grpc"            # "grpc" | "http_protobuf"
endpoint           = "otel-collector.infra.local:4317"
                                       # gRPC default 4317, http/protobuf default 4318
insecure           = false             # true => plaintext (TCP, not mTLS)
ca_file            = "/etc/infmon/tls/ca.pem"      # optional; system trust if unset
cert_file          = "/etc/infmon/tls/client.pem"  # optional client mTLS
key_file           = "/etc/infmon/tls/client.key"  # optional client mTLS;
                                       # PEM-encoded; PKCS#8 unencrypted private
                                       # key (RSA or EC P-256/P-384). PKCS#1 RSA
                                       # is also accepted on rustls; encrypted
                                       # keys are rejected at startup.
compression        = "gzip"            # "none" | "gzip"
export_interval    = "10s"             # how often to take a snapshot and ship
export_timeout     = "5s"              # per-request timeout
max_batch_points   = 8192              # see §7
queue_size         = 4                 # see §7
max_export_points_per_tick = 2_000_000 # see §8.2; runtime safety cap

[exporter.otlp.headers]
# arbitrary key=value, sent on every request (gRPC metadata or HTTP headers)
"x-tenant" = "dpu-east-1"

[exporter.otlp.resource]
# operator-set resource attributes, merged on top of §5 defaults
"infmon.dpu.id"       = "bf3-rack17-u4"
"infmon.dpu.platform" = "bluefield-3"
```

### 6.2 Defaults

Field-by-field defaults if absent:

- `enabled = false` — the exporter is opt-in.
- `protocol = "grpc"`.
- `endpoint` — required when `enabled = true`; no default.
- `insecure = false`.
- `compression = "gzip"`.
- `export_interval = "10s"`.
- `export_timeout = "5s"`.
- `max_batch_points = 8192`.
- `queue_size = 4`.
- `max_export_points_per_tick = 2_000_000`.
- `headers` and `resource` default to empty.

### 6.3 Validation

Reject the config (and refuse to start) if:

1. `enabled = true` and `endpoint` is missing or unparseable.
2. `protocol` is not in `{"grpc", "http_protobuf"}`.
3. `compression` is not in `{"none", "gzip"}`.
4. Any of `cert_file` / `key_file` is set without the other.
5. Any TLS file path is set but unreadable at startup.
6. `export_interval`, `export_timeout` are not positive durations.
7. `export_timeout >= export_interval` (would let backpressure run away).
   The boundary `export_timeout == export_interval` is rejected because a
   single request consuming the full interval leaves zero headroom for
   snapshot acquisition + serialization on the next tick. Operators that
   want generous timeouts should also widen `export_interval`.
8. `max_batch_points <= 0`.
9. `queue_size <= 0`.
10. `max_export_points_per_tick <= 0`.

Configuration validation is all-or-nothing, mirroring Spec 002 §5.3.

## 7. Batching, retry, timeout

### 7.1 Tick

The exporter runs on a single periodic tick of period `export_interval`.
The first tick is delayed by a uniform random jitter in
`[0, export_interval)` (computed once at startup). This staggers a fleet of
DPUs that come up around the same time (e.g. after a rolling upgrade) so
they don't hammer the collector in lock-step — same default behaviour as
the upstream OTel SDK.

On each tick:

1. Ask the backend for a fresh snapshot (Spec 004 surface). The snapshot
   is read-only and lock-free w.r.t. the data plane.
2. Walk every flow-rule. For each live flow emit the data points defined
   in §3.2. Append the per-flow-rule points (§3.4).
3. Group data points into OTLP `ResourceMetrics` → `ScopeMetrics` →
   `Metric` according to standard OTLP shape (one `ResourceMetrics` per
   process; one `ScopeMetrics` per InstrumentationScope `infmon`; one
   `Metric` per metric name).
4. Slice into batches of at most `max_batch_points` data points and hand
   them to the send queue.

If a tick is still busy when the next tick fires, the new tick is **dropped**
and `infmon_exporter_ticks_dropped_total` is incremented. The exporter
never queues unbounded snapshots.

### 7.2 Send queue

A bounded in-memory queue of size `queue_size` batches sits between the
tick loop and the network sender. If the queue is full when a new batch
arrives, the **oldest** batch is dropped (the freshest cumulative data
supersedes it anyway) and `infmon_exporter_batches_dropped_total` is
incremented.

> **Gauge note.** Dropping the oldest batch is unambiguously safe for the
> `Sum` metrics — the newest cumulative value supersedes the older one.
> For the `infmon.flow.last_seen` Gauge, dropping older batches means a
> receiver may see `last_seen` jump forward without observing intermediate
> samples. This is acceptable for a "most recent packet" gauge, but worth
> stating explicitly given the rest of this section is precise about
> semantics.

### 7.3 Retry

Per-batch retry policy:

- Retry only on transport-level errors and OTLP `RetryableError` /
  HTTP 429 / HTTP 5xx.
- Exponential backoff with full jitter: `min(cap, base * 2^attempt) *
  rand(0, 1)`, with `base = 1s`, `cap = 30s`.
- Maximum 3 retries per batch. After the final failure the batch is
  dropped and `infmon_exporter_batches_failed_total{reason}` is bumped.
- The per-request timeout is `export_timeout`. The total time spent on
  a single batch (including retries) is bounded by
  `4 * export_interval` to keep the queue head from rotting.

> **Trade-off note.** With default `export_interval = 10s` and
> `max_retries = 3`, a single batch can occupy the sender for up to ~40s.
> Combined with `queue_size = 4`, a sustained slow / flapping collector
> can silently age out up to 3 ticks of data while the head batch retries.
> This is intentional — `last_seen` and the cumulative counters self-heal
> once exports resume, so freshness is preferred over delivery guarantees.
> Operators that want tighter freshness should lower `export_interval`
> (which lowers the retry ceiling proportionally) rather than enlarge
> `queue_size`. The drop is observable via
> `infmon_exporter_ticks_dropped_total` and
> `infmon_exporter_batches_dropped_total`.

Non-retryable errors (auth failure, schema rejection, HTTP 4xx other than
429) drop the batch immediately and bump
`infmon_exporter_batches_failed_total{reason="non_retryable"}`. The
exporter does **not** poison-pill subsequent batches.

### 7.4 Shutdown

On graceful shutdown the exporter MUST:

1. Stop the tick loop.
2. Drain the send queue, with a hard ceiling of one `export_timeout` per
   batch and at most `queue_size` batches.
3. Close the gRPC channel / HTTP client.

A signal-driven hard exit MAY skip drain. The cumulative semantics
guarantee a restart will resume cleanly with a fresh
`start_time_unix_nano`.

## 8. Cardinality guidance and safeguards

OTLP cardinality is the single sharpest foot-gun in this exporter, since
each live flow becomes ≥ 3 unique time series in the receiving TSDB.

### 8.1 Operator-facing guidance

Document in the `infmon-frontend` operator README and CLI `--help`:

- A flow-rule's worst-case time-series contribution to the collector is
  `max_keys × distinct_data_points_per_flow`. With v1's 3 per-flow
  metrics, a flow-rule with `max_keys = 1_048_576` can land 3 Mi series in
  the receiving TSDB **per scrape interval**.
- The four v1 fields are *all* high-cardinality except `dscp`. A flow-rule
  keyed on `[src_ip, dst_ip, ip_proto, dscp]` is appropriate for an
  always-on production deployment **only** if the receiving TSDB is sized
  for it.
- Prefer narrower flow-rules (e.g. `[src_ip]` for talker tables, `[dscp]`
  for class-of-service rollups) for default-on deployments.
- The growth signal is `infmon_flow_rule_evictions_total`. A non-zero,
  growing eviction rate means cardinality exceeds the configured budget;
  either grow `max_keys` or narrow the field list.

### 8.2 Exporter-side safeguards

The exporter MUST enforce these, independently of the backend:

1. **Hard per-export point cap** — `max_export_points_per_tick` (operator
   configurable in the `[exporter.otlp]` TOML, see §6; default
   `2_000_000`). If a tick would emit more, the exporter ships
   approximately the cap-many points using a **per-flow-rule proportional
   allocation**: each flow-rule gets a budget of
   `floor(effective_cap × flow_rule_flows / total_flows)` flows, where
   `effective_cap = floor(cap / points_per_flow)` (currently 3: packets,
   bytes, last_seen) so the total emitted point count never exceeds the
   configured cap. Any leftover from rounding is distributed in
   flow-rule-name order. Each flow-rule then emits its first `budget`
   flows in iteration order (stable per snapshot) and **drops the rest**,
   bumping
   `infmon_exporter_points_dropped_total{reason="export_cap"}`. The
   proportional split avoids the O(n log n) global sort by
   `last_seen_ns` that an earlier draft required, and keeps each
   flow-rule's representation roughly proportional to its live size. This
   protects the collector from a backend misconfiguration.
2. **Per-attribute length cap** — string attribute values are truncated
   to 256 bytes (UTF-8 safe) with a trailing `…`. Bumps
   `infmon_exporter_attrs_truncated_total`.
3. **Drop empty flow-rules** — a flow-rule with zero live flows emits its
   per-flow-rule points (§3.4) but no per-flow points. This is normal,
   not an error.
4. **No metadata explosion** — the exporter MUST NOT add free-form
   per-flow attributes beyond §4. There is no escape hatch in v1.

### 8.3 Self-observability

The exporter exports its own health on the same OTLP stream:

| OTLP metric name                              | Instrument | Unit       |
|-----------------------------------------------|------------|------------|
| `infmon.exporter.ticks_dropped`               | Sum (cum.) | `{ticks}`  |
| `infmon.exporter.batches_sent`                | Sum (cum.) | `{batches}`|
| `infmon.exporter.batches_dropped`             | Sum (cum.) | `{batches}`|
| `infmon.exporter.batches_failed`              | Sum (cum.) | `{batches}`|
| `infmon.exporter.points_emitted`              | Sum (cum.) | `{points}` |
| `infmon.exporter.points_dropped`              | Sum (cum.) | `{points}` |
| `infmon.exporter.attrs_truncated`             | Sum (cum.) | `{attrs}`  |
| `infmon.exporter.export_duration`             | Gauge      | `s`        |
| `infmon.exporter.queue_depth`                 | Gauge      | `{batches}`|

These carry the §5 resource attributes and a `reason` attribute where
relevant; they do **not** carry flow-rule or flow attributes.

> **Naming convention.** The dot-separated `infmon.exporter.*` names in
> the table above are the **canonical** OTLP metric names — those are
> what the exporter MUST emit on the wire. Underscore-separated forms
> like `infmon_exporter_ticks_dropped_total` used elsewhere in this spec
> (§7.1, §7.3, §8.2) and in Spec 002 §8 are prose shorthand: they
> describe the same metric in Prometheus-style notation that some
> collectors will end up exposing after the OTLP→Prometheus naming
> transform (`.` → `_`, `Sum` cumulative → `_total` suffix). Implementers
> emit the dot form; operators reading dashboards may see the underscore
> form depending on their downstream pipeline.
>
> **Why `export_duration` is a Gauge in v1.** A single gauge per export
> loses tail-latency information (p50/p99) that a Histogram or Summary
> would expose. v1 ships a Gauge to keep self-observability cheap and
> the cardinality story trivial. A future revision can add
> `infmon.exporter.export_duration` as a `Histogram` with a small fixed
> bucket set (or pair the gauge with `infmon.exporter.export_duration_max`)
> once we have an operator asking for it; this is listed in §9 as a
> non-normative extension hook.

## 9. Future extension hooks (non-normative)

- **L4 attributes.** When Spec 002 §9.1 adds `src_port`, `dst_port`,
  `tcp_flags`, the exporter gains `flow.src_port`, `flow.dst_port`,
  `flow.tcp_flags` under the same `flow.*` namespace, no schema break.
- **RoCE attributes.** `flow.roce_dest_qp`, `flow.roce_opcode` etc.,
  one-to-one with Spec 002 §9.2 fields.
- **Histograms.** RTT samples (Spec 000 component map mentions them as
  a future flow field) will export as OTLP `Histogram`. The instrument
  type column in §3.2 / §3.4 is precisely why instrument is per-metric,
  not global.
- **Multiple destinations.** v1 explicitly forbids fan-out; v2 can add
  `[[exporter.otlp]]` array form without breaking the v1 single-block
  shape.
- **Delta temporality.** Reserved as a per-instance switch
  (`temporality = "cumulative" | "delta"`); v1 ships cumulative only.

## 10. Test plan

- **Unit (Rust, `frontend/`):**
  - Attribute mapping: every v1 field → expected OTLP attribute key/type,
    including IPv4-mapped-IPv6 rendering and DSCP integer form.
  - Resource attribute assembly: precedence of operator config over
    defaults; missing `/etc/machine-id` is silent, not fatal.
  - Config validation: every rule in §6.3 has a positive and negative
    test.
  - Batch slicer: a 100k-point input with `max_batch_points = 8192`
    produces the expected number of batches, each ≤ cap.
  - Retry policy: simulated retryable / non-retryable errors hit the
    expected counters and respect the per-batch time ceiling.
  - Cardinality cap: a synthetic snapshot above
    `max_export_points_per_tick` truncates deterministically and bumps
    the drop counter.
- **Integration (Rust):**
  - Stand up an in-process OTLP gRPC stub (`tonic` mock service). Run a
    seeded backend snapshot through the exporter and assert the received
    `ExportMetricsServiceRequest` matches a golden file for both `grpc`
    and `http_protobuf` protocols.
  - Compression on/off produces byte-identical decoded payloads.
- **E2E (`tests/`, not in CI):**
  - Replay a captured ERSPAN pcap into the backend, point the exporter
    at a real `otelcol-contrib` with a `file` exporter, and assert the
    on-disk JSON contains the expected metric names, resource set, and
    attribute keys.
- **Performance:**
  - On the v1 target host (BF-3 ARM cores), an export tick over 1M live
    flows across 8 flow-rules SHOULD complete in ≤ 2 s wall time (well
    under the 10 s default `export_interval`). This is a guideline, not
    a CI gate.

## 11. Open questions

- **Q1.** Should `host.id` fall back to a hash of `host.name` when
  `/etc/machine-id` is missing, to keep the resource set complete? v1
  default: omit. Revisit if collector-side joins prove painful.
- **Q2.** Do we expose `temporality = "delta"` as a v1 hidden flag for
  collectors that prefer it (Datadog, some SaaS APMs)? v1 default: no,
  ship cumulative only. Re-evaluate once we have a concrete user.
- **Q3.** Should `infmon.dpu.id` be a *required* operator-set value
  (refuse to start if unset) rather than falling back to `host.name`?
  Defer to operator feedback after first deploy.
- **Q4.** OTLP exemplars on the `Sum` metrics, linking back to a future
  trace span? Out of scope for v1; revisit when a tracing story exists.
