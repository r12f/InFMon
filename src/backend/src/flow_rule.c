/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 */

#include "infmon/flow_rule.h"

#include <stdlib.h>
#include <string.h>

/* ── Field metadata tables ───────────────────────────────────────── */

static const uint32_t field_widths[INFMON_FIELD__COUNT] = {
    [INFMON_FIELD_SRC_IP] = 16,  [INFMON_FIELD_DST_IP] = 16,        [INFMON_FIELD_IP_PROTO] = 1,
    [INFMON_FIELD_DSCP] = 1,     [INFMON_FIELD_MIRROR_SRC_IP] = 16, [INFMON_FIELD_SRC_PORT] = 2,
    [INFMON_FIELD_DST_PORT] = 2,
};

static const char *field_names[INFMON_FIELD__COUNT] = {
    [INFMON_FIELD_SRC_IP] = "src_ip",
    [INFMON_FIELD_DST_IP] = "dst_ip",
    [INFMON_FIELD_IP_PROTO] = "ip_proto",
    [INFMON_FIELD_DSCP] = "dscp",
    [INFMON_FIELD_MIRROR_SRC_IP] = "mirror_src_ip",
    [INFMON_FIELD_SRC_PORT] = "src_port",
    [INFMON_FIELD_DST_PORT] = "dst_port",
};

static const char *eviction_names[INFMON_EVICTION__COUNT] = {
    [INFMON_EVICTION_LRU_DROP] = "lru_drop",
};

uint32_t infmon_field_width(infmon_field_t field)
{
    if ((unsigned) field >= INFMON_FIELD__COUNT)
        return 0;
    return field_widths[field];
}

const char *infmon_field_name(infmon_field_t field)
{
    if ((unsigned) field >= INFMON_FIELD__COUNT)
        return NULL;
    return field_names[field];
}

bool infmon_field_parse(const char *name, infmon_field_t *out)
{
    if (!name)
        return false;
    for (int i = 0; i < INFMON_FIELD__COUNT; i++) {
        if (strcmp(name, field_names[i]) == 0) {
            if (out)
                *out = (infmon_field_t) i;
            return true;
        }
    }
    return false;
}

const char *infmon_eviction_policy_name(infmon_eviction_policy_t policy)
{
    if ((unsigned) policy >= INFMON_EVICTION__COUNT)
        return NULL;
    return eviction_names[policy];
}

bool infmon_eviction_policy_parse(const char *name, infmon_eviction_policy_t *out)
{
    if (!name)
        return false;
    for (int i = 0; i < INFMON_EVICTION__COUNT; i++) {
        if (strcmp(name, eviction_names[i]) == 0) {
            if (out)
                *out = (infmon_eviction_policy_t) i;
            return true;
        }
    }
    return false;
}

/* ── Name validation: ^[a-z0-9][a-z0-9_-]{1,30}$ ────────────────── */

static bool name_valid(const char *name)
{
    if (!name)
        return false;
    size_t len = strlen(name);
    if (len < 2 || len > INFMON_FLOW_RULE_NAME_MAX)
        return false;

    /* First char: [a-z0-9] */
    char c = name[0];
    if (!((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9')))
        return false;

    for (size_t i = 1; i < len; i++) {
        c = name[i];
        if (!((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_' || c == '-'))
            return false;
    }
    return true;
}

/* ── Key width ───────────────────────────────────────────────────── */

uint32_t infmon_flow_rule_key_width(const infmon_field_t *fields, uint32_t field_count)
{
    uint32_t w = 0;
    for (uint32_t i = 0; i < field_count; i++) {
        uint32_t fw = infmon_field_width(fields[i]);
        if (fw == 0)
            return 0;
        w += fw;
    }
    return w;
}

/* ── Validation (single rule) ────────────────────────────────────── */

infmon_flow_rule_result_t infmon_flow_rule_validate(const infmon_flow_rule_t *rule)
{
    if (!rule)
        return INFMON_FLOW_RULE_ERR_INVALID_SPEC;

    if (!name_valid(rule->name))
        return INFMON_FLOW_RULE_ERR_INVALID_SPEC;

    if (rule->field_count == 0 || rule->field_count > INFMON_FLOW_RULE_FIELDS_MAX)
        return INFMON_FLOW_RULE_ERR_INVALID_SPEC;

    /* Check unknown fields and duplicates */
    bool seen[INFMON_FIELD__COUNT] = {false};
    for (uint32_t i = 0; i < rule->field_count; i++) {
        if ((unsigned) rule->fields[i] >= INFMON_FIELD__COUNT)
            return INFMON_FLOW_RULE_ERR_INVALID_SPEC;
        if (seen[rule->fields[i]])
            return INFMON_FLOW_RULE_ERR_INVALID_SPEC;
        seen[rule->fields[i]] = true;
    }

    if (rule->max_keys == 0)
        return INFMON_FLOW_RULE_ERR_INVALID_SPEC;
    if (rule->max_keys > INFMON_FLOW_RULE_MAX_KEYS_BUDGET)
        return INFMON_FLOW_RULE_ERR_INVALID_SPEC;

    if ((unsigned) rule->eviction_policy >= INFMON_EVICTION__COUNT)
        return INFMON_FLOW_RULE_ERR_INVALID_SPEC;

    uint32_t kw = infmon_flow_rule_key_width(rule->fields, rule->field_count);
    if (kw == 0 || kw > INFMON_FLOW_RULE_KEY_MAX)
        return INFMON_FLOW_RULE_ERR_INVALID_SPEC;

    return INFMON_FLOW_RULE_OK;
}

/* ── Key encoding ────────────────────────────────────────────────── */

void infmon_flow_rule_encode_key(const infmon_flow_rule_t *rule, const infmon_flow_fields_t *fields,
                                 uint8_t *key_buf)
{
    if (!rule || !fields || !key_buf)
        return;

    uint32_t off = 0;
    for (uint32_t i = 0; i < rule->field_count; i++) {
        switch (rule->fields[i]) {
        case INFMON_FIELD_SRC_IP:
            memcpy(key_buf + off, fields->src_ip, 16);
            off += 16;
            break;
        case INFMON_FIELD_DST_IP:
            memcpy(key_buf + off, fields->dst_ip, 16);
            off += 16;
            break;
        case INFMON_FIELD_IP_PROTO:
            key_buf[off++] = fields->ip_proto;
            break;
        case INFMON_FIELD_DSCP:
            key_buf[off++] = fields->dscp & 0x3F;
            break;
        case INFMON_FIELD_MIRROR_SRC_IP:
            memcpy(key_buf + off, fields->mirror_src_ip, 16);
            off += 16;
            break;
        case INFMON_FIELD_SRC_PORT:
            key_buf[off] = (uint8_t) (fields->src_port >> 8);
            key_buf[off + 1] = (uint8_t) (fields->src_port & 0xFF);
            off += 2;
            break;
        case INFMON_FIELD_DST_PORT:
            key_buf[off] = (uint8_t) (fields->dst_port >> 8);
            key_buf[off + 1] = (uint8_t) (fields->dst_port & 0xFF);
            off += 2;
            break;
        default:
            break;
        }
    }
}

/* ── Flow rule set ───────────────────────────────────────────────── */

struct infmon_flow_rule_set {
    infmon_flow_rule_t rules[INFMON_FLOW_RULE_SET_MAX];
    uint32_t count;
    uint32_t max_keys_budget;
    uint32_t used_keys;
};

infmon_flow_rule_set_t *infmon_flow_rule_set_create(uint32_t max_keys_budget)
{
    if (max_keys_budget > INFMON_FLOW_RULE_MAX_KEYS_BUDGET)
        return NULL;
    infmon_flow_rule_set_t *s = calloc(1, sizeof(*s));
    if (!s)
        return NULL;
    s->max_keys_budget = max_keys_budget;
    return s;
}

void infmon_flow_rule_set_destroy(infmon_flow_rule_set_t *set)
{
    free(set);
}

uint32_t infmon_flow_rule_count(const infmon_flow_rule_set_t *set)
{
    return set ? set->count : 0;
}

const infmon_flow_rule_t *infmon_flow_rule_find(const infmon_flow_rule_set_t *set, const char *name)
{
    if (!set || !name)
        return NULL;
    for (uint32_t i = 0; i < set->count; i++) {
        if (strcmp(set->rules[i].name, name) == 0)
            return &set->rules[i];
    }
    return NULL;
}

const infmon_flow_rule_t *infmon_flow_rule_get(const infmon_flow_rule_set_t *set, uint32_t index)
{
    if (!set || index >= set->count)
        return NULL;
    return &set->rules[index];
}

infmon_flow_rule_result_t infmon_flow_rule_add(infmon_flow_rule_set_t *set,
                                               const infmon_flow_rule_t *rule)
{
    if (!set || !rule)
        return INFMON_FLOW_RULE_ERR_INTERNAL;

    infmon_flow_rule_result_t rc = infmon_flow_rule_validate(rule);
    if (rc != INFMON_FLOW_RULE_OK)
        return rc;

    if (set->count >= INFMON_FLOW_RULE_SET_MAX)
        return INFMON_FLOW_RULE_ERR_SET_FULL;

    if (infmon_flow_rule_find(set, rule->name))
        return INFMON_FLOW_RULE_ERR_NAME_EXISTS;

    if ((uint64_t) set->used_keys + rule->max_keys > set->max_keys_budget)
        return INFMON_FLOW_RULE_ERR_BUDGET_EXCEEDED;

    infmon_flow_rule_t *dst = &set->rules[set->count];
    *dst = *rule;
    dst->key_width = infmon_flow_rule_key_width(rule->fields, rule->field_count);
    set->used_keys += dst->max_keys;
    set->count++;
    return INFMON_FLOW_RULE_OK;
}

infmon_flow_rule_result_t infmon_flow_rule_rm(infmon_flow_rule_set_t *set, const char *name)
{
    if (!set || !name)
        return INFMON_FLOW_RULE_ERR_INTERNAL;

    for (uint32_t i = 0; i < set->count; i++) {
        if (strcmp(set->rules[i].name, name) == 0) {
            set->used_keys -= set->rules[i].max_keys;
            /* Shift remaining rules down */
            for (uint32_t j = i; j + 1 < set->count; j++)
                set->rules[j] = set->rules[j + 1];
            set->count--;
            memset(&set->rules[set->count], 0, sizeof(set->rules[0]));
            return INFMON_FLOW_RULE_OK;
        }
    }
    return INFMON_FLOW_RULE_ERR_NOT_FOUND;
}
