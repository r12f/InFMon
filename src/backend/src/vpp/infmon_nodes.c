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

#include <inttypes.h>
#include <vlib/unix/plugin.h>
#include <vlib/vlib.h>
#include <vnet/buffer.h>
#include <vnet/feature/feature.h>
#include <vnet/pg/pg.h>
#include <vnet/vnet.h>

#include "infmon/counter_table.h"
#include "infmon/graph_node.h"
#include "infmon/log.h"

/* ── VPP plugin registration ─────────────────────────────────────── */

#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
VLIB_PLUGIN_REGISTER() = {
    .version = "0.1.0",
    .description = "InFMon — Infrastructure Flow Monitor",
};
#pragma GCC diagnostic pop

/* ── Feature arc registration ────────────────────────────────────── */

#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
VNET_FEATURE_INIT(infmon_erspan_decap_feat, static) = {
    .arc_name = "device-input",
    .node_name = "infmon-erspan-decap",
    .runs_before = VNET_FEATURES("ethernet-input"),
};
#pragma GCC diagnostic pop

/* ── Feature enable/disable CLI ──────────────────────────────────── */

static clib_error_t *infmon_enable_disable_command_fn(CLIB_UNUSED(vlib_main_t *vm),
                                                      unformat_input_t *input,
                                                      CLIB_UNUSED(vlib_cli_command_t *cmd))
{
    u32 sw_if_index = ~0;
    int enable = 1;

    while (unformat_check_input(input) != UNFORMAT_END_OF_INPUT) {
        if (unformat(input, "enable"))
            enable = 1;
        else if (unformat(input, "disable"))
            enable = 0;
        else if (unformat(input, "%U", unformat_vnet_sw_interface, vnet_get_main(), &sw_if_index))
            ;
        else
            return clib_error_return(0, "unknown input '%U'", format_unformat_error, input);
    }

    if (sw_if_index == (u32) ~0)
        return clib_error_return(0, "please specify an interface");

    vnet_feature_enable_disable("device-input", "infmon-erspan-decap", sw_if_index, enable, 0, 0);
    return 0;
}

#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
VLIB_CLI_COMMAND(infmon_enable_disable_command, static) = {
    .path = "infmon enable",
    .short_help = "infmon enable <interface> [disable]",
    .function = infmon_enable_disable_command_fn,
};
#pragma GCC diagnostic pop

/* ── Per-worker thread-local scratch ─────────────────────────────── */

static __thread infmon_scratch_t infmon_tls_scratch;
static __thread uint8_t infmon_tls_key_buf[INFMON_KEY_BUF_MAX];

/* ── Shared plugin state (set by control plane) ──────────────────── */

/* -- Shared plugin state (definition in graph_node.h) */
infmon_plugin_main_t infmon_plugin_main;

/* ════════════════════════════════════════════════════════════════════
 *  Node 1: infmon-erspan-decap
 * ════════════════════════════════════════════════════════════════════ */

typedef struct {
    infmon_parse_result_t result;
    uint32_t inner_offset;
    uint32_t inner_len;
} infmon_erspan_decap_trace_t;

static u8 *format_infmon_erspan_decap_trace(u8 *s, va_list *args)
{
    CLIB_UNUSED(vlib_main_t * vm) = va_arg(*args, vlib_main_t *);
    CLIB_UNUSED(vlib_node_t * node) = va_arg(*args, vlib_node_t *);
    infmon_erspan_decap_trace_t *t = va_arg(*args, infmon_erspan_decap_trace_t *);

    s = format(s, "infmon-erspan-decap: result=%d inner_offset=%u inner_len=%u", t->result,
               t->inner_offset, t->inner_len);
    return s;
}

static uword infmon_erspan_decap_node_fn(vlib_main_t *vm, vlib_node_runtime_t *node,
                                         vlib_frame_t *frame)
{
    u32 n_left = frame->n_vectors;
    u32 *from = vlib_frame_vector_args(frame);
    u32 *to_next_match, *to_next_pass;
    u32 n_left_match = 0, n_left_pass = 0;
    u16 next_match = INFMON_ERSPAN_DECAP_NEXT_FLOW_MATCH;
    u16 next_pass = INFMON_ERSPAN_DECAP_NEXT_PASSTHROUGH;

    vlib_get_next_frame(vm, node, next_match, to_next_match, n_left_match);
    vlib_get_next_frame(vm, node, next_pass, to_next_pass, n_left_pass);

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
                vlib_buffer_advance(b, (i32) dr.inner_offset);
                b->current_length = dr.inner_len;
                b->flags &= ~VLIB_BUFFER_NEXT_PRESENT;
                infmon_buffer_opaque_t *op = (infmon_buffer_opaque_t *) b->opaque2;
                op->mirror_src_ip = dr.parsed.mirror_src_ip;

                to_next_match[0] = bi;
                to_next_match++;
                n_left_match--;
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

                /* Non-ERSPAN: pass through to normal pipeline */
                to_next_pass[0] = bi;
                to_next_pass++;
                n_left_pass--;
            }

            if (PREDICT_FALSE(b->flags & VLIB_BUFFER_IS_TRACED)) {
                infmon_erspan_decap_trace_t *t = vlib_add_trace(vm, node, b, sizeof(*t));
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
            vlib_buffer_advance(b, (i32) dr.inner_offset);
            b->current_length = dr.inner_len;
            b->flags &= ~VLIB_BUFFER_NEXT_PRESENT;

            /* Stash mirror_src_ip in buffer opaque for flow-match node */
            infmon_buffer_opaque_t *op = (infmon_buffer_opaque_t *) b->opaque2;
            op->mirror_src_ip = dr.parsed.mirror_src_ip;

            to_next_match[0] = bi;
            to_next_match++;
            n_left_match--;
        } else {
            /* Map parse error to node error counter */
            infmon_node_error_t err;
            if (rc == INFMON_PARSE_ERR_GRE_BAD_PROTO || rc == INFMON_PARSE_ERR_ERSPAN_BAD_VERSION)
                err = INFMON_NODE_ERR_ERSPAN_UNKNOWN_PROTO;
            else if (rc == INFMON_PARSE_ERR_ERSPAN_TRUNCATED ||
                     rc == INFMON_PARSE_ERR_OUTER_TRUNCATED)
                err = INFMON_NODE_ERR_ERSPAN_TRUNCATED;
            else
                err = INFMON_NODE_ERR_INNER_PARSE_FAILED;

            node->errors[err]++;

            /* Non-ERSPAN: pass through to normal pipeline */
            to_next_pass[0] = bi;
            to_next_pass++;
            n_left_pass--;
        }

        if (PREDICT_FALSE(b->flags & VLIB_BUFFER_IS_TRACED)) {
            infmon_erspan_decap_trace_t *t = vlib_add_trace(vm, node, b, sizeof(*t));
            t->result = rc;
            t->inner_offset = dr.inner_offset;
            t->inner_len = dr.inner_len;
        }

        from++;
        n_left--;
    }

    vlib_put_next_frame(vm, node, next_match, n_left_match);
    vlib_put_next_frame(vm, node, next_pass, n_left_pass);

    return frame->n_vectors;
}

#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
VLIB_REGISTER_NODE(infmon_erspan_decap_node) = {
    .function = infmon_erspan_decap_node_fn,
    .name = "infmon-erspan-decap",
    .vector_size = sizeof(u32),
    .format_trace = format_infmon_erspan_decap_trace,
    .type = VLIB_NODE_TYPE_INTERNAL,
    .n_errors = INFMON_NODE_ERR__COUNT, /* shared enum; unused counters read zero */
    .error_strings = (char **) infmon_node_error_strings, /* per-node subsets deferred to v2 */
    .n_next_nodes = INFMON_ERSPAN_DECAP_NEXT__COUNT,
    .next_nodes =
        {
            [INFMON_ERSPAN_DECAP_NEXT_FLOW_MATCH] = "infmon-flow-match",
            [INFMON_ERSPAN_DECAP_NEXT_DROP] = "drop",
            [INFMON_ERSPAN_DECAP_NEXT_PASSTHROUGH] = "ethernet-input",
        },
};
#pragma GCC diagnostic pop

/* ════════════════════════════════════════════════════════════════════
 *  Node 2: infmon-flow-match
 * ════════════════════════════════════════════════════════════════════ */

static uword infmon_flow_match_node_fn(vlib_main_t *vm, vlib_node_runtime_t *node,
                                       vlib_frame_t *frame)
{
    infmon_plugin_main_t *pm = &infmon_plugin_main;
    u32 n_left = frame->n_vectors;
    u32 *from = vlib_frame_vector_args(frame);

    /* Load flow rules with ACQUIRE once per frame (§8) */
    const infmon_flow_rule_set_ref_t *rs = __atomic_load_n(&pm->flow_rule_set, __ATOMIC_ACQUIRE);
    const infmon_flow_rule_t *rules = rs ? rs->rules : NULL;
    uint32_t rule_count = rs ? rs->count : 0;

    infmon_scratch_reset(&infmon_tls_scratch);

    u32 *to_next_counter, *to_next_drop;
    u32 n_left_counter = 0, n_left_drop = 0;

    vlib_get_next_frame(vm, node, INFMON_FLOW_MATCH_NEXT_COUNTER, to_next_counter, n_left_counter);
    vlib_get_next_frame(vm, node, INFMON_FLOW_MATCH_NEXT_DROP, to_next_drop, n_left_drop);

    while (n_left > 0) {
        u32 bi = from[0];
        vlib_buffer_t *b = vlib_get_buffer(vm, bi);
        const uint8_t *inner = vlib_buffer_get_current(b);
        uint32_t inner_len = b->current_length;

        infmon_buffer_opaque_t *op = (infmon_buffer_opaque_t *) b->opaque2;
        infmon_parsed_packet_t parsed_pkt;
        memset(&parsed_pkt, 0, sizeof(parsed_pkt));
        parsed_pkt.mirror_src_ip = op->mirror_src_ip;

        infmon_flow_fields_t fields;
        bool extracted = infmon_extract_flow_fields(&parsed_pkt, inner, inner_len, &fields);

        uint32_t matches = 0;
        if (extracted && rules && rule_count > 0) {
            matches = infmon_flow_match(rules, rule_count, &fields, &infmon_tls_scratch,
                                        infmon_tls_key_buf, vlib_buffer_length_in_chain(vm, b));
        }

        if (matches > 0) {
            to_next_counter[0] = bi;
            to_next_counter++;
            n_left_counter--;
        } else {
            node->errors[INFMON_NODE_ERR_FLOW_RULE_NO_MATCH]++;
            to_next_drop[0] = bi;
            to_next_drop++;
            n_left_drop--;
        }

        from++;
        n_left--;
    }

    vlib_put_next_frame(vm, node, INFMON_FLOW_MATCH_NEXT_COUNTER, n_left_counter);
    vlib_put_next_frame(vm, node, INFMON_FLOW_MATCH_NEXT_DROP, n_left_drop);

    return frame->n_vectors;
}

#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
VLIB_REGISTER_NODE(infmon_flow_match_node) = {
    .function = infmon_flow_match_node_fn,
    .name = "infmon-flow-match",
    .vector_size = sizeof(u32),
    .type = VLIB_NODE_TYPE_INTERNAL,
    .n_errors = INFMON_NODE_ERR__COUNT,
    .error_strings = (char **) infmon_node_error_strings,
    .n_next_nodes = INFMON_FLOW_MATCH_NEXT__COUNT,
    .next_nodes =
        {
            [INFMON_FLOW_MATCH_NEXT_COUNTER] = "infmon-counter",
            [INFMON_FLOW_MATCH_NEXT_DROP] = "drop",
        },
};
#pragma GCC diagnostic pop

/* ════════════════════════════════════════════════════════════════════
 *  Node 3: infmon-counter
 * ════════════════════════════════════════════════════════════════════ */

static uword infmon_counter_node_fn(vlib_main_t *vm, vlib_node_runtime_t *node, vlib_frame_t *frame)
{
    infmon_plugin_main_t *pm = &infmon_plugin_main;
    u32 n_left = frame->n_vectors;
    u32 *from = vlib_frame_vector_args(frame);

    /* Load table pointers with ACQUIRE once per frame (§8) — per-worker */
    u32 worker_id = vlib_get_thread_index();
    if (PREDICT_FALSE(worker_id >= INFMON_MAX_WORKERS)) {
        vlib_node_increment_counter(vm, node->node_index, INFMON_NODE_ERR_COUNTER_TABLE_FULL,
                                    frame->n_vectors);
        vlib_buffer_free(vm, vlib_frame_vector_args(frame), frame->n_vectors);
        return frame->n_vectors;
    }
    infmon_counter_table_t *tables[INFMON_MAX_ACTIVE_FLOW_RULES];
    for (uint32_t i = 0; i < INFMON_MAX_ACTIVE_FLOW_RULES; i++)
        tables[i] = __atomic_load_n(&pm->tables[worker_id][i], __ATOMIC_ACQUIRE);

    /* Bump tick once per frame */
    uint64_t tick = __atomic_fetch_add(&pm->tick, 1, __ATOMIC_RELAXED);

    uint64_t insert_retry = 0;
    uint64_t table_full = 0;

    /* All packets go to drop (InFMon never forwards) */
    u32 *to_next;
    u32 n_left_next = 0;
    vlib_get_next_frame(vm, node, INFMON_COUNTER_NEXT_DROP, to_next, n_left_next);

    while (n_left > 0) {
        u32 bi = from[0];
        to_next[0] = bi;
        to_next++;
        n_left_next--;
        from++;
        n_left--;
    }

    /* Update counters once for the entire scratch vector */
    infmon_counter_update(&infmon_tls_scratch, tables, tick, &insert_retry, &table_full);

    node->errors[INFMON_NODE_ERR_COUNTER_INSERT_RETRY_EXHAUSTED] += insert_retry;
    node->errors[INFMON_NODE_ERR_COUNTER_TABLE_FULL] += table_full;

    /* Reset scratch for next frame */
    infmon_scratch_reset(&infmon_tls_scratch);

    vlib_put_next_frame(vm, node, INFMON_COUNTER_NEXT_DROP, n_left_next);

    return frame->n_vectors;
}

#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
VLIB_REGISTER_NODE(infmon_counter_node) = {
    .function = infmon_counter_node_fn,
    .name = "infmon-counter",
    .vector_size = sizeof(u32),
    .type = VLIB_NODE_TYPE_INTERNAL,
    .n_errors = INFMON_NODE_ERR__COUNT,
    .error_strings = (char **) infmon_node_error_strings,
    .n_next_nodes = INFMON_COUNTER_NEXT__COUNT,
    .next_nodes =
        {
            [INFMON_COUNTER_NEXT_DROP] = "drop",
        },
};
#pragma GCC diagnostic pop

/* ════════════════════════════════════════════════════════════════════
 *  CLI: infmon flow-rule add <name> fields <f1,f2,...> [max-keys N]
 * ════════════════════════════════════════════════════════════════════ */

static infmon_flow_rule_set_t *infmon_cli_rule_set = NULL;

/* Persistent flow_rule_set_ref used by the graph nodes.
 * Updated atomically when rules change via CLI. */
static infmon_flow_rule_set_ref_t infmon_cli_rule_set_ref;

static void infmon_publish_rules(void)
{
    infmon_plugin_main_t *pm = &infmon_plugin_main;
    uint32_t n = infmon_flow_rule_count(infmon_cli_rule_set);

    infmon_cli_rule_set_ref.rules = (n > 0) ? infmon_flow_rule_get(infmon_cli_rule_set, 0) : NULL;
    infmon_cli_rule_set_ref.count = n;

    __atomic_store_n(&pm->flow_rule_set, &infmon_cli_rule_set_ref, __ATOMIC_RELEASE);
}

static clib_error_t *infmon_flow_rule_add_command_fn(CLIB_UNUSED(vlib_main_t *vm),
                                                     unformat_input_t *input,
                                                     CLIB_UNUSED(vlib_cli_command_t *cmd))
{
    u8 *name_vec = 0;
    u8 *fields_str = 0;
    u32 max_keys = 65536;

    while (unformat_check_input(input) != UNFORMAT_END_OF_INPUT) {
        if (unformat(input, "name %s", &name_vec))
            ;
        else if (unformat(input, "fields %s", &fields_str))
            ;
        else if (unformat(input, "max-keys %u", &max_keys))
            ;
        else {
            vec_free(name_vec);
            vec_free(fields_str);
            return clib_error_return(0, "unknown input '%U'", format_unformat_error, input);
        }
    }

    if (!name_vec) {
        vec_free(fields_str);
        return clib_error_return(0, "name required");
    }
    if (!fields_str) {
        vec_free(name_vec);
        return clib_error_return(0, "fields required");
    }

    /* Parse field names */
    infmon_flow_rule_t spec;
    memset(&spec, 0, sizeof(spec));
    strncpy(spec.name, (char *) name_vec, INFMON_FLOW_RULE_NAME_MAX);
    spec.name[INFMON_FLOW_RULE_NAME_MAX - 1] = '\0';
    spec.max_keys = max_keys;
    spec.eviction_policy = INFMON_EVICTION_LRU_DROP;

    /* Tokenize comma-separated fields */
    char *saveptr = NULL;
    char *tok = strtok_r((char *) fields_str, ",", &saveptr);
    while (tok && spec.field_count < INFMON_FLOW_RULE_FIELDS_MAX) {
        if (strcmp(tok, "src_ip") == 0)
            spec.fields[spec.field_count++] = INFMON_FIELD_SRC_IP;
        else if (strcmp(tok, "dst_ip") == 0)
            spec.fields[spec.field_count++] = INFMON_FIELD_DST_IP;
        else if (strcmp(tok, "ip_proto") == 0)
            spec.fields[spec.field_count++] = INFMON_FIELD_IP_PROTO;
        else if (strcmp(tok, "dscp") == 0)
            spec.fields[spec.field_count++] = INFMON_FIELD_DSCP;
        else if (strcmp(tok, "mirror_src_ip") == 0)
            spec.fields[spec.field_count++] = INFMON_FIELD_MIRROR_SRC_IP;
        else if (strcmp(tok, "src_port") == 0)
            spec.fields[spec.field_count++] = INFMON_FIELD_SRC_PORT;
        else if (strcmp(tok, "dst_port") == 0)
            spec.fields[spec.field_count++] = INFMON_FIELD_DST_PORT;
        else {
            vec_free(name_vec);
            vec_free(fields_str);
            return clib_error_return(0, "unknown field '%s'", tok);
        }
        tok = strtok_r(NULL, ",", &saveptr);
    }

    vec_free(fields_str);

    if (spec.field_count == 0) {
        vec_free(name_vec);
        return clib_error_return(0, "no valid fields specified");
    }

    /* Lazy-init rule set */
    if (!infmon_cli_rule_set)
        infmon_cli_rule_set = infmon_flow_rule_set_create(INFMON_FLOW_RULE_MAX_KEYS_BUDGET);

    if (!infmon_cli_rule_set) {
        vec_free(name_vec);
        return clib_error_return(0, "failed to create rule set");
    }

    infmon_flow_rule_result_t rc = infmon_flow_rule_add(infmon_cli_rule_set, &spec);
    if (rc != INFMON_FLOW_RULE_OK) {
        vec_free(name_vec);
        return clib_error_return(0, "flow_rule_add failed: %d", (int) rc);
    }

    /* Find the rule index to allocate per-worker counter tables */
    uint32_t n = infmon_flow_rule_count(infmon_cli_rule_set);
    const infmon_flow_rule_t *added = infmon_flow_rule_get(infmon_cli_rule_set, n - 1);
    if (added) {
        if ((n - 1) < INFMON_MAX_ACTIVE_FLOW_RULES) {
            /* NOTE: num_workers is latched from vlib_num_workers()+1 during
             * infmon_vpp_api_ctx_ensure, which runs post-worker-init.  CLI commands
             * also execute after workers are active.  If num_workers is 0 (e.g.
             * startup-config before workers launch — not currently supported),
             * fall back to 1 so the main thread always gets a table. */
            uint32_t nw = infmon_plugin_main.num_workers > 0 ? infmon_plugin_main.num_workers : 1;
            for (uint32_t w = 0; w < nw; w++) {
                infmon_counter_table_t *ct =
                    infmon_counter_table_create(added->max_keys, added->key_width);
                if (ct) {
                    __atomic_store_n(&infmon_plugin_main.tables[w][n - 1], ct, __ATOMIC_RELEASE);
                } else {
                    /* Allocation failed — roll back tables created for workers 0..w-1 */
                    for (uint32_t rw = 0; rw < w; rw++) {
                        infmon_counter_table_t *prev = __atomic_exchange_n(
                            &infmon_plugin_main.tables[rw][n - 1], NULL, __ATOMIC_RELEASE);
                        if (prev)
                            infmon_counter_table_destroy(prev);
                    }
                    break;
                }
            }
        }
    }

    infmon_publish_rules();

    vlib_cli_output(vm, "Added flow rule '%s' (index %u, key_width %u)", (char *) name_vec, n - 1,
                    added ? added->key_width : 0);
    vec_free(name_vec);
    return 0;
}

/* NOLINTNEXTLINE(clang-diagnostic-pedantic) */
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
VLIB_CLI_COMMAND(infmon_flow_rule_add_command, static) = {
    .path = "infmon flow-rule add",
    .short_help = "infmon flow-rule add name <name> fields <f1,f2,...> [max-keys N]",
    .function = infmon_flow_rule_add_command_fn,
};
#pragma GCC diagnostic pop

/* ════════════════════════════════════════════════════════════════════
 *  CLI: infmon flow-rule show
 * ════════════════════════════════════════════════════════════════════ */

static clib_error_t *infmon_flow_rule_show_command_fn(vlib_main_t *vm,
                                                      CLIB_UNUSED(unformat_input_t *input),
                                                      CLIB_UNUSED(vlib_cli_command_t *cmd))
{
    if (!infmon_cli_rule_set) {
        vlib_cli_output(vm, "No flow rules configured (plugin)");
        return 0;
    }

    uint32_t n = infmon_flow_rule_count(infmon_cli_rule_set);
    vlib_cli_output(vm, "%u flow rule(s):", n);
    for (uint32_t i = 0; i < n; i++) {
        const infmon_flow_rule_t *r = infmon_flow_rule_get(infmon_cli_rule_set, i);
        if (r)
            vlib_cli_output(vm, "  [%u] %s  fields=%u key_width=%u max_keys=%u", i, r->name,
                            r->field_count, r->key_width, r->max_keys);
    }
    return 0;
}

/* NOLINTNEXTLINE(clang-diagnostic-pedantic) */
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
VLIB_CLI_COMMAND(infmon_flow_rule_show_command, static) = {
    .path = "infmon flow-rule show",
    .short_help = "infmon flow-rule show",
    .function = infmon_flow_rule_show_command_fn,
};
#pragma GCC diagnostic pop

#endif /* INFMON_VPP_BUILD */
