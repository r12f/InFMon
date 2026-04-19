/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * Flow-rule data model — see specs/002-flow-tracking-model.md
 */

#ifndef INFMON_FLOW_RULE_H
#define INFMON_FLOW_RULE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Field enum ──────────────────────────────────────────────────── */

typedef enum {
    INFMON_FIELD_SRC_IP = 0,
    INFMON_FIELD_DST_IP,
    INFMON_FIELD_IP_PROTO,
    INFMON_FIELD_DSCP,
    INFMON_FIELD_MIRROR_SRC_IP,
    INFMON_FIELD__COUNT,
} infmon_field_t;

/* ── Eviction policy ─────────────────────────────────────────────── */

typedef enum {
    INFMON_EVICTION_LRU_DROP = 0,
    INFMON_EVICTION__COUNT,
} infmon_eviction_policy_t;

/* ── CRUD error codes ────────────────────────────────────────────── */

typedef enum {
    INFMON_FLOW_RULE_OK = 0,
    INFMON_FLOW_RULE_ERR_NAME_EXISTS,
    INFMON_FLOW_RULE_ERR_NOT_FOUND,
    INFMON_FLOW_RULE_ERR_INVALID_SPEC,
    INFMON_FLOW_RULE_ERR_BUDGET_EXCEEDED,
    INFMON_FLOW_RULE_ERR_INTERNAL,
} infmon_flow_rule_result_t;

/* ── Constants ───────────────────────────────────────────────────── */

#define INFMON_FLOW_RULE_NAME_MAX 31
#define INFMON_FLOW_RULE_KEY_MAX 64
#define INFMON_FLOW_RULE_FIELDS_MAX INFMON_FIELD__COUNT
#define INFMON_FLOW_RULE_MAX_KEYS_BUDGET (16u * 1024 * 1024) /* 16 Mi */
#define INFMON_FLOW_RULE_SET_MAX 16

/* ── Flow rule (immutable after creation) ────────────────────────── */

typedef struct {
    char name[INFMON_FLOW_RULE_NAME_MAX + 1];
    infmon_field_t fields[INFMON_FLOW_RULE_FIELDS_MAX];
    uint32_t field_count;
    uint32_t max_keys;
    infmon_eviction_policy_t eviction_policy;
    uint32_t key_width; /* computed, cached */
} infmon_flow_rule_t;

/* ── Per-flow-rule metrics ───────────────────────────────────────── */

typedef struct {
    uint64_t flows;           /* gauge: current key count */
    uint64_t evictions_total; /* counter */
    uint64_t drops_total;     /* counter */
    uint64_t packets_total;   /* counter */
    uint64_t bytes_total;     /* counter */
} infmon_flow_rule_metrics_t;

/* ── Normalised flow fields (input to key encoder) ───────────────── */

typedef struct {
    uint8_t src_ip[16];        /* IPv4-mapped-IPv6, network byte order */
    uint8_t dst_ip[16];        /* IPv4-mapped-IPv6, network byte order */
    uint8_t mirror_src_ip[16]; /* IPv4-mapped-IPv6, network byte order */
    uint8_t ip_proto;
    uint8_t dscp; /* 0..63, upper 2 bits zero */
} infmon_flow_fields_t;

/* ── Flow rule set (opaque) ──────────────────────────────────────── */

typedef struct infmon_flow_rule_set infmon_flow_rule_set_t;

/* ── Lifecycle ───────────────────────────────────────────────────── */

infmon_flow_rule_set_t *infmon_flow_rule_set_create(uint32_t max_keys_budget);
void infmon_flow_rule_set_destroy(infmon_flow_rule_set_t *set);

/* ── CRUD ────────────────────────────────────────────────────────── */

infmon_flow_rule_result_t infmon_flow_rule_add(infmon_flow_rule_set_t *set,
                                               const infmon_flow_rule_t *rule);
infmon_flow_rule_result_t infmon_flow_rule_rm(infmon_flow_rule_set_t *set, const char *name);
const infmon_flow_rule_t *infmon_flow_rule_find(const infmon_flow_rule_set_t *set,
                                                const char *name);
uint32_t infmon_flow_rule_count(const infmon_flow_rule_set_t *set);
const infmon_flow_rule_t *infmon_flow_rule_get(const infmon_flow_rule_set_t *set, uint32_t index);

/* ── Validation (single rule, no set constraints) ────────────────── */

infmon_flow_rule_result_t infmon_flow_rule_validate(const infmon_flow_rule_t *rule);

/* ── Key encoding ────────────────────────────────────────────────── */

uint32_t infmon_flow_rule_key_width(const infmon_field_t *fields, uint32_t field_count);

void infmon_flow_rule_encode_key(const infmon_flow_rule_t *rule, const infmon_flow_fields_t *fields,
                                 uint8_t *key_buf);

/* ── Field / policy metadata ─────────────────────────────────────── */

uint32_t infmon_field_width(infmon_field_t field);
const char *infmon_field_name(infmon_field_t field);
bool infmon_field_parse(const char *name, infmon_field_t *out);
const char *infmon_eviction_policy_name(infmon_eviction_policy_t policy);
bool infmon_eviction_policy_parse(const char *name, infmon_eviction_policy_t *out);

#ifdef __cplusplus
}
#endif

#endif /* INFMON_FLOW_RULE_H */
