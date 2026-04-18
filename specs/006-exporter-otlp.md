# Spec 006 — OTLP exporter (v1)

Status: Draft
Tracking issue: DPU-12 (project InFMon)
Parent epic: DPU-4 (EPIC: InFMon — flow telemetry service on BF-3)
Depends on: 000-overview (system overview),
            002-flow-tracking-model (defines trackers, keys, buckets,
            and the per-tracker observability counters this exporter ships)
Related: 004-backend-architecture (owns the snapshot mechanism the
         exporter consumes), 005-frontend (hosts the exporter process),
         007-cli (`infmon-cli exporter ...` surface)

## 1. Purpose

Define the **v1 OpenTelemetry (OTLP) exporter** for InFMon: how flow-tracker
state held by the backend (Spec 002) is turned into OTLP metrics and shipped
off-DPU to a collector.

The OTLP exporter is the **only** standard-format exporter in v1. It is the
sole production sink for flow data; everything else (CLI `flow show`, ad-hoc
JSON dumps) is for debugging. Getting this contract right means downstream
collectors, dashboards, and alerting can be built without waiting for InFMon
to stabilise further.

## 2. Concepts

### 2.1 Exporter instance

A single exporter instance is configured per `infmon-frontend` process.
Multiple destinations are not supported in v1 — operators who need fan-out
should run an OpenTelemetry Collector adjacent to the DPU.

### 2.2 Source of truth

The exporter never touches the data plane. It consumes the **snapshot**
surface owned by the backend (Spec 004) on a fixed cadence and translates
each tracker's live buckets into OTLP data points.

### 2.3 Wire shape

OTLP / metrics. v1 ships the gRPC transport by default and accepts
http/protobuf as a configuration switch. JSON-over-HTTP is **not** supported
(noisier on the wire, no observed collector that requires it).

## 3. Signal type and metric model

### 3.1 Signal: metrics

InFMon exports **metrics** only in v1. Logs and traces are out of scope —
flow data is intrinsically aggregate counter state, and shoehorning it
through `Logs` would lose the collector's native aggregation pipeline.

### 3.2 Per-bucket data points

Each live bucket in each tracker becomes a fixed set of OTLP data points,
all sharing the bucket's attribute set (§4):

| OTLP metric name             | Instrument | Unit  | Source field      | Notes                                     |
|------------------------------|------------|-------|-------------------|-------------------------------------------|
| `infmon.flow.packets`        | Sum (cumulative, monotonic) | `{packets}` | `bucket.packets` | Total packets attributed to this bucket. |
| `infmon.flow.bytes`          | Sum (cumulative, monotonic) | `By`        | `bucket.bytes`   | Total bytes (L3 length per Spec 003).    |
| `infmon.flow.last_seen`      | Gauge      | `ns`  | `bucket.last_seen_ns` | Wall-clock ns of the most recent packet that updated this bucket. |

`first_seen_ns` is **not** exported as its own data point. It is folded into
the data-point `start_time_unix_nano` of the two `Sum` metrics, which is the
OTLP-native way to express "this counter started accumulating at time T".

### 3.3 Aggregation temporality

`Cumulative` for both `Sum` metrics. Rationale:

- Cumulative is the OTLP default and what most collector pipelines (Prometheus
  remote-write, Mimir, Cortex) prefer.
- The backend owns the bucket lifetime; the exporter is stateless. Cumulative
  + a stable `start_time_unix_nano` lets a collector compute deltas without
  the exporter having to track previous-export state.
- On bucket eviction (Spec 002 §6) the counter is gone. Receivers that
  compute deltas will see a counter reset; OTLP Sum semantics handle that
  correctly when `start_time_unix_nano` advances.

### 3.4 Per-tracker observability points

The counters defined in Spec 002 §8 are exported alongside the per-bucket
data points. They are scoped to the tracker, not the key:

| OTLP metric name                  | Instrument | Unit         | Source                                    |
|-----------------------------------|------------|--------------|-------------------------------------------|
| `infmon.tracker.buckets`          | Gauge      | `{buckets}`  | `infmon_tracker_buckets`                  |
| `infmon.tracker.evictions`        | Sum (cum.) | `{evictions}`| `infmon_tracker_evictions_total`          |
| `infmon.tracker.drops`            | Sum (cum.) | `{drops}`    | `infmon_tracker_drops_total{reason}`      |
| `infmon.tracker.packets`          | Sum (cum.) | `{packets}`  | `infmon_tracker_packets_total`            |
| `infmon.tracker.bytes`            | Sum (cum.) | `By`         | `infmon_tracker_bytes_total`              |

Their attribute set is `{tracker}` (plus `reason` for `drops`), nothing else.

## 4. Attribute mapping (per-bucket points)

Each per-bucket data point carries a fixed attribute set derived from
the tracker's field list (Spec 002 §3) plus a `tracker` label:

| Attribute         | Type        | Source                                  |
|-------------------|-------------|-----------------------------------------|
| `tracker`         | string      | tracker `name` (Spec 002 §2.1)          |
| `flow.src_ip`     | string      | `enc(src_ip)` rendered as canonical text (IPv4 if v4-mapped, else v6) |
| `flow.dst_ip`     | string      | same rule for `dst_ip`                  |
| `flow.ip_proto`   | int         | numeric `ip_proto`, 0–255               |
| `flow.dscp`       | int         | numeric `dscp`, 0–63                    |

Rules:

1. The exporter MUST emit **only** attributes that correspond to fields
   actually present in the tracker's `ordered_field_list`. A tracker keyed
   on `[dscp]` produces points with attributes `{tracker, flow.dscp}` —
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
| `host.arch`                     | constant `"arm64"` for the v1 .deb build      | Matches Spec 000 §"Build & Release Model". |
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
key_file           = "/etc/infmon/tls/client.key"  # optional client mTLS
compression        = "gzip"            # "none" | "gzip"
export_interval    = "10s"             # how often to take a snapshot and ship
export_timeout     = "5s"              # per-request timeout
max_batch_points   = 8192              # see §7
queue_size         = 4                 # see §7

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
8. `max_batch_points <= 0`.
9. `queue_size <= 0`.

Configuration validation is all-or-nothing, mirroring Spec 002 §5.3.

## 7. Batching, retry, timeout

### 7.1 Tick

The exporter runs on a single periodic tick of period `export_interval`.
On each tick:

1. Ask the backend for a fresh snapshot (Spec 004 surface). The snapshot
   is read-only and lock-free w.r.t. the data plane.
2. Walk every tracker. For each live bucket emit the data points defined
   in §3.2. Append the per-tracker points (§3.4).
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
each live bucket becomes ≥ 3 unique time series in the receiving TSDB.

### 8.1 Operator-facing guidance

Document in the `infmon-frontend` operator README and CLI `--help`:

- A tracker's worst-case time-series contribution to the collector is
  `max_keys × distinct_data_points_per_bucket`. With v1's 3 per-bucket
  metrics, a tracker with `max_keys = 1_048_576` can land 3 Mi series in
  the receiving TSDB **per scrape interval**.
- The four v1 fields are *all* high-cardinality except `dscp`. A tracker
  keyed on `[src_ip, dst_ip, ip_proto, dscp]` is appropriate for an
  always-on production deployment **only** if the receiving TSDB is sized
  for it.
- Prefer narrower trackers (e.g. `[src_ip]` for talker tables, `[dscp]`
  for class-of-service rollups) for default-on deployments.
- The growth signal is `infmon_tracker_evictions_total`. A non-zero,
  growing eviction rate means cardinality exceeds the configured budget;
  either grow `max_keys` or narrow the field list.

### 8.2 Exporter-side safeguards

The exporter MUST enforce these, independently of the backend:

1. **Hard per-export point cap** — `max_export_points_per_tick`
   (compile-time default `2_000_000`). If a tick would emit more, the
   exporter ships the first cap-many points sorted by tracker name then
   bucket recency and **drops the rest**, bumping
   `infmon_exporter_points_dropped_total{reason="export_cap"}`. This
   protects the collector from a backend misconfiguration.
2. **Per-attribute length cap** — string attribute values are truncated
   to 256 bytes (UTF-8 safe) with a trailing `…`. Bumps
   `infmon_exporter_attrs_truncated_total`.
3. **Drop empty trackers** — a tracker with zero live buckets emits its
   per-tracker points (§3.4) but no per-bucket points. This is normal,
   not an error.
4. **No metadata explosion** — the exporter MUST NOT add free-form
   per-bucket attributes beyond §4. There is no escape hatch in v1.

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
relevant; they do **not** carry tracker or bucket attributes.

## 9. Future extension hooks (non-normative)

- **L4 attributes.** When Spec 002 §9.1 adds `src_port`, `dst_port`,
  `tcp_flags`, the exporter gains `flow.src_port`, `flow.dst_port`,
  `flow.tcp_flags` under the same `flow.*` namespace, no schema break.
- **RoCE attributes.** `flow.roce_dest_qp`, `flow.roce_opcode` etc.,
  one-to-one with Spec 002 §9.2 fields.
- **Histograms.** RTT samples (Spec 000 component map mentions them as
  a future bucket field) will export as OTLP `Histogram`. The instrument
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
    buckets across 8 trackers SHOULD complete in ≤ 2 s wall time (well
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
