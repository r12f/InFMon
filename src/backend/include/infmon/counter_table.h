/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * Counter table — see specs/004-backend-architecture.md §5
 */

#ifndef INFMON_COUNTER_TABLE_H
#define INFMON_COUNTER_TABLE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Constants ───────────────────────────────────────────────────── */

#define INFMON_SLOT_FREE      0x0000
#define INFMON_SLOT_OCCUPIED  0x0001
#define INFMON_SLOT_TOMBSTONE 0x0002

#define INFMON_SLOTS_PER_GROUP 8
#define INFMON_INSERT_RETRY    4

/* ── Slot (64 B, cache-line aligned) ─────────────────────────────── */

typedef struct {
    uint64_t key_hash;   /*  0 */
    uint64_t packets;    /*  8  atomic, monotonic */
    uint64_t bytes;      /* 16  atomic, monotonic */
    uint32_t key_offset; /* 24  offset into key arena */
    uint16_t key_len;    /* 28 */
    uint16_t flags;      /* 30  free / occupied / tombstone */
    uint64_t last_update;/* 32  tick counter for LRU eviction */
    uint8_t  _pad[24];   /* 40  pad to 64 B */
} __attribute__((aligned(64))) infmon_slot_t;

/* Verify ABI */
#ifdef __cplusplus
static_assert(sizeof(infmon_slot_t) == 64, "ABI: slot must be 64 B");
#else
_Static_assert(sizeof(infmon_slot_t) == 64, "ABI: slot must be 64 B");
#endif

/* ── Seqlock (per 8-slot group) ──────────────────────────────────── */

typedef struct {
    uint32_t seq;  /* even = stable, odd = write in progress */
} infmon_seqlock_t;

/* ── Counter table ───────────────────────────────────────────────── */

typedef struct {
    infmon_slot_t    *slots;
    uint32_t          num_slots;          /* power of 2 */
    uint32_t          slot_mask;          /* num_slots - 1 */
    uint8_t          *key_arena;
    uint32_t          key_arena_capacity;
    uint32_t          key_arena_used;     /* bump allocator head */
    infmon_seqlock_t *seqlocks;           /* num_slots / INFMON_SLOTS_PER_GROUP */
    uint32_t          num_groups;
    uint64_t          generation;
    uint64_t          epoch_ns;
    uint64_t          insert_failed;      /* cumulative */
    uint64_t          table_full;         /* cumulative */
    uint32_t          occupied_count;     /* current number of occupied slots */
} infmon_counter_table_t;

/* ── Lifecycle ───────────────────────────────────────────────────── */

/**
 * Create a counter table.
 * @param max_keys  Maximum number of keys (rounded up to next power of 2).
 * @param max_key_width  Maximum key size in bytes (for arena sizing).
 * @return Heap-allocated table, or NULL on failure.
 */
infmon_counter_table_t *infmon_counter_table_create(uint32_t max_keys,
                                                     uint32_t max_key_width);

void infmon_counter_table_destroy(infmon_counter_table_t *table);

/* ── Data-path operations ────────────────────────────────────────── */

/**
 * Look up or insert a key, then atomically increment counters.
 *
 * @param table      The counter table.
 * @param key_hash   Full 64-bit hash of the key blob.
 * @param key        Pointer to key blob.
 * @param key_len    Length of key blob in bytes.
 * @param pkt_bytes  Byte count of the packet.
 * @param tick       Current tick counter (for LRU tracking).
 * @return true if counters were updated, false if insert failed (table full or CAS exhausted).
 */
bool infmon_counter_table_update(infmon_counter_table_t *table,
                                  uint64_t key_hash,
                                  const uint8_t *key,
                                  uint16_t key_len,
                                  uint64_t pkt_bytes,
                                  uint64_t tick);

/* ── Read-side operations (for snapshot / stats) ─────────────────── */

/**
 * Read a slot with seqlock protection (for live table reads).
 * Copies slot data into *out. Returns true if a consistent read was obtained.
 */
bool infmon_counter_table_read_slot(const infmon_counter_table_t *table,
                                     uint32_t index,
                                     infmon_slot_t *out);

/**
 * Get a pointer to the key blob for a slot.
 * Only valid for occupied slots. Returns NULL if offset is out of range.
 */
const uint8_t *infmon_counter_table_key(const infmon_counter_table_t *table,
                                         const infmon_slot_t *slot);

/* ── Utility ─────────────────────────────────────────────────────── */

/** Round up to the next power of 2 (returns v if already a power of 2). */
uint32_t infmon_next_pow2(uint32_t v);

#ifdef __cplusplus
}
#endif

#endif /* INFMON_COUNTER_TABLE_H */
