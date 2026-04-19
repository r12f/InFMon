/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * Stats-segment exposure implementation — see specs/004-backend-architecture.md §6
 */

#include "infmon/stats_segment.h"

#include <string.h>

/* ── Lifecycle ───────────────────────────────────────────────────── */

void infmon_stats_registry_init(infmon_stats_registry_t *reg, uintptr_t segment_base)
{
    if (!reg)
        return;
    memset(reg, 0, sizeof(*reg));
    reg->segment_base = segment_base;
}

void infmon_stats_registry_destroy(infmon_stats_registry_t *reg)
{
    if (!reg)
        return;
    for (uint32_t i = 0; i < INFMON_STATS_MAX_DESCRIPTORS; i++)
        __atomic_store_n(&reg->descriptors[i].active, 0, __ATOMIC_RELEASE);
    reg->count = 0;
}

/* ── Publish ─────────────────────────────────────────────────────── */

infmon_stats_result_t infmon_stats_publish(infmon_stats_registry_t *reg,
                                           const infmon_counter_table_t *table,
                                           infmon_flow_rule_id_t flow_rule_id,
                                           uint32_t flow_rule_index)
{
    if (!reg)
        return INFMON_STATS_ERR_INVALID_ARG;
    if (!table)
        return INFMON_STATS_ERR_NULL_TABLE;
    if (reg->count >= INFMON_STATS_MAX_DESCRIPTORS)
        return INFMON_STATS_ERR_REGISTRY_FULL;

    /* Find first inactive slot */
    for (uint32_t i = 0; i < INFMON_STATS_MAX_DESCRIPTORS; i++) {
        if (!reg->descriptors[i].active) {
            infmon_stats_descriptor_t *d = &reg->descriptors[i];
            memset(d, 0, sizeof(*d));

            d->flow_rule_id = flow_rule_id;
            d->flow_rule_index = flow_rule_index;
            d->generation = table->generation;
            d->epoch_ns = table->epoch_ns;

            /* Compute offsets relative to segment base.
             * infmon_stats_offset_of asserts ptr != NULL; table->slots
             * and table->key_arena are guaranteed non-NULL by
             * infmon_counter_table_create (allocation failure returns NULL
             * table, caught by the NULL-table check above). */
            d->slots_offset = infmon_stats_offset_of(reg->segment_base, table->slots);
            d->slots_len = table->num_slots;
            d->key_arena_offset = infmon_stats_offset_of(reg->segment_base, table->key_arena);
            d->key_arena_capacity = table->key_arena_capacity;
            d->key_arena_used = table->key_arena_used;
            d->insert_failed = table->insert_failed;
            d->table_full = table->table_full;

            /* Publish with release semantics so readers see all fields */
            __atomic_store_n(&d->active, 1, __ATOMIC_RELEASE);
            reg->count++;
            return INFMON_STATS_OK;
        }
    }

    /* Should be unreachable given the count check above */
    return INFMON_STATS_ERR_REGISTRY_FULL;
}

/* ── Unpublish ───────────────────────────────────────────────────── */

infmon_stats_result_t infmon_stats_unpublish(infmon_stats_registry_t *reg,
                                             infmon_flow_rule_id_t flow_rule_id,
                                             uint64_t generation)
{
    if (!reg)
        return INFMON_STATS_ERR_INVALID_ARG;

    for (uint32_t i = 0; i < INFMON_STATS_MAX_DESCRIPTORS; i++) {
        infmon_stats_descriptor_t *d = &reg->descriptors[i];
        if (d->active && infmon_flow_rule_id_eq(d->flow_rule_id, flow_rule_id) &&
            d->generation == generation) {
            __atomic_store_n(&d->active, 0, __ATOMIC_RELEASE);
            reg->count--;
            return INFMON_STATS_OK;
        }
    }
    return INFMON_STATS_ERR_NOT_FOUND;
}

uint32_t infmon_stats_unpublish_all(infmon_stats_registry_t *reg,
                                    infmon_flow_rule_id_t flow_rule_id)
{
    if (!reg)
        return 0;

    uint32_t removed = 0;
    for (uint32_t i = 0; i < INFMON_STATS_MAX_DESCRIPTORS; i++) {
        infmon_stats_descriptor_t *d = &reg->descriptors[i];
        if (d->active && infmon_flow_rule_id_eq(d->flow_rule_id, flow_rule_id)) {
            __atomic_store_n(&d->active, 0, __ATOMIC_RELEASE);
            reg->count--;
            removed++;
        }
    }
    return removed;
}

/* ── Refresh ─────────────────────────────────────────────────────── */

infmon_stats_result_t infmon_stats_refresh(infmon_stats_registry_t *reg,
                                           infmon_flow_rule_id_t flow_rule_id, uint64_t generation,
                                           const infmon_counter_table_t *table)
{
    if (!reg || !table)
        return INFMON_STATS_ERR_INVALID_ARG;

    for (uint32_t i = 0; i < INFMON_STATS_MAX_DESCRIPTORS; i++) {
        infmon_stats_descriptor_t *d = &reg->descriptors[i];
        if (d->active && infmon_flow_rule_id_eq(d->flow_rule_id, flow_rule_id) &&
            d->generation == generation) {
            /* Update mutable fields with release stores.
             *
             * Note: three separate atomic stores means a reader may see a
             * partially updated snapshot (e.g. new insert_failed but old
             * key_arena_used).  This is acceptable for stats: readers get
             * eventually-consistent values and never see torn 32/64-bit
             * words.  If consistent snapshots become required, add a
             * sequence counter (bump before/after, reader retries on
             * mismatch). */
            __atomic_store_n(&d->key_arena_used, table->key_arena_used, __ATOMIC_RELEASE);
            __atomic_store_n(&d->insert_failed, table->insert_failed, __ATOMIC_RELEASE);
            __atomic_store_n(&d->table_full, table->table_full, __ATOMIC_RELEASE);
            return INFMON_STATS_OK;
        }
    }
    return INFMON_STATS_ERR_NOT_FOUND;
}

/* ── Queries ─────────────────────────────────────────────────────── */

const infmon_stats_descriptor_t *infmon_stats_find(const infmon_stats_registry_t *reg,
                                                   infmon_flow_rule_id_t flow_rule_id,
                                                   uint64_t generation)
{
    if (!reg)
        return NULL;

    for (uint32_t i = 0; i < INFMON_STATS_MAX_DESCRIPTORS; i++) {
        const infmon_stats_descriptor_t *d = &reg->descriptors[i];
        if (d->active && infmon_flow_rule_id_eq(d->flow_rule_id, flow_rule_id) &&
            d->generation == generation)
            return d;
    }
    return NULL;
}

const infmon_stats_descriptor_t *infmon_stats_find_latest(const infmon_stats_registry_t *reg,
                                                          infmon_flow_rule_id_t flow_rule_id)
{
    if (!reg)
        return NULL;

    const infmon_stats_descriptor_t *best = NULL;
    for (uint32_t i = 0; i < INFMON_STATS_MAX_DESCRIPTORS; i++) {
        const infmon_stats_descriptor_t *d = &reg->descriptors[i];
        if (d->active && infmon_flow_rule_id_eq(d->flow_rule_id, flow_rule_id)) {
            if (!best || d->generation > best->generation)
                best = d;
        }
    }
    return best;
}

uint32_t infmon_stats_count(const infmon_stats_registry_t *reg)
{
    return reg ? reg->count : 0;
}

const infmon_stats_descriptor_t *infmon_stats_get(const infmon_stats_registry_t *reg,
                                                  uint32_t index)
{
    if (!reg)
        return NULL;

    uint32_t seen = 0;
    for (uint32_t i = 0; i < INFMON_STATS_MAX_DESCRIPTORS; i++) {
        if (reg->descriptors[i].active) {
            if (seen == index)
                return &reg->descriptors[i];
            seen++;
        }
    }
    return NULL;
}
