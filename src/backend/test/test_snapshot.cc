/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * Tests for snapshot_and_clear — see specs/004-backend-architecture.md §7.2
 */

#include <atomic>
#include <chrono>
#include <cstring>
#include <gtest/gtest.h>
#include <thread>
#include <vector>

extern "C" {
#include "infmon/counter_table.h"
#include "infmon/snapshot.h"
}

/* ── Test helpers ────────────────────────────────────────────────── */

static std::atomic<uint64_t> g_fake_clock_ns{1000000000ULL}; /* 1 second */

static uint64_t fake_clock_ns(void)
{
    return g_fake_clock_ns.load(std::memory_order_relaxed);
}

static void advance_clock_ns(uint64_t delta)
{
    g_fake_clock_ns.fetch_add(delta, std::memory_order_relaxed);
}

/* Max flow rules (matches graph_node.h) */
#define MAX_FLOW_RULES 64
#define MAX_KEY_WIDTH 32
#define TEST_NUM_WORKERS 1

class SnapshotTest : public ::testing::Test
{
  protected:
    infmon_snapshot_mgr_t mgr{};
    infmon_counter_table_t *tables[INFMON_MAX_WORKERS][MAX_FLOW_RULES]{};

    void SetUp() override
    {
        g_fake_clock_ns.store(1000000000ULL);
        infmon_snapshot_mgr_init(&mgr, 4, INFMON_RETIRE_GRACE_NS, fake_clock_ns);
    }

    void TearDown() override
    {
        infmon_snapshot_mgr_destroy(&mgr);
        for (uint32_t w = 0; w < INFMON_MAX_WORKERS; w++) {
            for (uint32_t i = 0; i < MAX_FLOW_RULES; i++) {
                if (tables[w][i]) {
                    infmon_counter_table_destroy(tables[w][i]);
                    tables[w][i] = nullptr;
                }
            }
        }
    }

    /* Install a table at the given index for worker 0 */
    void install_table(uint32_t idx, uint32_t max_keys = 1024)
    {
        tables[0][idx] = infmon_counter_table_create(max_keys, MAX_KEY_WIDTH);
        ASSERT_NE(tables[0][idx], nullptr);
    }

    /* Insert a key into worker 0's table and return whether it succeeded */
    bool insert_key(uint32_t table_idx, uint64_t hash, uint64_t pkt_bytes)
    {
        uint8_t key[8];
        memcpy(key, &hash, sizeof(key));
        return infmon_counter_table_update(tables[0][table_idx], hash, key, 8, pkt_bytes, 1);
    }

    /* Helper to call snapshot_and_clear with our 2D tables */
    void do_snapshot(uint32_t flow_rule_index, uint32_t num_workers, infmon_snap_reply_t *reply)
    {
        infmon_snapshot_and_clear(&mgr, &tables[0][0], MAX_FLOW_RULES, num_workers, flow_rule_index,
                                  MAX_FLOW_RULES, MAX_KEY_WIDTH, reply);
    }
};

/* ── Basic snapshot ──────────────────────────────────────────────── */

TEST_F(SnapshotTest, BasicSwap)
{
    install_table(0);

    /* Insert some data into the table */
    ASSERT_TRUE(insert_key(0, 0xAAAA, 100));
    ASSERT_TRUE(insert_key(0, 0xBBBB, 200));

    infmon_counter_table_t *original = tables[0][0];
    ASSERT_EQ(original->generation, 0u);
    ASSERT_EQ(original->occupied_count, 2u);

    /* Perform snapshot */
    infmon_snap_reply_t reply{};
    do_snapshot(0, TEST_NUM_WORKERS, &reply);

    ASSERT_EQ(reply.result, INFMON_SNAP_OK);
    ASSERT_EQ(reply.retired_tables[0], original);
    ASSERT_EQ(reply.retired_generation, 0u);

    /* New table is empty with generation = 1 */
    ASSERT_NE(tables[0][0], original);
    ASSERT_EQ(tables[0][0]->generation, 1u);
    ASSERT_EQ(tables[0][0]->occupied_count, 0u);

    /* Old table data is still intact (immutable post-swap) */
    ASSERT_EQ(original->occupied_count, 2u);
}

/* ── Multiple sequential snapshots ───────────────────────────────── */

TEST_F(SnapshotTest, SequentialSwaps)
{
    install_table(0);

    /* Advance workers past each epoch and poll to free retired tables */
    for (uint32_t gen = 0; gen < 5; gen++) {
        insert_key(0, 0x1000 + gen, 64);

        infmon_snap_reply_t reply{};
        do_snapshot(0, TEST_NUM_WORKERS, &reply);
        ASSERT_EQ(reply.result, INFMON_SNAP_OK);
        ASSERT_EQ(reply.retired_generation, gen);
        ASSERT_EQ(tables[0][0]->generation, gen + 1);

        /* Advance workers past the swap epoch (need epoch > swap_epoch) */
        for (int bump = 0; bump <= (int) gen + 1; bump++)
            for (uint32_t w = 0; w < mgr.num_workers; w++)
                infmon_worker_epoch_bump(&mgr, w);

        /* Advance clock past grace period */
        advance_clock_ns(INFMON_RETIRE_GRACE_NS + 1);

        /* Poll to free retired tables */
        uint32_t freed = infmon_retire_poll(&mgr);
        ASSERT_EQ(freed, 1u);
    }
}

/* ── Generation tracking ─────────────────────────────────────────── */

TEST_F(SnapshotTest, GenerationIncrement)
{
    install_table(0);
    tables[0][0]->generation = 42;

    infmon_snap_reply_t reply{};
    do_snapshot(0, TEST_NUM_WORKERS, &reply);

    ASSERT_EQ(reply.result, INFMON_SNAP_OK);
    ASSERT_EQ(reply.retired_generation, 42u);
    ASSERT_EQ(tables[0][0]->generation, 43u);
}

/* ── New table has epoch_ns set ──────────────────────────────────── */

TEST_F(SnapshotTest, EpochNsSet)
{
    install_table(0);

    infmon_snap_reply_t reply{};
    do_snapshot(0, TEST_NUM_WORKERS, &reply);

    ASSERT_EQ(reply.result, INFMON_SNAP_OK);
    ASSERT_GT(tables[0][0]->epoch_ns, 0u);
}

/* ── Error: invalid index ────────────────────────────────────────── */

TEST_F(SnapshotTest, InvalidIndex)
{
    infmon_snap_reply_t reply{};
    do_snapshot(MAX_FLOW_RULES, TEST_NUM_WORKERS, &reply);
    ASSERT_EQ(reply.result, INFMON_SNAP_INVALID_INDEX);
}

/* ── Error: null table ───────────────────────────────────────────── */

TEST_F(SnapshotTest, NullTable)
{
    infmon_snap_reply_t reply{};
    do_snapshot(0, TEST_NUM_WORKERS, &reply);
    ASSERT_EQ(reply.result, INFMON_SNAP_NULL_TABLE);
}

/* ── Retired table is not freed before grace period ──────────────── */

TEST_F(SnapshotTest, GracePeriodRespected)
{
    install_table(0);
    insert_key(0, 0xDEAD, 512);

    infmon_snap_reply_t reply{};
    do_snapshot(0, TEST_NUM_WORKERS, &reply);
    ASSERT_EQ(reply.result, INFMON_SNAP_OK);
    ASSERT_EQ(mgr.retired_count, 1u);

    /* Advance workers but NOT the clock */
    for (uint32_t w = 0; w < mgr.num_workers; w++) {
        infmon_worker_epoch_bump(&mgr, w);
        infmon_worker_epoch_bump(&mgr, w);
    }

    uint32_t freed = infmon_retire_poll(&mgr);
    ASSERT_EQ(freed, 0u); /* Grace period not elapsed */
    ASSERT_EQ(mgr.retired_count, 1u);

    /* Now advance clock */
    advance_clock_ns(INFMON_RETIRE_GRACE_NS + 1);
    freed = infmon_retire_poll(&mgr);
    ASSERT_EQ(freed, 1u);
    ASSERT_EQ(mgr.retired_count, 0u);
}

/* ── Workers must advance past swap epoch ────────────────────────── */

TEST_F(SnapshotTest, WorkersMustAdvance)
{
    install_table(0);

    infmon_snap_reply_t reply{};
    do_snapshot(0, TEST_NUM_WORKERS, &reply);
    ASSERT_EQ(reply.result, INFMON_SNAP_OK);

    /* Advance clock but NOT workers */
    advance_clock_ns(INFMON_RETIRE_GRACE_NS + 1);

    uint32_t freed = infmon_retire_poll(&mgr);
    ASSERT_EQ(freed, 0u); /* Workers haven't advanced */

    /* Advance only some workers */
    infmon_worker_epoch_bump(&mgr, 0);
    infmon_worker_epoch_bump(&mgr, 0);
    infmon_worker_epoch_bump(&mgr, 1);
    infmon_worker_epoch_bump(&mgr, 1);
    freed = infmon_retire_poll(&mgr);
    ASSERT_EQ(freed, 0u); /* Not all workers */

    /* Advance remaining workers */
    infmon_worker_epoch_bump(&mgr, 2);
    infmon_worker_epoch_bump(&mgr, 2);
    infmon_worker_epoch_bump(&mgr, 3);
    infmon_worker_epoch_bump(&mgr, 3);
    freed = infmon_retire_poll(&mgr);
    ASSERT_EQ(freed, 1u);
}

/* ── Retired ring full ───────────────────────────────────────────── */

TEST_F(SnapshotTest, RetiredRingFull)
{
    /* Fill the retired ring */
    for (uint32_t i = 0; i < INFMON_MAX_RETIRED; i++) {
        install_table(i);
        infmon_snap_reply_t reply{};
        do_snapshot(i, TEST_NUM_WORKERS, &reply);
        ASSERT_EQ(reply.result, INFMON_SNAP_OK);
    }

    /* Next swap should fail with TOO_MANY_RETIRED */
    infmon_counter_table_destroy(tables[0][0]);
    tables[0][0] = nullptr;
    install_table(0); /* tables[0][0] was swapped above, so it's a fresh table */
    infmon_snap_reply_t reply{};
    do_snapshot(0, TEST_NUM_WORKERS, &reply);
    ASSERT_EQ(reply.result, INFMON_SNAP_TOO_MANY_RETIRED);
}

/* ── all_workers_past helper ─────────────────────────────────────── */

TEST_F(SnapshotTest, AllWorkersPast)
{
    ASSERT_FALSE(infmon_all_workers_past(&mgr, 0));

    for (uint32_t w = 0; w < mgr.num_workers; w++)
        infmon_worker_epoch_bump(&mgr, w);

    ASSERT_TRUE(infmon_all_workers_past(&mgr, 0));
    ASSERT_FALSE(infmon_all_workers_past(&mgr, 1));

    for (uint32_t w = 0; w < mgr.num_workers; w++)
        infmon_worker_epoch_bump(&mgr, w);

    ASSERT_TRUE(infmon_all_workers_past(&mgr, 1));
}

/* ── Concurrent workers bumping epochs during snapshot ────────────── */

TEST_F(SnapshotTest, ConcurrentEpochBumps)
{
    install_table(0);
    insert_key(0, 0xCAFE, 128);

    /* Start worker threads that continuously bump epochs */
    std::atomic<bool> stop{false};
    std::vector<std::thread> workers;
    for (uint32_t w = 0; w < mgr.num_workers; w++) {
        workers.emplace_back([this, w, &stop]() {
            while (!stop.load(std::memory_order_relaxed)) {
                infmon_worker_epoch_bump(&mgr, w);
            }
        });
    }

    /* Perform several snapshots while workers are running */
    for (int i = 0; i < 10; i++) {
        infmon_snap_reply_t reply{};
        do_snapshot(0, TEST_NUM_WORKERS, &reply);
        ASSERT_EQ(reply.result, INFMON_SNAP_OK);

        advance_clock_ns(INFMON_RETIRE_GRACE_NS + 1);
        /* Poll until freed — workers are bumping so this should eventually succeed */
        int attempt;
        for (attempt = 0; attempt < 100; attempt++) {
            if (infmon_retire_poll(&mgr) > 0)
                break;
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        }
        ASSERT_LT(attempt, 100) << "retire_poll never freed the table";
    }

    stop.store(true);
    for (auto &t : workers)
        t.join();

    /* Clean up any remaining retired tables */
    advance_clock_ns(INFMON_RETIRE_GRACE_NS + 1);
    infmon_retire_poll(&mgr);
}

/* ── Atomic pointer swap is visible to reader thread ─────────────── */

TEST_F(SnapshotTest, AtomicSwapVisibility)
{
    install_table(0);
    infmon_counter_table_t *original = tables[0][0];

    std::atomic<bool> swapped{false};
    std::atomic<infmon_counter_table_t *> observed{nullptr};

    /* Reader thread: spin until it sees a different table pointer */
    std::thread reader([this, original, &swapped, &observed]() {
        while (!swapped.load(std::memory_order_acquire)) {
            /* nothing */
        }
        /* After swap flag is set, load the table pointer with ACQUIRE */
        infmon_counter_table_t *t = __atomic_load_n(&tables[0][0], __ATOMIC_ACQUIRE);
        observed.store(t, std::memory_order_release);
    });

    /* Perform snapshot (writes with RELEASE) */
    infmon_snap_reply_t reply{};
    do_snapshot(0, TEST_NUM_WORKERS, &reply);
    ASSERT_EQ(reply.result, INFMON_SNAP_OK);

    swapped.store(true, std::memory_order_release);
    reader.join();

    /* Reader must see the new table, not the old one */
    ASSERT_NE(observed.load(), nullptr);
    ASSERT_NE(observed.load(), original);
    ASSERT_EQ(observed.load(), tables[0][0]);
}

/* ── Zero-worker edge case ───────────────────────────────────────── */

TEST_F(SnapshotTest, ZeroWorkers)
{
    infmon_snapshot_mgr_t mgr0{};
    infmon_snapshot_mgr_init(&mgr0, 0, INFMON_RETIRE_GRACE_NS, fake_clock_ns);

    /* With 0 workers, all_workers_past should always be true */
    ASSERT_TRUE(infmon_all_workers_past(&mgr0, 0));
    ASSERT_TRUE(infmon_all_workers_past(&mgr0, 999));

    install_table(0);
    infmon_snap_reply_t reply{};
    infmon_snapshot_and_clear(&mgr0, &tables[0][0], MAX_FLOW_RULES, TEST_NUM_WORKERS, 0,
                              MAX_FLOW_RULES, MAX_KEY_WIDTH, &reply);
    ASSERT_EQ(reply.result, INFMON_SNAP_OK);

    /* Should be freeable after grace period only */
    advance_clock_ns(INFMON_RETIRE_GRACE_NS + 1);
    uint32_t freed = infmon_retire_poll(&mgr0);
    ASSERT_EQ(freed, 1u);

    infmon_snapshot_mgr_destroy(&mgr0);
}

/* ── Destroy frees pending retired tables ────────────────────────── */

TEST_F(SnapshotTest, DestroyFreesPending)
{
    install_table(0);

    infmon_snap_reply_t reply{};
    do_snapshot(0, TEST_NUM_WORKERS, &reply);
    ASSERT_EQ(reply.result, INFMON_SNAP_OK);
    ASSERT_EQ(mgr.retired_count, 1u);

    /* Destroy should free the retired table without crashing */
    infmon_snapshot_mgr_destroy(&mgr);
    ASSERT_EQ(mgr.retired_count, 0u);

    /* Re-init for TearDown */
    infmon_snapshot_mgr_init(&mgr, 4, INFMON_RETIRE_GRACE_NS, fake_clock_ns);
}

/* ── Multiple flow rules can be snapshotted independently ────────── */

TEST_F(SnapshotTest, MultipleFlowRules)
{
    install_table(0);
    install_table(1);
    install_table(2);

    insert_key(0, 0xAAAA, 100);
    insert_key(1, 0xBBBB, 200);
    insert_key(2, 0xCCCC, 300);

    /* Snapshot flow_rule 1 only */
    infmon_snap_reply_t reply{};
    do_snapshot(1, TEST_NUM_WORKERS, &reply);
    ASSERT_EQ(reply.result, INFMON_SNAP_OK);

    /* flow_rule 0 and 2 untouched */
    ASSERT_EQ(tables[0][0]->occupied_count, 1u);
    ASSERT_EQ(tables[0][2]->occupied_count, 1u);

    /* flow_rule 1 has a fresh empty table */
    ASSERT_EQ(tables[0][1]->occupied_count, 0u);
    ASSERT_EQ(tables[0][1]->generation, 1u);

    /* Retired table has the old data */
    ASSERT_EQ(reply.retired_tables[0]->occupied_count, 1u);
}

/* ── Multi-worker tests ─────────────────────────────────────────── */

static constexpr uint32_t TEST_MULTI_WORKERS = 4;

TEST_F(SnapshotTest, MultiWorkerBasicSwap)
{
    /* Install tables for all workers at flow-rule index 0 */
    for (uint32_t w = 0; w < TEST_MULTI_WORKERS; w++) {
        tables[w][0] = infmon_counter_table_create(1024, MAX_KEY_WIDTH);
        ASSERT_NE(tables[w][0], nullptr);
    }

    /* Insert different data into each worker's table */
    for (uint32_t w = 0; w < TEST_MULTI_WORKERS; w++) {
        uint8_t key[8];
        uint64_t hash = 0xAA00 + w;
        memcpy(key, &hash, sizeof(key));
        ASSERT_TRUE(infmon_counter_table_update(tables[w][0], hash, key, 8, (w + 1) * 100, 1));
    }

    infmon_snap_reply_t reply{};
    do_snapshot(0, TEST_MULTI_WORKERS, &reply);
    ASSERT_EQ(reply.result, INFMON_SNAP_OK);

    /* Each worker gets a fresh empty table */
    for (uint32_t w = 0; w < TEST_MULTI_WORKERS; w++) {
        ASSERT_NE(tables[w][0], nullptr);
        ASSERT_EQ(tables[w][0]->occupied_count, 0u);
        ASSERT_EQ(tables[w][0]->generation, 1u);
    }

    /* Retired tables bundle all workers */
    ASSERT_EQ(reply.num_retired, TEST_MULTI_WORKERS);
    for (uint32_t w = 0; w < TEST_MULTI_WORKERS; w++) {
        ASSERT_NE(reply.retired_tables[w], nullptr);
        ASSERT_EQ(reply.retired_tables[w]->occupied_count, 1u);
    }
}

TEST_F(SnapshotTest, MultiWorkerRetiredCountMatchesWorkers)
{
    /* Set up 2 workers */
    constexpr uint32_t nw = 2;
    for (uint32_t w = 0; w < nw; w++) {
        tables[w][0] = infmon_counter_table_create(256, MAX_KEY_WIDTH);
        ASSERT_NE(tables[w][0], nullptr);
    }

    infmon_snap_reply_t reply{};
    do_snapshot(0, nw, &reply);
    ASSERT_EQ(reply.result, INFMON_SNAP_OK);
    ASSERT_EQ(reply.num_retired, nw);
    for (uint32_t w = 0; w < nw; w++)
        ASSERT_NE(reply.retired_tables[w], nullptr);
}
