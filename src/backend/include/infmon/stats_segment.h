/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * Stats-segment exposure — per-flow-rule descriptors with offsets.
 * See specs/004-backend-architecture.md §6
 *
 * The frontend mmaps the VPP stats segment at an arbitrary virtual address,
 * so all "pointer-shaped" fields are byte offsets from the stats-segment
 * base, never raw pointers.  This header defines the descriptor layout
 * and a portable registry that can be wired to VPP's stat_directory in
 * the real plugin.
 */

#ifndef INFMON_STATS_SEGMENT_H
#define INFMON_STATS_SEGMENT_H

#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "infmon/counter_table.h"

#ifdef __cplusplus
extern "C" {
#endif

/* ── Constants ───────────────────────────────────────────────────── */

/** Maximum number of descriptors in the registry. */
#define INFMON_STATS_MAX_DESCRIPTORS 128

/* ── flow_rule_id (128-bit UUID) ─────────────────────────────────── */

typedef struct {
    uint64_t hi; /**< Upper 64 bits. */
    uint64_t lo; /**< Lower 64 bits. */
} infmon_flow_rule_id_t;

static inline bool infmon_flow_rule_id_eq(infmon_flow_rule_id_t a, infmon_flow_rule_id_t b)
{
    return a.hi == b.hi && a.lo == b.lo;
}

/* Used by future flow_rule lifecycle management (publish-if-nonzero guard). */
static inline bool infmon_flow_rule_id_is_zero(infmon_flow_rule_id_t id) __attribute__((unused));
static inline bool infmon_flow_rule_id_is_zero(infmon_flow_rule_id_t id)
{
    return id.hi == 0 && id.lo == 0;
}

/* ── Per-table descriptor ────────────────────────────────────────── */

/**
 * Descriptor published in the stats segment under
 * /infmon/<flow_rule_id>/<generation>.
 *
 * All offset fields are byte offsets from the stats-segment base address.
 * The frontend resolves them as: ptr = segment_base + offset.
 *
 * This struct is designed to be placed directly in shared memory
 * (stats segment) and read by an untrusted reader (the frontend)
 * via mmap.  Therefore:
 *   - No pointers (only offsets).
 *   - Fixed-size, no padding ambiguity (explicit layout).
 *   - Reader must validate offsets against segment bounds.
 */
typedef struct {
    infmon_flow_rule_id_t flow_rule_id; /*  0: UUID of the flow_rule */
    uint32_t flow_rule_index;           /* 16: internal handle */
    uint32_t _pad0;                     /* 20: alignment */
    uint64_t generation;                /* 24: bumped on each snapshot_and_clear */
    uint64_t epoch_ns;                  /* 32: wall-clock at table creation */
    uint64_t slots_offset;              /* 40: byte offset to slot array */
    uint32_t slots_len;                 /* 48: number of slots */
    uint32_t _pad1;                     /* 52: alignment */
    uint64_t key_arena_offset;          /* 56: byte offset to key arena */
    uint32_t key_arena_capacity;        /* 64: total bytes allocated */
    uint32_t key_arena_used;            /* 68: high-water mark (bump head) */
    uint64_t insert_failed;             /* 72: cumulative */
    uint64_t table_full;                /* 80: cumulative */
    uint8_t active;                     /* 88: 1 if this descriptor is live (uint8_t for
                                         *     guaranteed 1-byte layout in shared memory) */
    uint8_t _pad2[7];                   /* 89: pad to 96 B total */
} infmon_stats_descriptor_t;

#ifdef __cplusplus
static_assert(sizeof(infmon_stats_descriptor_t) == 96,
              "descriptor must be exactly 96 bytes for shared-memory layout stability");
#else
_Static_assert(sizeof(infmon_stats_descriptor_t) == 96,
               "descriptor must be exactly 96 bytes for shared-memory layout stability");
#endif

/* ── Stats-segment registry ──────────────────────────────────────── */

/**
 * The registry holds all published descriptors.  In the real VPP plugin
 * this maps to stat_directory entries; this portable implementation uses
 * a flat array so the logic can be unit-tested without VPP.
 *
 * Thread safety:
 *   - The control thread calls add/remove/update (serialised).
 *   - The frontend reads descriptors from shared memory (read-only mmap).
 */
typedef struct {
    infmon_stats_descriptor_t descriptors[INFMON_STATS_MAX_DESCRIPTORS];
    uint32_t count; /**< Number of active descriptors. */

    /**
     * Simulated stats-segment base address.  In the real VPP plugin this
     * is the start of the mmap'd stats segment.  For testing we set it
     * to a known base and compute offsets relative to it.
     */
    uintptr_t segment_base;

    /**
     * Total size (bytes) of the stats segment.  Used by checked resolve
     * helpers for bounds validation.  Set to 0 to disable bounds checks
     * (e.g. in unit tests where the segment is simulated).
     */
    uint64_t segment_size;
} infmon_stats_registry_t;

/* ── Result codes ────────────────────────────────────────────────── */

typedef enum {
    INFMON_STATS_OK = 0,
    INFMON_STATS_ERR_REGISTRY_FULL,
    INFMON_STATS_ERR_NOT_FOUND,
    INFMON_STATS_ERR_NULL_TABLE,
    INFMON_STATS_ERR_INVALID_ARG,
} infmon_stats_result_t;

/* ── Lifecycle ───────────────────────────────────────────────────── */

/**
 * Initialise the registry.
 *
 * @param reg           Registry to initialise.
 * @param segment_base  Base address of the stats segment (for offset computation).
 */
void infmon_stats_registry_init(infmon_stats_registry_t *reg, uintptr_t segment_base);

/**
 * Destroy the registry (marks all descriptors inactive).
 */
void infmon_stats_registry_destroy(infmon_stats_registry_t *reg);

/* ── Descriptor management ───────────────────────────────────────── */

/**
 * Publish a counter table as a new descriptor in the registry.
 *
 * Computes byte offsets for slots and key_arena relative to segment_base.
 *
 * @param reg              Registry.
 * @param table            Counter table to expose (must outlive the descriptor).
 * @param flow_rule_id     External UUID of the flow_rule.
 * @param flow_rule_index  Internal index.
 * @return INFMON_STATS_OK on success.
 */
infmon_stats_result_t infmon_stats_publish(infmon_stats_registry_t *reg,
                                           const infmon_counter_table_t *table,
                                           infmon_flow_rule_id_t flow_rule_id,
                                           uint32_t flow_rule_index);

/**
 * Remove a descriptor by flow_rule_id and generation.
 *
 * @return INFMON_STATS_OK on success, INFMON_STATS_ERR_NOT_FOUND if no match.
 */
infmon_stats_result_t infmon_stats_unpublish(infmon_stats_registry_t *reg,
                                             infmon_flow_rule_id_t flow_rule_id,
                                             uint64_t generation);

/**
 * Remove all descriptors for a given flow_rule_id (all generations).
 *
 * @return Number of descriptors removed.
 */
uint32_t infmon_stats_unpublish_all(infmon_stats_registry_t *reg,
                                    infmon_flow_rule_id_t flow_rule_id);

/**
 * Refresh a descriptor's mutable fields from its counter table.
 *
 * Updates key_arena_used, insert_failed, and table_full from the live
 * table data.  Called periodically by the control thread.
 *
 * @return INFMON_STATS_OK or INFMON_STATS_ERR_NOT_FOUND.
 */
infmon_stats_result_t infmon_stats_refresh(infmon_stats_registry_t *reg,
                                           infmon_flow_rule_id_t flow_rule_id, uint64_t generation,
                                           const infmon_counter_table_t *table);

/* ── Queries ─────────────────────────────────────────────────────── */

/**
 * Find a descriptor by flow_rule_id and generation.
 *
 * @return Pointer to the descriptor, or NULL if not found.
 */
const infmon_stats_descriptor_t *infmon_stats_find(const infmon_stats_registry_t *reg,
                                                   infmon_flow_rule_id_t flow_rule_id,
                                                   uint64_t generation);

/**
 * Find the latest-generation descriptor for a given flow_rule_id.
 *
 * @return Pointer to the descriptor with the highest generation, or NULL.
 */
const infmon_stats_descriptor_t *infmon_stats_find_latest(const infmon_stats_registry_t *reg,
                                                          infmon_flow_rule_id_t flow_rule_id);

/**
 * Get the number of active descriptors in the registry.
 */
uint32_t infmon_stats_count(const infmon_stats_registry_t *reg);

/**
 * Get a descriptor by index (for enumeration).
 *
 * Iterates only over active descriptors; index is 0-based in the
 * active set.  Returns NULL if index >= active count.
 */
/**
 * Note: infmon_stats_get performs a linear scan to find the Nth active
 * descriptor, so full enumeration via for(i=0;i<count;i++) get(reg,i)
 * is O(count × MAX_DESCRIPTORS).  With MAX_DESCRIPTORS=128 this is
 * acceptable; if the limit grows, consider adding an iterator API.
 */
const infmon_stats_descriptor_t *infmon_stats_get(const infmon_stats_registry_t *reg,
                                                  uint32_t index);

/* ── Frontend-side helpers ───────────────────────────────────────── */

/**
 * Resolve a byte offset to a pointer, given the segment base.
 */
static inline void *infmon_stats_resolve(uintptr_t segment_base, uint64_t offset)
{
    return (void *) (segment_base + offset);
}

/**
 * Compute the byte offset of a pointer relative to a base.
 */
static inline uint64_t infmon_stats_offset_of(uintptr_t segment_base, const void *ptr)
{
    assert(ptr != NULL && "infmon_stats_offset_of: ptr must not be NULL");
    assert((uintptr_t) ptr >= segment_base &&
           "infmon_stats_offset_of: ptr must be >= segment_base (unsigned underflow)");
    return (uint64_t) ((uintptr_t) ptr - segment_base);
}

#ifdef __cplusplus
}
#endif

#endif /* INFMON_STATS_SEGMENT_H */
