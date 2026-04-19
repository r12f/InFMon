/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * Unit tests for counter_table
 */

#include <gtest/gtest.h>
#include <cstring>

extern "C" {
#include "infmon/counter_table.h"
}

/* ── infmon_next_pow2 ────────────────────────────────────────────── */

TEST(NextPow2, Zero)       { EXPECT_EQ(infmon_next_pow2(0), 1u); }
TEST(NextPow2, One)        { EXPECT_EQ(infmon_next_pow2(1), 1u); }
TEST(NextPow2, Two)        { EXPECT_EQ(infmon_next_pow2(2), 2u); }
TEST(NextPow2, Three)      { EXPECT_EQ(infmon_next_pow2(3), 4u); }
TEST(NextPow2, Five)       { EXPECT_EQ(infmon_next_pow2(5), 8u); }
TEST(NextPow2, PowerOf2)   { EXPECT_EQ(infmon_next_pow2(1024), 1024u); }
TEST(NextPow2, Large)      { EXPECT_EQ(infmon_next_pow2(0x7FFFFFFFu), 0x80000000u); }

/* ── Create / Destroy ────────────────────────────────────────────── */

TEST(CounterTable, CreateDestroy)
{
    auto *t = infmon_counter_table_create(16, 32);
    ASSERT_NE(t, nullptr);
    EXPECT_EQ(t->num_slots, 16u);
    EXPECT_EQ(t->slot_mask, 15u);
    EXPECT_EQ(t->occupied_count, 0u);
    infmon_counter_table_destroy(t);
}

TEST(CounterTable, CreateRoundsUp)
{
    auto *t = infmon_counter_table_create(10, 32);
    ASSERT_NE(t, nullptr);
    EXPECT_EQ(t->num_slots, 16u);
    infmon_counter_table_destroy(t);
}

TEST(CounterTable, CreateZeroReturnsNull)
{
    EXPECT_EQ(infmon_counter_table_create(0, 32), nullptr);
    EXPECT_EQ(infmon_counter_table_create(16, 0), nullptr);
}

TEST(CounterTable, DestroyNull)
{
    infmon_counter_table_destroy(nullptr); /* should not crash */
}

/* ── Single insert ───────────────────────────────────────────────── */

TEST(CounterTable, SingleInsert)
{
    auto *t = infmon_counter_table_create(16, 32);
    ASSERT_NE(t, nullptr);

    uint8_t key[] = {1, 2, 3, 4};
    uint64_t hash = 0xDEADBEEF;

    bool ok = infmon_counter_table_update(t, hash, key, sizeof(key), 100, 1);
    EXPECT_TRUE(ok);
    EXPECT_EQ(t->occupied_count, 1u);

    /* Find and verify via linear scan */
    infmon_slot_t slot;
    bool found = false;
    for (uint32_t i = 0; i < t->num_slots; i++) {
        ASSERT_TRUE(infmon_counter_table_read_slot(t, i, &slot));
        if (slot.flags == INFMON_SLOT_OCCUPIED && slot.key_hash == hash) {
            EXPECT_EQ(slot.packets, 1u);
            EXPECT_EQ(slot.bytes, 100u);
            found = true;
            break;
        }
    }
    EXPECT_TRUE(found);

    infmon_counter_table_destroy(t);
}

/* ── Duplicate key updates ───────────────────────────────────────── */

TEST(CounterTable, DuplicateKeyAccumulates)
{
    auto *t = infmon_counter_table_create(16, 32);
    ASSERT_NE(t, nullptr);

    uint8_t key[] = {10, 20};
    uint64_t hash = 0x1234;

    infmon_counter_table_update(t, hash, key, sizeof(key), 64, 1);
    infmon_counter_table_update(t, hash, key, sizeof(key), 128, 2);
    infmon_counter_table_update(t, hash, key, sizeof(key), 256, 3);

    EXPECT_EQ(t->occupied_count, 1u);

    /* Find the slot */
    infmon_slot_t slot;
    for (uint32_t i = 0; i < t->num_slots; i++) {
        infmon_counter_table_read_slot(t, i, &slot);
        if (slot.flags == INFMON_SLOT_OCCUPIED && slot.key_hash == hash) {
            EXPECT_EQ(slot.packets, 3u);
            EXPECT_EQ(slot.bytes, 64u + 128u + 256u);
            EXPECT_EQ(slot.last_update, 3u);
            break;
        }
    }

    infmon_counter_table_destroy(t);
}

/* ── Multiple distinct keys ──────────────────────────────────────── */

TEST(CounterTable, MultipleKeys)
{
    auto *t = infmon_counter_table_create(16, 32);
    ASSERT_NE(t, nullptr);

    for (uint32_t i = 0; i < 8; i++) {
        uint8_t key[4];
        memcpy(key, &i, sizeof(i));
        EXPECT_TRUE(infmon_counter_table_update(t, (uint64_t)i + 1, key, 4, 100, i));
    }
    EXPECT_EQ(t->occupied_count, 8u);

    infmon_counter_table_destroy(t);
}

/* ── Key retrieval ───────────────────────────────────────────────── */

TEST(CounterTable, KeyRetrieval)
{
    auto *t = infmon_counter_table_create(16, 32);
    ASSERT_NE(t, nullptr);

    uint8_t key[] = {0xAA, 0xBB, 0xCC};
    uint64_t hash = 0x5555;
    infmon_counter_table_update(t, hash, key, sizeof(key), 50, 1);

    infmon_slot_t slot;
    for (uint32_t i = 0; i < t->num_slots; i++) {
        infmon_counter_table_read_slot(t, i, &slot);
        if (slot.flags == INFMON_SLOT_OCCUPIED && slot.key_hash == hash) {
            const uint8_t *k = infmon_counter_table_key(t, &slot);
            ASSERT_NE(k, nullptr);
            EXPECT_EQ(memcmp(k, key, sizeof(key)), 0);
            break;
        }
    }

    infmon_counter_table_destroy(t);
}

/* ── Read slot out of range ──────────────────────────────────────── */

TEST(CounterTable, ReadSlotOutOfRange)
{
    auto *t = infmon_counter_table_create(8, 16);
    ASSERT_NE(t, nullptr);

    infmon_slot_t slot;
    EXPECT_FALSE(infmon_counter_table_read_slot(t, 999, &slot));

    infmon_counter_table_destroy(t);
}

/* ── Table full + LRU eviction ───────────────────────────────────── */

TEST(CounterTable, TableFullAndEviction)
{
    /* Small table: 8 slots */
    auto *t = infmon_counter_table_create(8, 16);
    ASSERT_NE(t, nullptr);
    EXPECT_EQ(t->num_slots, 8u);

    /* Fill all slots with increasing tick */
    for (uint32_t i = 0; i < 8; i++) {
        uint8_t key[4];
        memcpy(key, &i, sizeof(i));
        uint64_t hash = (uint64_t)(i + 1) * 0x100;
        bool ok = infmon_counter_table_update(t, hash, key, 4, 64, i + 1);
        EXPECT_TRUE(ok);
    }
    EXPECT_EQ(t->occupied_count, 8u);

    /* Insert one more — should trigger eviction of lowest tick (tick=1) */
    uint8_t new_key[] = {0xFF, 0xFF, 0xFF, 0xFF};
    uint64_t new_hash = 0xAAAA;
    bool ok = infmon_counter_table_update(t, new_hash, new_key, 4, 99, 100);
    EXPECT_TRUE(ok);
    /* table_full should have been incremented at least once */
    EXPECT_GE(t->table_full, 1u);

    /* Verify the new key is present */
    bool found_new = false;
    infmon_slot_t slot;
    for (uint32_t i = 0; i < t->num_slots; i++) {
        infmon_counter_table_read_slot(t, i, &slot);
        if (slot.flags == INFMON_SLOT_OCCUPIED && slot.key_hash == new_hash) {
            found_new = true;
            EXPECT_EQ(slot.packets, 1u);
            EXPECT_EQ(slot.bytes, 99u);
        }
    }
    EXPECT_TRUE(found_new);

    infmon_counter_table_destroy(t);
}

/* ── Key on free slot returns NULL ────────────────────────────────── */

TEST(CounterTable, KeyOnFreeSlotReturnsNull)
{
    infmon_slot_t slot = {};
    slot.flags = INFMON_SLOT_FREE;

    auto *t = infmon_counter_table_create(8, 16);
    ASSERT_NE(t, nullptr);
    EXPECT_EQ(infmon_counter_table_key(t, &slot), nullptr);
    infmon_counter_table_destroy(t);
}
