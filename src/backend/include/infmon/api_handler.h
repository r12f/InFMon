/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * API handler — orchestrates flow_rule_add / flow_rule_del across the
 * flow-rule set, counter tables, and stats registry.
 */

#ifndef INFMON_API_HANDLER_H
#define INFMON_API_HANDLER_H

#include <stdint.h>

#include "infmon/counter_table.h"
#include "infmon/flow_rule.h"
#include "infmon/stats_segment.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── Error codes ─────────────────────────────────────────────────── */

typedef enum {
    INFMON_API_OK = 0,
    INFMON_API_ERR_INVALID_RULE,
    INFMON_API_ERR_NAME_EXISTS,
    INFMON_API_ERR_NOT_FOUND,
    INFMON_API_ERR_BUDGET_EXCEEDED,
    INFMON_API_ERR_SET_FULL,
    INFMON_API_ERR_STATS_PUBLISH,
    INFMON_API_ERR_INTERNAL,
} infmon_api_result_t;

/* ── Context (caller-owned, long-lived) ──────────────────────────── */

typedef struct {
    infmon_flow_rule_set_t *rule_set;
    infmon_stats_registry_t *stats_reg;
    /* private: Per-rule counter tables, indexed by rule position in the set.
     * Managed internally by add/del; caller must not touch. */
    infmon_counter_table_t *tables[INFMON_FLOW_RULE_SET_MAX];
} infmon_api_ctx_t;

/* ── Lifecycle ───────────────────────────────────────────────────── */

/**
 * Initialise an API context.  Caller must have already created
 * @p rule_set and @p stats_reg.
 */
void infmon_api_ctx_init(infmon_api_ctx_t *ctx, infmon_flow_rule_set_t *rule_set,
                         infmon_stats_registry_t *stats_reg);

/** Tear down: destroys all counter tables owned by the context. */
void infmon_api_ctx_destroy(infmon_api_ctx_t *ctx);

/* ── Operations ──────────────────────────────────────────────────── */

/**
 * Add a flow rule: validate → insert into the rule set → create a
 * counter table → publish stats descriptor.
 */
infmon_api_result_t infmon_api_flow_rule_add(infmon_api_ctx_t *ctx, const infmon_flow_rule_t *rule);

/**
 * Delete a flow rule by name: unpublish stats → destroy counter table →
 * remove from the rule set.
 */
infmon_api_result_t infmon_api_flow_rule_del(infmon_api_ctx_t *ctx, const char *name);

#ifdef __cplusplus
}
#endif

#endif /* INFMON_API_HANDLER_H */
