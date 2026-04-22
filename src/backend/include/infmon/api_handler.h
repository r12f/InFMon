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
#include "infmon/graph_node.h"
#include "infmon/snapshot.h"
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
    INFMON_API_ERR_ALLOC_FAILED,
    INFMON_API_ERR_TOO_MANY_RETIRED,
    INFMON_API_ERR_NULL_TABLE,
    INFMON_API_ERR_NO_SNAPSHOT_MGR,
} infmon_api_result_t;

/* ── Context (caller-owned, long-lived) ──────────────────────────── */

typedef struct {
    infmon_flow_rule_set_t *rule_set;
    infmon_stats_registry_t *stats_reg;
    infmon_snapshot_mgr_t *snap_mgr; /**< Optional; required for snapshot_and_clear. */
    /* private: Per-rule counter tables, indexed by rule position in the set.
     * Managed internally by add/del; caller must not touch. */
    infmon_counter_table_t *tables[INFMON_FLOW_RULE_SET_MAX];
    /* Per-rule UUID, indexed in parallel with tables[]. */
    infmon_flow_rule_id_t flow_rule_ids[INFMON_FLOW_RULE_SET_MAX];
    /* Per-worker status counters (set by caller, read by infmon_api_status). */
    infmon_worker_counters_t *worker_counters; /**< Array of worker_count elements. */
    uint32_t worker_count;                     /**< Number of workers. */
} infmon_api_ctx_t;

/* ── Snapshot reply ──────────────────────────────────────────────── */

/**
 * Result of infmon_api_snapshot_and_clear().  On success, the descriptor
 * is fully populated with retired table metadata.
 */
typedef struct {
    infmon_api_result_t result;
    infmon_stats_descriptor_t descriptor;            /**< Populated on success. */
    infmon_counter_table_t *retired_table; /**< Direct pointer to the retired table (VPP-internal). */
} infmon_api_snap_reply_t;

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
 * Add a flow rule with an explicit UUID.
 * Same as infmon_api_flow_rule_add but also records @p id so that
 * snapshot_and_clear can resolve the ID → index mapping.
 */
infmon_api_result_t infmon_api_flow_rule_add_with_id(infmon_api_ctx_t *ctx,
                                                     const infmon_flow_rule_t *rule,
                                                     infmon_flow_rule_id_t id);

/**
 * Delete a flow rule by name: unpublish stats → destroy counter table →
 * remove from the rule set.
 */
infmon_api_result_t infmon_api_flow_rule_del(infmon_api_ctx_t *ctx, const char *name);

/**
 * Callback invoked once per active flow rule during infmon_api_flow_rule_list().
 * @p rule  points into the rule set (valid until the next add/rm operation).
 * @p index is the rule's position in the set (dense, 0..count-1).
 * @p user  is the opaque pointer passed to infmon_api_flow_rule_list().
 */
typedef void (*infmon_api_flow_rule_list_cb_t)(const infmon_flow_rule_t *rule, uint32_t index,
                                               void *user);

/**
 * Enumerate all active flow rules, invoking @p cb once per rule.
 * The rule set must not be modified during iteration (no re-entrant
 * add/del from within the callback).
 * @p cb may be NULL (no-op, returns INFMON_API_OK immediately).
 * Returns INFMON_API_OK on success, or an error if @p ctx is NULL.
 */
infmon_api_result_t infmon_api_flow_rule_list(const infmon_api_ctx_t *ctx,
                                              infmon_api_flow_rule_list_cb_t cb, void *user);

/**
 * Retrieve a single flow rule by name.
 * On success, *out_rule points into the rule set (valid until next add/rm)
 * and *out_index holds the rule's position.
 *
 * @return INFMON_API_OK on success.
 * @return INFMON_API_ERR_INVALID_RULE if @p ctx or @p name is NULL.
 * @return INFMON_API_ERR_NOT_FOUND if no rule with @p name exists.
 */
infmon_api_result_t infmon_api_flow_rule_get_by_name(const infmon_api_ctx_t *ctx, const char *name,
                                                     const infmon_flow_rule_t **out_rule,
                                                     uint32_t *out_index);

/**
 * Perform an atomic snapshot_and_clear on the counter table belonging
 * to the flow rule identified by @p flow_rule_id.
 *
 * On success the reply's descriptor is fully populated with the
 * retired table's metadata (generation, offsets, counters).
 *
 * Requires ctx->snap_mgr and ctx->stats_reg to be set.
 * @p reply must be non-NULL; if NULL, returns INFMON_API_ERR_INTERNAL.
 * @p flow_rule_id must be non-zero (zero is a sentinel for "no ID assigned").
 *
 * @return INFMON_API_OK on success, or an error code.  The same code is also
 *         stored in reply->result for callers that prefer struct-based checking.
 */
infmon_api_result_t infmon_api_snapshot_and_clear(infmon_api_ctx_t *ctx,
                                                  infmon_flow_rule_id_t flow_rule_id,
                                                  infmon_api_snap_reply_t *reply);

/* ── Status ─────────────────────────────────────────────────────── */

/**
 * Result of infmon_api_status().
 *
 * On success, @p workers points to the internal worker_counters array
 * (valid as long as the context is alive) and @p worker_count is set.
 * The caller must NOT free the workers pointer.
 */
typedef struct {
    infmon_api_result_t result;
    const infmon_worker_counters_t *workers; /**< Points into ctx->worker_counters. */
    uint32_t worker_count;
} infmon_api_status_reply_t;

/**
 * Retrieve per-worker error/health counters.
 *
 * Populates @p reply with a pointer to the internal worker_counters
 * array and the worker count.  The counters are a live view — they
 * may continue to be updated by worker threads after this call returns.
 * The caller sees the latest values but does not get a point-in-time
 * snapshot; use memcpy on the returned array if a frozen copy is needed.
 *
 * @return INFMON_API_OK on success.
 * @return INFMON_API_ERR_INTERNAL if @p ctx, @p reply, or worker_counters is NULL.
 */
infmon_api_result_t infmon_api_status(const infmon_api_ctx_t *ctx,
                                      infmon_api_status_reply_t *reply);

#ifdef __cplusplus
}
#endif

#endif /* INFMON_API_HANDLER_H */
