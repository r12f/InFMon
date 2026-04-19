/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 */

#include <cstring>
#include <gtest/gtest.h>

extern "C" {
#include "infmon/flow_rule.h"
}

/* ── Helpers ─────────────────────────────────────────────────────── */

static infmon_flow_rule_t make_rule(const char *name, const infmon_field_t *fields, uint32_t fc,
                                    uint32_t max_keys = 1024)
{
    infmon_flow_rule_t r{};
    std::memset(r.name, 0, sizeof(r.name));
    std::strncpy(r.name, name, sizeof(r.name) - 1);
    if (fc > 0) {
        std::memcpy(r.fields, fields, fc * sizeof(infmon_field_t));
    }
    r.field_count = fc;
    r.max_keys = max_keys;
    r.eviction_policy = INFMON_EVICTION_LRU_DROP;
    return r;
}

/* ── 1. Field width / name lookups ───────────────────────────────── */

TEST(FieldMeta, Widths)
{
    EXPECT_EQ(infmon_field_width(INFMON_FIELD_SRC_IP), 16u);
    EXPECT_EQ(infmon_field_width(INFMON_FIELD_DST_IP), 16u);
    EXPECT_EQ(infmon_field_width(INFMON_FIELD_IP_PROTO), 1u);
    EXPECT_EQ(infmon_field_width(INFMON_FIELD_DSCP), 1u);
    EXPECT_EQ(infmon_field_width(INFMON_FIELD_MIRROR_SRC_IP), 16u);
    EXPECT_EQ(infmon_field_width((infmon_field_t) 99), 0u);
}

TEST(FieldMeta, Names)
{
    EXPECT_STREQ(infmon_field_name(INFMON_FIELD_SRC_IP), "src_ip");
    EXPECT_STREQ(infmon_field_name(INFMON_FIELD_DSCP), "dscp");
    EXPECT_EQ(infmon_field_name((infmon_field_t) 99), nullptr);
}

TEST(FieldMeta, Parse)
{
    infmon_field_t f;
    EXPECT_TRUE(infmon_field_parse("src_ip", &f));
    EXPECT_EQ(f, INFMON_FIELD_SRC_IP);
    EXPECT_TRUE(infmon_field_parse("mirror_src_ip", &f));
    EXPECT_EQ(f, INFMON_FIELD_MIRROR_SRC_IP);
    EXPECT_FALSE(infmon_field_parse("bogus", &f));
    EXPECT_FALSE(infmon_field_parse(nullptr, &f));
}

TEST(FieldMeta, EvictionPolicy)
{
    EXPECT_STREQ(infmon_eviction_policy_name(INFMON_EVICTION_LRU_DROP), "lru_drop");
    EXPECT_EQ(infmon_eviction_policy_name((infmon_eviction_policy_t) 99), nullptr);

    infmon_eviction_policy_t p;
    EXPECT_TRUE(infmon_eviction_policy_parse("lru_drop", &p));
    EXPECT_EQ(p, INFMON_EVICTION_LRU_DROP);
    EXPECT_FALSE(infmon_eviction_policy_parse("fifo", &p));
}

/* ── 2. Name validation ─────────────────────────────────────────── */

TEST(Validation, NameValid)
{
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP};
    auto r = make_rule("ab", f, 1);
    EXPECT_EQ(infmon_flow_rule_validate(&r), INFMON_FLOW_RULE_OK);

    r = make_rule("my-rule_01", f, 1);
    EXPECT_EQ(infmon_flow_rule_validate(&r), INFMON_FLOW_RULE_OK);
}

TEST(Validation, NameTooShort)
{
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP};
    auto r = make_rule("a", f, 1);
    EXPECT_EQ(infmon_flow_rule_validate(&r), INFMON_FLOW_RULE_ERR_INVALID_SPEC);
}

TEST(Validation, NameMaxLengthOk)
{
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP};
    // 31 chars = exactly at limit, should be valid
    auto r = make_rule("abcdefghijklmnopqrstuvwxyz01234", f, 1);
    EXPECT_EQ(infmon_flow_rule_validate(&r), INFMON_FLOW_RULE_OK);
    // Buffer is 32 bytes (31+1), can't store >31 chars, so too-long is
    // prevented by the buffer size itself.
}

TEST(Validation, NameInvalidChars)
{
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP};
    auto r = make_rule("AB-cd", f, 1);
    EXPECT_EQ(infmon_flow_rule_validate(&r), INFMON_FLOW_RULE_ERR_INVALID_SPEC);
}

TEST(Validation, NameStartsWithHyphen)
{
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP};
    auto r = make_rule("-abc", f, 1);
    EXPECT_EQ(infmon_flow_rule_validate(&r), INFMON_FLOW_RULE_ERR_INVALID_SPEC);
}

/* ── 3. Flow rule validation ────────────────────────────────────── */

TEST(Validation, EmptyFields)
{
    auto r = make_rule("test01", nullptr, 0);
    EXPECT_EQ(infmon_flow_rule_validate(&r), INFMON_FLOW_RULE_ERR_INVALID_SPEC);
}

TEST(Validation, DuplicateFields)
{
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP, INFMON_FIELD_SRC_IP};
    auto r = make_rule("test01", f, 2);
    EXPECT_EQ(infmon_flow_rule_validate(&r), INFMON_FLOW_RULE_ERR_INVALID_SPEC);
}

TEST(Validation, UnknownField)
{
    infmon_field_t f[] = {(infmon_field_t) 99};
    auto r = make_rule("test01", f, 1);
    EXPECT_EQ(infmon_flow_rule_validate(&r), INFMON_FLOW_RULE_ERR_INVALID_SPEC);
}

TEST(Validation, ZeroMaxKeys)
{
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP};
    auto r = make_rule("test01", f, 1, 0);
    EXPECT_EQ(infmon_flow_rule_validate(&r), INFMON_FLOW_RULE_ERR_INVALID_SPEC);
}

TEST(Validation, BadEvictionPolicy)
{
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP};
    auto r = make_rule("test01", f, 1);
    r.eviction_policy = (infmon_eviction_policy_t) 99;
    EXPECT_EQ(infmon_flow_rule_validate(&r), INFMON_FLOW_RULE_ERR_INVALID_SPEC);
}

TEST(Validation, ValidAllFields)
{
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP, INFMON_FIELD_DST_IP, INFMON_FIELD_IP_PROTO,
                          INFMON_FIELD_DSCP, INFMON_FIELD_MIRROR_SRC_IP};
    auto r = make_rule("all-fields", f, 5);
    EXPECT_EQ(infmon_flow_rule_validate(&r), INFMON_FLOW_RULE_OK);
    // Total width = 16+16+1+1+16 = 50, within 64
}

/* ── 4. Flow rule set CRUD ──────────────────────────────────────── */

TEST(RuleSet, AddFindRm)
{
    auto *set = infmon_flow_rule_set_create(INFMON_FLOW_RULE_MAX_KEYS_BUDGET);
    ASSERT_NE(set, nullptr);

    infmon_field_t f[] = {INFMON_FIELD_SRC_IP, INFMON_FIELD_DST_IP};
    auto r = make_rule("my-rule", f, 2, 100);

    EXPECT_EQ(infmon_flow_rule_add(set, &r), INFMON_FLOW_RULE_OK);
    EXPECT_EQ(infmon_flow_rule_count(set), 1u);

    auto *found = infmon_flow_rule_find(set, "my-rule");
    ASSERT_NE(found, nullptr);
    EXPECT_STREQ(found->name, "my-rule");
    EXPECT_EQ(found->key_width, 32u);

    EXPECT_EQ(infmon_flow_rule_rm(set, "my-rule"), INFMON_FLOW_RULE_OK);
    EXPECT_EQ(infmon_flow_rule_count(set), 0u);
    EXPECT_EQ(infmon_flow_rule_find(set, "my-rule"), nullptr);

    infmon_flow_rule_set_destroy(set);
}

TEST(RuleSet, DuplicateName)
{
    auto *set = infmon_flow_rule_set_create(INFMON_FLOW_RULE_MAX_KEYS_BUDGET);
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP};
    auto r = make_rule("dup-rule", f, 1, 10);

    EXPECT_EQ(infmon_flow_rule_add(set, &r), INFMON_FLOW_RULE_OK);
    EXPECT_EQ(infmon_flow_rule_add(set, &r), INFMON_FLOW_RULE_ERR_NAME_EXISTS);

    infmon_flow_rule_set_destroy(set);
}

TEST(RuleSet, BudgetExceeded)
{
    auto *set = infmon_flow_rule_set_create(100);
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP};
    auto r = make_rule("rule-aa", f, 1, 60);
    EXPECT_EQ(infmon_flow_rule_add(set, &r), INFMON_FLOW_RULE_OK);

    auto r2 = make_rule("rule-bb", f, 1, 41);
    EXPECT_EQ(infmon_flow_rule_add(set, &r2), INFMON_FLOW_RULE_ERR_BUDGET_EXCEEDED);

    auto r3 = make_rule("rule-cc", f, 1, 40);
    EXPECT_EQ(infmon_flow_rule_add(set, &r3), INFMON_FLOW_RULE_OK);

    infmon_flow_rule_set_destroy(set);
}

TEST(RuleSet, RmNotFound)
{
    auto *set = infmon_flow_rule_set_create(100);
    EXPECT_EQ(infmon_flow_rule_rm(set, "nope"), INFMON_FLOW_RULE_ERR_NOT_FOUND);
    infmon_flow_rule_set_destroy(set);
}

TEST(RuleSet, GetByIndex)
{
    auto *set = infmon_flow_rule_set_create(INFMON_FLOW_RULE_MAX_KEYS_BUDGET);
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP};
    auto r1 = make_rule("rule-aa", f, 1, 10);
    auto r2 = make_rule("rule-bb", f, 1, 10);
    infmon_flow_rule_add(set, &r1);
    infmon_flow_rule_add(set, &r2);

    EXPECT_STREQ(infmon_flow_rule_get(set, 0)->name, "rule-aa");
    EXPECT_STREQ(infmon_flow_rule_get(set, 1)->name, "rule-bb");
    EXPECT_EQ(infmon_flow_rule_get(set, 2), nullptr);

    infmon_flow_rule_set_destroy(set);
}

/* ── 5. Key encoding ────────────────────────────────────────────── */

TEST(KeyEncode, BasicLayout)
{
    infmon_field_t f[] = {INFMON_FIELD_IP_PROTO, INFMON_FIELD_SRC_IP};
    auto r = make_rule("ke-test", f, 2);
    r.key_width = infmon_flow_rule_key_width(f, 2);
    EXPECT_EQ(r.key_width, 17u);

    infmon_flow_fields_t ff{};
    ff.ip_proto = 6; // TCP
    // src_ip = ::ffff:10.0.0.1 (IPv4-mapped)
    ff.src_ip[10] = 0xff;
    ff.src_ip[11] = 0xff;
    ff.src_ip[12] = 10;
    ff.src_ip[13] = 0;
    ff.src_ip[14] = 0;
    ff.src_ip[15] = 1;

    uint8_t key[64] = {};
    infmon_flow_rule_encode_key(&r, &ff, key);

    EXPECT_EQ(key[0], 6); // ip_proto first
    // Then 16 bytes of src_ip
    EXPECT_EQ(key[1], 0); // first 10 bytes zero
    EXPECT_EQ(key[11], 0xff);
    EXPECT_EQ(key[12], 0xff);
    EXPECT_EQ(key[13], 10);
    EXPECT_EQ(key[14], 0);
    EXPECT_EQ(key[15], 0);
    EXPECT_EQ(key[16], 1);
}

/* ── 6. IPv4-mapped-IPv6 in keys ─────────────────────────────────── */

TEST(KeyEncode, IPv4Mapped)
{
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP};
    auto r = make_rule("v4map", f, 1);
    r.key_width = 16;

    // Build ::ffff:192.168.1.2
    infmon_flow_fields_t ff{};
    std::memset(ff.src_ip, 0, 16);
    ff.src_ip[10] = 0xff;
    ff.src_ip[11] = 0xff;
    ff.src_ip[12] = 192;
    ff.src_ip[13] = 168;
    ff.src_ip[14] = 1;
    ff.src_ip[15] = 2;

    uint8_t key[16] = {};
    infmon_flow_rule_encode_key(&r, &ff, key);

    // Verify: 10 zero bytes, 0xff, 0xff, 192, 168, 1, 2
    for (int i = 0; i < 10; i++)
        EXPECT_EQ(key[i], 0) << "byte " << i;
    EXPECT_EQ(key[10], 0xff);
    EXPECT_EQ(key[11], 0xff);
    EXPECT_EQ(key[12], 192);
    EXPECT_EQ(key[13], 168);
    EXPECT_EQ(key[14], 1);
    EXPECT_EQ(key[15], 2);
}

TEST(KeyEncode, DscpMasked)
{
    infmon_field_t f[] = {INFMON_FIELD_DSCP};
    auto r = make_rule("dscp-test", f, 1);
    r.key_width = 1;

    infmon_flow_fields_t ff{};
    ff.dscp = 0xFF; // upper bits should be masked

    uint8_t key[1] = {};
    infmon_flow_rule_encode_key(&r, &ff, key);
    EXPECT_EQ(key[0], 0x3F);
}

TEST(KeyWidth, Computation)
{
    infmon_field_t f[] = {INFMON_FIELD_SRC_IP, INFMON_FIELD_DST_IP, INFMON_FIELD_IP_PROTO,
                          INFMON_FIELD_DSCP, INFMON_FIELD_MIRROR_SRC_IP};
    EXPECT_EQ(infmon_flow_rule_key_width(f, 5), 50u);
    EXPECT_EQ(infmon_flow_rule_key_width(f, 0), 0u);

    infmon_field_t bad[] = {(infmon_field_t) 99};
    EXPECT_EQ(infmon_flow_rule_key_width(bad, 1), 0u);
}
