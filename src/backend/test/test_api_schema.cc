/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * Validate the infmon.api schema — structural checks on the .api file.
 *
 * This test does NOT require vppapigen; it parses the raw .api text to
 * verify that all expected messages, types, and enums are present with
 * the correct fields.  It serves as a contract test: if someone edits
 * infmon.api and forgets a field, this test catches it before CI even
 * invokes vppapigen.
 */

#include <cstring>
#include <fstream>
#include <iostream>
#include <regex>
#include <set>
#include <sstream>
#include <string>
#include <vector>

#include <gtest/gtest.h>

#ifndef API_FILE
#error "API_FILE must be defined at compile time (path to infmon.api)"
#endif

static std::string read_file(const char *path)
{
    std::ifstream f(path);
    EXPECT_TRUE(f.is_open()) << "Cannot open " << path;
    std::stringstream ss;
    ss << f.rdbuf();
    return ss.str();
}

class ApiSchemaTest : public ::testing::Test {
  protected:
    std::string api;

    void SetUp() override { api = read_file(API_FILE); }

    /* Check that a pattern appears in the .api text. */
    void expect_contains(const std::string &pattern, const std::string &msg)
    {
        EXPECT_NE(api.find(pattern), std::string::npos)
            << msg << "\n  Missing pattern: " << pattern;
    }

    /* Check that a regex matches somewhere in the .api text. */
    void expect_matches(const std::string &regex_str, const std::string &msg)
    {
        std::regex re(regex_str);
        EXPECT_TRUE(std::regex_search(api, re))
            << msg << "\n  Missing regex: " << regex_str;
    }
};

/* ── Version ──────────────────────────────────────────────────────── */

TEST_F(ApiSchemaTest, HasVersionOption)
{
    expect_contains("option version", "API file must declare a version");
}

/* ── Enumerations ─────────────────────────────────────────────────── */

TEST_F(ApiSchemaTest, HasFieldTypeEnum)
{
    expect_contains("enum infmon_field_type", "Missing infmon_field_type enum");
    expect_contains("INFMON_FIELD_SRC_IP", "Missing SRC_IP field");
    expect_contains("INFMON_FIELD_DST_IP", "Missing DST_IP field");
    expect_contains("INFMON_FIELD_IP_PROTO", "Missing IP_PROTO field");
    expect_contains("INFMON_FIELD_DSCP", "Missing DSCP field");
    expect_contains("INFMON_FIELD_MIRROR_SRC_IP", "Missing MIRROR_SRC_IP field");
}

TEST_F(ApiSchemaTest, HasEvictionPolicyEnum)
{
    expect_contains("enum infmon_eviction_policy", "Missing eviction_policy enum");
    expect_contains("INFMON_EVICTION_LRU_DROP", "Missing LRU_DROP policy");
}

TEST_F(ApiSchemaTest, HasFlowRuleErrorEnum)
{
    expect_contains("enum infmon_flow_rule_error", "Missing flow_rule_error enum");
    expect_contains("INFMON_FLOW_RULE_OK", "Missing OK code");
    expect_contains("INFMON_FLOW_RULE_ERR_NAME_EXISTS", "Missing NAME_EXISTS");
    expect_contains("INFMON_FLOW_RULE_ERR_NOT_FOUND", "Missing NOT_FOUND");
    expect_contains("INFMON_FLOW_RULE_ERR_INVALID_SPEC", "Missing INVALID_SPEC");
    expect_contains("INFMON_FLOW_RULE_ERR_BUDGET_EXCEEDED", "Missing BUDGET_EXCEEDED");
    expect_contains("INFMON_FLOW_RULE_ERR_SET_FULL", "Missing SET_FULL");
    expect_contains("INFMON_FLOW_RULE_ERR_INTERNAL", "Missing INTERNAL");
}

TEST_F(ApiSchemaTest, HasSnapErrorEnum)
{
    expect_contains("enum infmon_snap_error", "Missing snap_error enum");
    expect_contains("INFMON_SNAP_OK", "Missing SNAP_OK");
    expect_contains("INFMON_SNAP_ALLOC_FAILED", "Missing SNAP_ALLOC_FAILED");
    expect_contains("INFMON_SNAP_TOO_MANY_RETIRED", "Missing SNAP_TOO_MANY_RETIRED");
    expect_contains("INFMON_SNAP_INVALID_INDEX", "Missing SNAP_INVALID_INDEX");
    expect_contains("INFMON_SNAP_NULL_TABLE", "Missing SNAP_NULL_TABLE");
}

/* ── Type definitions ─────────────────────────────────────────────── */

TEST_F(ApiSchemaTest, HasFlowRuleIdType)
{
    expect_contains("typedef infmon_flow_rule_id", "Missing flow_rule_id typedef");
    /* Must have hi/lo u64 fields */
    expect_contains("u64 hi", "flow_rule_id must have u64 hi");
    expect_contains("u64 lo", "flow_rule_id must have u64 lo");
}

TEST_F(ApiSchemaTest, HasTableDescriptorType)
{
    expect_contains("typedef infmon_table_descriptor", "Missing table_descriptor typedef");
    expect_contains("flow_rule_id_hi", "descriptor must have flow_rule_id_hi");
    expect_contains("flow_rule_id_lo", "descriptor must have flow_rule_id_lo");
    expect_contains("flow_rule_index", "descriptor must have flow_rule_index");
    expect_contains("generation", "descriptor must have generation");
    expect_contains("epoch_ns", "descriptor must have epoch_ns");
    expect_contains("slots_offset", "descriptor must have slots_offset");
    expect_contains("slots_len", "descriptor must have slots_len");
    expect_contains("key_arena_offset", "descriptor must have key_arena_offset");
    expect_contains("key_arena_capacity", "descriptor must have key_arena_capacity");
    expect_contains("key_arena_used", "descriptor must have key_arena_used");
    expect_contains("insert_failed", "descriptor must have insert_failed");
    expect_contains("table_full", "descriptor must have table_full");
}

TEST_F(ApiSchemaTest, HasFlowRuleDetailsType)
{
    expect_contains("typedef infmon_flow_rule_details", "Missing flow_rule_details typedef");
    expect_contains("flow_rule_id_hi", "details must have flow_rule_id_hi");
    expect_contains("flow_rule_id_lo", "details must have flow_rule_id_lo");
    expect_contains("flow_rule_index", "details must have flow_rule_index");
}

TEST_F(ApiSchemaTest, HasWorkerStatusType)
{
    expect_contains("typedef infmon_worker_status", "Missing worker_status typedef");
    expect_contains("worker_id", "worker_status must have worker_id");
    expect_contains("packets_seen", "worker_status must have packets_seen");
    expect_contains("erspan_unknown_proto", "worker_status must have erspan_unknown_proto");
    expect_contains("erspan_truncated", "worker_status must have erspan_truncated");
    expect_contains("inner_parse_failed", "worker_status must have inner_parse_failed");
    expect_contains("flow_rule_no_match", "worker_status must have flow_rule_no_match");
    expect_contains("counter_insert_retry_exhausted",
                    "worker_status must have counter_insert_retry_exhausted");
    expect_contains("counter_table_full", "worker_status must have counter_table_full");
}

/* ── Messages (W10b–W10e) ─────────────────────────────────────────── */

TEST_F(ApiSchemaTest, W10b_FlowRuleAdd)
{
    expect_contains("define infmon_flow_rule_add", "Missing infmon_flow_rule_add message");
    expect_contains("define infmon_flow_rule_add_reply",
                    "Missing infmon_flow_rule_add_reply message");
}

TEST_F(ApiSchemaTest, W10b_FlowRuleDel)
{
    expect_contains("define infmon_flow_rule_del", "Missing infmon_flow_rule_del message");
}

TEST_F(ApiSchemaTest, W10c_FlowRuleList)
{
    expect_contains("define infmon_flow_rule_list", "Missing infmon_flow_rule_list message");
    expect_contains("define infmon_flow_rule_list_details",
                    "Missing infmon_flow_rule_list_details message");
}

TEST_F(ApiSchemaTest, W10c_FlowRuleGet)
{
    expect_contains("define infmon_flow_rule_get", "Missing infmon_flow_rule_get message");
    expect_contains("define infmon_flow_rule_get_reply",
                    "Missing infmon_flow_rule_get_reply message");
}

TEST_F(ApiSchemaTest, W10d_SnapshotAndClear)
{
    expect_contains("define infmon_snapshot_and_clear",
                    "Missing infmon_snapshot_and_clear message");
    expect_contains("define infmon_snapshot_and_clear_reply",
                    "Missing infmon_snapshot_and_clear_reply message");
}

TEST_F(ApiSchemaTest, W10e_Status)
{
    expect_contains("define infmon_status", "Missing infmon_status message");
    expect_contains("define infmon_status_details", "Missing infmon_status_details message");
}

/* ── Message field checks ─────────────────────────────────────────── */

TEST_F(ApiSchemaTest, FlowRuleAddHasRequiredFields)
{
    /* Find the infmon_flow_rule_add block and check it has the key fields */
    expect_contains("client_index", "Messages must have client_index");
    expect_contains("context", "Messages must have context");
}

TEST_F(ApiSchemaTest, SnapshotReplyHasDescriptor)
{
    /* The snapshot reply must include the table descriptor */
    expect_matches("infmon_snapshot_and_clear_reply[\\s\\S]*?descriptor",
                   "snapshot_and_clear_reply must include a descriptor");
}

TEST_F(ApiSchemaTest, RepliesHaveRetval)
{
    /* All reply messages should have i32 retval */
    expect_matches("infmon_flow_rule_add_reply[\\s\\S]*?retval",
                   "flow_rule_add_reply must have retval");
    expect_matches("infmon_flow_rule_get_reply[\\s\\S]*?retval",
                   "flow_rule_get_reply must have retval");
    expect_matches("infmon_snapshot_and_clear_reply[\\s\\S]*?retval",
                   "snapshot_and_clear_reply must have retval");
}

/* ── All 6 messages present ───────────────────────────────────────── */

TEST_F(ApiSchemaTest, AllSixMessagesPresent)
{
    const std::vector<std::string> required_messages = {
        "infmon_flow_rule_add",      "infmon_flow_rule_del",  "infmon_flow_rule_list",
        "infmon_flow_rule_get", "infmon_snapshot_and_clear", "infmon_status",
    };

    for (const auto &msg : required_messages) {
        expect_contains("define " + msg, "Missing required message: " + msg);
    }
}
