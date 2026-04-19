/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * snapshot_and_clear implementation — see specs/004-backend-architecture.md §7.2
 */

#include "infmon/snapshot.h"

#include <assert.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* ── Default clock ───────────────────────────────────────────────── */

static uint64_t default_clock_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t) ts.tv_sec * 1000000000ULL + (uint64_t) ts.tv_nsec;
}

/* ── Lifecycle ───────────────────────────────────────────────────── */

void infmon_snapshot_mgr_init(infmon_snapshot_mgr_t *mgr, uint32_t num_workers, uint64_t grace_ns,
                              uint64_t (*clock_ns)(void))
{
    memset(mgr, 0, sizeof(*mgr));

    assert(num_workers <= INFMON_MAX_WORKERS && "num_workers exceeds INFMON_MAX_WORKERS");

    mgr->num_workers = num_workers;
    mgr->global_epoch = 0;
    mgr->retired_count = 0;
    mgr->grace_ns = (grace_ns > 0) ? grace_ns : INFMON_RETIRE_GRACE_NS;
    mgr->clock_ns = clock_ns ? clock_ns : default_clock_ns;
}

void infmon_snapshot_mgr_destroy(infmon_snapshot_mgr_t *mgr)
{
    if (!mgr)
        return;

    /* Free any retired tables still pending */
    for (uint32_t i = 0; i < INFMON_MAX_RETIRED; i++) {
        if (mgr->retired[i].pending && mgr->retired[i].table) {
            infmon_counter_table_destroy(mgr->retired[i].table);
            mgr->retired[i].table = NULL;
            mgr->retired[i].pending = false;
        }
    }
    mgr->retired_count = 0;
}

/* ── Quiescence check ────────────────────────────────────────────── */

bool infmon_all_workers_past(const infmon_snapshot_mgr_t *mgr, uint64_t epoch)
{
    for (uint32_t i = 0; i < mgr->num_workers; i++) {
        if (infmon_worker_epoch_read(mgr, i) <= epoch)
            return false;
    }
    return true;
}

/* ── Snapshot and clear ──────────────────────────────────────────── */

void infmon_snapshot_and_clear(infmon_snapshot_mgr_t *mgr, infmon_counter_table_t **tables,
                               uint32_t flow_rule_index, uint32_t max_flow_rules,
                               uint32_t max_key_width, infmon_snap_reply_t *reply)
{
    memset(reply, 0, sizeof(*reply));

    /* Validate index */
    if (flow_rule_index >= max_flow_rules) {
        reply->result = INFMON_SNAP_INVALID_INDEX;
        return;
    }

    /* Get the current table */
    infmon_counter_table_t *old_table = __atomic_load_n(&tables[flow_rule_index], __ATOMIC_ACQUIRE);

    if (!old_table) {
        reply->result = INFMON_SNAP_NULL_TABLE;
        return;
    }

    /* Check retired ring capacity */
    if (mgr->retired_count >= INFMON_MAX_RETIRED) {
        reply->result = INFMON_SNAP_TOO_MANY_RETIRED;
        return;
    }

    /* Allocate new empty table with same dimensions */
    infmon_counter_table_t *new_table =
        infmon_counter_table_create(old_table->num_slots, max_key_width);

    if (!new_table) {
        reply->result = INFMON_SNAP_ALLOC_FAILED;
        return;
    }

    /* Set new table metadata */
    uint64_t new_gen = old_table->generation + 1;
    new_table->generation = new_gen;

    /* Capture wall-clock once for both new table and retired entry. */
    uint64_t now = mgr->clock_ns();
    new_table->epoch_ns = now;

    /* Bump global epoch for this swap.
     * NOT thread-safe: caller must serialize (single control thread). */
    uint64_t swap_epoch = ++mgr->global_epoch;

    /*
     * Atomic pointer swap: RELEASE ensures the new table's contents
     * (zeroed slots, metadata) are visible before any worker observes
     * the new pointer.  Workers load with ACQUIRE once per frame.
     */
    __atomic_store_n(&tables[flow_rule_index], new_table, __ATOMIC_RELEASE);

    /* Enqueue old table for retirement */
    for (uint32_t i = 0; i < INFMON_MAX_RETIRED; i++) {
        if (!mgr->retired[i].pending) {
            mgr->retired[i].table = old_table;
            mgr->retired[i].swap_epoch = swap_epoch;
            mgr->retired[i].swap_timestamp_ns = now;
            mgr->retired[i].flow_rule_index = flow_rule_index;
            mgr->retired[i].pending = true;
            mgr->retired_count++;
            break;
        }
    }
    /* Should be unreachable: retired_count was checked above */
    /* (If we exit the loop without break, something is out of sync.) */

    /* Fill reply */
    reply->result = INFMON_SNAP_OK;
    reply->retired_table = old_table;
    reply->retired_generation = old_table->generation;
}

/* ── Retire poll ─────────────────────────────────────────────────── */

uint32_t infmon_retire_poll(infmon_snapshot_mgr_t *mgr)
{
    uint32_t freed = 0;
    uint64_t now = mgr->clock_ns();

    for (uint32_t i = 0; i < INFMON_MAX_RETIRED; i++) {
        if (!mgr->retired[i].pending)
            continue;

        infmon_retired_table_t *rt = &mgr->retired[i];

        /* Two conditions for safe reclamation:
         * 1. All workers have advanced past the swap epoch.
         * 2. Grace period has elapsed since the swap.
         */
        if (!infmon_all_workers_past(mgr, rt->swap_epoch))
            continue;

        if (now <= rt->swap_timestamp_ns || (now - rt->swap_timestamp_ns) < mgr->grace_ns)
            continue;

        /* Safe to free */
        infmon_counter_table_destroy(rt->table);
        rt->table = NULL;
        rt->pending = false;
        mgr->retired_count--;
        freed++;
    }

    return freed;
}
