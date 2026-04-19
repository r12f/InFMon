/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * Tests for stats-segment exposure — see specs/004-backend-architecture.md §6
 */

#include <gtest/gtest.h>

extern "C" {
#include "infmon/counter_table.h"
#include "infmon/stats_segment.h"
}

/* ── Helpers ─────────────────────────────────────────────────────── */

static infmon_flow_rule_id_t make_id(uint64_t hi, uint64_t lo)
{
    infmon_flow_rule_id_t id;
    id.hi = hi;
    id.lo = lo;
    return id;
}

class StatsSegmentTest : public ::testing::Test
{
  protected:
    infmon_stats_registry_t reg;
    infmon_counter_table_t *table1 = nullptr;
    infmon_counter_table_t *table2 = nullptr;

    void SetUp() override
    {
        /* Use 0 as segment base — offsets become raw pointers for testing. */
        infmon_stats_registry_init(&reg, 0);

        table1 = infmon_counter_table_create(16, 32);
        ASSERT_NE(table1, nullptr);
        table1->generation = 1;
        table1->epoch_ns = 1000000;

        table2 = infmon_counter_table_create(32, 16);
        ASSERT_NE(table2, nullptr);
        table2->generation = 2;
        table2->epoch_ns = 2000000;
    }

    void TearDown() override
    {
        infmon_stats_registry_destroy(&reg);
        infmon_counter_table_destroy(table1);
        infmon_counter_table_destroy(table2);
    }
};

/* ── Basic lifecycle ─────────────────────────────────────────────── */

TEST_F(StatsSegmentTest, InitEmpty)
{
    EXPECT_EQ(infmon_stats_count(&reg), 0u);
}

TEST_F(StatsSegmentTest, PublishAndFind)
{
    auto id = make_id(0xAA, 0xBB);
    EXPECT_EQ(infmon_stats_publish(&reg, table1, id, 0), INFMON_STATS_OK);
    EXPECT_EQ(infmon_stats_count(&reg), 1u);

    auto *d = infmon_stats_find(&reg, id, 1);
    ASSERT_NE(d, nullptr);
    EXPECT_TRUE(infmon_flow_rule_id_eq(d->flow_rule_id, id));
    EXPECT_EQ(d->generation, 1u);
    EXPECT_EQ(d->epoch_ns, 1000000u);
    EXPECT_EQ(d->flow_rule_index, 0u);
    EXPECT_EQ(d->slots_len, table1->num_slots);
    EXPECT_TRUE(d->active);
}

TEST_F(StatsSegmentTest, PublishNullTable)
{
    auto id = make_id(1, 2);
    EXPECT_EQ(infmon_stats_publish(&reg, nullptr, id, 0), INFMON_STATS_ERR_NULL_TABLE);
    EXPECT_EQ(infmon_stats_count(&reg), 0u);
}

TEST_F(StatsSegmentTest, PublishNullRegistry)
{
    auto id = make_id(1, 2);
    EXPECT_EQ(infmon_stats_publish(nullptr, table1, id, 0), INFMON_STATS_ERR_INVALID_ARG);
}

/* ── Unpublish ───────────────────────────────────────────────────── */

TEST_F(StatsSegmentTest, UnpublishByGeneration)
{
    auto id = make_id(0xAA, 0xBB);
    EXPECT_EQ(infmon_stats_publish(&reg, table1, id, 0), INFMON_STATS_OK);
    EXPECT_EQ(infmon_stats_publish(&reg, table2, id, 0), INFMON_STATS_OK);
    EXPECT_EQ(infmon_stats_count(&reg), 2u);

    EXPECT_EQ(infmon_stats_unpublish(&reg, id, 1), INFMON_STATS_OK);
    EXPECT_EQ(infmon_stats_count(&reg), 1u);

    EXPECT_EQ(infmon_stats_find(&reg, id, 1), nullptr);
    EXPECT_NE(infmon_stats_find(&reg, id, 2), nullptr);
}

TEST_F(StatsSegmentTest, UnpublishNotFound)
{
    auto id = make_id(0xAA, 0xBB);
    EXPECT_EQ(infmon_stats_unpublish(&reg, id, 99), INFMON_STATS_ERR_NOT_FOUND);
}

TEST_F(StatsSegmentTest, UnpublishAll)
{
    auto id1 = make_id(1, 1);
    auto id2 = make_id(2, 2);
    EXPECT_EQ(infmon_stats_publish(&reg, table1, id1, 0), INFMON_STATS_OK);
    EXPECT_EQ(infmon_stats_publish(&reg, table2, id1, 0), INFMON_STATS_OK);
    EXPECT_EQ(infmon_stats_publish(&reg, table1, id2, 1), INFMON_STATS_OK);
    EXPECT_EQ(infmon_stats_count(&reg), 3u);

    EXPECT_EQ(infmon_stats_unpublish_all(&reg, id1), 2u);
    EXPECT_EQ(infmon_stats_count(&reg), 1u);
    EXPECT_NE(infmon_stats_find(&reg, id2, 1), nullptr);
}

/* ── Find latest ─────────────────────────────────────────────────── */

TEST_F(StatsSegmentTest, FindLatest)
{
    auto id = make_id(0xAA, 0xBB);
    EXPECT_EQ(infmon_stats_publish(&reg, table1, id, 0), INFMON_STATS_OK);
    EXPECT_EQ(infmon_stats_publish(&reg, table2, id, 0), INFMON_STATS_OK);

    auto *d = infmon_stats_find_latest(&reg, id);
    ASSERT_NE(d, nullptr);
    EXPECT_EQ(d->generation, 2u);
}

TEST_F(StatsSegmentTest, FindLatestNotFound)
{
    auto id = make_id(99, 99);
    EXPECT_EQ(infmon_stats_find_latest(&reg, id), nullptr);
}

/* ── Enumeration ─────────────────────────────────────────────────── */

TEST_F(StatsSegmentTest, GetByIndex)
{
    auto id1 = make_id(1, 1);
    auto id2 = make_id(2, 2);
    EXPECT_EQ(infmon_stats_publish(&reg, table1, id1, 0), INFMON_STATS_OK);
    EXPECT_EQ(infmon_stats_publish(&reg, table2, id2, 1), INFMON_STATS_OK);

    auto *d0 = infmon_stats_get(&reg, 0);
    auto *d1 = infmon_stats_get(&reg, 1);
    ASSERT_NE(d0, nullptr);
    ASSERT_NE(d1, nullptr);
    EXPECT_NE(d0, d1);

    /* Out of range */
    EXPECT_EQ(infmon_stats_get(&reg, 2), nullptr);
    EXPECT_EQ(infmon_stats_get(&reg, 100), nullptr);
}

/* ── Refresh ─────────────────────────────────────────────────────── */

TEST_F(StatsSegmentTest, Refresh)
{
    auto id = make_id(0xAA, 0xBB);
    EXPECT_EQ(infmon_stats_publish(&reg, table1, id, 0), INFMON_STATS_OK);

    /* Mutate the table and refresh */
    table1->key_arena_used = 42;
    table1->insert_failed = 7;
    table1->table_full = 3;

    EXPECT_EQ(infmon_stats_refresh(&reg, id, 1, table1), INFMON_STATS_OK);

    auto *d = infmon_stats_find(&reg, id, 1);
    ASSERT_NE(d, nullptr);
    EXPECT_EQ(d->key_arena_used, 42u);
    EXPECT_EQ(d->insert_failed, 7u);
    EXPECT_EQ(d->table_full, 3u);
}

TEST_F(StatsSegmentTest, RefreshNotFound)
{
    auto id = make_id(99, 99);
    EXPECT_EQ(infmon_stats_refresh(&reg, id, 0, table1), INFMON_STATS_ERR_NOT_FOUND);
}

/* ── Offset computation ──────────────────────────────────────────── */

TEST_F(StatsSegmentTest, OffsetsAreRelativeToBase)
{
    /* Use a non-zero base to verify offset arithmetic */
    uintptr_t fake_base = 0x10000;
    infmon_stats_registry_t reg2;
    infmon_stats_registry_init(&reg2, fake_base);

    /* We can't easily place the table's slots in the "segment" at
     * fake_base, but we can verify the offset computation is:
     *   offset = (uintptr_t)slots - fake_base
     * by checking the formula directly. */
    uint64_t expected_slots_off = infmon_stats_offset_of(fake_base, table1->slots);
    uint64_t expected_arena_off = infmon_stats_offset_of(fake_base, table1->key_arena);

    auto id = make_id(0xAA, 0xBB);
    EXPECT_EQ(infmon_stats_publish(&reg2, table1, id, 0), INFMON_STATS_OK);

    auto *d = infmon_stats_find(&reg2, id, 1);
    ASSERT_NE(d, nullptr);
    EXPECT_EQ(d->slots_offset, expected_slots_off);
    EXPECT_EQ(d->key_arena_offset, expected_arena_off);

    /* Resolve back and verify we get the original pointers */
    void *resolved_slots = infmon_stats_resolve(fake_base, d->slots_offset);
    void *resolved_arena = infmon_stats_resolve(fake_base, d->key_arena_offset);
    EXPECT_EQ(resolved_slots, (void *) table1->slots);
    EXPECT_EQ(resolved_arena, (void *) table1->key_arena);

    infmon_stats_registry_destroy(&reg2);
}

/* ── Registry full ───────────────────────────────────────────────── */

TEST_F(StatsSegmentTest, RegistryFull)
{
    auto id = make_id(0, 0);
    for (uint32_t i = 0; i < INFMON_STATS_MAX_DESCRIPTORS; i++) {
        auto rid = make_id(i, i);
        /* Each table needs a unique generation for separate entries */
        infmon_counter_table_t *t = infmon_counter_table_create(8, 4);
        ASSERT_NE(t, nullptr);
        t->generation = i;
        EXPECT_EQ(infmon_stats_publish(&reg, t, rid, i), INFMON_STATS_OK);
        infmon_counter_table_destroy(t);
    }
    EXPECT_EQ(infmon_stats_count(&reg), INFMON_STATS_MAX_DESCRIPTORS);

    /* One more should fail */
    id = make_id(999, 999);
    EXPECT_EQ(infmon_stats_publish(&reg, table1, id, 0), INFMON_STATS_ERR_REGISTRY_FULL);
}

/* ── Slot reuse after unpublish ──────────────────────────────────── */

TEST_F(StatsSegmentTest, SlotReuseAfterUnpublish)
{
    /* Fill the registry */
    for (uint32_t i = 0; i < INFMON_STATS_MAX_DESCRIPTORS; i++) {
        auto rid = make_id(i, i);
        infmon_counter_table_t *t = infmon_counter_table_create(8, 4);
        ASSERT_NE(t, nullptr);
        t->generation = i;
        EXPECT_EQ(infmon_stats_publish(&reg, t, rid, i), INFMON_STATS_OK);
        infmon_counter_table_destroy(t);
    }

    /* Remove one */
    auto remove_id = make_id(5, 5);
    EXPECT_EQ(infmon_stats_unpublish(&reg, remove_id, 5), INFMON_STATS_OK);
    EXPECT_EQ(infmon_stats_count(&reg), INFMON_STATS_MAX_DESCRIPTORS - 1);

    /* Now we can publish one more */
    auto new_id = make_id(999, 999);
    EXPECT_EQ(infmon_stats_publish(&reg, table1, new_id, 0), INFMON_STATS_OK);
    EXPECT_EQ(infmon_stats_count(&reg), INFMON_STATS_MAX_DESCRIPTORS);
}

/* ── Negative tests for NULL args ────────────────────────────────── */

TEST_F(StatsSegmentTest, RefreshNullTable)
{
    auto id = make_id(0xAA, 0xBB);
    EXPECT_EQ(infmon_stats_publish(&reg, table1, id, 0), INFMON_STATS_OK);
    EXPECT_EQ(infmon_stats_refresh(&reg, id, 1, nullptr), INFMON_STATS_ERR_INVALID_ARG);
}

TEST_F(StatsSegmentTest, CountNullRegistry)
{
    EXPECT_EQ(infmon_stats_count(nullptr), 0u);
}

TEST_F(StatsSegmentTest, GetNullRegistry)
{
    EXPECT_EQ(infmon_stats_get(nullptr, 0), nullptr);
}

TEST_F(StatsSegmentTest, FindNullRegistry)
{
    auto id = make_id(1, 1);
    EXPECT_EQ(infmon_stats_find(nullptr, id, 0), nullptr);
}

TEST_F(StatsSegmentTest, FindLatestNullRegistry)
{
    auto id = make_id(1, 1);
    EXPECT_EQ(infmon_stats_find_latest(nullptr, id), nullptr);
}

/* ── flow_rule_id helpers ────────────────────────────────────────── */

TEST(FlowRuleIdTest, Equality)
{
    auto a = make_id(1, 2);
    auto b = make_id(1, 2);
    auto c = make_id(1, 3);
    EXPECT_TRUE(infmon_flow_rule_id_eq(a, b));
    EXPECT_FALSE(infmon_flow_rule_id_eq(a, c));
}

TEST(FlowRuleIdTest, IsZero)
{
    auto zero = make_id(0, 0);
    auto nonzero = make_id(0, 1);
    EXPECT_TRUE(infmon_flow_rule_id_is_zero(zero));
    EXPECT_FALSE(infmon_flow_rule_id_is_zero(nonzero));
}

/* ── Descriptor field verification ───────────────────────────────── */

TEST_F(StatsSegmentTest, DescriptorCapturesAllTableFields)
{
    auto id = make_id(0xDEAD, 0xBEEF);
    table1->generation = 42;
    table1->epoch_ns = 12345678;
    table1->key_arena_used = 100;
    table1->insert_failed = 5;
    table1->table_full = 2;

    EXPECT_EQ(infmon_stats_publish(&reg, table1, id, 7), INFMON_STATS_OK);

    auto *d = infmon_stats_find(&reg, id, 42);
    ASSERT_NE(d, nullptr);
    EXPECT_EQ(d->flow_rule_index, 7u);
    EXPECT_EQ(d->generation, 42u);
    EXPECT_EQ(d->epoch_ns, 12345678u);
    EXPECT_EQ(d->slots_len, table1->num_slots);
    EXPECT_EQ(d->key_arena_capacity, table1->key_arena_capacity);
    EXPECT_EQ(d->key_arena_used, 100u);
    EXPECT_EQ(d->insert_failed, 5u);
    EXPECT_EQ(d->table_full, 2u);
}
