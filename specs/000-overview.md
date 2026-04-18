# 000 — System Overview

## Version history

| Version | Date       | Author      | Changes |
| ------- | ---------- | ----------- | ------- |
| 0.1     | 2026-04-18 | Riff (r12f) | Initial draft. Establishes mission, scope, component map, repo layout, build/release model, glossary (incl. flow / flow-rule), and pointers to the canonical spec template at [`TEMPLATE.md`](TEMPLATE.md). |
| 0.2     | 2026-04-18 | Riff (r12f)  | Fix exporter format references: OTLP is the only v1 exporter (per spec 006). Update cross-references from spec 004 to spec 006, remove IPFIX-first language. |

---

## Context

Modern DPU-equipped servers (BlueField-3 and similar) terminate large volumes
of east-west and north-south traffic at line rate. Operators need per-flow
visibility — packets, bytes, TCP flags, RTT estimates, drops — without
sacrificing data-plane throughput. Existing tools (sFlow, IPFIX from kernel
paths, vendor-specific telemetry) either sample too aggressively, run on the
host CPU, or are tied to one ASIC.

InFMon is a flow-telemetry service that runs **on the DPU itself**, on top of
VPP/DPDK, consumes ERSPAN III mirrored copies of production traffic, builds
flow records in user-space, and exposes them through a local frontend and
standard exporters. Because it never sits inline, it cannot drop production
traffic, and because it runs on the DPU's ARM cores, it offloads telemetry
work entirely off the host.

## Mission

Provide a high-throughput, low-overhead, **DPU-resident flow telemetry
service** that turns mirrored packets into accurate, queryable flow records
and feeds them to operator tooling.

## Scope

In-scope for v1:

- Ingest ERSPAN III mirrored packets via VPP on BlueField-3 ARM cores.
- Parse L2–L4 (Ethernet, VLAN, IPv4/IPv6, TCP/UDP/ICMP).
- Maintain a flow table keyed by 5-tuple (+ VRF/VNI when present — see note
  below on extraction source).
- Maintain per-flow counters: packets, bytes, first/last seen, TCP flag union,
  and a deliberately narrow TCP RTT estimate for v1: handshake RTT only
  (time delta between observed `SYN` and matching `SYN-ACK` on the reverse
  direction of the same flow). Mid-stream / data-segment RTT estimation from
  mirrored traffic is out of scope for v1 and is deferred to a later spec.

> **Note on VRF/VNI source.** When the inbound packet is ERSPAN III, the
> `VRF/VNI` field in the flow key is taken from the ERSPAN III header itself
> (session ID / VRF-ID / SGT fields carried in the ERSPAN III shim). InFMon
> v1 does **not** parse inner tunnel encapsulation (VXLAN, GENEVE, inner
> GRE) to derive a VNI; doing so would exceed the L2–L4 scope above. If the
> mirrored payload happens to be a VXLAN frame, the VNI is treated as part
> of the opaque inner payload, not extracted into the flow key. This will be
> revisited in spec 002 (parser).
- Expose a snapshot/aggregate API consumed by `infmon-frontend`.
- Ship an OTLP exporter as the only v1 export format (see
  [spec 006](006-exporter-otlp.md)).
- A CLI (`infmon-cli`) for inspection, debugging, and admin.
- Packaging as a `.deb` for **arm64 / aarch64**, single artifact for v1.

Out-of-scope for v1 (non-goals):

- Inline packet steering or modification.
- Stateful application-layer parsing (TLS SNI, HTTP, gRPC, etc.).
- Multi-DPU clustering / federated aggregation.
- A long-term storage backend (InFMon is a producer, not a TSDB).
- A GUI dashboard. The frontend exposes APIs; visualization is downstream.
- x86 host builds (portable in principle; not a v1 deliverable).

## Component Map

```
                +-----------------------------------------------+
                |                  BlueField-3 DPU              |
                |                                               |
ERSPAN III ---> |  VPP graph                                    |
mirrored        |     |                                         |
packets         |     v                                         |
                |  +-------------------+                        |
                |  | infmon-backend    |                        |
                |  | (C/C++ VPP plug)  |                        |
                |  | - decap ERSPAN    |                        |
                |  | - parse L2–L4     |                        |
                |  | - flow table      |                        |
                |  | - counters        |                        |
                |  +---------+---------+                        |
                |            |  shared rings / SHM              |
                |            |  (snapshot/aggregate API,        |
                |            |   backend -> frontend only)      |
                |            v                                  |
                |  +-------------------+    +---------------+   |
                |  | infmon-frontend   |--->| exporters     |---+--> collectors
                |  | (Rust)            |    | (OTLP)        |   |     (off-DPU)
                |  | - aggregate       |    +---------------+   |
                |  | - serve API       |                        |
                |  +---------+---------+                        |
                |            ^                                  |
                +------------|----------------------------------+
                             |  local UDS, loopback only
                       +-----+------+
                       | infmon-cli |
                       |  (Rust)    |
                       +------------+
```

| Component         | Language | Test framework | Purpose                                      |
|-------------------|----------|----------------|----------------------------------------------|
| `infmon-backend`  | C/C++    | gtest          | VPP plugin: ERSPAN decap, parse, flow table  |
| `infmon-frontend` | Rust     | cargo test     | Snapshot/aggregate, exporter drivers, API    |
| `infmon-cli`      | Rust     | cargo test     | Operator/admin CLI over the frontend API     |
| `tests/`          | mixed    | pytest + pcap  | E2E real-packet replay (see *E2E execution* below) |

> **Backend language note.** "C/C++" here means: production VPP plugin code
> (graph nodes, parser, flow-table) is **C11**, matching VPP's own conventions
> and its C-only node registration macros (`VLIB_REGISTER_NODE` etc.). C++17
> is allowed only inside `backend/tests/` for GoogleTest fixtures and helpers,
> which link against the plugin's C ABI through `extern "C"` headers. No C++
> runtime is loaded into the VPP process in production.

> **Frontend → CLI API and security.** The frontend exposes its API to the
> CLI over a Unix domain socket bound to a path under `/run/infmon/` with
> `0660` permissions and an `infmon` group. Membership in that group is the
> v1 authorization model: any local process whose user is in `infmon` may
> query; others get `EACCES` at connect time. There is no network listener
> on this socket — it is loopback/UDS only, never TCP. Stronger auth (token,
> mTLS) is deferred and tracked as an open question; this is called out so
> operators on a shared DPU understand the trust boundary.

> **E2E execution.** The `tests/` suite is **not** run on every PR because
> it requires either a physical BlueField-3 with a real ERSPAN source or an
> emulated DPU rig with replayed pcaps. It is run in two situations:
> (1) **release-candidate gating** — every `vX.Y.Z-rc*` tag must pass the
> full E2E suite on the reference rig before promotion to a final tag, and
> (2) **nightly** on the maintainers' rig against `main`, with failures
> filed as issues. A manual `workflow_dispatch` job is also provided so a
> reviewer can request an E2E run on a specific PR when warranted.

## Data Flow

1. **Mirror.** Upstream switch / host vSwitch mirrors selected traffic via
   ERSPAN III, encapsulated in GRE, to an IP on the DPU.
2. **Receive.** DPDK PMD on the DPU delivers packets to VPP.
3. **Decap & dispatch.** A VPP node owned by `infmon-backend` recognizes
   ERSPAN III, strips the outer GRE/ERSPAN headers, and reinjects the inner
   frame **into a private InFMon-only sub-graph** for L2–L4 parsing and flow
   accounting. The decapped inner frame is **never** returned to VPP's
   standard L2/L3 forwarding graph: InFMon is a passive telemetry consumer
   and must not cause the DPU to forward, route, or otherwise act on the
   mirrored copy. Once parsing and counter updates complete, the buffer is
   freed at the terminal node.
4. **Match & key.** L2–L4 parser produces a flow key (5-tuple + VRF/VNI when
   available) and per-packet metadata (length, TCP flags, timestamp).
5. **Update.** Flow table lookup; on hit, update counters in place; on miss,
   insert a new flow record (with eviction policy per spec 003 to be written).
6. **Snapshot.** Frontend periodically reads the backend's exposed snapshot
   (lock-free / RCU-style; details in spec 002) and builds an aggregate view.
7. **Export.** Frontend pushes records to configured exporters and serves
   `infmon-cli` queries against the aggregate.

## Repo Layout

```
InFMon/
├── README.md
├── LICENSE                 # Apache-2.0
├── specs/
│   ├── 000-overview.md     # this file
│   ├── TEMPLATE.md         # canonical copy of the spec skeleton
│   └── NNN-<slug>.md       # one spec per accepted feature
├── backend/                # infmon-backend (C/C++ VPP plugin)
│   ├── src/
│   ├── include/
│   ├── tests/              # gtest unit tests
│   └── CMakeLists.txt
├── frontend/               # infmon-frontend (Rust)
│   ├── src/
│   ├── tests/
│   └── Cargo.toml
├── cli/                    # infmon-cli (Rust)
│   ├── src/
│   └── Cargo.toml
├── tests/                  # E2E, real-packet replay (NOT in CI)
│   ├── pcaps/
│   └── scenarios/
├── packaging/
│   └── debian/             # aarch64 .deb build files
└── .github/workflows/      # CI: build + unit tests for all components
```

## Language & Test Conventions

- **C/C++ (backend):**
  - C11 / C++17. Match VPP's existing style (4-space indent, snake_case
    functions, `vlib_*` / `vnet_*` patterns).
  - `clang-format` enforced in CI. Headers in `backend/include/`.
  - Unit tests: GoogleTest (`gtest`). Built only when
    `-DINFMON_BUILD_TESTS=ON`.
- **Rust (frontend, cli):**
  - Stable toolchain pinned via `rust-toolchain.toml`.
  - `rustfmt` and `clippy -D warnings` enforced in CI.
  - `cargo test` for unit + integration tests.
  - Workspace `Cargo.toml` at repo root if/when shared crates appear.
- **Commits:** Conventional Commits (`feat:`, `fix:`, `docs:`, `refactor:`,
  `test:`, `ci:`, `chore:`). All commits include `Signed-off-by:` (DCO).
- **Git history / merge style:** PR branches should be **rebased onto
  `main`** (not merge-committed) before merge. PRs are merged via GitHub's
  "Squash and merge" for single-logical-change PRs, or "Rebase and merge"
  for series of self-contained, individually reviewable commits. Merge
  commits on `main` are not used; `main` stays linear. Force-pushes are
  allowed on a PR branch *up to* approval; after approval, prefer additive
  fixup commits so reviewers can re-read deltas.
- **Specs:** every feature begins life as `specs/NNN-<slug>.md`. A spec is
  *accepted* when its PR merges into `main`. Implementation PRs reference the
  spec number in their description.

## Build & Release Model

v1 target: a single Debian package for **aarch64** (BlueField-3 ARM cores
running Ubuntu 22.04 / DOCA-supported distro).

- Build host: ARM64 Linux with VPP development headers and a recent stable
  Rust toolchain. Cross-compilation from x86 is a future nice-to-have, not a
  v1 requirement.
- Artifact:
  - `infmon_<version>_arm64.deb` — installs the VPP plugin (`.so` into VPP's
    plugin dir), the `infmon-frontend` daemon (systemd unit), and the
    `infmon-cli` binary into `/usr/bin`.
  - The package ships its plugin under VPP's standard plugin directory but
    **does not** force-enable it. VPP's default behavior is to auto-load
    every `.so` in the plugin dir; to keep InFMon opt-in on shared DPUs
    that may not need telemetry, the package installs a drop-in VPP config
    snippet at `/etc/vpp/startup.d/10-infmon.conf` that **disables**
    autoload by default:
    ```
    plugins {
        plugin infmon_plugin.so { disable }
    }
    ```
    Operators flip `disable` → `enable` (or remove the snippet) on the
    instances where InFMon should run. The systemd unit for
    `infmon-frontend` is also installed disabled and must be `systemctl
    enable --now`'d explicitly.
- Versioning: SemVer. The first published artifact is `v1.0.0`, cut once
  specs 000–005 are merged and their implementations are green in CI.
  **No `v0.x.y` releases are planned**; pre-`v1.0.0` testing happens off
  tagged builds (branch builds and `-rc` candidates of `v1.0.0`, e.g.
  `v1.0.0-rc1`). The `v0.x.y` scheme is therefore intentionally not used
  and should not appear on tags.
- CI scope:
  - **Per-PR (GitHub-hosted runners, x86_64):** `infmon-frontend` and
    `infmon-cli` Rust builds + `cargo test` + `clippy` + `rustfmt`. Spec /
    docs lint. The backend's C code is **syntax/format-checked** here
    (`clang-format --dry-run`, header-only compile checks where feasible)
    but its full build and `gtest` suite are *not* run on x86, because the
    plugin links against VPP headers that target the deployment ABI.
  - **Per-PR (self-hosted aarch64 runner with VPP dev headers):** full
    `infmon-backend` build + `gtest` suite. This job is required for
    merge. The aarch64 runner is the same class of host described under
    "Build host" above.
  - **Manual / nightly (reference rig):** the `tests/` E2E suite, as
    described in *E2E execution* in the Component Map section.

## Glossary

- **DPU** — Data Processing Unit. Smart-NIC with general-purpose cores
  (BlueField-3 has ARM Cortex-A78AE cores) plus accelerators.
- **BlueField-3 (BF-3)** — NVIDIA's third-gen DPU. v1 target platform.
- **VPP** — Vector Packet Processing. FD.io's user-space packet processing
  framework built on DPDK; processes packets in vectors through a graph of
  nodes.
- **DPDK** — Data Plane Development Kit. User-space NIC drivers and packet
  I/O primitives that VPP sits on top of.
- **ERSPAN** — Encapsulated Remote Switched Port Analyzer. A traffic mirror
  protocol that wraps mirrored frames in GRE so they can be sent across an
  IP network. **ERSPAN III** is the version with a richer header (timestamps,
  hardware ID, security group tag).
- **Flow** — A unidirectional sequence of packets sharing the same key
  (typically 5-tuple: src IP, dst IP, src port, dst port, protocol; extended
  with VRF/VNI when present). Flows are produced by **flow-rules**: each
  flow-rule observes mirrored traffic and generates one flow per distinct
  key tuple it sees, with its own packet/byte counters.
- **Flow-rule** — A configured matcher (key-set + limits) that selects
  packets and emits flows. One flow-rule yields many flows (one per unique
  key tuple). Flow-rules are managed via `infmon-cli flow-rule {add,rm,list,show}`;
  the resulting flows are read with `infmon-cli flow {list,show}`.
- **Flow table** — In-memory data structure that maps a flow key to its
  current counters and metadata; the backend's core state.
- **Snapshot** — A point-in-time, read-only view of the flow table that the
  frontend consumes without blocking the data path.
- **Aggregate** — Frontend-side reduction of snapshots over a window (e.g.
  top-N talkers, per-VRF totals).
- **Exporter** — Component that pushes flow records to an external collector
  in a standard format (OTLP for v1; see [spec 006](006-exporter-otlp.md)).
- **Collector** — Off-DPU consumer of exported records.
- **Frontend** — `infmon-frontend`, the Rust user-space service that owns
  aggregation, exporters, and the API surface for the CLI.
- **CLI** — `infmon-cli`, operator/admin tool that talks to the frontend.
- **DCO / Signed-off-by** — Developer Certificate of Origin attestation
  required on every commit (`git commit -s`).

## Spec Template

Every subsequent spec lives at `specs/NNN-<slug>.md` and follows the
skeleton tracked at [`specs/TEMPLATE.md`](TEMPLATE.md). Copy that file
verbatim when starting a new spec; do not hand-roll the header.

Conventions enforced by `TEMPLATE.md` (and required for every spec):

- **One PR = one row** in the `## Version history` table. When you push
  fixes addressing review comments on the same PR, **amend the existing
  row's `Changes` cell** instead of appending a new row per iteration.
- The `Author` column uses the GitHub display name and handle of the
  PR author, in the form `Name (handle)` — e.g. `Riff (r12f)`.
- Cross-spec metadata (`Depends on`, `Related`, `Parent epic`, `Affects`)
  is optional — include only what applies. **`Depends on` and `Related`
  entries must be markdown links** to the target spec file (e.g.
  `[002-flow-tracking-model](002-flow-tracking-model.md)`), never bare
  names.
- The following fields are **forbidden** and must not appear in any spec:
  `Owner` / `Owners`, `Status`, `Reviewer` / `Reviewers`, `Last updated`,
  `Tracking issue`. The PR itself plus the version-history table cover
  ownership, status, and timestamps; tracking issues are not used for
  spec docs.

See [`TEMPLATE.md`](TEMPLATE.md) for the full skeleton and the inline
explanations of each rule.

## Open Questions

1. **Exporter format for v1** — OTLP is the only v1 exporter. Decided in
   [spec 006](006-exporter-otlp.md).
2. **Snapshot transport** — shared memory ring vs. local Unix-domain socket
   vs. gRPC. Decided in spec 002. *Default:* shared-memory ring (lowest
   overhead on DPU).
3. **Flow table eviction policy** — LRU, time-based, or hybrid. Decided in
   spec 003. *Default:* time-based with active/idle timeouts (IPFIX-aligned).
4. **Distro target** — Ubuntu 22.04 only, or also DOCA's recommended base?
   Decided in spec 005. *Default:* Ubuntu 22.04 arm64.
5. **Frontend API authn/authz** — v1 ships with UNIX-group-based access on a
   UDS (see Component Map note). Should we add a token or mTLS layer for
   multi-tenant DPU scenarios? Tracked for a post-v1 spec. *Default:* keep
   group-based for v1; revisit when a concrete multi-tenant requirement
   lands.
