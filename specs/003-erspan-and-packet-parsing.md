# 003 — ERSPAN and Packet Parsing

## Version history

| Version | Date       | Author       | Changes |
| ------- | ---------- | ------------ | ------- |
| 0.1     | 2026-04-18 | Riff (r12f)  | Initial draft. Defines ERSPAN III over GRE decap, parser boundary (mirror transport vs user packet), and §4.2.1 explicit outer-IPv4/IPv6 source-IP extraction → `mirror_src_ip` as the only outer-header value crossing the parser boundary into flow-keys (Spec 002). ERSPAN session ID is dropped. |

## 1. Motivation

InFMon is fed exclusively by **mirrored** copies of production traffic, not by
inline packets. On BlueField-3 the mirror is delivered as **ERSPAN Type III
encapsulated in GRE** over IPv4/IPv6, often with the inner packet **truncated**
to ~128 bytes to keep mirror bandwidth bounded. Every later stage of the
pipeline — flow tracking (spec 002), backend architecture (spec 004),
exporters — operates on the *inner* packet only. The line between "mirror
transport" (which we throw away) and "user packet" (which we measure) has to be
drawn explicitly and once, so that:

- the parser is the only component that ever touches outer headers,
- truncation is handled in one place with one set of rules,
- a future encapsulation (VXLAN, GENEVE, RoCEv2) can be added behind a single
  hook without rewriting the rest of the pipeline,
- ERSPAN session IDs do **not** leak into telemetry records (they describe
  operator topology, not flows).

This spec defines the parsing pipeline, its bounds-safety contract, the
counters it MUST emit, and the seam reserved for future inner encapsulations.

## 2. Scope

In scope:

- Parsing **ERSPAN Type III over GRE** (GRE proto `0x22EB`).
- Skipping all outer headers (Ethernet, optional single VLAN, IPv4/IPv6, GRE,
  ERSPAN III header + optional Platform-Specific Sub-Header).
- Returning a bounded view of the **inner packet** to downstream code.
- Surfacing **mirror metadata** (currently the outer source IP — i.e. the
  IP of the device that mirrored the packet to us) alongside the inner view
  so downstream flow-rules can key on or label by mirror device. This is
  the only outer-header field that propagates downstream; everything else
  is dropped by §4.
- Tolerating truncated mirror copies (~128 B end-to-end) without overreads.
- A **single inner-decap hook** reserving room for one future extra
  encapsulation layer (VXLAN / GENEVE / RoCEv2). v1 ships the hook **disabled**
  (identity).
- Per-reason drop counters and an "accepted-but-truncated" counter.
- Test plan (unit, golden PCAPs, fuzz).

Out of scope (deliberately):

- ERSPAN Type I / Type II.
- ERSPAN session-ID exposure or per-session demultiplexing.
- IP fragment reassembly (outer or inner).
- Outer IP / GRE checksum verification (mirror copies are trusted).
- Stateful or cross-packet logic.
- RoCEv2 BTH parsing — only the design seam is reserved (§7).
- Performance tuning (batching, NUMA, prefetch). Covered later.

## 3. Inputs and Assumptions

- Wire format: `Ethernet [VLAN] → IPv4|IPv6 → GRE(proto=0x22EB) → ERSPAN III →
  inner packet`.
- GRE flags: only `S` (sequence) MAY be set. `C` (checksum) and `K` (key) MUST
  NOT be set; if they are, the packet is dropped (counter
  `gre_unexpected_flags`).
- Mirror source: BlueField-3 hardware mirror, configurable snap length,
  default ~128 bytes total wire length.
- Buffer contract: the parser receives a single contiguous DPDK mbuf segment.
  Multi-segment mbufs are linearised by the VPP input node before the parser
  runs (the input node is out of scope here; it is mentioned for clarity).
  If a multi-segment mbuf nonetheless reaches the parser, the contract is
  violated; the parser MUST drop the packet and increment
  `mbuf_not_contiguous` (see §6) rather than risk a silent overread. Debug
  builds also assert.
- The parser is a **pure function of bytes**: no allocations, no syscalls, no
  per-packet logging, no cross-packet state.

## 4. Parsing Pipeline

### 4.1 Stages

```text
[ outer L2 ] -> [ outer L3 ] -> [ GRE ] -> [ ERSPAN III ] -> [ INNER PACKET ] -> downstream
```

Outer headers exist almost entirely to locate the inner packet. With **one
named exception** — the outer L3 source IP, surfaced as `mirror_src_ip`
(§4.2.1) — **no field from any outer header is propagated downstream**.
Telemetry describes the mirrored flow; the only piece of mirror-infrastructure
identity that crosses the parser boundary is the IP address of the device
that performed the mirror, because flow-rules (Spec 002) need it to attribute
flows to a mirroring source.

### 4.2 Outer Header Skip

| Layer    | Action                                                                                                              |
| -------- | ------------------------------------------------------------------------------------------------------------------- |
| Ethernet | skip 14 B; if EtherType = `0x8100`, skip 4 more B (single VLAN tag). QinQ (two stacked tags): drop, `outer_qinq_unsupported`. Any EtherType other than `0x0800` (IPv4), `0x86DD` (IPv6), or `0x8100` (single VLAN) — including MPLS (`0x8847`/`0x8848`), ARP (`0x0806`), QinQ outer S-tag (`0x88A8`), etc. — is dropped, `outer_ethertype_unsupported`. |
| IPv4     | require version=4, IHL ≥ 5, protocol = `47` (GRE). Skip `IHL*4` bytes. Checksum NOT verified.                       |
| IPv6     | require version=6. Extension headers: parse and skip Hop-by-Hop Options (next-header `0`) and Destination Options (next-header `60`) using their `Hdr Ext Len` field; these are commonly inserted by infrastructure and silently dropping them would lose mirrored traffic. Any other extension header (Routing, Fragment, AH, ESP, Mobility, …) → drop, `outer_v6_ext_unsupported`. After skipping allowed options, the final next-header MUST be `47` (GRE). |
| GRE      | require version=0, protocol = `0x22EB`. Length = 4 B base + 4 B if `S` flag set. Flag handling is a strict allowlist: only the `S` bit (sequence) MAY be set; any other non-zero flag bit (`C`, `K`, `R`, or any reserved/recursion bit) → drop, `gre_unexpected_flags`. Non-zero GRE version → drop, `gre_bad_version`. Other proto (with version=0 and only `S` set) → drop, `gre_bad_proto`. |

### 4.2.1 Outer source IP extraction → `mirror_src_ip`

Before the outer headers are skipped past, the parser **copies out the outer
L3 source address** as the parsed field `mirror_src_ip`. This is the single
authoritative place where that extraction happens.

- For an outer **IPv4** header, take the 4-byte `Source Address` (IPv4 SA)
  and store it in `mirror_src_ip` as `{family = v4, addr = SA}`.
- For an outer **IPv6** header, take the 16-byte `Source Address` (IPv6 SA)
  and store it in `mirror_src_ip` as `{family = v6, addr = SA}`.
- The outer **destination IP, TTL/hop-limit, ToS/TC, flow label, and any
  other outer-header field** are NOT extracted and NOT exposed.
- The ERSPAN session ID remains explicitly stripped (§4.4); `mirror_src_ip`
  is not a back door for it.

`mirror_src_ip` is then carried alongside the inner-packet view all the way
to the flow-tracking layer. It is the only outer-header value any downstream
stage may key off of, and Spec 002 lists it in the v1 flow-key field set as
`mirror_src_ip` (opt-in per flow-rule, included in the recommended default
flow-rule). Its representation when stored in a key is fixed by Spec 002
§3 (16 B, IPv4 mapped to v6); the parser hands it to the flow layer in the
tagged-union form described under §4.5 and the flow layer applies the
mapping when assembling the key bytes.

The extraction is unconditional: every successfully parsed ERSPAN frame
carries a `mirror_src_ip` value. Flow-rules that don't list it simply
ignore it; there is no separate "extract on demand" path, which keeps the
parser branch-free with respect to flow-rule configuration.

### 4.3 ERSPAN Type III Header

Fixed 12-byte header (per `draft-foschiano-erspan-03`). The diagram below is
indicative; the authoritative bit ranges are listed underneath:

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|  Ver  |  VLAN         | COS |BSO|T|        Session ID         |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                          Timestamp                            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|     SGT       |P|  FT |  Hw ID  |D|Gra|O|     Reserved        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

Authoritative bit layout (word, bit-range, width):

- Word 0 (bits 0–31): `Ver[0:3]` (4), `VLAN[4:15]` (12), `COS[16:18]` (3),
  `BSO[19:20]` (2), `T[21]` (1), `Session ID[22:31]` (10).
- Word 1 (bits 32–63): `Timestamp[32:63]` (32).
- Word 2 (bits 64–95): `SGT[64:79]` (16), `P[80]` (1), `FT[81:85]` (5),
  `Hw ID[86:91]` (6), `D[92]` (1), `Gra[93:94]` (2), `O[95]` (1),
  followed by 0 reserved bits in word 2.

(Total: 12 B = 96 bits. Implementations MUST mask/shift from these ranges,
not from the ASCII art.)

Validate `Ver == 2`. Otherwise drop, counter `erspan_bad_version`.

The ERSPAN `T` bit (truncated-by-source) is **not consumed**; truncation
status is derived solely from the length comparison in §5 (`inner_len <
declared_inner_len`). Even when `T == 1` and lengths indicate a complete
inner packet, `inner_truncated` remains `false`. This keeps truncation
semantics single-sourced.

If `O = 1`, an 8-byte **Platform-Specific Sub-Header** follows. The parser
**skips** it as opaque bytes. Its contents are NOT exposed.

The 32-bit ERSPAN **Timestamp** field is also **NOT** propagated downstream,
for the same reasons given for the Session ID in §4.4 (it describes mirror
infrastructure, not the mirrored flow). The parser may use it locally for
debugging but MUST scrub it from any struct it hands onward.

### 4.4 Session ID — Explicitly NOT Exposed

The 10-bit ERSPAN Session ID **MUST NOT** appear in any record handed to
downstream stages, exporters, or logs. Rationale:

- Session IDs describe the mirroring fabric (which SPAN session copied the
  packet), not the mirrored flow.
- Surfacing them couples downstream consumers to operator topology.
- They leak mirror configuration into telemetry, which is a small but real
  information disclosure.

The parser MAY use the Session ID **locally** (e.g. for per-session drop
counters during debugging) but MUST scrub it from any buffer or struct it
hands onward. A negative test (§8) enforces this.

If multi-session demultiplexing is ever needed it will arrive behind an
explicit feature flag and a new spec.

### 4.5 Inner Packet — Match Always Begins Here

The "inner packet" begins at the first byte after the ERSPAN III header (and
Platform Sub-Header, if present). All downstream parsers, matchers, and
feature extractors **see only the inner packet**, with the single exception
of `mirror_src_ip` (see "The parser returns" below) which travels alongside
the inner view as opaque mirror-device metadata.

The parser returns:

- `inner_ptr` — pointer into the original mbuf (no copy).
- `inner_len` — number of inner bytes available (may be less than the declared
  inner length; see §5).
- `inner_truncated` — bool, true iff `inner_len < declared_inner_len`.
- `mirror_src_ip` — the **outer source IP** of the GRE/ERSPAN packet,
  i.e. the IP address of the device that performed the mirror. Stored as
  a tagged union (`v4` / `v6`), copied out of the outer L3 header before
  the outer headers are discarded. This is the only outer-header value
  that crosses the parser boundary; downstream code MUST treat all other
  outer fields as gone. The ERSPAN session ID remains stripped (§4.4).

`declared_inner_len` is computed from the outer IP length field minus the
bytes consumed by outer L3 + GRE + ERSPAN III + (optional) Platform
Sub-Header. Note the IPv4/IPv6 length-field semantics differ:

- **IPv4:** `declared_inner_len = ipv4.total_length - IHL*4 - gre_len - erspan_len`
  (IPv4 `Total Length` includes the IP header.)
- **IPv6:** `declared_inner_len = ipv6.payload_length - v6_ext_len - gre_len - erspan_len`
  (IPv6 `Payload Length` excludes the 40-byte fixed header; subtract any
  parsed extension-header bytes from §4.2.)

Where `erspan_len = 12 + (O ? 8 : 0)` and `gre_len = 4 + (S ? 4 : 0)`. The
parser never trusts a length field without bounds-checking it against the
actual mbuf length.

### 4.6 One Inner Encap Layer (Reserved Seam)

To leave room for VXLAN, GENEVE, RoCEv2, etc. without rewriting the parser,
the spec reserves a single **inner-decap hook** that may peel **at most one**
additional encapsulation between the ERSPAN payload and the user packet:

```c
typedef enum {
    INFMON_DECAP_NONE = 0,    /* identity, v1 default */
    INFMON_DECAP_VXLAN,       /* future */
    INFMON_DECAP_GENEVE,      /* future */
    INFMON_DECAP_ROCEV2,      /* future, see §7 */
} infmon_inner_decap_t;

/* Returns 0 on success; sets *out_ptr / *out_len to the de-encapsulated view.
 * `in_truncated` MUST be propagated from the outer parser so the decap
 * function can avoid reading past a partially-present inner-encap header
 * (e.g. a VXLAN/GENEVE/BTH header that didn't survive the mirror snap).
 * Returns nonzero on error (caller drops the packet, increments a per-decap
 * counter). MUST be called at most once per packet. */
int infmon_inner_decap(infmon_inner_decap_t kind,
                       const uint8_t *in, uint32_t in_len, bool in_truncated,
                       const uint8_t **out_ptr, uint32_t *out_len,
                       bool *out_truncated);
```

Constraints:

- v1 wires `INFMON_DECAP_NONE` only.
- Nested inner encap is not supported. A second call per packet is a
  programming error, asserted in debug builds, counted as
  `inner_double_encap_dropped` in release builds.

## 5. Truncated Packet Handling (~128 B Snap)

A typical BF-3 mirror snap of 128 B leaves, after outer Ethernet (14) + outer
IPv4 (20) + GRE (4 or 8) + ERSPAN III (12) = **50–54 B of overhead**, roughly
**74–78 B of inner packet**. That is enough for inner Ethernet + inner IPv4 +
TCP/UDP ports, which is the minimum we need for flow tracking (spec 002). The
parser's contract under truncation is:

### 5.1 Short-Read Tolerance

- The parser **never reads past the end of the mbuf**. Every header step
  bounds-checks before dereferencing.
- Short read on **outer** headers → fatal drop, `outer_truncated`.
- Short read on the **ERSPAN III header itself** (or on the Platform
  Sub-Header when `O=1`) → fatal drop, `erspan_truncated`.
- Short read on the **inner packet** → **non-fatal**. The parser returns the
  partial bytes with `inner_truncated = true` and `inner_len` set to the
  bytes actually present.

### 5.2 Partial-Header Rules for the Inner Packet

When `inner_truncated == true`, downstream feature extractors apply these
rules:

1. **Inner Ethernet (14 B)** — required. Missing → drop, `inner_eth_truncated`.
2. **Inner L3 fixed header** — required (20 B IPv4, 40 B IPv6). Missing →
   drop, `inner_l3_truncated`. Without it the 5-tuple cannot be formed and the
   record is useless for flow telemetry.
3. **Inner L4 header** — best-effort, **TCP and UDP only** in v1:
   - Ports (first 4 B of TCP/UDP): if **fully** present, extract; if
     **partially** present, treat as absent and mark `flow_key_partial = true`
     on the record.
   - TCP flags / window / sequence / ack: extract only fields whose bytes are
     fully present. Missing fields are reported as `unknown` (NOT zero — zero
     would corrupt aggregates downstream). The concrete representation is a
     parallel `valid_fields` bitmask on the parser output struct: each
     extractable inner-L4 field has a corresponding bit (`PORTS_VALID`,
     `TCP_FLAGS_VALID`, `TCP_SEQ_VALID`, `TCP_ACK_VALID`,
     `TCP_WINDOW_VALID`, …); the field's storage value is undefined when its
     valid-bit is 0 and MUST NOT be read by downstream code. This avoids
     consumers picking ad-hoc sentinels (`0xFF…`) that collide with real
     wire values.
   - **Other L4 protocols** (SCTP, ICMP/ICMPv6, GRE, ESP, AH, …): no
     port/flag extraction is attempted in v1. The L3 5-tuple becomes a
     3-tuple-plus-protocol; `PORTS_VALID` is 0 and `flow_key_partial = true`.
     The packet is still accepted (counts as `parsed_ok` /
     `inner_truncated_ok`); a future spec MAY add per-protocol extractors.
4. **L4 payload** — only the *observed length* (`l4_payload_observed_len`) is
   recorded in v1. Content is not inspected.

### 5.3 Counter for Successful Truncated Records

A packet that is truncated but still yields a usable 5-tuple-bearing record
increments `inner_truncated_ok`. A packet that is killed by truncation
increments the specific `*_truncated` reason. A fully present packet
increments `parsed_ok`.

## 6. Error Model and Counters

All counters are exported through the existing VPP per-node counter
infrastructure. The parser **never panics**, **never logs per packet**, and
**never allocates**.

| Counter                       | Meaning                                                           |
| ----------------------------- | ----------------------------------------------------------------- |
| `parsed_ok`                   | accepted, full inner packet present                               |
| `inner_truncated_ok`          | accepted with `inner_truncated = true` and a usable 5-tuple        |
| `outer_qinq_unsupported`      | dropped: stacked VLAN on outer                                    |
| `outer_ethertype_unsupported` | dropped: outer EtherType ∉ {`0x0800`, `0x86DD`, `0x8100`}         |
| `outer_v6_ext_unsupported`    | dropped: outer IPv6 extension header other than Hop-by-Hop / Destination Options |
| `outer_truncated`             | dropped: outer headers do not fit in the mbuf                     |
| `mbuf_not_contiguous`         | dropped: multi-segment mbuf reached the parser (contract violation) |
| `gre_unexpected_flags`        | dropped: GRE flag bit other than `S` set (includes `C`, `K`, `R`, reserved) |
| `gre_bad_version`             | dropped: GRE version field ≠ 0                                    |
| `gre_bad_proto`               | dropped: GRE protocol ≠ `0x22EB`                                  |
| `erspan_bad_version`          | dropped: ERSPAN Ver ≠ 2                                           |
| `erspan_truncated`            | dropped: ERSPAN III (or Platform Sub-Header) does not fit         |
| `inner_eth_truncated`         | dropped: inner Ethernet missing                                   |
| `inner_l3_truncated`          | dropped: inner L3 fixed header missing                            |
| `inner_double_encap_dropped`  | dropped: more than one inner encap layer requested                |

## 7. Future RoCEv2 Hook (Design Seam Only)

RoCEv2 carries the InfiniBand transport over UDP/4791. When InFMon eventually
needs RoCEv2 telemetry, the BTH (Base Transport Header, 12 B) will be parsed
**inside** the inner-decap hook (`INFMON_DECAP_ROCEV2`). The hook consumes
the inner Ethernet/IP/UDP/BTH headers and returns the **BTH payload** as the
new inner-packet view (the BTH itself is parsed for its fields but, like
other transport headers, is removed from the returned view); BTH-derived
fields are surfaced separately in the RoCEv2 record. Conceptually the view
goes from `Eth | IP | UDP | BTH | payload` to `payload`. The hook signature
in §4.6 already accommodates this — it returns a fresh `(ptr, len)` pair —
and the "single extra layer" rule means BTH peeling consumes that one
allowance.

Out of scope for this spec (will be a future spec):

- BTH semantics: opcode tables, PSN tracking, AETH/RETH parsing.
- ICRC validation.
- Per-QP state.

The seam exists so that adding RoCEv2 later is **purely additive**: a new enum
value, a new decap function, a new spec — no changes to the ERSPAN code path.

## 8. Test Plan

The implementation PR (separate from this spec PR) MUST include at minimum:

### 8.1 Golden PCAPs

Stored under `tests/e2e/scenarios/` (each scenario directory contains an `input.pcap`):

- `erspan3_full/input.pcap` — full inner packet, no truncation.
- `erspan3_with_seq/input.pcap` — GRE `S` flag set.
- `erspan3_o_bit/input.pcap` — Platform-Specific Sub-Header present.
- `erspan3_o_bit_truncated/input.pcap` — `O = 1` with the mbuf truncated **inside**
  the 8-byte Platform-Specific Sub-Header (key bounds-check case for §5.1).
- `erspan3_ipv6_full/input.pcap` — IPv6 outer transport, full inner packet.
- `erspan3_ipv6_trunc128/input.pcap` — IPv6 outer with BF-3-style 128 B snap.
- `erspan3_trunc128/input.pcap` — BF-3-style 128 B snap (IPv4 outer).
- `erspan3_trunc_outer/input.pcap` — outer-header truncation (must drop).
- `erspan3_bad_version/input.pcap` — Ver=1 (must drop).
- `erspan3_qinq/input.pcap` — outer QinQ (must drop).
- `erspan3_gre_keyed/input.pcap` — GRE K flag set (must drop).

### 8.2 Unit Tests (gtest)

- For every counter in §6, assert it increments under exactly the conditions
  listed and under no others.
- Assert `inner_ptr` lies inside the original mbuf (no copy was made).
- **Negative test:** assert the ERSPAN Session ID does NOT appear in any
  emitted record or struct passed downstream.
- Assert the inner-decap hook is invoked at most once per packet.
- Assert that truncation in §5.2 produces `unknown` (not zero) for missing
  TCP/UDP fields.

### 8.3 Fuzz Target

A libFuzzer harness over the entire parser, default max input length 2048 B
(configurable via the harness `LLVMFuzzerCustomMutator` / size arg so the
fuzzer can exercise both the BF-3 ~128 B snap regime *and* full-MTU jumbo
mirrors that exceed the snap), run in CI for a bounded budget per PR.
Asserts no out-of-bounds read and no infinite loop. Crashes are saved as new
corpus entries.

### 8.4 Out of CI

End-to-end real-packet replay lives in `tests/` and is **not** gated by CI
per the EPIC (DPU-4).

## 9. Open Questions

1. Do we need a CLI / config knob to *enable* exposure of the ERSPAN session
   ID for debugging? Current spec says no — revisit only if operators ask.
2. Should `inner_truncated_ok` records be tagged with the snap length the
   mirror was configured with? Likely yes once the control plane lands;
   leaving out of v1.
3. GENEVE option TLVs — when GENEVE decap is added, do we surface options or
   skip them? Defer to the GENEVE spec.

## 10. Acceptance

This spec is **accepted** when merged to `main`. The implementation PR
(`feat/erspan-parser`) MUST cite this file in its description and update §9
if any open question is resolved during implementation. The reviewer
(@banidoru) signs off on both the spec and the implementation PRs separately.
