/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 */

#include <cstdio>
#include <cstring>
#include <gtest/gtest.h>
#include <string>
#include <vector>

extern "C" {
#include "infmon/api_handler.h"
#include "infmon/snapshot.h"
#include "infmon/stats_segment.h"
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
    EXPECT_EQ(infmon_api_flow_rule_get_by_name(&ctx_, "rule_zz", nullptr, nullptr), INFMON_API_OK);
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

    /* Verify rule_bb shifted to index 0 after deletion. */
    const infmon_flow_rule_t *found = nullptr;
    uint32_t idx = (uint32_t) -1;
    EXPECT_EQ(infmon_api_flow_rule_get_by_name(&ctx_, "rule_bb", &found, &idx), INFMON_API_OK);
    EXPECT_EQ(idx, 0u);
    ASSERT_NE(found, nullptr);
    EXPECT_STREQ(found->name, "rule_bb");
}

/* ── W10d: snapshot_and_clear tests ──────────────────────────────── */

static uint64_t fake_clock_val = 1000000000ULL;
static uint64_t fake_clock_ns(void)
{
    return fake_clock_val;
}

class ApiHandlerSnapTest : public ::testing::Test
{
  protected:
    void SetUp() override
    {
        rule_set_ = infmon_flow_rule_set_create(INFMON_FLOW_RULE_MAX_KEYS_BUDGET);
        ASSERT_NE(rule_set_, nullptr);
        infmon_stats_registry_init(&stats_reg_, /* segment_base */ 0);

        snap_mgr_ = new infmon_snapshot_mgr_t;
        infmon_snapshot_mgr_init(snap_mgr_, /* num_workers */ 2, /* grace_ns */ 0, fake_clock_ns);

        infmon_api_ctx_init(&ctx_, rule_set_, &stats_reg_);
        ctx_.snap_mgr = snap_mgr_;
    }

    void TearDown() override
    {
        infmon_api_ctx_destroy(&ctx_);
        infmon_snapshot_mgr_destroy(snap_mgr_);
        delete snap_mgr_;
        infmon_stats_registry_destroy(&stats_reg_);
        infmon_flow_rule_set_destroy(rule_set_);
    }

    /** Helper: add a rule with an explicit ID. */
    infmon_flow_rule_id_t add_rule_with_id(const char *name, uint64_t hi, uint64_t lo)
    {
        infmon_flow_rule_t r = make_rule(name);
        infmon_flow_rule_id_t id = {hi, lo};
        EXPECT_EQ(infmon_api_flow_rule_add_with_id(&ctx_, &r, id), INFMON_API_OK);
        return id;
    }

    infmon_flow_rule_set_t *rule_set_ = nullptr;
    infmon_stats_registry_t stats_reg_{};
    infmon_snapshot_mgr_t *snap_mgr_ = nullptr;
    infmon_api_ctx_t ctx_{};
};

TEST_F(ApiHandlerSnapTest, SnapshotAndClearBasic)
{
    infmon_flow_rule_id_t id = add_rule_with_id("snap_rule", 0xAA, 0xBB);

    infmon_api_snap_reply_t reply;
    infmon_api_snapshot_and_clear(&ctx_, id, &reply);

    EXPECT_EQ(reply.result, INFMON_API_OK);
    EXPECT_EQ(reply.descriptor.flow_rule_id.hi, 0xAAu);
    EXPECT_EQ(reply.descriptor.flow_rule_id.lo, 0xBBu);
    EXPECT_EQ(reply.descriptor.flow_rule_index, 0u);
    EXPECT_EQ(reply.descriptor.generation, 0u); /* retired table is gen 0 */
    EXPECT_EQ(reply.descriptor.active, 1u);

    /* New table should be at generation 1. */
    EXPECT_NE(ctx_.tables[0], nullptr);
    EXPECT_EQ(ctx_.tables[0]->generation, 1u);
}

TEST_F(ApiHandlerSnapTest, SnapshotAndClearNotFoundId)
{
    add_rule_with_id("some_rule", 0x11, 0x22);

    infmon_flow_rule_id_t bad_id = {0xFF, 0xFF};
    infmon_api_snap_reply_t reply;
    infmon_api_snapshot_and_clear(&ctx_, bad_id, &reply);

    EXPECT_EQ(reply.result, INFMON_API_ERR_NOT_FOUND);
}

TEST_F(ApiHandlerSnapTest, SnapshotAndClearNoSnapMgr)
{
    ctx_.snap_mgr = nullptr;

    infmon_flow_rule_id_t id = {0x11, 0x22};
    infmon_api_snap_reply_t reply;
    infmon_api_snapshot_and_clear(&ctx_, id, &reply);

    EXPECT_EQ(reply.result, INFMON_API_ERR_NO_SNAPSHOT_MGR);
}

TEST_F(ApiHandlerSnapTest, SnapshotAndClearNullCtx)
{
    infmon_flow_rule_id_t id = {0x11, 0x22};
    infmon_api_snap_reply_t reply;
    infmon_api_snapshot_and_clear(nullptr, id, &reply);

    EXPECT_EQ(reply.result, INFMON_API_ERR_INTERNAL);
}

TEST_F(ApiHandlerSnapTest, SnapshotAndClearDescriptorFields)
{
    infmon_flow_rule_id_t id = add_rule_with_id("desc_rule", 0xDE, 0xAD);

    /* The initial table has gen=0, epoch_ns=0 (counter_table_create zeroes everything). */
    infmon_api_snap_reply_t reply;
    infmon_api_snapshot_and_clear(&ctx_, id, &reply);

    ASSERT_EQ(reply.result, INFMON_API_OK);

    const infmon_stats_descriptor_t &d = reply.descriptor;
    EXPECT_EQ(d.flow_rule_id.hi, 0xDEu);
    EXPECT_EQ(d.flow_rule_id.lo, 0xADu);
    EXPECT_EQ(d.flow_rule_index, 0u);
    EXPECT_EQ(d.generation, 0u);
    /* slots_len should match the table's num_slots (counter_table rounds up to power of 2). */
    EXPECT_GT(d.slots_len, 0u);
    EXPECT_EQ(d.active, 1u);
}

TEST_F(ApiHandlerSnapTest, SnapshotAndClearMultipleSwaps)
{
    infmon_flow_rule_id_t id = add_rule_with_id("multi_rule", 0x01, 0x02);

    /* First swap: retired gen=0, new gen=1. */
    infmon_api_snap_reply_t reply1;
    infmon_api_snapshot_and_clear(&ctx_, id, &reply1);
    ASSERT_EQ(reply1.result, INFMON_API_OK);
    EXPECT_EQ(reply1.descriptor.generation, 0u);

    /* Second swap: retired gen=1, new gen=2. */
    infmon_api_snap_reply_t reply2;
    infmon_api_snapshot_and_clear(&ctx_, id, &reply2);
    ASSERT_EQ(reply2.result, INFMON_API_OK);
    EXPECT_EQ(reply2.descriptor.generation, 1u);
    EXPECT_EQ(ctx_.tables[0]->generation, 2u);
}

TEST_F(ApiHandlerSnapTest, AddWithIdPreservesIdAcrossDelete)
{
    add_rule_with_id("rule_aa", 0x10, 0x20);
    infmon_flow_rule_id_t id_bb = add_rule_with_id("rule_bb", 0x30, 0x40);

    /* Delete first rule — rule_bb should compact to index 0 with its ID intact. */
    EXPECT_EQ(infmon_api_flow_rule_del(&ctx_, "rule_aa"), INFMON_API_OK);

    infmon_api_snap_reply_t reply;
    infmon_api_snapshot_and_clear(&ctx_, id_bb, &reply);
    ASSERT_EQ(reply.result, INFMON_API_OK);
    EXPECT_EQ(reply.descriptor.flow_rule_id.hi, 0x30u);
    EXPECT_EQ(reply.descriptor.flow_rule_id.lo, 0x40u);
    EXPECT_EQ(reply.descriptor.flow_rule_index, 0u);
}

TEST_F(ApiHandlerSnapTest, SnapshotAndClearRejectsZeroId)
{
    /* Add a rule via plain add (no explicit ID). */
    infmon_flow_rule_t r = make_rule("plain_rule");
    EXPECT_EQ(infmon_api_flow_rule_add(&ctx_, &r), INFMON_API_OK);

    /* Calling snapshot_and_clear with a zero ID should return NOT_FOUND,
     * not silently match the slot whose ID was zeroed by the add path. */
    infmon_flow_rule_id_t zero_id = {0, 0};
    infmon_api_snap_reply_t reply;
    infmon_api_result_t rc = infmon_api_snapshot_and_clear(&ctx_, zero_id, &reply);
    EXPECT_EQ(rc, INFMON_API_ERR_NOT_FOUND);
    EXPECT_EQ(reply.result, INFMON_API_ERR_NOT_FOUND);
}

TEST_F(ApiHandlerSnapTest, SnapshotAndClearReturnType)
{
    /* Verify the return value matches reply.result. */
    infmon_flow_rule_id_t id = add_rule_with_id("ret_rule", 0xCC, 0xDD);

    infmon_api_snap_reply_t reply;
    infmon_api_result_t rc = infmon_api_snapshot_and_clear(&ctx_, id, &reply);
    EXPECT_EQ(rc, INFMON_API_OK);
    EXPECT_EQ(rc, reply.result);
}

/* ── W10e: infmon_status tests ──────────────────────────────────── */

TEST_F(ApiHandlerTest, StatusBasic)
{
    /* Set up 2 workers with known counter values. */
    infmon_worker_counters_t workers[2];
    infmon_worker_counters_init(&workers[0], 0);
    infmon_worker_counters_init(&workers[1], 1);

    workers[0].packets_seen = 100;
    workers[0].erspan_unknown_proto = 1;
    workers[0].erspan_truncated = 2;
    workers[0].inner_parse_failed = 3;
    workers[0].flow_rule_no_match = 10;
    workers[0].counter_insert_retry_exhausted = 0;
    workers[0].counter_table_full = 0;

    workers[1].packets_seen = 200;
    workers[1].erspan_unknown_proto = 0;
    workers[1].erspan_truncated = 0;
    workers[1].inner_parse_failed = 0;
    workers[1].flow_rule_no_match = 5;
    workers[1].counter_insert_retry_exhausted = 1;
    workers[1].counter_table_full = 2;

    ctx_.worker_counters = workers;
    ctx_.worker_count = 2;

    infmon_api_status_reply_t reply;
    infmon_api_result_t rc = infmon_api_status(&ctx_, &reply);
    EXPECT_EQ(rc, INFMON_API_OK);
    EXPECT_EQ(reply.result, INFMON_API_OK);
    ASSERT_EQ(reply.worker_count, 2u);
    ASSERT_NE(reply.workers, nullptr);

    EXPECT_EQ(reply.workers[0].worker_id, 0u);
    EXPECT_EQ(reply.workers[0].packets_seen, 100u);
    EXPECT_EQ(reply.workers[0].erspan_unknown_proto, 1u);
    EXPECT_EQ(reply.workers[0].flow_rule_no_match, 10u);

    EXPECT_EQ(reply.workers[1].worker_id, 1u);
    EXPECT_EQ(reply.workers[1].packets_seen, 200u);
    EXPECT_EQ(reply.workers[1].counter_insert_retry_exhausted, 1u);
    EXPECT_EQ(reply.workers[1].counter_table_full, 2u);
}

TEST_F(ApiHandlerTest, StatusNullCtxReturnsError)
{
    infmon_api_status_reply_t reply;
    EXPECT_EQ(infmon_api_status(nullptr, &reply), INFMON_API_ERR_INTERNAL);
    EXPECT_EQ(reply.result, INFMON_API_ERR_INTERNAL);
}

TEST_F(ApiHandlerTest, StatusNullReplyReturnsError)
{
    EXPECT_EQ(infmon_api_status(&ctx_, nullptr), INFMON_API_ERR_INTERNAL);
}

TEST_F(ApiHandlerTest, StatusNoWorkersReturnsError)
{
    /* ctx_ has worker_counters = NULL, worker_count = 0 by default. */
    infmon_api_status_reply_t reply;
    EXPECT_EQ(infmon_api_status(&ctx_, &reply), INFMON_API_ERR_INTERNAL);
    EXPECT_EQ(reply.result, INFMON_API_ERR_INTERNAL);
}

TEST_F(ApiHandlerTest, StatusSingleWorkerZeroCounters)
{
    infmon_worker_counters_t wc;
    infmon_worker_counters_init(&wc, 42);

    ctx_.worker_counters = &wc;
    ctx_.worker_count = 1;

    infmon_api_status_reply_t reply;
    EXPECT_EQ(infmon_api_status(&ctx_, &reply), INFMON_API_OK);
    ASSERT_EQ(reply.worker_count, 1u);
    EXPECT_EQ(reply.workers[0].worker_id, 42u);
    EXPECT_EQ(reply.workers[0].packets_seen, 0u);
    EXPECT_EQ(reply.workers[0].erspan_unknown_proto, 0u);
    EXPECT_EQ(reply.workers[0].erspan_truncated, 0u);
    EXPECT_EQ(reply.workers[0].inner_parse_failed, 0u);
    EXPECT_EQ(reply.workers[0].flow_rule_no_match, 0u);
    EXPECT_EQ(reply.workers[0].counter_insert_retry_exhausted, 0u);
    EXPECT_EQ(reply.workers[0].counter_table_full, 0u);
}

TEST_F(ApiHandlerTest, WorkerCountersInitSetsWorkerIdOnly)
{
    infmon_worker_counters_t wc;
    /* Fill with garbage first. */
    memset(&wc, 0xFF, sizeof(wc));
    infmon_worker_counters_init(&wc, 7);

    EXPECT_EQ(wc.worker_id, 7u);
    EXPECT_EQ(wc.packets_seen, 0u);
    EXPECT_EQ(wc.erspan_unknown_proto, 0u);
    EXPECT_EQ(wc.erspan_truncated, 0u);
    EXPECT_EQ(wc.inner_parse_failed, 0u);
    EXPECT_EQ(wc.flow_rule_no_match, 0u);
    EXPECT_EQ(wc.counter_insert_retry_exhausted, 0u);
    EXPECT_EQ(wc.counter_table_full, 0u);
}

TEST_F(ApiHandlerTest, WorkerCountersInitNullNoOp)
{
    /* Should not crash. */
    infmon_worker_counters_init(nullptr, 0);
}
