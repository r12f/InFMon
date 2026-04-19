/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 */

#include <cstdio>
#include <cstring>
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
