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
    mgr->grace_ns = grace_ns; /* 0 = no grace period */
    mgr->clock_ns = clock_ns ? clock_ns : default_clock_ns;
}

void infmon_snapshot_mgr_destroy(infmon_snapshot_mgr_t *mgr)
{
    if (!mgr)
        return;

    /* Free any retired tables still pending */
    for (uint32_t i = 0; i < INFMON_MAX_RETIRED; i++) {
        if (mgr->retired[i].pending) {
            for (uint32_t w = 0; w < mgr->retired[i].num_tables; w++) {
                if (mgr->retired[i].tables[w]) {
                    infmon_counter_table_destroy(mgr->retired[i].tables[w]);
                    mgr->retired[i].tables[w] = NULL;
                }
            }
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

void infmon_snapshot_and_clear(infmon_snapshot_mgr_t *mgr, infmon_counter_table_t **tables_flat,
                               uint32_t tables_stride, uint32_t num_workers,
                               uint32_t flow_rule_index, uint32_t max_flow_rules,
                               uint32_t max_key_width, infmon_snap_reply_t *reply)
{
    memset(reply, 0, sizeof(*reply));

    /* Validate index */
    if (flow_rule_index >= max_flow_rules) {
        reply->result = INFMON_SNAP_INVALID_INDEX;
        return;
    }

    uint32_t nw = num_workers > 0 ? num_workers : 1;

    /* Get the current table from worker 0 to check existence */
    infmon_counter_table_t *old_table0 =
        __atomic_load_n(&tables_flat[0 * tables_stride + flow_rule_index], __ATOMIC_ACQUIRE);

    if (!old_table0) {
        reply->result = INFMON_SNAP_NULL_TABLE;
        return;
    }

    /* Check retired ring capacity */
    if (mgr->retired_count >= INFMON_MAX_RETIRED) {
        reply->result = INFMON_SNAP_TOO_MANY_RETIRED;
        return;
    }

    /* Allocate new tables for all workers, saving old pointers to avoid TOCTOU */
    infmon_counter_table_t *new_tables[INFMON_MAX_WORKERS] = {0};
    infmon_counter_table_t *old_tables[INFMON_MAX_WORKERS] = {0};
    for (uint32_t w = 0; w < nw; w++) {
        infmon_counter_table_t *old_w =
            __atomic_load_n(&tables_flat[w * tables_stride + flow_rule_index], __ATOMIC_ACQUIRE);
        old_tables[w] = old_w;
        if (!old_w)
            continue;
        new_tables[w] = infmon_counter_table_create(old_w->num_slots, max_key_width);
        if (!new_tables[w]) {
            /* Cleanup already allocated */
            for (uint32_t cw = 0; cw < w; cw++) {
                if (new_tables[cw]) {
                    infmon_counter_table_destroy(new_tables[cw]);
                }
            }
            reply->result = INFMON_SNAP_ALLOC_FAILED;
            return;
        }
    }

    /* Capture wall-clock once for all swaps */
    uint64_t now = mgr->clock_ns();

    /* Bump global epoch for this swap */
    uint64_t swap_epoch = ++mgr->global_epoch;

    /* Swap each worker's table (reusing old pointers from allocation pass) */
    uint64_t gen = 0;
    bool gen_set = false;
    for (uint32_t w = 0; w < nw; w++) {
        if (!old_tables[w] || !new_tables[w])
            continue;

        uint64_t new_gen = old_tables[w]->generation + 1;
        new_tables[w]->generation = new_gen;
        new_tables[w]->epoch_ns = now;
        if (!gen_set) {
            gen = old_tables[w]->generation;
            gen_set = true;
        }

        __atomic_store_n(&tables_flat[w * tables_stride + flow_rule_index], new_tables[w],
                         __ATOMIC_RELEASE);
    }

    /* Enqueue one retired entry with all per-worker tables */
    for (uint32_t i = 0; i < INFMON_MAX_RETIRED; i++) {
        if (!mgr->retired[i].pending) {
            memset(&mgr->retired[i], 0, sizeof(mgr->retired[i]));
            for (uint32_t w = 0; w < nw; w++)
                mgr->retired[i].tables[w] = old_tables[w];
            mgr->retired[i].num_tables = nw;
            mgr->retired[i].swap_epoch = swap_epoch;
            mgr->retired[i].swap_timestamp_ns = now;
            mgr->retired[i].flow_rule_index = flow_rule_index;
            mgr->retired[i].pending = true;
            mgr->retired_count++;
            break;
        }
    }

    /* Fill reply */
    reply->result = INFMON_SNAP_OK;
    for (uint32_t w = 0; w < nw; w++)
        reply->retired_tables[w] = old_tables[w];
    reply->num_retired = nw;
    reply->retired_generation = gen;
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
        for (uint32_t w = 0; w < rt->num_tables; w++) {
            if (rt->tables[w]) {
                infmon_counter_table_destroy(rt->tables[w]);
                rt->tables[w] = NULL;
            }
        }
        rt->pending = false;
        mgr->retired_count--;
        freed++;
    }

    return freed;
}
