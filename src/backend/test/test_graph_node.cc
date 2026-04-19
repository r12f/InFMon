/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 */

#include <cstring>
#include <gtest/gtest.h>

extern "C" {
#include "infmon/graph_node.h"
}

/* ── Helpers ─────────────────────────────────────────────────────── */

static infmon_flow_rule_t make_rule(const char *name,
                                     const infmon_field_t *fields,
                                     uint32_t fc,
                                     uint32_t max_keys = 1024)
{
    infmon_flow_rule_t r{};
    std::size_t len = std::strlen(name);
    if (len > INFMON_FLOW_RULE_NAME_MAX)
        len = INFMON_FLOW_RULE_NAME_MAX;
    std::memcpy(r.name, name, len);
    if (fc > 0)
        std::memcpy(r.fields, fields, fc * sizeof(infmon_field_t));
    r.field_count = fc;
    r.max_keys = max_keys;
    r.eviction_policy = INFMON_EVICTION_LRU_DROP;
    r.key_width = infmon_flow_rule_key_width(r.fields, r.field_count);
    return r;
}

static infmon_flow_fields_t make_fields(uint8_t proto, uint8_t dscp,
                                         const uint8_t src[4],
                                         const uint8_t dst[4])
{
    infmon_flow_fields_t f{};
    f.ip_proto = proto;
    f.dscp = dscp;
    /* Store as IPv4-mapped-IPv6 */
    f.src_ip[10] = 0xff;
    f.src_ip[11] = 0xff;
    std::memcpy(f.src_ip + 12, src, 4);
    f.dst_ip[10] = 0xff;
    f.dst_ip[11] = 0xff;
    std::memcpy(f.dst_ip + 12, dst, 4);
    return f;
}

/* ── Scratch reset ──────────────────────────────────────────────── */

TEST(Scratch, ResetSetsCountToZero)
{
    infmon_scratch_t scratch{};
    scratch.count = 42;
    infmon_scratch_reset(&scratch);
    EXPECT_EQ(scratch.count, 0u);
}

/* ── Flow match — single rule, single packet ────────────────────── */

TEST(FlowMatch, SingleRuleSingleMatch)
{
    infmon_field_t fields[] = {INFMON_FIELD_SRC_IP, INFMON_FIELD_DST_IP};
    auto rule = make_rule("test", fields, 2);

    uint8_t src[] = {10, 0, 0, 1};
    uint8_t dst[] = {10, 0, 0, 2};
    auto ff = make_fields(6, 0, src, dst);

    infmon_scratch_t scratch{};
    uint8_t key_buf[INFMON_KEY_BUF_MAX]{};

    uint32_t m = infmon_flow_match(&rule, 1, &ff, &scratch, key_buf);
    EXPECT_EQ(m, 1u);
    EXPECT_EQ(scratch.count, 1u);
    EXPECT_EQ(scratch.entries[0].flow_rule_index, 0u);
    EXPECT_NE(scratch.entries[0].key_hash, 0u);
    EXPECT_EQ(scratch.entries[0].key_len, 32u); /* 16+16 */
}

/* ── Flow match — multiple rules ────────────────────────────────── */

TEST(FlowMatch, MultipleRulesAllMatch)
{
    infmon_field_t f1[] = {INFMON_FIELD_SRC_IP};
    infmon_field_t f2[] = {INFMON_FIELD_IP_PROTO};
    infmon_flow_rule_t rules[2];
    rules[0] = make_rule("r1", f1, 1);
    rules[1] = make_rule("r2", f2, 1);

    uint8_t src[] = {192, 168, 1, 1};
    uint8_t dst[] = {192, 168, 1, 2};
    auto ff = make_fields(17, 0, src, dst);

    infmon_scratch_t scratch{};
    uint8_t key_buf[INFMON_KEY_BUF_MAX]{};

    uint32_t m = infmon_flow_match(rules, 2, &ff, &scratch, key_buf);
    EXPECT_EQ(m, 2u);
    EXPECT_EQ(scratch.count, 2u);
    EXPECT_EQ(scratch.entries[0].flow_rule_index, 0u);
    EXPECT_EQ(scratch.entries[0].key_len, 16u);
    EXPECT_EQ(scratch.entries[1].flow_rule_index, 1u);
    EXPECT_EQ(scratch.entries[1].key_len, 1u);
}

/* ── Flow match — NULL inputs ───────────────────────────────────── */

TEST(FlowMatch, NullInputsReturnZero)
{
    EXPECT_EQ(infmon_flow_match(nullptr, 0, nullptr, nullptr, nullptr), 0u);
}

/* ── Flow match — deterministic hashing ─────────────────────────── */

TEST(FlowMatch, SameInputSameHash)
{
    infmon_field_t fields[] = {INFMON_FIELD_SRC_IP, INFMON_FIELD_IP_PROTO};
    auto rule = make_rule("h", fields, 2);

    uint8_t src[] = {1, 2, 3, 4};
    uint8_t dst[] = {5, 6, 7, 8};
    auto ff = make_fields(6, 0, src, dst);

    infmon_scratch_t s1{}, s2{};
    uint8_t kb1[INFMON_KEY_BUF_MAX]{}, kb2[INFMON_KEY_BUF_MAX]{};

    infmon_flow_match(&rule, 1, &ff, &s1, kb1);
    infmon_flow_match(&rule, 1, &ff, &s2, kb2);

    EXPECT_EQ(s1.entries[0].key_hash, s2.entries[0].key_hash);
}

/* ── Flow match — different packets produce different hashes ───── */

TEST(FlowMatch, DifferentInputsDifferentHash)
{
    infmon_field_t fields[] = {INFMON_FIELD_SRC_IP};
    auto rule = make_rule("d", fields, 1);

    uint8_t src1[] = {10, 0, 0, 1};
    uint8_t src2[] = {10, 0, 0, 2};
    uint8_t dst[] = {0, 0, 0, 0};
    auto ff1 = make_fields(6, 0, src1, dst);
    auto ff2 = make_fields(6, 0, src2, dst);

    infmon_scratch_t s1{}, s2{};
    uint8_t kb1[INFMON_KEY_BUF_MAX]{}, kb2[INFMON_KEY_BUF_MAX]{};

    infmon_flow_match(&rule, 1, &ff1, &s1, kb1);
    infmon_flow_match(&rule, 1, &ff2, &s2, kb2);

    EXPECT_NE(s1.entries[0].key_hash, s2.entries[0].key_hash);
}

/* ── Counter update — basic integration ─────────────────────────── */

TEST(CounterUpdate, UpdatesCounterTable)
{
    /* Create a counter table */
    infmon_counter_table_t *table = infmon_counter_table_create(64, 32);
    ASSERT_NE(table, nullptr);

    /* Build a scratch with one entry */
    infmon_field_t fields[] = {INFMON_FIELD_SRC_IP};
    auto rule = make_rule("cu", fields, 1);

    uint8_t src[] = {10, 1, 1, 1};
    uint8_t dst[] = {10, 1, 1, 2};
    auto ff = make_fields(6, 0, src, dst);

    infmon_scratch_t scratch{};
    uint8_t key_buf[INFMON_KEY_BUF_MAX]{};

    infmon_flow_match(&rule, 1, &ff, &scratch, key_buf);
    ASSERT_EQ(scratch.count, 1u);

    /* Set up tables array */
    infmon_counter_table_t *tables[INFMON_MAX_ACTIVE_FLOW_RULES]{};
    tables[0] = table;

    uint64_t retry = 0, full = 0;
    infmon_counter_update(&scratch, tables, 100, 1, &retry, &full);

    EXPECT_EQ(retry, 0u);
    EXPECT_EQ(full, 0u);

    /* Verify counter was updated by reading the slot */
    bool found = false;
    for (uint32_t i = 0; i < table->num_slots; i++) {
        infmon_slot_t slot;
        if (infmon_counter_table_read_slot(table, i, &slot) &&
            slot.flags == INFMON_SLOT_OCCUPIED) {
            EXPECT_EQ(slot.packets, 1u);
            EXPECT_EQ(slot.bytes, 100u);
            found = true;
            break;
        }
    }
    EXPECT_TRUE(found);

    /* Update again — counters should accumulate */
    infmon_counter_update(&scratch, tables, 200, 2, &retry, &full);

    found = false;
    for (uint32_t i = 0; i < table->num_slots; i++) {
        infmon_slot_t slot;
        if (infmon_counter_table_read_slot(table, i, &slot) &&
            slot.flags == INFMON_SLOT_OCCUPIED) {
            EXPECT_EQ(slot.packets, 2u);
            EXPECT_EQ(slot.bytes, 300u);
            found = true;
            break;
        }
    }
    EXPECT_TRUE(found);

    infmon_counter_table_destroy(table);
}

/* ── Counter update — NULL tables handled ───────────────────────── */

TEST(CounterUpdate, NullTableSkipsGracefully)
{
    infmon_scratch_t scratch{};
    scratch.count = 1;
    scratch.entries[0].flow_rule_index = 0;
    scratch.entries[0].key_len = 16;
    scratch.entries[0].key_hash = 0x12345;
    uint8_t dummy[16]{};
    scratch.entries[0].key_ptr = dummy;

    infmon_counter_table_t *tables[INFMON_MAX_ACTIVE_FLOW_RULES]{};
    /* tables[0] is null */

    uint64_t retry = 0, full = 0;
    infmon_counter_update(&scratch, tables, 100, 1, &retry, &full);
    /* Should not crash, and no errors */
    EXPECT_EQ(retry, 0u);
    EXPECT_EQ(full, 0u);
}

/* ── ERSPAN decap ───────────────────────────────────────────────── */

TEST(ErspanDecap, TruncatedBufferFails)
{
    uint8_t buf[10]{};
    infmon_decap_result_t dr;
    auto rc = infmon_erspan_decap(buf, sizeof(buf), &dr);
    EXPECT_NE(rc, INFMON_PARSE_OK);
}

/* ── Extract flow fields ────────────────────────────────────────── */

TEST(ExtractFlowFields, NullInputReturnsFalse)
{
    infmon_flow_fields_t out{};
    EXPECT_FALSE(infmon_extract_flow_fields(nullptr, nullptr, 0, &out));
}

TEST(ExtractFlowFields, IPv4InnerFrame)
{
    /* Build a minimal inner Ethernet+IPv4 frame */
    uint8_t inner[34]{};
    /* Ethernet: dst(6) + src(6) + type(2) = 14 bytes */
    inner[12] = 0x08;
    inner[13] = 0x00; /* IPv4 */
    /* IPv4 header at offset 14 */
    inner[14] = 0x45; /* version=4, IHL=5 */
    inner[15] = 0xB8; /* TOS = 0xB8 => DSCP = 0x2E (46) */
    /* Protocol at offset 14+9 = 23 */
    inner[23] = 17; /* UDP */
    /* Src IP at offset 14+12 = 26: 192.168.1.100 */
    inner[26] = 192;
    inner[27] = 168;
    inner[28] = 1;
    inner[29] = 100;
    /* Dst IP at offset 14+16 = 30: 10.0.0.1 */
    inner[30] = 10;
    inner[31] = 0;
    inner[32] = 0;
    inner[33] = 1;

    /* Need a parsed packet for mirror_src_ip */
    infmon_parsed_packet_t parsed{};
    parsed.mirror_src_ip.family = INFMON_AF_V4;
    parsed.mirror_src_ip.addr.v4[0] = 172;
    parsed.mirror_src_ip.addr.v4[1] = 16;
    parsed.mirror_src_ip.addr.v4[2] = 0;
    parsed.mirror_src_ip.addr.v4[3] = 1;

    infmon_flow_fields_t out{};
    bool ok = infmon_extract_flow_fields(&parsed, inner, sizeof(inner), &out);
    EXPECT_TRUE(ok);
    EXPECT_EQ(out.ip_proto, 17u);
    EXPECT_EQ(out.dscp, 46u);
    /* Check src_ip is IPv4-mapped */
    EXPECT_EQ(out.src_ip[10], 0xff);
    EXPECT_EQ(out.src_ip[11], 0xff);
    EXPECT_EQ(out.src_ip[12], 192u);
    EXPECT_EQ(out.src_ip[15], 100u);
    /* Check mirror_src_ip */
    EXPECT_EQ(out.mirror_src_ip[10], 0xff);
    EXPECT_EQ(out.mirror_src_ip[11], 0xff);
    EXPECT_EQ(out.mirror_src_ip[12], 172u);
}
