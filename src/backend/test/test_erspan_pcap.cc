// SPDX-License-Identifier: Apache-2.0
// Golden PCAP test vectors for the ERSPAN III parser.
// See specs/003-erspan-and-packet-parsing.md §8.1

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <gtest/gtest.h>
#include <string>
#include <type_traits>
#include <vector>

extern "C" {
#include "infmon/erspan_parser.h"
}

// ---------------------------------------------------------------------------
// Minimal PCAP reader (libpcap format, host-endian or swapped)
// ---------------------------------------------------------------------------
#pragma pack(push, 1)
struct pcap_file_hdr {
    uint32_t magic;
    uint16_t version_major;
    uint16_t version_minor;
    int32_t thiszone;
    uint32_t sigfigs;
    uint32_t snaplen;
    uint32_t linktype;
};

struct pcap_pkt_hdr {
    uint32_t ts_sec;
    uint32_t ts_usec;
    uint32_t incl_len;
    uint32_t orig_len;
};
#pragma pack(pop)

static bool read_first_packet(const std::string &path, std::vector<uint8_t> &out)
{
    FILE *f = fopen(path.c_str(), "rb");
    if (!f)
        return false;

    pcap_file_hdr fh;
    if (fread(&fh, sizeof(fh), 1, f) != 1) {
        fclose(f);
        return false;
    }

    bool swap = false;
    if (fh.magic == 0xD4C3B2A1u)
        swap = true;
    else if (fh.magic != 0xA1B2C3D4u) {
        fclose(f);
        return false;
    }

    // Validate linktype: only DLT_EN10MB (Ethernet) is supported
    uint32_t lt = swap ? __builtin_bswap32(fh.linktype) : fh.linktype;
    if (lt != 1) { // DLT_EN10MB
        fclose(f);
        return false;
    }

    pcap_pkt_hdr ph;
    if (fread(&ph, sizeof(ph), 1, f) != 1) {
        fclose(f);
        return false;
    }

    uint32_t len = swap ? __builtin_bswap32(ph.incl_len) : ph.incl_len;
    if (len > 65535) {
        fclose(f);
        return false;
    }

    out.resize(len);
    if (fread(out.data(), 1, len, f) != len) {
        fclose(f);
        return false;
    }
    fclose(f);
    return true;
}

// ---------------------------------------------------------------------------
// Test fixture
// ---------------------------------------------------------------------------
class PcapTest : public ::testing::Test
{
  protected:
    infmon_parsed_packet_t out;
    std::vector<uint8_t> pkt;

    void SetUp() override
    {
        memset(&out, 0, sizeof(out));
    }

    bool loadPcap(const std::string &name)
    {
        // Derive scenario name by stripping .pcap suffix
        std::string scenario = name;
        auto pos = scenario.rfind(".pcap");
        if (pos != std::string::npos)
            scenario.erase(pos);

        // Try scenario layout first, then legacy flat layout
        std::string paths[] = {
            std::string("../tests/e2e/scenarios/") + scenario + "/input.pcap",
            std::string("tests/e2e/scenarios/") + scenario + "/input.pcap",
            std::string(PCAP_DIR "/") + scenario + "/input.pcap",
            std::string("../tests/pcaps/erspan/") + name,
            std::string("tests/pcaps/erspan/") + name,
            std::string(PCAP_DIR "/") + name,
        };
        for (auto &p : paths) {
            if (read_first_packet(p, pkt))
                return true;
        }
        return false;
    }
};

// ---------------------------------------------------------------------------
// Golden PCAP tests
// ---------------------------------------------------------------------------

TEST_F(PcapTest, ErspanFull)
{
    ASSERT_TRUE(loadPcap("erspan3_full.pcap"));
    auto rc = infmon_parse_erspan(pkt.data(), pkt.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_OK);
    EXPECT_FALSE(out.inner_truncated);
    EXPECT_EQ(out.mirror_src_ip.family, INFMON_AF_V4);
    EXPECT_EQ(out.mirror_src_ip.addr.v4[0], 10);
    EXPECT_EQ(out.mirror_src_ip.addr.v4[1], 0);
    EXPECT_EQ(out.mirror_src_ip.addr.v4[2], 0);
    EXPECT_EQ(out.mirror_src_ip.addr.v4[3], 1);
    EXPECT_TRUE(out.valid_fields & INFMON_VALID_PORTS);
    EXPECT_EQ(out.src_port, 12345);
    EXPECT_EQ(out.dst_port, 80);
}

TEST_F(PcapTest, ErspanWithSeq)
{
    ASSERT_TRUE(loadPcap("erspan3_with_seq.pcap"));
    auto rc = infmon_parse_erspan(pkt.data(), pkt.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_OK);
    EXPECT_FALSE(out.inner_truncated);
}

TEST_F(PcapTest, ErspanOBit)
{
    ASSERT_TRUE(loadPcap("erspan3_o_bit.pcap"));
    auto rc = infmon_parse_erspan(pkt.data(), pkt.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_OK);
    EXPECT_FALSE(out.inner_truncated);
}

TEST_F(PcapTest, ErspanOBitTruncated)
{
    ASSERT_TRUE(loadPcap("erspan3_o_bit_truncated.pcap"));
    auto rc = infmon_parse_erspan(pkt.data(), pkt.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_ERR_ERSPAN_TRUNCATED);
}

TEST_F(PcapTest, ErspanIPv6Full)
{
    ASSERT_TRUE(loadPcap("erspan3_ipv6_full.pcap"));
    auto rc = infmon_parse_erspan(pkt.data(), pkt.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_OK);
    EXPECT_EQ(out.mirror_src_ip.family, INFMON_AF_V6);
}

TEST_F(PcapTest, ErspanIPv6Trunc128)
{
    ASSERT_TRUE(loadPcap("erspan3_ipv6_trunc128.pcap"));
    auto rc = infmon_parse_erspan(pkt.data(), pkt.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_INNER_TRUNCATED_OK);
    EXPECT_TRUE(out.inner_truncated);
    // Should still have valid ports (enough room after 70B overhead = 58B inner)
    EXPECT_TRUE(out.valid_fields & INFMON_VALID_PORTS);
}

TEST_F(PcapTest, ErspanTrunc128)
{
    ASSERT_TRUE(loadPcap("erspan3_trunc128.pcap"));
    auto rc = infmon_parse_erspan(pkt.data(), pkt.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_INNER_TRUNCATED_OK);
    EXPECT_TRUE(out.inner_truncated);
    EXPECT_TRUE(out.valid_fields & INFMON_VALID_PORTS);
}

TEST_F(PcapTest, ErspanTruncOuter)
{
    ASSERT_TRUE(loadPcap("erspan3_trunc_outer.pcap"));
    auto rc = infmon_parse_erspan(pkt.data(), pkt.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_ERR_OUTER_TRUNCATED);
}

TEST_F(PcapTest, ErspanBadVersion)
{
    ASSERT_TRUE(loadPcap("erspan3_bad_version.pcap"));
    auto rc = infmon_parse_erspan(pkt.data(), pkt.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_ERR_ERSPAN_BAD_VERSION);
}

TEST_F(PcapTest, ErspanQinQ)
{
    ASSERT_TRUE(loadPcap("erspan3_qinq.pcap"));
    auto rc = infmon_parse_erspan(pkt.data(), pkt.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_ERR_OUTER_QINQ_UNSUPPORTED);
}

TEST_F(PcapTest, ErspanGREKeyed)
{
    ASSERT_TRUE(loadPcap("erspan3_gre_keyed.pcap"));
    auto rc = infmon_parse_erspan(pkt.data(), pkt.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_ERR_GRE_UNEXPECTED_FLAGS);
}

TEST_F(PcapTest, ZeroLengthInput)
{
    // Empty / zero-length buffer must not crash.
    auto rc = infmon_parse_erspan(nullptr, 0, &out);
    // Any error code is acceptable — just must not segfault.
    EXPECT_NE(rc, INFMON_PARSE_OK);
}

// ---------------------------------------------------------------------------
// Negative: ERSPAN session ID must NOT appear in output struct
// ---------------------------------------------------------------------------

// SFINAE helper: detects if T has a `session_id` member.
template <typename T, typename = void> struct has_session_id : std::false_type {
};
template <typename T>
struct has_session_id<T, std::void_t<decltype(T::session_id)>> : std::true_type {
};

TEST_F(PcapTest, SessionIdNotExposed)
{
    // Compile-time guarantee: session_id must not exist in the struct.
    static_assert(!has_session_id<infmon_parsed_packet_t>::value,
                  "infmon_parsed_packet_t must NOT expose session_id (spec §4.4)");

    // Runtime sanity: parse still succeeds.
    ASSERT_TRUE(loadPcap("erspan3_full.pcap"));
    auto rc = infmon_parse_erspan(pkt.data(), pkt.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_OK);
    (void) out.inner_ptr;
    (void) out.mirror_src_ip;
    (void) out.valid_fields;
}
