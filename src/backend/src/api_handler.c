/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 */

#include "infmon/api_handler.h"

#include <string.h>

#include "infmon/snapshot.h" /* INFMON_MAX_WORKERS */

/* ── Helpers ─────────────────────────────────────────────────────── */

/** Map flow-rule CRUD result to API result. */
static infmon_api_result_t map_rule_result(infmon_flow_rule_result_t r)
{
    switch (r) {
    case INFMON_FLOW_RULE_OK:
        return INFMON_API_OK;
    case INFMON_FLOW_RULE_ERR_NAME_EXISTS:
        return INFMON_API_ERR_NAME_EXISTS;
    case INFMON_FLOW_RULE_ERR_NOT_FOUND:
        return INFMON_API_ERR_NOT_FOUND;
    case INFMON_FLOW_RULE_ERR_INVALID_SPEC:
        return INFMON_API_ERR_INVALID_RULE;
    case INFMON_FLOW_RULE_ERR_BUDGET_EXCEEDED:
        return INFMON_API_ERR_BUDGET_EXCEEDED;
    case INFMON_FLOW_RULE_ERR_SET_FULL:
        return INFMON_API_ERR_SET_FULL;
    default:
        return INFMON_API_ERR_INTERNAL;
    }
}

/** Map snapshot result to API result. */
static infmon_api_result_t map_snap_result(infmon_snap_result_t r)
{
    switch (r) {
    case INFMON_SNAP_OK:
        return INFMON_API_OK;
    case INFMON_SNAP_ALLOC_FAILED:
        return INFMON_API_ERR_ALLOC_FAILED;
    case INFMON_SNAP_TOO_MANY_RETIRED:
        return INFMON_API_ERR_TOO_MANY_RETIRED;
    case INFMON_SNAP_INVALID_INDEX:
        return INFMON_API_ERR_NOT_FOUND;
    case INFMON_SNAP_NULL_TABLE:
        return INFMON_API_ERR_NULL_TABLE;
    default:
        return INFMON_API_ERR_INTERNAL;
    }
}

/**
 * Find the index of a rule by name in the set.
 * Returns the index, or (uint32_t) -1 if not found.
 */
static uint32_t find_rule_index(const infmon_flow_rule_set_t *set, const char *name)
{
    uint32_t n = infmon_flow_rule_count(set);
    for (uint32_t i = 0; i < n; i++) {
        const infmon_flow_rule_t *r = infmon_flow_rule_get(set, i);
        if (r && strcmp(r->name, name) == 0)
            return i;
    }
    return (uint32_t) -1;
}

/**
 * Find the index of a rule by flow_rule_id in the context.
 * Returns the index, or (uint32_t) -1 if not found.
 */
static uint32_t find_rule_index_by_id(const infmon_api_ctx_t *ctx, infmon_flow_rule_id_t id)
{
    uint32_t n = infmon_flow_rule_count(ctx->rule_set);
    for (uint32_t i = 0; i < n; i++) {
        if (infmon_flow_rule_id_eq(ctx->flow_rule_ids[i], id))
            return i;
    }
    return (uint32_t) -1;
}

/* ── Lifecycle ───────────────────────────────────────────────────── */

void infmon_api_ctx_init(infmon_api_ctx_t *ctx, infmon_flow_rule_set_t *rule_set,
                         infmon_stats_registry_t *stats_reg)
{
    if (!ctx)
        return;
    memset(ctx, 0, sizeof(*ctx));
    ctx->rule_set = rule_set;
    ctx->stats_reg = stats_reg;
}

void infmon_api_ctx_destroy(infmon_api_ctx_t *ctx)
{
    if (!ctx)
        return;
    for (uint32_t w = 0; w < INFMON_MAX_WORKERS; w++) {
        for (uint32_t i = 0; i < INFMON_FLOW_RULE_SET_MAX; i++) {
            if (ctx->tables[w][i]) {
                infmon_counter_table_destroy(ctx->tables[w][i]);
                ctx->tables[w][i] = NULL;
            }
        }
    }
    memset(ctx, 0, sizeof(*ctx));
}

/* ── Operations ──────────────────────────────────────────────────── */

/**
 * Internal: add a flow rule, optionally recording its UUID.
 */
static infmon_api_result_t flow_rule_add_internal(infmon_api_ctx_t *ctx,
                                                  const infmon_flow_rule_t *rule,
                                                  const infmon_flow_rule_id_t *id)
{
    if (!ctx || !rule)
        return INFMON_API_ERR_INVALID_RULE;

    /* 1. Insert into the rule set (validates + checks budget). */
    infmon_flow_rule_result_t rr = infmon_flow_rule_add(ctx->rule_set, rule);
    if (rr != INFMON_FLOW_RULE_OK)
        return map_rule_result(rr);

    /* 2. Find the index the rule landed at. */
    uint32_t idx = find_rule_index(ctx->rule_set, rule->name);
    if (idx == (uint32_t) -1) {
        /* Shouldn't happen — rule was just added. */
        infmon_flow_rule_rm(ctx->rule_set, rule->name);
        return INFMON_API_ERR_INTERNAL;
    }

    /* 3. Retrieve the inserted rule (key_width is now computed). */
    const infmon_flow_rule_t *inserted = infmon_flow_rule_get(ctx->rule_set, idx);
    if (!inserted) {
        infmon_flow_rule_rm(ctx->rule_set, rule->name);
        return INFMON_API_ERR_INTERNAL;
    }

    /* 4. Create counter tables for each worker. */
    uint32_t nw = ctx->worker_count > 0 ? ctx->worker_count : 1;
    for (uint32_t w = 0; w < nw; w++) {
        infmon_counter_table_t *ct =
            infmon_counter_table_create(inserted->max_keys, inserted->key_width);
        if (!ct) {
            for (uint32_t cw = 0; cw < w; cw++) {
                infmon_counter_table_destroy(ctx->tables[cw][idx]);
                ctx->tables[cw][idx] = NULL;
            }
            infmon_flow_rule_rm(ctx->rule_set, rule->name);
            return INFMON_API_ERR_INTERNAL;
        }
        ctx->tables[w][idx] = ct;
    }

    /* 5. Record the flow_rule_id if provided, otherwise zero out the slot
     *    to avoid stale IDs from a previous occupant of this index. */
    if (id) {
        ctx->flow_rule_ids[idx] = *id;
    } else {
        memset(&ctx->flow_rule_ids[idx], 0, sizeof(ctx->flow_rule_ids[0]));
    }

    return INFMON_API_OK;
}

infmon_api_result_t infmon_api_flow_rule_add(infmon_api_ctx_t *ctx, const infmon_flow_rule_t *rule)
{
    return flow_rule_add_internal(ctx, rule, NULL);
}

infmon_api_result_t infmon_api_flow_rule_add_with_id(infmon_api_ctx_t *ctx,
                                                     const infmon_flow_rule_t *rule,
                                                     infmon_flow_rule_id_t id)
{
    return flow_rule_add_internal(ctx, rule, &id);
}

infmon_api_result_t infmon_api_flow_rule_del(infmon_api_ctx_t *ctx, const char *name)
{
    if (!ctx || !name)
        return INFMON_API_ERR_INVALID_RULE;

    /* 1. Find the rule index before removing. */
    uint32_t idx = find_rule_index(ctx->rule_set, name);
    if (idx == (uint32_t) -1)
        return INFMON_API_ERR_NOT_FOUND;

    /* 2. Remove from the rule set first (so on failure we still have the table). */
    infmon_flow_rule_result_t rr = infmon_flow_rule_rm(ctx->rule_set, name);
    if (rr != INFMON_FLOW_RULE_OK)
        return map_rule_result(rr);

    /* 3. Destroy the counter tables for all workers. */
    uint32_t nw = ctx->worker_count > 0 ? ctx->worker_count : 1;
    for (uint32_t w = 0; w < nw; w++) {
        if (ctx->tables[w][idx]) {
            infmon_counter_table_destroy(ctx->tables[w][idx]);
            ctx->tables[w][idx] = NULL;
        }
    }

    /* 4. Compact the tables and flow_rule_ids arrays (rm shifts entries in the set). */
    uint32_t count = infmon_flow_rule_count(ctx->rule_set);
    for (uint32_t i = idx; i < count; i++) {
        for (uint32_t w = 0; w < nw; w++)
            ctx->tables[w][i] = ctx->tables[w][i + 1];
        ctx->flow_rule_ids[i] = ctx->flow_rule_ids[i + 1];
    }
    for (uint32_t w = 0; w < nw; w++)
        ctx->tables[w][count] = NULL;
    memset(&ctx->flow_rule_ids[count], 0, sizeof(ctx->flow_rule_ids[0]));

    return INFMON_API_OK;
}

infmon_api_result_t infmon_api_flow_rule_list(const infmon_api_ctx_t *ctx,
                                              infmon_api_flow_rule_list_cb_t cb, void *user)
{
    /* ERR_INVALID_RULE is reused for null-ctx: no dedicated "invalid context"
     * code yet — the rule is the primary domain object, so this is the closest
     * semantic match.  Same applies to get_by_name with null name. */
    if (!ctx)
        return INFMON_API_ERR_INVALID_RULE;

    if (!cb)
        return INFMON_API_OK;

    /* The rule set is dense (add appends, rm compacts), so
     * infmon_flow_rule_get() should not return NULL for i < count.
     * The NULL guard is purely defensive. */
    uint32_t n = infmon_flow_rule_count(ctx->rule_set);
    for (uint32_t i = 0; i < n; i++) {
        const infmon_flow_rule_t *r = infmon_flow_rule_get(ctx->rule_set, i);
        if (r)
            cb(r, i, user);
    }
    return INFMON_API_OK;
}

infmon_api_result_t infmon_api_flow_rule_get_by_name(const infmon_api_ctx_t *ctx, const char *name,
                                                     const infmon_flow_rule_t **out_rule,
                                                     uint32_t *out_index)
{
    if (!ctx || !name)
        return INFMON_API_ERR_INVALID_RULE;

    uint32_t n = infmon_flow_rule_count(ctx->rule_set);
    for (uint32_t i = 0; i < n; i++) {
        const infmon_flow_rule_t *r = infmon_flow_rule_get(ctx->rule_set, i);
        if (r && strcmp(r->name, name) == 0) {
            if (out_rule)
                *out_rule = r;
            if (out_index)
                *out_index = i;
            return INFMON_API_OK;
        }
    }
    return INFMON_API_ERR_NOT_FOUND;
}

/* ── Snapshot and clear ──────────────────────────────────────────── */

infmon_api_result_t infmon_api_snapshot_and_clear(infmon_api_ctx_t *ctx,
                                                  infmon_flow_rule_id_t flow_rule_id,
                                                  infmon_api_snap_reply_t *reply)
{
    if (!reply)
        return INFMON_API_ERR_INTERNAL;

    memset(reply, 0, sizeof(*reply));

    if (!ctx) {
        reply->result = INFMON_API_ERR_INTERNAL;
        return INFMON_API_ERR_INTERNAL;
    }

    if (!ctx->snap_mgr) {
        reply->result = INFMON_API_ERR_NO_SNAPSHOT_MGR;
        return INFMON_API_ERR_NO_SNAPSHOT_MGR;
    }

    /* Reject zero IDs — they are a sentinel for "no ID assigned". */
    if (infmon_flow_rule_id_is_zero(flow_rule_id)) {
        reply->result = INFMON_API_ERR_NOT_FOUND;
        return INFMON_API_ERR_NOT_FOUND;
    }

    /* Resolve flow_rule_id → index. */
    uint32_t idx = find_rule_index_by_id(ctx, flow_rule_id);
    if (idx == (uint32_t) -1) {
        reply->result = INFMON_API_ERR_NOT_FOUND;
        return INFMON_API_ERR_NOT_FOUND;
    }

    /* stats_reg is required — without a valid base the offsets are meaningless.
     * Check before the swap so we never retire a table we can't describe. */
    if (!ctx->stats_reg) {
        reply->result = INFMON_API_ERR_INTERNAL;
        return INFMON_API_ERR_INTERNAL;
    }

    /* Delegate to the snapshot manager. */
    infmon_snap_reply_t snap_reply;
    memset(&snap_reply, 0, sizeof(snap_reply));
    const infmon_flow_rule_t *rule = infmon_flow_rule_get(ctx->rule_set, idx);
    if (!rule) {
        reply->result = INFMON_API_ERR_INTERNAL;
        return INFMON_API_ERR_INTERNAL;
    }
    infmon_snapshot_and_clear(ctx->snap_mgr, &ctx->tables[0][0], INFMON_FLOW_RULE_SET_MAX,
                              ctx->worker_count > 0 ? ctx->worker_count : 1, idx,
                              INFMON_FLOW_RULE_SET_MAX, rule->key_width, &snap_reply);

    reply->result = map_snap_result(snap_reply.result);

    if (snap_reply.result != INFMON_SNAP_OK)
        return reply->result;

    /* Build the descriptor by aggregating across all retired worker tables. */
    infmon_counter_table_t *retired = snap_reply.retired_tables[0];
    infmon_stats_descriptor_t *desc = &reply->descriptor;

    desc->flow_rule_id = flow_rule_id;
    desc->flow_rule_index = idx;
    desc->generation = snap_reply.retired_generation;
    desc->epoch_ns = retired->epoch_ns;

    /* Compute byte offsets relative to stats segment base. */
    uintptr_t seg_base = ctx->stats_reg->segment_base;
    desc->slots_offset = infmon_stats_offset_of(seg_base, retired->slots);
    desc->slots_len = retired->num_slots;
    desc->key_arena_offset = infmon_stats_offset_of(seg_base, retired->key_arena);
    desc->key_arena_capacity = retired->key_arena_capacity;
    desc->key_arena_used = retired->key_arena_used;

    /* Aggregate insert_failed and table_full across all workers. */
    uint64_t total_insert_failed = 0;
    uint64_t total_table_full = 0;
    for (uint32_t w = 0; w < snap_reply.num_retired; w++) {
        if (snap_reply.retired_tables[w]) {
            total_insert_failed += snap_reply.retired_tables[w]->insert_failed;
            total_table_full |= snap_reply.retired_tables[w]->table_full;
        }
    }
    desc->insert_failed = total_insert_failed;
    desc->table_full = total_table_full;
    desc->active = 1;

    /* Expose the retired tables for inline callers. */
    for (uint32_t w = 0; w < snap_reply.num_retired; w++)
        reply->retired_tables[w] = snap_reply.retired_tables[w];
    reply->num_retired = snap_reply.num_retired;

    return INFMON_API_OK;
}

/* ── Status ──────────────────────────────────────────────────────── */

infmon_api_result_t infmon_api_status(const infmon_api_ctx_t *ctx, infmon_api_status_reply_t *reply)
{
    if (!reply)
        return INFMON_API_ERR_INTERNAL;

    memset(reply, 0, sizeof(*reply));

    if (!ctx) {
        reply->result = INFMON_API_ERR_INTERNAL;
        return INFMON_API_ERR_INTERNAL;
    }

    if (!ctx->worker_counters) {
        reply->result = INFMON_API_ERR_INTERNAL;
        return INFMON_API_ERR_INTERNAL;
    }

    if (ctx->worker_count == 0) {
        reply->result = INFMON_API_ERR_INTERNAL;
        return INFMON_API_ERR_INTERNAL;
    }

    if (ctx->worker_count > INFMON_MAX_WORKERS) {
        reply->result = INFMON_API_ERR_INTERNAL;
        return INFMON_API_ERR_INTERNAL;
    }

    reply->workers = ctx->worker_counters;
    reply->worker_count = ctx->worker_count;
    reply->result = INFMON_API_OK;
    return INFMON_API_OK;
}
