/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * snapshot_and_clear — atomic table swap with epoch-based RCU retirement.
 * See specs/004-backend-architecture.md §7.2
 */

#ifndef INFMON_SNAPSHOT_H
#define INFMON_SNAPSHOT_H

#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "infmon/counter_table.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── Constants ───────────────────────────────────────────────────── */

/** Maximum number of worker threads tracked for RCU. */
#define INFMON_MAX_WORKERS 32

/** Maximum number of tables pending retirement at any time. */
#define INFMON_MAX_RETIRED 16

/** Default grace period in nanoseconds (5 seconds). */
#define INFMON_RETIRE_GRACE_NS ((uint64_t) 5000000000ULL)

/* ── Per-worker epoch counter ────────────────────────────────────── */

/**
 * Cache-line-aligned epoch counter.  Each worker bumps its own epoch
 * once per dispatch loop iteration with RELEASE ordering; the control
 * thread reads all worker epochs with ACQUIRE to detect quiescence.
 */
typedef struct {
    uint64_t epoch;
    uint8_t pad[64 - sizeof(uint64_t)]; /* pad to 64 B to avoid false sharing */
} __attribute__((aligned(64))) infmon_worker_epoch_t;

/* ── Retired table descriptor ────────────────────────────────────── */

typedef struct {
    infmon_counter_table_t *tables[INFMON_MAX_WORKERS]; /**< Retired tables, one per worker. */
    uint32_t num_tables;                                /**< Number of workers that had tables. */
    uint64_t swap_epoch;                                /**< Global epoch at time of swap. */
    uint64_t swap_timestamp_ns;                         /**< Wall-clock timestamp of swap. */
    uint32_t flow_rule_index;                           /**< Which flow_rule this belonged to. */
    bool pending;                                       /**< true if still awaiting retirement. */
} infmon_retired_table_t;

/* ── Snapshot manager ────────────────────────────────────────────── */

/**
 * The snapshot manager owns:
 *   - per-worker epoch counters,
 *   - a ring of retired tables awaiting grace-period expiry,
 *   - a global epoch counter bumped on each swap.
 *
 * Thread safety:
 *   - Workers call infmon_worker_epoch_bump() from the data path (no lock).
 *   - The control thread calls infmon_snapshot_and_clear() and
 *     infmon_retire_poll() (serialised — only one control thread).
 *
 * @note This struct is >3KB due to cache-line-padded worker epochs.
 *       Do not stack-allocate; use heap allocation or static/BSS placement.
 */
typedef struct {
    infmon_worker_epoch_t worker_epochs[INFMON_MAX_WORKERS];
    uint32_t num_workers;

    uint64_t global_epoch; /**< Bumped on each swap. */

    infmon_retired_table_t retired[INFMON_MAX_RETIRED];
    uint32_t retired_count; /**< Number of pending entries. */

    uint64_t grace_ns; /**< Configurable grace window. */

    /* Callback for getting wall-clock nanoseconds (injectable for testing). */
    uint64_t (*clock_ns)(void);
} infmon_snapshot_mgr_t;

/* ── Snapshot result ─────────────────────────────────────────────── */

typedef enum {
    INFMON_SNAP_OK = 0,
    INFMON_SNAP_ALLOC_FAILED,     /**< Could not allocate replacement table. */
    INFMON_SNAP_TOO_MANY_RETIRED, /**< Retired ring is full. */
    INFMON_SNAP_INVALID_INDEX,    /**< flow_rule_index out of range. */
    INFMON_SNAP_NULL_TABLE,       /**< No table installed at that index. */
} infmon_snap_result_t;

typedef struct {
    infmon_snap_result_t result;
    infmon_counter_table_t *retired_tables[INFMON_MAX_WORKERS]; /**< Per-worker retired tables. */
    uint32_t num_retired;        /**< Number of retired tables (= num_workers). */
    uint64_t retired_generation; /**< Generation of the retired tables. */
} infmon_snap_reply_t;

/* ── Lifecycle ───────────────────────────────────────────────────── */

/**
 * Initialise a snapshot manager.
 *
 * @param mgr          Manager to initialise.
 * @param num_workers  Number of worker threads.
 * @param grace_ns     Grace period in nanoseconds (0 = use default).
 * @param clock_ns     Wall-clock function (NULL = use clock_gettime).
 */
void infmon_snapshot_mgr_init(infmon_snapshot_mgr_t *mgr, uint32_t num_workers, uint64_t grace_ns,
                              uint64_t (*clock_ns)(void));

/**
 * Destroy the snapshot manager.  Frees any retired tables still pending.
 */
void infmon_snapshot_mgr_destroy(infmon_snapshot_mgr_t *mgr);

/* ── Worker-side (data path) ─────────────────────────────────────── */

/**
 * Bump the calling worker's epoch.  Must be called once per dispatch
 * loop iteration.  The worker_id must be in [0, num_workers).
 */
static inline void infmon_worker_epoch_bump(infmon_snapshot_mgr_t *mgr, uint32_t worker_id)
{
    assert(worker_id < mgr->num_workers);
    /* Single-writer: relaxed load + release store avoids locked RMW. */
    uint64_t e = __atomic_load_n(&mgr->worker_epochs[worker_id].epoch, __ATOMIC_RELAXED);
    __atomic_store_n(&mgr->worker_epochs[worker_id].epoch, e + 1, __ATOMIC_RELEASE);
}

/**
 * Read a worker's published epoch (for control thread use).
 */
static inline uint64_t infmon_worker_epoch_read(const infmon_snapshot_mgr_t *mgr,
                                                uint32_t worker_id)
{
    assert(worker_id < mgr->num_workers);
    return __atomic_load_n(&mgr->worker_epochs[worker_id].epoch, __ATOMIC_ACQUIRE);
}

/* ── Control-plane operations ────────────────────────────────────── */

/**
 * Perform an atomic snapshot_and_clear on one flow_rule's counter table.
 *
 * 1. Allocates a new empty table with the same dimensions.
 * 2. Sets generation = G+1 and epoch_ns on the new table.
 * 3. Atomically swaps the table pointer in tables[flow_rule_index].
 * 4. Enqueues the old table for RCU retirement.
 *
 * @param mgr              Snapshot manager.
 * @param tables_flat      Pointer to first element of 2D tables array.
 * @param tables_stride    Number of flow rule slots per worker row.
 * @param num_workers      Number of workers (rows in tables).
 * @param flow_rule_index  Index of the flow_rule to snapshot.
 * @param max_flow_rules   Size of the tables array.
 * @param max_key_width    Key width for the new table (same as old).
 * @param reply            Output: result + retired table info.
 */
void infmon_snapshot_and_clear(infmon_snapshot_mgr_t *mgr, infmon_counter_table_t **tables_flat,
                               uint32_t tables_stride, uint32_t num_workers,
                               uint32_t flow_rule_index, uint32_t max_flow_rules,
                               uint32_t max_key_width, infmon_snap_reply_t *reply);

/**
 * Poll for retired tables whose grace period has expired.
 * Frees any that are safe to reclaim.
 *
 * @param mgr  Snapshot manager.
 * @return Number of tables freed this call.
 */
uint32_t infmon_retire_poll(infmon_snapshot_mgr_t *mgr);

/**
 * Check whether all workers have advanced past a given epoch.
 *
 * @param mgr    Snapshot manager.
 * @param epoch  The epoch to check against.
 * @return true if every worker's epoch > epoch.
 */
bool infmon_all_workers_past(const infmon_snapshot_mgr_t *mgr, uint64_t epoch);

#ifdef __cplusplus
}
#endif

#endif /* INFMON_SNAPSHOT_H */
