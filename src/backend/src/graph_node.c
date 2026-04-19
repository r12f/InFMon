/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * VPP graph node logic — see specs/004-backend-architecture.md §4, §9
 *
 * Portable processing functions used by the VPP graph nodes.
 * No VPP dependency — all VPP-specific wiring lives in vpp/infmon_nodes.c.
 */

#include "infmon/graph_node.h"

#include <string.h>

/* ── Error counter metadata ──────────────────────────────────────── */

const char *infmon_node_error_names[] = {
    [INFMON_NODE_ERR_ERSPAN_UNKNOWN_PROTO] = "erspan_unknown_proto",
    [INFMON_NODE_ERR_ERSPAN_TRUNCATED] = "erspan_truncated",
    [INFMON_NODE_ERR_INNER_PARSE_FAILED] = "inner_parse_failed",
    [INFMON_NODE_ERR_FLOW_RULE_NO_MATCH] = "flow_rule_no_match",
    [INFMON_NODE_ERR_COUNTER_INSERT_RETRY_EXHAUSTED] = "counter_insert_retry_exhausted",
    [INFMON_NODE_ERR_COUNTER_TABLE_FULL] = "counter_table_full",
};

const char *infmon_node_error_strings[] = {
    [INFMON_NODE_ERR_ERSPAN_UNKNOWN_PROTO] = "Outer header parsed but ERSPAN type unrecognised",
    [INFMON_NODE_ERR_ERSPAN_TRUNCATED] = "Buffer too short for declared ERSPAN header",
    [INFMON_NODE_ERR_INNER_PARSE_FAILED] = "Inner L2/L3/L4 parse error after decap",
    [INFMON_NODE_ERR_FLOW_RULE_NO_MATCH] = "Packet matched zero flow rules",
    [INFMON_NODE_ERR_COUNTER_INSERT_RETRY_EXHAUSTED] = "CAS retries exceeded INFMON_INSERT_RETRY",
    [INFMON_NODE_ERR_COUNTER_TABLE_FULL] = "Table reached max_keys_per_flow_rule",
};

/* ── Helpers ──────────────────────────────────────────────────────── */

static inline uint16_t read_u16(const uint8_t *p)
{
    return (uint16_t) ((uint16_t) p[0] << 8 | p[1]);
}

/* ── Simple FNV-1a 64-bit hash ───────────────────────────────────── */

static uint64_t fnv1a_64(const uint8_t *data, uint32_t len)
{
    uint64_t h = 0xcbf29ce484222325ULL;
    for (uint32_t i = 0; i < len; i++) {
        h ^= data[i];
        h *= 0x100000001b3ULL;
    }
    return h;
}

/* ── ERSPAN decap ────────────────────────────────────────────────── */

infmon_parse_result_t infmon_erspan_decap(const uint8_t *data, uint32_t len,
                                          infmon_decap_result_t *out)
{
    memset(out, 0, sizeof(*out));

    infmon_parse_result_t rc = infmon_parse_erspan(data, len, &out->parsed);
    out->parse_result = rc;

    if (rc == INFMON_PARSE_OK || rc == INFMON_PARSE_INNER_TRUNCATED_OK) {
        /* Compute inner_offset as the difference between inner_ptr and data */
        out->inner_offset = (uint32_t) (out->parsed.inner_ptr - data);
        out->inner_len = out->parsed.inner_len;
    }

    return rc;
}

/* ── Flow field extraction ───────────────────────────────────────── */

bool infmon_extract_flow_fields(const infmon_parsed_packet_t *parsed, const uint8_t *inner,
                                uint32_t inner_len, infmon_flow_fields_t *out)
{
    memset(out, 0, sizeof(*out));

    if (!parsed || !inner)
        return false;

    /* Mirror source IP — always IPv4-mapped-IPv6 in the normalised struct */
    if (parsed->mirror_src_ip.family == INFMON_AF_V4) {
        /* ::ffff:x.x.x.x */
        out->mirror_src_ip[10] = 0xff;
        out->mirror_src_ip[11] = 0xff;
        memcpy(out->mirror_src_ip + 12, parsed->mirror_src_ip.addr.v4, 4);
    } else if (parsed->mirror_src_ip.family == INFMON_AF_V6) {
        memcpy(out->mirror_src_ip, parsed->mirror_src_ip.addr.v6, 16);
    } else {
        return false; /* unknown address family */
    }

    /* Inner L3 addresses and protocol */
    uint32_t inner_l3_off = 14;

    /* Handle inner VLAN */
    if (inner_len >= 16) {
        uint16_t inner_et = read_u16(inner + 12);
        if (inner_et == 0x8100) {
            inner_l3_off = 18;
        } else if (inner_et == 0x88a8) {
            /* QinQ / 802.1ad -- not supported in v1 */
            return false;
        }
    }

    if (inner_len < inner_l3_off + 1)
        return false;

    uint16_t inner_et = (inner_l3_off == 14) ? read_u16(inner + 12) : read_u16(inner + 16);

    if (inner_et == 0x0800) {
        /* IPv4 */
        if (inner_len < inner_l3_off + 20)
            return false;

        out->ip_proto = inner[inner_l3_off + 9];

        /* DSCP: top 6 bits of TOS byte (offset +1) */
        out->dscp = (inner[inner_l3_off + 1] >> 2) & 0x3F;

        /* src_ip and dst_ip as IPv4-mapped-IPv6 */
        out->src_ip[10] = 0xff;
        out->src_ip[11] = 0xff;
        memcpy(out->src_ip + 12, inner + inner_l3_off + 12, 4);

        out->dst_ip[10] = 0xff;
        out->dst_ip[11] = 0xff;
        memcpy(out->dst_ip + 12, inner + inner_l3_off + 16, 4);
    } else if (inner_et == 0x86DD) {
        /* IPv6 */
        if (inner_len < inner_l3_off + 40)
            return false;

        out->ip_proto = inner[inner_l3_off + 6];

        /* DSCP: bits 4-9 of the first 32-bit word (Traffic Class top 6 bits) */
        uint8_t tc_hi = (inner[inner_l3_off] & 0x0F) << 4;
        uint8_t tc_lo = (inner[inner_l3_off + 1] >> 4) & 0x0F;
        uint8_t tc = tc_hi | tc_lo;
        out->dscp = (tc >> 2) & 0x3F;

        memcpy(out->src_ip, inner + inner_l3_off + 8, 16);
        memcpy(out->dst_ip, inner + inner_l3_off + 24, 16);
    } else {
        /* Non-IP inner frame — can't extract flow fields */
        return false;
    }

    return true;
}

/* ── Flow matching ───────────────────────────────────────────────── */

uint32_t infmon_flow_match(const infmon_flow_rule_t *rules, uint32_t rule_count,
                           const infmon_flow_fields_t *fields, infmon_scratch_t *scratch,
                           uint8_t *key_buf)
{
    if (!rules || !fields || !scratch || !key_buf)
        return 0;

    uint32_t matches = 0;

    for (uint32_t i = 0; i < rule_count && i < INFMON_MAX_ACTIVE_FLOW_RULES; i++) {
        const infmon_flow_rule_t *rule = &rules[i];

        /* v1: no filter expression — every packet matches every rule.
         * Filter evaluation would go here in v2. */

        /* Encode key */
        infmon_flow_rule_encode_key(rule, fields, key_buf);

        /* Hash */
        uint32_t kw = rule->key_width;
        if (kw == 0)
            kw = infmon_flow_rule_key_width(rule->fields, rule->field_count);

        uint64_t hash = fnv1a_64(key_buf, kw);

        /* Append to scratch */
        if (scratch->count < INFMON_FRAME_SIZE * INFMON_MAX_ACTIVE_FLOW_RULES) {
            infmon_scratch_entry_t *e = &scratch->entries[scratch->count++];
            e->flow_rule_index = i;
            e->key_len = (uint16_t) kw;
            e->_pad = 0;
            e->key_hash = hash;
            /* Copy key into per-entry storage to avoid aliasing */
            memcpy(e->key_data, key_buf, kw);
            e->key_ptr = e->key_data;
            matches++;
        }
    }

    return matches;
}

/* ── Counter update ──────────────────────────────────────────────── */

void infmon_counter_update(const infmon_scratch_t *scratch, infmon_counter_table_t **tables,
                           uint64_t pkt_bytes, uint64_t tick, uint64_t *insert_retry_exhausted,
                           uint64_t *table_full_count)
{
    if (!scratch || !tables)
        return;

    for (uint32_t i = 0; i < scratch->count; i++) {
        const infmon_scratch_entry_t *e = &scratch->entries[i];
        infmon_counter_table_t *table = tables[e->flow_rule_index];

        if (!table)
            continue;

        if (e->key_len == 0)
            continue; /* safety: skip entries with no key width */

        bool ok = infmon_counter_table_update(table, e->key_hash, e->key_ptr, e->key_len, pkt_bytes,
                                              tick);

        if (!ok) {
            /* Distinguish: table full vs CAS exhausted.  Note: reading
             * occupied_count after the failed update is racy (another
             * worker could have changed it), but this is best-effort
             * diagnostics -- exact attribution requires returning an
             * enum from counter_table_update (tracked for v2). */
            if (table->occupied_count >= table->num_slots) {
                if (table_full_count)
                    (*table_full_count)++;
            } else {
                if (insert_retry_exhausted)
                    (*insert_retry_exhausted)++;
            }
        }
    }
}
