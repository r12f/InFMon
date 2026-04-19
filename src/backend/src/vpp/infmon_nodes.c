/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * VPP graph node registration and dual-loop implementations.
 *
 * This file requires VPP headers and is compiled only as part of the
 * VPP plugin build (not the standalone unit-test build).
 *
 * See specs/004-backend-architecture.md §4, §9.
 *
 * Node chain:
 *   dpdk-input → infmon-erspan-decap → infmon-flow-match → infmon-counter → drop
 */

#ifdef INFMON_VPP_BUILD

#include <vlib/vlib.h>
#include <vnet/vnet.h>
#include <vnet/pg/pg.h>

#include "infmon/graph_node.h"

/* ── Per-worker thread-local scratch ─────────────────────────────── */

static __thread infmon_scratch_t infmon_tls_scratch;
static __thread uint8_t infmon_tls_key_buf[INFMON_KEY_BUF_MAX];

/* ── Shared plugin state (set by control plane) ──────────────────── */

typedef struct {
    /* Flow rules — loaded once per frame with ACQUIRE. */
    const infmon_flow_rule_t *flow_rules;
    uint32_t flow_rule_count;

    /* Counter tables — one per flow_rule_index.
     * Loaded once per frame with ACQUIRE for atomic swap support. */
    infmon_counter_table_t *tables[INFMON_MAX_ACTIVE_FLOW_RULES];

    /* Tick counter for LRU tracking (bumped once per frame). */
    uint64_t tick;
} infmon_plugin_main_t;

extern infmon_plugin_main_t infmon_plugin_main;

/* ════════════════════════════════════════════════════════════════════
 *  Node 1: infmon-erspan-decap
 * ════════════════════════════════════════════════════════════════════ */

typedef struct {
    infmon_parse_result_t result;
    uint32_t inner_offset;
    uint32_t inner_len;
} infmon_erspan_decap_trace_t;

static u8 *
format_infmon_erspan_decap_trace(u8 *s, va_list *args)
{
    CLIB_UNUSED(vlib_main_t * vm) = va_arg(*args, vlib_main_t *);
    CLIB_UNUSED(vlib_node_t * node) = va_arg(*args, vlib_node_t *);
    infmon_erspan_decap_trace_t *t = va_arg(*args, infmon_erspan_decap_trace_t *);

    s = format(s, "infmon-erspan-decap: result=%d inner_offset=%u inner_len=%u",
               t->result, t->inner_offset, t->inner_len);
    return s;
}

static uword
infmon_erspan_decap_node_fn(vlib_main_t *vm, vlib_node_runtime_t *node,
                            vlib_frame_t *frame)
{
    u32 n_left = frame->n_vectors;
    u32 *from = vlib_frame_vector_args(frame);
    u32 *to_next_match, *to_next_drop;
    u32 n_match = 0, n_drop = 0;
    u16 next_match = INFMON_ERSPAN_DECAP_NEXT_FLOW_MATCH;
    u16 next_drop = INFMON_ERSPAN_DECAP_NEXT_DROP;

    vlib_get_next_frame(vm, node, next_match, to_next_match, n_match);
    vlib_get_next_frame(vm, node, next_drop, to_next_drop, n_drop);

    /* ── Dual loop ──────────────────────────────────────────────── */
    while (n_left >= 4) {
        /* Prefetch +2, +3 */
        vlib_prefetch_buffer_header(vlib_get_buffer(vm, from[2]), LOAD);
        vlib_prefetch_buffer_header(vlib_get_buffer(vm, from[3]), LOAD);

        for (int i = 0; i < 2; i++) {
            u32 bi = from[i];
            vlib_buffer_t *b = vlib_get_buffer(vm, bi);
            const uint8_t *data = vlib_buffer_get_current(b);
            uint32_t len = b->current_length;

            infmon_decap_result_t dr;
            infmon_parse_result_t rc = infmon_erspan_decap(data, len, &dr);

            if (rc == INFMON_PARSE_OK || rc == INFMON_PARSE_INNER_TRUNCATED_OK) {
                /* Advance buffer to inner frame */
                vlib_buffer_advance(b, (i32)dr.inner_offset);
                b->current_length = dr.inner_len;

                to_next_match[0] = bi;
                to_next_match++;
                n_match++;
            } else {
                /* Map parse error to node error counter */
                infmon_node_error_t err;
                if (rc == INFMON_PARSE_ERR_GRE_BAD_PROTO ||
                    rc == INFMON_PARSE_ERR_ERSPAN_BAD_VERSION)
                    err = INFMON_NODE_ERR_ERSPAN_UNKNOWN_PROTO;
                else if (rc == INFMON_PARSE_ERR_ERSPAN_TRUNCATED ||
                         rc == INFMON_PARSE_ERR_OUTER_TRUNCATED)
                    err = INFMON_NODE_ERR_ERSPAN_TRUNCATED;
                else
                    err = INFMON_NODE_ERR_INNER_PARSE_FAILED;

                node->errors[err]++;

                to_next_drop[0] = bi;
                to_next_drop++;
                n_drop++;
            }

            if (PREDICT_FALSE(b->flags & VLIB_BUFFER_IS_TRACED)) {
                infmon_erspan_decap_trace_t *t =
                    vlib_add_trace(vm, node, b, sizeof(*t));
                t->result = rc;
                t->inner_offset = dr.inner_offset;
                t->inner_len = dr.inner_len;
            }
        }

        from += 2;
        n_left -= 2;
    }

    /* ── Single loop remainder ───────────────────────────────────── */
    while (n_left > 0) {
        u32 bi = from[0];
        vlib_buffer_t *b = vlib_get_buffer(vm, bi);
        const uint8_t *data = vlib_buffer_get_current(b);
        uint32_t len = b->current_length;

        infmon_decap_result_t dr;
        infmon_parse_result_t rc = infmon_erspan_decap(data, len, &dr);

        if (rc == INFMON_PARSE_OK || rc == INFMON_PARSE_INNER_TRUNCATED_OK) {
            vlib_buffer_advance(b, (i32)dr.inner_offset);
            b->current_length = dr.inner_len;
            to_next_match[0] = bi;
            to_next_match++;
            n_match++;
        } else {
            infmon_node_error_t err;
            if (rc == INFMON_PARSE_ERR_GRE_BAD_PROTO ||
                rc == INFMON_PARSE_ERR_ERSPAN_BAD_VERSION)
                err = INFMON_NODE_ERR_ERSPAN_UNKNOWN_PROTO;
            else if (rc == INFMON_PARSE_ERR_ERSPAN_TRUNCATED ||
                     rc == INFMON_PARSE_ERR_OUTER_TRUNCATED)
                err = INFMON_NODE_ERR_ERSPAN_TRUNCATED;
            else
                err = INFMON_NODE_ERR_INNER_PARSE_FAILED;
            node->errors[err]++;
            to_next_drop[0] = bi;
            to_next_drop++;
            n_drop++;
        }

        if (PREDICT_FALSE(b->flags & VLIB_BUFFER_IS_TRACED)) {
            infmon_erspan_decap_trace_t *t =
                vlib_add_trace(vm, node, b, sizeof(*t));
            t->result = rc;
            t->inner_offset = dr.inner_offset;
            t->inner_len = dr.inner_len;
        }

        from++;
        n_left--;
    }

    vlib_put_next_frame(vm, node, next_match, n_match);
    vlib_put_next_frame(vm, node, next_drop, n_drop);

    return frame->n_vectors;
}

VLIB_REGISTER_NODE(infmon_erspan_decap_node) = {
    .function = infmon_erspan_decap_node_fn,
    .name = "infmon-erspan-decap",
    .vector_size = sizeof(u32),
    .format_trace = format_infmon_erspan_decap_trace,
    .type = VLIB_NODE_TYPE_INTERNAL,
    .n_errors = INFMON_NODE_ERR__COUNT,
    .error_strings = infmon_node_error_strings,
    .n_next_nodes = INFMON_ERSPAN_DECAP_NEXT__COUNT,
    .next_nodes = {
        [INFMON_ERSPAN_DECAP_NEXT_FLOW_MATCH] = "infmon-flow-match",
        [INFMON_ERSPAN_DECAP_NEXT_DROP] = "drop",
    },
};

/* ════════════════════════════════════════════════════════════════════
 *  Node 2: infmon-flow-match
 * ════════════════════════════════════════════════════════════════════ */

static uword
infmon_flow_match_node_fn(vlib_main_t *vm, vlib_node_runtime_t *node,
                          vlib_frame_t *frame)
{
    infmon_plugin_main_t *pm = &infmon_plugin_main;
    u32 n_left = frame->n_vectors;
    u32 *from = vlib_frame_vector_args(frame);

    /* Load flow rules with ACQUIRE once per frame (§8) */
    const infmon_flow_rule_t *rules =
        __atomic_load_n(&pm->flow_rules, __ATOMIC_ACQUIRE);
    uint32_t rule_count =
        __atomic_load_n(&pm->flow_rule_count, __ATOMIC_ACQUIRE);

    infmon_scratch_reset(&infmon_tls_scratch);

    u32 *to_next_counter, *to_next_drop;
    u32 n_counter = 0, n_drop = 0;

    vlib_get_next_frame(vm, node, INFMON_FLOW_MATCH_NEXT_COUNTER,
                        to_next_counter, n_counter);
    vlib_get_next_frame(vm, node, INFMON_FLOW_MATCH_NEXT_DROP,
                        to_next_drop, n_drop);

    while (n_left > 0) {
        u32 bi = from[0];
        vlib_buffer_t *b = vlib_get_buffer(vm, bi);
        const uint8_t *inner = vlib_buffer_get_current(b);
        uint32_t inner_len = b->current_length;

        infmon_flow_fields_t fields;
        bool extracted = infmon_extract_flow_fields(NULL, inner, inner_len, &fields);

        uint32_t matches = 0;
        if (extracted && rules && rule_count > 0) {
            matches = infmon_flow_match(rules, rule_count, &fields,
                                        &infmon_tls_scratch, infmon_tls_key_buf);
        }

        if (matches > 0) {
            to_next_counter[0] = bi;
            to_next_counter++;
            n_counter++;
        } else {
            node->errors[INFMON_NODE_ERR_FLOW_RULE_NO_MATCH]++;
            to_next_drop[0] = bi;
            to_next_drop++;
            n_drop++;
        }

        from++;
        n_left--;
    }

    vlib_put_next_frame(vm, node, INFMON_FLOW_MATCH_NEXT_COUNTER, n_counter);
    vlib_put_next_frame(vm, node, INFMON_FLOW_MATCH_NEXT_DROP, n_drop);

    return frame->n_vectors;
}

VLIB_REGISTER_NODE(infmon_flow_match_node) = {
    .function = infmon_flow_match_node_fn,
    .name = "infmon-flow-match",
    .vector_size = sizeof(u32),
    .type = VLIB_NODE_TYPE_INTERNAL,
    .n_errors = INFMON_NODE_ERR__COUNT,
    .error_strings = infmon_node_error_strings,
    .n_next_nodes = INFMON_FLOW_MATCH_NEXT__COUNT,
    .next_nodes = {
        [INFMON_FLOW_MATCH_NEXT_COUNTER] = "infmon-counter",
        [INFMON_FLOW_MATCH_NEXT_DROP] = "drop",
    },
};

/* ════════════════════════════════════════════════════════════════════
 *  Node 3: infmon-counter
 * ════════════════════════════════════════════════════════════════════ */

static uword
infmon_counter_node_fn(vlib_main_t *vm, vlib_node_runtime_t *node,
                       vlib_frame_t *frame)
{
    infmon_plugin_main_t *pm = &infmon_plugin_main;
    u32 n_left = frame->n_vectors;
    u32 *from = vlib_frame_vector_args(frame);

    /* Load table pointers with ACQUIRE once per frame (§8) */
    infmon_counter_table_t *tables[INFMON_MAX_ACTIVE_FLOW_RULES];
    for (uint32_t i = 0; i < INFMON_MAX_ACTIVE_FLOW_RULES; i++)
        tables[i] = __atomic_load_n(&pm->tables[i], __ATOMIC_ACQUIRE);

    /* Bump tick once per frame */
    uint64_t tick = __atomic_fetch_add(&pm->tick, 1, __ATOMIC_RELAXED);

    uint64_t insert_retry = 0;
    uint64_t table_full = 0;

    /* All packets go to drop (InFMon never forwards) */
    u32 *to_next;
    u32 n_next = 0;
    vlib_get_next_frame(vm, node, INFMON_COUNTER_NEXT_DROP, to_next, n_next);

    /* Walk scratch vector for counter updates */
    /* Note: pkt_bytes should be per-packet, but for batch efficiency
     * we process the entire scratch in one call. The scratch was filled
     * by flow-match for all packets in this frame. For correct per-packet
     * byte counts, the VPP integration would need to store pkt_bytes
     * in the scratch entry or pass it separately. For v1, use average. */
    while (n_left > 0) {
        u32 bi = from[0];
        vlib_buffer_t *b = vlib_get_buffer(vm, bi);

        /* Each packet's byte contribution */
        uint64_t pkt_bytes = vlib_buffer_length_in_chain(vm, b);

        /* Update counters for this packet's scratch entries */
        infmon_counter_update(&infmon_tls_scratch, tables, pkt_bytes, tick,
                              &insert_retry, &table_full);

        to_next[0] = bi;
        to_next++;
        n_next++;
        from++;
        n_left--;
    }

    node->errors[INFMON_NODE_ERR_COUNTER_INSERT_RETRY_EXHAUSTED] += insert_retry;
    node->errors[INFMON_NODE_ERR_COUNTER_TABLE_FULL] += table_full;

    /* Reset scratch for next frame */
    infmon_scratch_reset(&infmon_tls_scratch);

    vlib_put_next_frame(vm, node, INFMON_COUNTER_NEXT_DROP, n_next);

    return frame->n_vectors;
}

VLIB_REGISTER_NODE(infmon_counter_node) = {
    .function = infmon_counter_node_fn,
    .name = "infmon-counter",
    .vector_size = sizeof(u32),
    .type = VLIB_NODE_TYPE_INTERNAL,
    .n_errors = INFMON_NODE_ERR__COUNT,
    .error_strings = infmon_node_error_strings,
    .n_next_nodes = INFMON_COUNTER_NEXT__COUNT,
    .next_nodes = {
        [INFMON_COUNTER_NEXT_DROP] = "drop",
    },
};

#endif /* INFMON_VPP_BUILD */
