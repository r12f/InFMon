/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * VPP graph node logic — see specs/004-backend-architecture.md §4, §9
 *
 * This header defines the per-worker scratch vector and the portable
 * processing functions used by the VPP graph nodes.  The functions here
 * are pure C with no VPP dependency so they can be unit-tested on any
 * platform.
 */

#ifndef INFMON_GRAPH_NODE_H
#define INFMON_GRAPH_NODE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "infmon/counter_table.h"
#include "infmon/erspan_parser.h"
#include "infmon/flow_rule.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── Constants ───────────────────────────────────────────────────── */

/** Maximum number of active flow rules per worker (v1 hard cap). */
#define INFMON_MAX_ACTIVE_FLOW_RULES 64

/** VPP frame size — maximum packets per batch. */
#define INFMON_FRAME_SIZE 256

/* ── Error counters (for VPP show errors) ────────────────────────── */

typedef enum {
    INFMON_NODE_ERR_ERSPAN_UNKNOWN_PROTO = 0,
    INFMON_NODE_ERR_ERSPAN_TRUNCATED,
    INFMON_NODE_ERR_INNER_PARSE_FAILED,
    INFMON_NODE_ERR_FLOW_RULE_NO_MATCH,
    INFMON_NODE_ERR_COUNTER_INSERT_RETRY_EXHAUSTED,
    INFMON_NODE_ERR_COUNTER_TABLE_FULL,
    INFMON_NODE_ERR__COUNT,
} infmon_node_error_t;

extern const char *infmon_node_error_names[];
extern const char *infmon_node_error_strings[];

/* ── Next-index enum for each node ───────────────────────────────── */

typedef enum {
    INFMON_ERSPAN_DECAP_NEXT_FLOW_MATCH = 0,
    INFMON_ERSPAN_DECAP_NEXT_DROP,
    INFMON_ERSPAN_DECAP_NEXT__COUNT,
} infmon_erspan_decap_next_t;

typedef enum {
    INFMON_FLOW_MATCH_NEXT_COUNTER = 0,
    INFMON_FLOW_MATCH_NEXT_DROP,
    INFMON_FLOW_MATCH_NEXT__COUNT,
} infmon_flow_match_next_t;

typedef enum {
    INFMON_COUNTER_NEXT_DROP = 0,
    INFMON_COUNTER_NEXT__COUNT,
} infmon_counter_next_t;

/* -- VPP buffer opaque (inter-node data) */

/**
 * Data stashed in vlib_buffer opaque2 by infmon-erspan-decap for
 * consumption by downstream nodes (infmon-flow-match).  Must fit in
 * VLIB_BUFFER_OPAQUE2_SIZE (64 bytes).
 */
typedef struct {
    infmon_mirror_src_ip_t mirror_src_ip;
} infmon_buffer_opaque_t;

/* -- Atomic flow-rule reference (TOCTOU-safe) */

/**
 * Packed pointer+count for single-pointer atomic swap.  The control
 * plane allocates a new instance, populates it, then does a single
 * __atomic_store_n of the pointer.  Data-plane loads it with ACQUIRE.
 */
typedef struct {
    const infmon_flow_rule_t *rules;
    uint32_t count;
} infmon_flow_rule_set_ref_t;

/* -- Shared plugin state (set by control plane) */

typedef struct {
    const infmon_flow_rule_set_ref_t *flow_rule_set;
    infmon_counter_table_t *tables[INFMON_MAX_ACTIVE_FLOW_RULES];
    uint64_t tick;
} infmon_plugin_main_t;

/* ── Key encoding buffer (per-worker, reused across packets) ─────── */

/**
 * Maximum possible key width: all 5 fields = 16+16+16+1+1 = 50 bytes.
 * Round up to 64 for alignment.
 */
#define INFMON_KEY_BUF_MAX 64

/* ── Scratch vector entry (§9) ───────────────────────────────────── */

/**
 * One (flow_rule_index, key_hash, key_blob_ptr) triple.
 * The key_data[] array stores a per-entry copy of the encoded key so that
 * each scratch entry owns its key independently (no aliasing of a shared
 * key buffer).
 */
typedef struct {
    uint32_t flow_rule_index;             /*  0: index into flow_rule vector */
    uint16_t key_len;                     /*  4: length of key blob in bytes */
    uint16_t _pad;                        /*  6: alignment padding           */
    uint64_t key_hash;                    /*  8: full 64-bit hash            */
    uint64_t pkt_bytes;                   /* per-packet byte count           */
    const uint8_t *key_ptr;               /* pointer to key blob             */
    uint8_t key_data[INFMON_KEY_BUF_MAX]; /* per-entry key copy  */
} infmon_scratch_entry_t;

/**
 * Per-worker scratch vector.  Statically sized, lives in TLS.
 * Max entries = INFMON_FRAME_SIZE * INFMON_MAX_ACTIVE_FLOW_RULES
 *             = 256 * 64 = 16384
 *
 * Sizing justification: 256 packets/frame x 64 rules = 16384 entries.
 * Each entry is ~96 bytes (with key_data + pkt_bytes), so ~1.5 MiB per worker.
 * With 4 workers that is ~5.6 MiB total TLS -- acceptable for a
 * dedicated appliance where VPP already consumes GiBs of hugepage.
 * A per-packet cap or dynamic allocation can be revisited in v2 if
 * memory-constrained deployments appear.
 */
typedef struct {
    infmon_scratch_entry_t entries[INFMON_FRAME_SIZE * INFMON_MAX_ACTIVE_FLOW_RULES];
    uint32_t count; /* number of valid entries this frame */
} infmon_scratch_t;

/* ── ERSPAN decap result (portable) ──────────────────────────────── */

typedef struct {
    infmon_parse_result_t parse_result;
    infmon_parsed_packet_t parsed;
    uint32_t inner_offset; /* byte offset from buffer start to inner frame */
    uint32_t inner_len;    /* length of inner frame */
} infmon_decap_result_t;

/**
 * Decapsulate one ERSPAN packet.
 *
 * @param data   Start of outer Ethernet frame.
 * @param len    Total buffer length.
 * @param out    Result (populated on success).
 *
 * @return  INFMON_PARSE_OK or INFMON_PARSE_INNER_TRUNCATED_OK on success;
 *          an error code otherwise.  On success, out->inner_offset and
 *          out->inner_len indicate where the inner frame begins.
 */
infmon_parse_result_t infmon_erspan_decap(const uint8_t *data, uint32_t len,
                                          infmon_decap_result_t *out);

/* ── Flow matching (portable) ────────────────────────────────────── */

/**
 * Extract normalised flow fields from a parsed packet.
 *
 * @param parsed  Parser output (from infmon_parse_erspan).
 * @param inner   Pointer to start of inner Ethernet frame.
 * @param inner_len  Length of inner frame.
 * @param out     Normalised fields output.
 *
 * @return true if extraction succeeded.
 */
bool infmon_extract_flow_fields(const infmon_parsed_packet_t *parsed, const uint8_t *inner,
                                uint32_t inner_len, infmon_flow_fields_t *out);

/**
 * Match one packet against all active flow rules, appending entries
 * to the scratch vector.
 *
 * @param rules       Array of active flow rules.
 * @param rule_count  Number of active rules.
 * @param fields      Normalised flow fields for this packet.
 * @param scratch     Per-worker scratch vector (entries appended).
 * @param key_buf     Scratch key buffer (at least INFMON_KEY_BUF_MAX bytes).
 *
 * @return Number of matches (0 = no match, packet should be dropped
 *         with flow_rule_no_match counter).
 */
uint32_t infmon_flow_match(const infmon_flow_rule_t *rules, uint32_t rule_count,
                           const infmon_flow_fields_t *fields, infmon_scratch_t *scratch,
                           uint8_t *key_buf, uint64_t pkt_bytes);

/* ── Counter update (portable) ───────────────────────────────────── */

/**
 * Walk the scratch vector and update counter tables.
 *
 * @param scratch   Per-worker scratch vector.
 * @param tables    Array of counter table pointers (indexed by flow_rule_index).
 * @param tick      Current tick for LRU tracking.
 * @param insert_retry_exhausted  Incremented for each CAS-exhausted update.
 * @param table_full_count        Incremented for each table-full update.
 */
void infmon_counter_update(const infmon_scratch_t *scratch, infmon_counter_table_t **tables, uint64_t tick, uint64_t *insert_retry_exhausted,
                           uint64_t *table_full_count);

/* ── Scratch vector helpers ──────────────────────────────────────── */

static inline void infmon_scratch_reset(infmon_scratch_t *scratch)
{
    scratch->count = 0;
}

#ifdef __cplusplus
}
#endif

#endif /* INFMON_GRAPH_NODE_H */
