/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * Counter table implementation — see specs/004-backend-architecture.md §5
 */

#include "infmon/counter_table.h"

#include <stdlib.h>
#include <string.h>

/* ── Utility ─────────────────────────────────────────────────────── */

uint32_t infmon_next_pow2(uint32_t v)
{
    if (v == 0)
        return 1;
    if (v > 0x80000000u)
        return 0; /* overflow — caller must check */
    v--;
    v |= v >> 1;
    v |= v >> 2;
    v |= v >> 4;
    v |= v >> 8;
    v |= v >> 16;
    return v + 1;
}

/* ── Seqlock helpers ─────────────────────────────────────────────── */

static inline void seqlock_write_begin(infmon_seqlock_t *sl)
{
    __atomic_fetch_add(&sl->seq, 1, __ATOMIC_RELEASE);
}

static inline void seqlock_write_end(infmon_seqlock_t *sl)
{
    __atomic_fetch_add(&sl->seq, 1, __ATOMIC_RELEASE);
}

static inline uint32_t seqlock_read_begin(const infmon_seqlock_t *sl)
{
    uint32_t s = __atomic_load_n(&sl->seq, __ATOMIC_ACQUIRE);
    return s;
}

static inline bool seqlock_read_retry(const infmon_seqlock_t *sl, uint32_t start)
{
    __atomic_thread_fence(__ATOMIC_ACQUIRE);
    uint32_t s = __atomic_load_n(&sl->seq, __ATOMIC_RELAXED);
    return (start & 1) || (s != start);
}

/* ── Arena allocator ─────────────────────────────────────────────── */

static uint32_t arena_alloc(infmon_counter_table_t *table, const uint8_t *key, uint16_t key_len)
{
    uint32_t offset = __atomic_fetch_add(&table->key_arena_used, key_len, __ATOMIC_RELAXED);
    if (offset + key_len > table->key_arena_capacity) {
        /* Roll back — best effort; concurrent allocs may have advanced further,
         * but the over-allocated region is harmless (never read). */
        __atomic_fetch_sub(&table->key_arena_used, key_len, __ATOMIC_RELAXED);
        return UINT32_MAX;
    }
    memcpy(table->key_arena + offset, key, key_len);
    return offset;
}

/* ── Key comparison ──────────────────────────────────────────────── */

static inline bool key_matches(const infmon_counter_table_t *table, const infmon_slot_t *slot,
                               uint64_t key_hash, const uint8_t *key, uint16_t key_len)
{
    if (slot->key_hash != key_hash)
        return false;
    if (slot->key_len != key_len)
        return false;
    if (slot->key_offset + key_len > table->key_arena_capacity)
        return false;
    return memcmp(table->key_arena + slot->key_offset, key, key_len) == 0;
}

/* ── Lifecycle ───────────────────────────────────────────────────── */

infmon_counter_table_t *infmon_counter_table_create(uint32_t max_keys, uint32_t max_key_width)
{
    if (max_keys == 0 || max_key_width == 0)
        return NULL;

    uint32_t num_slots = infmon_next_pow2(max_keys);
    /* Guard against overflow */
    if (num_slots < max_keys || num_slots == 0)
        return NULL;

    uint32_t num_groups = num_slots / INFMON_SLOTS_PER_GROUP;
    if (num_groups == 0)
        num_groups = 1;

    uint64_t arena_cap = (uint64_t) num_slots * max_key_width;
    if (arena_cap > UINT32_MAX)
        return NULL;

    infmon_counter_table_t *table = calloc(1, sizeof(*table));
    if (!table)
        return NULL;

    /* 64-byte aligned slot array */
    void *slot_mem = NULL;
    if (posix_memalign(&slot_mem, 64, (size_t) num_slots * sizeof(infmon_slot_t)) != 0) {
        free(table);
        return NULL;
    }
    memset(slot_mem, 0, (size_t) num_slots * sizeof(infmon_slot_t));

    table->slots = (infmon_slot_t *) slot_mem;
    table->num_slots = num_slots;
    table->slot_mask = num_slots - 1;
    table->key_arena = (uint8_t *) malloc((uint32_t) arena_cap);
    table->key_arena_capacity = (uint32_t) arena_cap;
    table->key_arena_used = 0;
    table->seqlocks = (infmon_seqlock_t *) calloc(num_groups, sizeof(infmon_seqlock_t));
    table->num_groups = num_groups;
    table->generation = 0;
    table->epoch_ns = 0;
    table->insert_failed = 0;
    table->table_full = 0;
    table->occupied_count = 0;

    if (!table->key_arena || !table->seqlocks) {
        infmon_counter_table_destroy(table);
        return NULL;
    }

    return table;
}

void infmon_counter_table_destroy(infmon_counter_table_t *table)
{
    if (!table)
        return;
    free(table->slots);
    free(table->key_arena);
    free(table->seqlocks);
    free(table);
}

/* ── LRU eviction ────────────────────────────────────────────────── */

static bool evict_lru(infmon_counter_table_t *table, uint32_t probe_start,
                      uint64_t tick __attribute__((unused)))
{
    /* Scan a bounded window to find approximate LRU victim */
    uint32_t victim = UINT32_MAX;
    uint64_t min_tick = UINT64_MAX;

    uint32_t window = (table->num_slots < 32) ? table->num_slots : 32;
    for (uint32_t i = 0; i < window; i++) {
        uint32_t idx = (probe_start + i) & table->slot_mask;
        uint16_t f = __atomic_load_n(&table->slots[idx].flags, __ATOMIC_ACQUIRE);
        if (f == INFMON_SLOT_OCCUPIED) {
            uint64_t lu = __atomic_load_n(&table->slots[idx].last_update, __ATOMIC_RELAXED);
            if (lu < min_tick) {
                min_tick = lu;
                victim = idx;
            }
        }
    }

    if (victim == UINT32_MAX)
        return false;

    uint32_t group = victim / INFMON_SLOTS_PER_GROUP;
    infmon_seqlock_t *sl = &table->seqlocks[group];

    seqlock_write_begin(sl);
    /* Re-check under seqlock — another thread may have evicted or re-inserted */
    if (__atomic_load_n(&table->slots[victim].flags, __ATOMIC_RELAXED) != INFMON_SLOT_OCCUPIED) {
        seqlock_write_end(sl);
        return false;
    }
    /* Mark as tombstone; note: arena bytes for this key are not reclaimed */
    __atomic_store_n(&table->slots[victim].flags, INFMON_SLOT_TOMBSTONE, __ATOMIC_RELEASE);
    __atomic_fetch_sub(&table->occupied_count, 1, __ATOMIC_RELAXED);
    seqlock_write_end(sl);

    return true;
}

/* ── Data-path operations ────────────────────────────────────────── */

bool infmon_counter_table_update(infmon_counter_table_t *table, uint64_t key_hash,
                                 const uint8_t *key, uint16_t key_len, uint64_t pkt_bytes,
                                 uint64_t tick)
{
    uint32_t start = (uint32_t) (key_hash & table->slot_mask);

    /* Phase 1: search for existing key, tracking first reusable slot */
    uint32_t candidate_idx = UINT32_MAX;

    for (uint32_t i = 0; i < table->num_slots; i++) {
        uint32_t idx = (start + i) & table->slot_mask;
        infmon_slot_t *slot = &table->slots[idx];
        uint16_t f = __atomic_load_n(&slot->flags, __ATOMIC_ACQUIRE);

        if (f == INFMON_SLOT_OCCUPIED) {
            if (key_matches(table, slot, key_hash, key, key_len)) {
                /* Found — increment counters */
                __atomic_fetch_add(&slot->packets, 1, __ATOMIC_RELAXED);
                __atomic_fetch_add(&slot->bytes, pkt_bytes, __ATOMIC_RELAXED);
                __atomic_store_n(&slot->last_update, tick, __ATOMIC_RELAXED);
                return true;
            }
            continue;
        }

        if (f == INFMON_SLOT_INSERTING) {
            continue;
        }

        if (f == INFMON_SLOT_TOMBSTONE) {
            if (candidate_idx == UINT32_MAX)
                candidate_idx = idx;
            continue; /* keep probing — key may exist past the tombstone */
        }

        /* FREE — key definitely doesn't exist in the table */
        if (candidate_idx == UINT32_MAX)
            candidate_idx = idx;
        break;
    }

    /* Try to insert at the candidate slot */
    if (candidate_idx != UINT32_MAX) {
        infmon_slot_t *slot = &table->slots[candidate_idx];
        uint16_t f = __atomic_load_n(&slot->flags, __ATOMIC_ACQUIRE);
        if (f == INFMON_SLOT_FREE || f == INFMON_SLOT_TOMBSTONE) {
            uint16_t expected = f;
            bool ok = false;
            for (int retry = 0; retry < INFMON_INSERT_RETRY; retry++) {
                expected = f;
                if (__atomic_compare_exchange_n(&slot->flags, &expected, INFMON_SLOT_INSERTING,
                                                false, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
                    ok = true;
                    break;
                }
                if (expected == INFMON_SLOT_OCCUPIED &&
                    key_matches(table, slot, key_hash, key, key_len)) {
                    __atomic_fetch_add(&slot->packets, 1, __ATOMIC_RELAXED);
                    __atomic_fetch_add(&slot->bytes, pkt_bytes, __ATOMIC_RELAXED);
                    __atomic_store_n(&slot->last_update, tick, __ATOMIC_RELAXED);
                    return true;
                }
            }
            if (!ok) {
                __atomic_fetch_add(&table->insert_failed, 1, __ATOMIC_RELAXED);
                return false;
            }

            uint32_t group = candidate_idx / INFMON_SLOTS_PER_GROUP;
            infmon_seqlock_t *sl = &table->seqlocks[group];

            uint32_t key_off = arena_alloc(table, key, key_len);
            if (key_off == UINT32_MAX) {
                __atomic_store_n(&slot->flags, INFMON_SLOT_FREE, __ATOMIC_RELEASE);
                __atomic_fetch_add(&table->insert_failed, 1, __ATOMIC_RELAXED);
                return false;
            }

            seqlock_write_begin(sl);
            slot->key_hash = key_hash;
            slot->packets = 1;
            slot->bytes = pkt_bytes;
            slot->key_offset = key_off;
            slot->key_len = key_len;
            slot->last_update = tick;
            seqlock_write_end(sl);
            __atomic_store_n(&slot->flags, INFMON_SLOT_OCCUPIED, __ATOMIC_RELEASE);

            __atomic_fetch_add(&table->occupied_count, 1, __ATOMIC_RELAXED);
            return true;
        }
    }

    /* Table is full — attempt LRU eviction, retry once */
    __atomic_fetch_add(&table->table_full, 1, __ATOMIC_RELAXED);
    if (!evict_lru(table, start, tick)) {
        __atomic_fetch_add(&table->insert_failed, 1, __ATOMIC_RELAXED);
        return false;
    }

    /* Single retry: scan again for the newly freed slot */
    for (uint32_t i = 0; i < table->num_slots; i++) {
        uint32_t idx = (start + i) & table->slot_mask;
        infmon_slot_t *slot = &table->slots[idx];
        uint16_t f = __atomic_load_n(&slot->flags, __ATOMIC_ACQUIRE);

        if (f == INFMON_SLOT_OCCUPIED) {
            if (key_matches(table, slot, key_hash, key, key_len)) {
                __atomic_fetch_add(&slot->packets, 1, __ATOMIC_RELAXED);
                __atomic_fetch_add(&slot->bytes, pkt_bytes, __ATOMIC_RELAXED);
                __atomic_store_n(&slot->last_update, tick, __ATOMIC_RELAXED);
                return true;
            }
            continue;
        }

        if (f == INFMON_SLOT_INSERTING)
            continue;

        if (f == INFMON_SLOT_FREE || f == INFMON_SLOT_TOMBSTONE) {
            uint16_t expected = f;
            if (__atomic_compare_exchange_n(&slot->flags, &expected, INFMON_SLOT_INSERTING, false,
                                            __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
                uint32_t key_off = arena_alloc(table, key, key_len);
                if (key_off == UINT32_MAX) {
                    __atomic_store_n(&slot->flags, INFMON_SLOT_FREE, __ATOMIC_RELEASE);
                    __atomic_fetch_add(&table->insert_failed, 1, __ATOMIC_RELAXED);
                    return false;
                }
                uint32_t group = idx / INFMON_SLOTS_PER_GROUP;
                infmon_seqlock_t *sl = &table->seqlocks[group];
                seqlock_write_begin(sl);
                slot->key_hash = key_hash;
                slot->packets = 1;
                slot->bytes = pkt_bytes;
                slot->key_offset = key_off;
                slot->key_len = key_len;
                slot->last_update = tick;
                seqlock_write_end(sl);
                __atomic_store_n(&slot->flags, INFMON_SLOT_OCCUPIED, __ATOMIC_RELEASE);
                __atomic_fetch_add(&table->occupied_count, 1, __ATOMIC_RELAXED);
                return true;
            }
        }
    }

    __atomic_fetch_add(&table->insert_failed, 1, __ATOMIC_RELAXED);
    return false;
}

/* ── Read-side operations ────────────────────────────────────────── */

bool infmon_counter_table_read_slot(const infmon_counter_table_t *table, uint32_t index,
                                    infmon_slot_t *out)
{
    if (index >= table->num_slots)
        return false;

    uint32_t group = index / INFMON_SLOTS_PER_GROUP;
    const infmon_seqlock_t *sl = &table->seqlocks[group];

    for (int attempt = 0; attempt < 16; attempt++) {
        uint32_t seq = seqlock_read_begin(sl);
        *out = table->slots[index];
        if (!seqlock_read_retry(sl, seq))
            return true;
    }
    return false;
}

const uint8_t *infmon_counter_table_key(const infmon_counter_table_t *table,
                                        const infmon_slot_t *slot)
{
    if (!slot || slot->flags != INFMON_SLOT_OCCUPIED)
        return NULL;
    if (slot->key_offset + slot->key_len > table->key_arena_capacity)
        return NULL;
    return table->key_arena + slot->key_offset;
}
