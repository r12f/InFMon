/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 */

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>
#include <gtest/gtest.h>

extern "C" {
#include "infmon/api_handler.h"
}

/* ── Helpers ─────────────────────────────────────────────────────── */

static infmon_flow_rule_t make_rule(const char *name, uint32_t max_keys = 1024)
{
    infmon_flow_rule_t r{};
    snprintf(r.name, sizeof(r.name), "%s", name);
    r.fields[0] = INFMON_FIELD_SRC_IP;
    r.fields[1] = INFMON_FIELD_DST_IP;
    r.field_count = 2;
    r.max_keys = max_keys;
    r.eviction_policy = INFMON_EVICTION_LRU_DROP;
    return r;
}

class ApiHandlerTest : public ::testing::Test
{
  protected:
    void SetUp() override
    {
        rule_set_ = infmon_flow_rule_set_create(INFMON_FLOW_RULE_MAX_KEYS_BUDGET);
        ASSERT_NE(rule_set_, nullptr);
        infmon_stats_registry_init(&stats_reg_, /* segment_base */ 0);
        infmon_api_ctx_init(&ctx_, rule_set_, &stats_reg_);
    }

    void TearDown() override
    {
        infmon_api_ctx_destroy(&ctx_);
        infmon_stats_registry_destroy(&stats_reg_);
        infmon_flow_rule_set_destroy(rule_set_);
    }

    infmon_flow_rule_set_t *rule_set_ = nullptr;
    infmon_stats_registry_t stats_reg_{};
    infmon_api_ctx_t ctx_{};
};

/* ── Tests ───────────────────────────────────────────────────────── */

TEST_F(ApiHandlerTest, AddAndDelBasic)
{
    infmon_flow_rule_t rule = make_rule("test_rule");
    EXPECT_EQ(infmon_api_flow_rule_add(&ctx_, &rule), INFMON_API_OK);
    EXPECT_EQ(infmon_flow_rule_count(rule_set_), 1u);

    EXPECT_EQ(infmon_api_flow_rule_del(&ctx_, "test_rule"), INFMON_API_OK);
    EXPECT_EQ(infmon_flow_rule_count(rule_set_), 0u);
}

TEST_F(ApiHandlerTest, AddDuplicateReturnsNameExists)
{
    infmon_flow_rule_t rule = make_rule("dup_rule");
    EXPECT_EQ(infmon_api_flow_rule_add(&ctx_, &rule), INFMON_API_OK);
    EXPECT_EQ(infmon_api_flow_rule_add(&ctx_, &rule), INFMON_API_ERR_NAME_EXISTS);
}

TEST_F(ApiHandlerTest, DelNonexistentReturnsNotFound)
{
    EXPECT_EQ(infmon_api_flow_rule_del(&ctx_, "no_such_rule"), INFMON_API_ERR_NOT_FOUND);
}

TEST_F(ApiHandlerTest, AddMultipleThenDeleteFirst)
{
    infmon_flow_rule_t r1 = make_rule("rule_aa");
    infmon_flow_rule_t r2 = make_rule("rule_bb");
    EXPECT_EQ(infmon_api_flow_rule_add(&ctx_, &r1), INFMON_API_OK);
    EXPECT_EQ(infmon_api_flow_rule_add(&ctx_, &r2), INFMON_API_OK);
    EXPECT_EQ(infmon_flow_rule_count(rule_set_), 2u);

    EXPECT_EQ(infmon_api_flow_rule_del(&ctx_, "rule_aa"), INFMON_API_OK);
    EXPECT_EQ(infmon_flow_rule_count(rule_set_), 1u);

    /* rule_bb should still be findable. */
    const infmon_flow_rule_t *found = infmon_flow_rule_find(rule_set_, "rule_bb");
    ASSERT_NE(found, nullptr);
    EXPECT_STREQ(found->name, "rule_bb");

    /* counter table for rule_bb should have been compacted into slot 0. */
    EXPECT_NE(ctx_.tables[0], nullptr);
    EXPECT_EQ(ctx_.tables[1], nullptr);
}

TEST_F(ApiHandlerTest, NullInputsReturnInvalidRule)
{
    EXPECT_EQ(infmon_api_flow_rule_add(nullptr, nullptr), INFMON_API_ERR_INVALID_RULE);
    EXPECT_EQ(infmon_api_flow_rule_del(nullptr, nullptr), INFMON_API_ERR_INVALID_RULE);
}

TEST_F(ApiHandlerTest, AddBudgetExceeded)
{
    /* Exhaust the key budget with one rule, then try adding another. */
    infmon_flow_rule_t big = make_rule("big_rule", INFMON_FLOW_RULE_MAX_KEYS_BUDGET);
    EXPECT_EQ(infmon_api_flow_rule_add(&ctx_, &big), INFMON_API_OK);

    infmon_flow_rule_t extra = make_rule("extra_rule", 1);
    EXPECT_EQ(infmon_api_flow_rule_add(&ctx_, &extra), INFMON_API_ERR_BUDGET_EXCEEDED);
}

TEST_F(ApiHandlerTest, AddSetFull)
{
    /* Fill all slots in the rule set. */
    for (uint32_t i = 0; i < INFMON_FLOW_RULE_SET_MAX; i++) {
        char name[INFMON_FLOW_RULE_NAME_MAX];
        snprintf(name, sizeof(name), "rule_%u", i);
        infmon_flow_rule_t r = make_rule(name, 1);
        ASSERT_EQ(infmon_api_flow_rule_add(&ctx_, &r), INFMON_API_OK) << "Failed at i=" << i;
    }

    /* Next add should fail with SET_FULL. */
    infmon_flow_rule_t overflow = make_rule("overflow");
    EXPECT_EQ(infmon_api_flow_rule_add(&ctx_, &overflow), INFMON_API_ERR_SET_FULL);
}

/* ── W10c: flow_rule_list / flow_rule_get tests ──────────────────── */

struct ListEntry {
    std::string name;
    uint32_t index;
};

static void list_cb(const infmon_flow_rule_t *rule, uint32_t index, void *user)
{
    auto *vec = static_cast<std::vector<ListEntry> *>(user);
    vec->push_back({rule->name, index});
}

TEST_F(ApiHandlerTest, ListEmptyReturnsOk)
{
    std::vector<ListEntry> entries;
    EXPECT_EQ(infmon_api_flow_rule_list(&ctx_, list_cb, &entries), INFMON_API_OK);
    EXPECT_TRUE(entries.empty());
}

TEST_F(ApiHandlerTest, ListMultipleRules)
{
    infmon_flow_rule_t r1 = make_rule("rule_aa");
    infmon_flow_rule_t r2 = make_rule("rule_bb");
    infmon_flow_rule_t r3 = make_rule("rule_cc");
    ASSERT_EQ(infmon_api_flow_rule_add(&ctx_, &r1), INFMON_API_OK);
    ASSERT_EQ(infmon_api_flow_rule_add(&ctx_, &r2), INFMON_API_OK);
    ASSERT_EQ(infmon_api_flow_rule_add(&ctx_, &r3), INFMON_API_OK);

    std::vector<ListEntry> entries;
    EXPECT_EQ(infmon_api_flow_rule_list(&ctx_, list_cb, &entries), INFMON_API_OK);
    ASSERT_EQ(entries.size(), 3u);
    EXPECT_EQ(entries[0].name, "rule_aa");
    EXPECT_EQ(entries[0].index, 0u);
    EXPECT_EQ(entries[1].name, "rule_bb");
    EXPECT_EQ(entries[1].index, 1u);
    EXPECT_EQ(entries[2].name, "rule_cc");
    EXPECT_EQ(entries[2].index, 2u);
}

TEST_F(ApiHandlerTest, ListNullCtxReturnsError)
{
    EXPECT_EQ(infmon_api_flow_rule_list(nullptr, list_cb, nullptr), INFMON_API_ERR_INVALID_RULE);
}

TEST_F(ApiHandlerTest, ListNullCallbackDoesNotCrash)
{
    infmon_flow_rule_t r = make_rule("rule_xx");
    ASSERT_EQ(infmon_api_flow_rule_add(&ctx_, &r), INFMON_API_OK);
    EXPECT_EQ(infmon_api_flow_rule_list(&ctx_, nullptr, nullptr), INFMON_API_OK);
}

TEST_F(ApiHandlerTest, GetByNameFound)
{
    infmon_flow_rule_t r1 = make_rule("rule_aa");
    infmon_flow_rule_t r2 = make_rule("rule_bb");
    ASSERT_EQ(infmon_api_flow_rule_add(&ctx_, &r1), INFMON_API_OK);
    ASSERT_EQ(infmon_api_flow_rule_add(&ctx_, &r2), INFMON_API_OK);

    const infmon_flow_rule_t *found = nullptr;
    uint32_t idx = (uint32_t) -1;
    EXPECT_EQ(infmon_api_flow_rule_get_by_name(&ctx_, "rule_bb", &found, &idx), INFMON_API_OK);
    ASSERT_NE(found, nullptr);
    EXPECT_STREQ(found->name, "rule_bb");
    EXPECT_EQ(idx, 1u);
    EXPECT_EQ(found->field_count, 2u);
}

TEST_F(ApiHandlerTest, GetByNameNotFound)
{
    EXPECT_EQ(infmon_api_flow_rule_get_by_name(&ctx_, "no_such", nullptr, nullptr),
              INFMON_API_ERR_NOT_FOUND);
}

TEST_F(ApiHandlerTest, GetByNameNullInputs)
{
    EXPECT_EQ(infmon_api_flow_rule_get_by_name(nullptr, "x", nullptr, nullptr),
              INFMON_API_ERR_INVALID_RULE);
    EXPECT_EQ(infmon_api_flow_rule_get_by_name(&ctx_, nullptr, nullptr, nullptr),
              INFMON_API_ERR_INVALID_RULE);
}

TEST_F(ApiHandlerTest, GetByNameNullOutputs)
{
    infmon_flow_rule_t r = make_rule("rule_zz");
    ASSERT_EQ(infmon_api_flow_rule_add(&ctx_, &r), INFMON_API_OK);

    /* Both out params NULL — should still return OK without crashing. */
    EXPECT_EQ(infmon_api_flow_rule_get_by_name(&ctx_, "rule_zz", nullptr, nullptr),
              INFMON_API_OK);
}

TEST_F(ApiHandlerTest, ListAfterDeleteReflectsRemoval)
{
    infmon_flow_rule_t r1 = make_rule("rule_aa");
    infmon_flow_rule_t r2 = make_rule("rule_bb");
    ASSERT_EQ(infmon_api_flow_rule_add(&ctx_, &r1), INFMON_API_OK);
    ASSERT_EQ(infmon_api_flow_rule_add(&ctx_, &r2), INFMON_API_OK);

    ASSERT_EQ(infmon_api_flow_rule_del(&ctx_, "rule_aa"), INFMON_API_OK);

    std::vector<ListEntry> entries;
    EXPECT_EQ(infmon_api_flow_rule_list(&ctx_, list_cb, &entries), INFMON_API_OK);
    ASSERT_EQ(entries.size(), 1u);
    EXPECT_EQ(entries[0].name, "rule_bb");
    EXPECT_EQ(entries[0].index, 0u);

    /* get should also reflect removal. */
    EXPECT_EQ(infmon_api_flow_rule_get_by_name(&ctx_, "rule_aa", nullptr, nullptr),
              INFMON_API_ERR_NOT_FOUND);
}
