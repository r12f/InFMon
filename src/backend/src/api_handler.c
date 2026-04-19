/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 */

#include "infmon/api_handler.h"

#include <string.h>

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
    for (uint32_t i = 0; i < INFMON_FLOW_RULE_SET_MAX; i++) {
        if (ctx->tables[i]) {
            infmon_counter_table_destroy(ctx->tables[i]);
            ctx->tables[i] = NULL;
        }
    }
    memset(ctx, 0, sizeof(*ctx));
}

/* ── Operations ──────────────────────────────────────────────────── */

infmon_api_result_t infmon_api_flow_rule_add(infmon_api_ctx_t *ctx, const infmon_flow_rule_t *rule)
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

    /* 4. Create a counter table. */
    infmon_counter_table_t *ct =
        infmon_counter_table_create(inserted->max_keys, inserted->key_width);
    if (!ct) {
        infmon_flow_rule_rm(ctx->rule_set, rule->name);
        return INFMON_API_ERR_INTERNAL;
    }
    ctx->tables[idx] = ct;

    return INFMON_API_OK;
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

    /* 3. Destroy the counter table (rule is already gone, safe to free). */
    if (ctx->tables[idx]) {
        infmon_counter_table_destroy(ctx->tables[idx]);
        ctx->tables[idx] = NULL;
    }

    /* 4. Compact the tables array (rm shifts entries in the set). */
    uint32_t count = infmon_flow_rule_count(ctx->rule_set);
    for (uint32_t i = idx; i < count; i++)
        ctx->tables[i] = ctx->tables[i + 1];
    ctx->tables[count] = NULL;

    return INFMON_API_OK;
}
