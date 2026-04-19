// SPDX-License-Identifier: MIT
// Comprehensive tests for the ERSPAN III parser.

#include <gtest/gtest.h>
#include <cstring>
#include <vector>
#include <arpa/inet.h>

extern "C" {
#include "infmon/erspan_parser.h"
}

// ---------------------------------------------------------------------------
// Helper: packet builder
// ---------------------------------------------------------------------------
class PacketBuilder {
public:
    std::vector<uint8_t> buf;

    void push8(uint8_t v) { buf.push_back(v); }
    void push16(uint16_t v) { buf.push_back(v >> 8); buf.push_back(v & 0xff); }
    void push32(uint32_t v) {
        buf.push_back((v >> 24) & 0xff);
        buf.push_back((v >> 16) & 0xff);
        buf.push_back((v >> 8) & 0xff);
        buf.push_back(v & 0xff);
    }
    void pushN(const uint8_t *d, size_t n) { buf.insert(buf.end(), d, d + n); }
    void pushZeros(size_t n) { buf.insert(buf.end(), n, 0); }

    size_t size() const { return buf.size(); }
    uint8_t *data() { return buf.data(); }
    const uint8_t *data() const { return buf.data(); }

    // Patch a big-endian 16-bit value at offset
    void patch16(size_t off, uint16_t v) {
        buf[off] = v >> 8; buf[off+1] = v & 0xff;
    }
    void patch8(size_t off, uint8_t v) { buf[off] = v; }
};

// Build outer Ethernet header. Returns offset after it.
static size_t addOuterEth(PacketBuilder &pb, uint16_t ethertype) {
    // dst mac
    uint8_t dst[6] = {0x00,0x11,0x22,0x33,0x44,0x55};
    uint8_t src[6] = {0x66,0x77,0x88,0x99,0xaa,0xbb};
    pb.pushN(dst, 6);
    pb.pushN(src, 6);
    pb.push16(ethertype);
    return pb.size();
}

// Add 802.1Q VLAN tag before the real ethertype. Call INSTEAD of addOuterEth.
static size_t addOuterEthVlan(PacketBuilder &pb, uint16_t vlan_id, uint16_t inner_ethertype) {
    uint8_t dst[6] = {0x00,0x11,0x22,0x33,0x44,0x55};
    uint8_t src[6] = {0x66,0x77,0x88,0x99,0xaa,0xbb};
    pb.pushN(dst, 6);
    pb.pushN(src, 6);
    pb.push16(0x8100); // TPID
    pb.push16(vlan_id & 0x0fff);
    pb.push16(inner_ethertype);
    return pb.size();
}

// Add QinQ
static size_t addOuterEthQinQ(PacketBuilder &pb) {
    uint8_t dst[6] = {0x00,0x11,0x22,0x33,0x44,0x55};
    uint8_t src[6] = {0x66,0x77,0x88,0x99,0xaa,0xbb};
    pb.pushN(dst, 6);
    pb.pushN(src, 6);
    pb.push16(0x88a8); // outer TPID (QinQ)
    pb.push16(100);
    pb.push16(0x8100); // inner TPID
    pb.push16(200);
    pb.push16(0x0800);
    return pb.size();
}

static const uint8_t kSrcIPv4[4] = {10,0,0,1};
static const uint8_t kDstIPv4[4] = {10,0,0,2};

// Add outer IPv4. Returns offset of start of IPv4.
// Caller must patch total_length after building rest.
static size_t addOuterIPv4(PacketBuilder &pb, size_t *ip_start_out) {
    size_t ip_start = pb.size();
    if (ip_start_out) *ip_start_out = ip_start;
    pb.push8(0x45); // ver=4, ihl=5
    pb.push8(0x00); // DSCP/ECN
    pb.push16(0);   // total length - patch later
    pb.push16(0);   // identification
    pb.push16(0x4000); // flags=DF, frag=0
    pb.push8(64);   // TTL
    pb.push8(47);   // protocol = GRE
    pb.push16(0);   // checksum
    pb.pushN(kSrcIPv4, 4);
    pb.pushN(kDstIPv4, 4);
    return pb.size();
}

static const uint8_t kSrcIPv6[16] = {0x20,0x01,0x0d,0xb8, 0,0,0,0, 0,0,0,0, 0,0,0,1};
static const uint8_t kDstIPv6[16] = {0x20,0x01,0x0d,0xb8, 0,0,0,0, 0,0,0,0, 0,0,0,2};

static size_t addOuterIPv6(PacketBuilder &pb, size_t *ip_start_out) {
    size_t ip_start = pb.size();
    if (ip_start_out) *ip_start_out = ip_start;
    pb.push32(0x60000000); // ver=6, tc=0, flow=0
    pb.push16(0);          // payload length - patch later
    pb.push8(47);          // next header = GRE
    pb.push8(64);          // hop limit
    pb.pushN(kSrcIPv6, 16);
    pb.pushN(kDstIPv6, 16);
    return pb.size();
}

// Add GRE header. Returns offset after GRE.
static size_t addGRE(PacketBuilder &pb, uint16_t flags_ver = 0x0000, uint16_t proto = 0x22EB, bool add_seq = false) {
    pb.push16(flags_ver);
    pb.push16(proto);
    if (add_seq) {
        pb.push32(0x00000042); // sequence number
    }
    return pb.size();
}

// Add ERSPAN III header (12 bytes). ver=2 in top nibble of word0.
// Returns offset after ERSPAN.
static size_t addERSPANIII(PacketBuilder &pb, bool o_bit = false) {
    // Word 0: ver(4) | vlan(12) | cos(3) | bso(2) | t(1) | session_id(10)
    // ver=2 => top nibble = 0x2
    uint32_t word0 = 0x20000000; // ver=2, rest zero
    pb.push32(word0);
    // Word 1: timestamp
    pb.push32(0x12345678);
    // Word 2: sgt(16) | p(1) | ft(5) | hw_id(6) | d(1) | gra(2) | o(1)
    uint32_t word2 = 0;
    if (o_bit) word2 |= 0x00000001; // O bit is LSB
    pb.push32(word2);
    if (o_bit) {
        // 8-byte platform specific sub-header
        pb.push32(0xDEADBEEF);
        pb.push32(0xCAFEBABE);
    }
    return pb.size();
}

// Inner Ethernet + IPv4 + TCP (full)
static size_t addInnerEthIPv4TCP(PacketBuilder &pb, size_t *inner_start_out = nullptr) {
    if (inner_start_out) *inner_start_out = pb.size();
    // Inner Ethernet
    uint8_t idst[6] = {0xaa,0xbb,0xcc,0xdd,0xee,0xff};
    uint8_t isrc[6] = {0x11,0x22,0x33,0x44,0x55,0x66};
    pb.pushN(idst, 6);
    pb.pushN(isrc, 6);
    pb.push16(0x0800);

    // Inner IPv4
    (void)pb.size(); /* inner IPv4 offset — not needed here */
    pb.push8(0x45);
    pb.push8(0x00);
    uint16_t inner_ip_total = 20 + 20; // IPv4 + TCP
    pb.push16(inner_ip_total);
    pb.push16(0); // id
    pb.push16(0x4000);
    pb.push8(64);
    pb.push8(6); // TCP
    pb.push16(0); // checksum
    uint8_t isrcip[4] = {192,168,1,1};
    uint8_t idstip[4] = {192,168,1,2};
    pb.pushN(isrcip, 4);
    pb.pushN(idstip, 4);

    // TCP header (20 bytes min)
    pb.push16(12345); // src port
    pb.push16(80);    // dst port
    pb.push32(0xAABBCCDD); // seq
    pb.push32(0x11223344); // ack
    pb.push8(0x50); // data offset = 5 (20 bytes), reserved
    pb.push8(0x12); // flags: SYN+ACK
    pb.push16(65535); // window
    pb.push16(0); // checksum
    pb.push16(0); // urgent

    return pb.size();
}

// Inner Ethernet + IPv4 + UDP
static size_t addInnerEthIPv4UDP(PacketBuilder &pb, size_t *inner_start_out = nullptr) {
    if (inner_start_out) *inner_start_out = pb.size();
    uint8_t idst[6] = {0xaa,0xbb,0xcc,0xdd,0xee,0xff};
    uint8_t isrc[6] = {0x11,0x22,0x33,0x44,0x55,0x66};
    pb.pushN(idst, 6);
    pb.pushN(isrc, 6);
    pb.push16(0x0800);

    pb.push8(0x45);
    pb.push8(0x00);
    pb.push16(20 + 8); // IPv4 + UDP
    pb.push16(0);
    pb.push16(0x4000);
    pb.push8(64);
    pb.push8(17); // UDP
    pb.push16(0);
    uint8_t isrcip[4] = {192,168,1,1};
    uint8_t idstip[4] = {192,168,1,2};
    pb.pushN(isrcip, 4);
    pb.pushN(idstip, 4);

    pb.push16(5353);  // src port
    pb.push16(53);    // dst port
    pb.push16(8);     // length
    pb.push16(0);     // checksum
    return pb.size();
}

// Inner Ethernet + IPv4 + ICMP (non-TCP/UDP)
static size_t addInnerEthIPv4ICMP(PacketBuilder &pb, size_t *inner_start_out = nullptr) {
    if (inner_start_out) *inner_start_out = pb.size();
    uint8_t idst[6] = {0xaa,0xbb,0xcc,0xdd,0xee,0xff};
    uint8_t isrc[6] = {0x11,0x22,0x33,0x44,0x55,0x66};
    pb.pushN(idst, 6);
    pb.pushN(isrc, 6);
    pb.push16(0x0800);

    pb.push8(0x45);
    pb.push8(0x00);
    pb.push16(20 + 8); // IPv4 + 8 bytes ICMP
    pb.push16(0);
    pb.push16(0x4000);
    pb.push8(64);
    pb.push8(1); // ICMP
    pb.push16(0);
    uint8_t isrcip[4] = {192,168,1,1};
    uint8_t idstip[4] = {192,168,1,2};
    pb.pushN(isrcip, 4);
    pb.pushN(idstip, 4);

    // ICMP echo
    pb.push8(8); pb.push8(0); pb.push16(0); pb.push16(1); pb.push16(1);
    return pb.size();
}

// Finalize IPv4 total_length field
static void fixupIPv4Length(PacketBuilder &pb, size_t ip_start) {
    uint16_t total = (uint16_t)(pb.size() - ip_start);
    pb.patch16(ip_start + 2, total);
}

// Finalize IPv6 payload_length
static void fixupIPv6Length(PacketBuilder &pb, size_t ip_start) {
    uint16_t payload = (uint16_t)(pb.size() - ip_start - 40);
    pb.patch16(ip_start + 4, payload);
}

// Build a standard valid ERSPAN III over GRE over IPv4 with inner TCP
static PacketBuilder buildValidPacketTCP() {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb);
    addERSPANIII(pb);
    addInnerEthIPv4TCP(pb);
    fixupIPv4Length(pb, ip_start);
    return pb;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

class ErspanParserTest : public ::testing::Test {
protected:
    infmon_parsed_packet_t out;
    void SetUp() override { memset(&out, 0, sizeof(out)); }
};

TEST_F(ErspanParserTest, ValidFullERSPANIII_IPv4_TCP) {
    auto pb = buildValidPacketTCP();
    auto rc = infmon_parse_erspan(pb.data(), pb.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_OK);
    EXPECT_FALSE(out.inner_truncated);
    EXPECT_EQ(out.mirror_src_ip.family, INFMON_AF_V4);
    EXPECT_EQ(memcmp(out.mirror_src_ip.addr.v4, kSrcIPv4, 4), 0);
    EXPECT_EQ(out.inner_ip_proto, 6);
    EXPECT_EQ(out.inner_af, INFMON_AF_V4);
    EXPECT_FALSE(out.flow_key_partial);
}

TEST_F(ErspanParserTest, ValidGRESequenceBit) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb, 0x1000, 0x22EB, true); // S flag + seq
    addERSPANIII(pb);
    addInnerEthIPv4TCP(pb);
    fixupIPv4Length(pb, ip_start);
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_OK);
}

TEST_F(ErspanParserTest, ValidOBitPlatformSubheader) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb);
    addERSPANIII(pb, true); // O=1, adds 8B sub-header
    addInnerEthIPv4TCP(pb);
    fixupIPv4Length(pb, ip_start);
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_OK);
}

TEST_F(ErspanParserTest, ValidOverIPv6) {
    PacketBuilder pb;
    addOuterEth(pb, 0x86DD);
    size_t ip_start;
    addOuterIPv6(pb, &ip_start);
    addGRE(pb);
    addERSPANIII(pb);
    addInnerEthIPv4TCP(pb);
    fixupIPv6Length(pb, ip_start);
    auto rc = infmon_parse_erspan(pb.data(), pb.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_OK);
    EXPECT_EQ(out.mirror_src_ip.family, INFMON_AF_V6);
    EXPECT_EQ(memcmp(out.mirror_src_ip.addr.v6, kSrcIPv6, 16), 0);
}

TEST_F(ErspanParserTest, TruncatedInnerPacket) {
    // Build full packet then truncate
    auto pb = buildValidPacketTCP();
    // Truncate: remove last 20 bytes (cut into TCP header)
    size_t truncated_len = pb.size() - 20;
    EXPECT_EQ(infmon_parse_erspan(pb.data(), truncated_len, &out), INFMON_PARSE_INNER_TRUNCATED_OK);
    EXPECT_TRUE(out.inner_truncated);
}

TEST_F(ErspanParserTest, OuterQinQ) {
    PacketBuilder pb;
    addOuterEthQinQ(pb);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb);
    addERSPANIII(pb);
    addInnerEthIPv4TCP(pb);
    fixupIPv4Length(pb, ip_start);
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_ERR_OUTER_QINQ_UNSUPPORTED);
}

TEST_F(ErspanParserTest, BadOuterEtherType) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0806); // ARP
    pb.pushZeros(60); // some payload
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_ERR_OUTER_ETHERTYPE_UNSUPPORTED);
}

TEST_F(ErspanParserTest, GREWithKFlag) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb, 0x2000, 0x22EB); // K flag set (bit 13)
    addERSPANIII(pb);
    addInnerEthIPv4TCP(pb);
    fixupIPv4Length(pb, ip_start);
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_ERR_GRE_UNEXPECTED_FLAGS);
}

TEST_F(ErspanParserTest, BadGREVersion) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb, 0x0001, 0x22EB); // version=1
    addERSPANIII(pb);
    addInnerEthIPv4TCP(pb);
    fixupIPv4Length(pb, ip_start);
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_ERR_GRE_BAD_VERSION);
}

TEST_F(ErspanParserTest, WrongGREProto) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb, 0x0000, 0x0800); // proto=IPv4 instead of ERSPAN
    addERSPANIII(pb);
    addInnerEthIPv4TCP(pb);
    fixupIPv4Length(pb, ip_start);
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_ERR_GRE_BAD_PROTO);
}

TEST_F(ErspanParserTest, ERSPANBadVersion) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb);
    // Manually build ERSPAN with ver=1 (top nibble = 0x1)
    pb.push32(0x10000000); // ver=1
    pb.push32(0x12345678);
    pb.push32(0x00000000);
    addInnerEthIPv4TCP(pb);
    fixupIPv4Length(pb, ip_start);
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_ERR_ERSPAN_BAD_VERSION);
}

TEST_F(ErspanParserTest, TruncatedOuterHeaders) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    // Only partial IPv4 header (10 bytes instead of 20)
    pb.push8(0x45);
    pb.pushZeros(9);
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_ERR_OUTER_TRUNCATED);
}

TEST_F(ErspanParserTest, TruncatedERSPANHeader) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb);
    // Only 8 bytes of ERSPAN instead of 12
    pb.push32(0x20000000);
    pb.push32(0x12345678);
    fixupIPv4Length(pb, ip_start);
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_ERR_ERSPAN_TRUNCATED);
}

TEST_F(ErspanParserTest, InnerEthernetTruncated) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb);
    addERSPANIII(pb);
    // Only 10 bytes of inner Ethernet (need 14)
    pb.pushZeros(10);
    fixupIPv4Length(pb, ip_start);
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_ERR_INNER_ETH_TRUNCATED);
}

TEST_F(ErspanParserTest, InnerL3Truncated) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb);
    addERSPANIII(pb);
    // Inner Ethernet (14B) but only partial IPv4 (10 bytes)
    uint8_t idst[6] = {0xaa,0xbb,0xcc,0xdd,0xee,0xff};
    uint8_t isrc[6] = {0x11,0x22,0x33,0x44,0x55,0x66};
    pb.pushN(idst, 6);
    pb.pushN(isrc, 6);
    pb.push16(0x0800);
    pb.push8(0x45);
    pb.pushZeros(9); // only 10 bytes of IPv4
    fixupIPv4Length(pb, ip_start);
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_ERR_INNER_L3_TRUNCATED);
}

TEST_F(ErspanParserTest, MirrorSrcIPv4Extracted) {
    auto pb = buildValidPacketTCP();
    infmon_parse_erspan(pb.data(), pb.size(), &out);
    EXPECT_EQ(out.mirror_src_ip.family, INFMON_AF_V4);
    EXPECT_EQ(out.mirror_src_ip.addr.v4[0], 10);
    EXPECT_EQ(out.mirror_src_ip.addr.v4[1], 0);
    EXPECT_EQ(out.mirror_src_ip.addr.v4[2], 0);
    EXPECT_EQ(out.mirror_src_ip.addr.v4[3], 1);
}

TEST_F(ErspanParserTest, MirrorSrcIPv6Extracted) {
    PacketBuilder pb;
    addOuterEth(pb, 0x86DD);
    size_t ip_start;
    addOuterIPv6(pb, &ip_start);
    addGRE(pb);
    addERSPANIII(pb);
    addInnerEthIPv4TCP(pb);
    fixupIPv6Length(pb, ip_start);
    infmon_parse_erspan(pb.data(), pb.size(), &out);
    EXPECT_EQ(out.mirror_src_ip.family, INFMON_AF_V6);
    EXPECT_EQ(memcmp(out.mirror_src_ip.addr.v6, kSrcIPv6, 16), 0);
}

TEST_F(ErspanParserTest, InnerPtrPointsIntoOriginalBuffer) {
    auto pb = buildValidPacketTCP();
    infmon_parse_erspan(pb.data(), pb.size(), &out);
    ASSERT_NE(out.inner_ptr, nullptr);
    // inner_ptr should point within the original buffer
    EXPECT_GE(out.inner_ptr, pb.data());
    EXPECT_LT(out.inner_ptr, pb.data() + pb.size());
}

TEST_F(ErspanParserTest, TCPPortExtraction) {
    auto pb = buildValidPacketTCP();
    infmon_parse_erspan(pb.data(), pb.size(), &out);
    EXPECT_EQ(out.src_port, 12345);
    EXPECT_EQ(out.dst_port, 80);
    EXPECT_TRUE(out.valid_fields & INFMON_VALID_PORTS);
}

TEST_F(ErspanParserTest, UDPPortExtraction) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb);
    addERSPANIII(pb);
    addInnerEthIPv4UDP(pb);
    fixupIPv4Length(pb, ip_start);
    auto rc = infmon_parse_erspan(pb.data(), pb.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_OK);
    EXPECT_EQ(out.src_port, 5353);
    EXPECT_EQ(out.dst_port, 53);
    EXPECT_TRUE(out.valid_fields & INFMON_VALID_PORTS);
    EXPECT_EQ(out.inner_ip_proto, 17);
}

TEST_F(ErspanParserTest, TCPFlagsSeqAckWindow) {
    auto pb = buildValidPacketTCP();
    infmon_parse_erspan(pb.data(), pb.size(), &out);
    EXPECT_TRUE(out.valid_fields & INFMON_VALID_TCP_FLAGS);
    EXPECT_TRUE(out.valid_fields & INFMON_VALID_TCP_SEQ);
    EXPECT_TRUE(out.valid_fields & INFMON_VALID_TCP_ACK);
    EXPECT_TRUE(out.valid_fields & INFMON_VALID_TCP_WINDOW);
    EXPECT_EQ(out.tcp_flags, 0x12); // SYN+ACK
    EXPECT_EQ(out.tcp_seq, 0xAABBCCDD);
    EXPECT_EQ(out.tcp_ack, 0x11223344);
    EXPECT_EQ(out.tcp_window, 65535);
}

TEST_F(ErspanParserTest, FlowKeyPartialForICMP) {
    PacketBuilder pb;
    addOuterEth(pb, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb);
    addERSPANIII(pb);
    addInnerEthIPv4ICMP(pb);
    fixupIPv4Length(pb, ip_start);
    auto rc = infmon_parse_erspan(pb.data(), pb.size(), &out);
    EXPECT_EQ(rc, INFMON_PARSE_OK);
    EXPECT_TRUE(out.flow_key_partial);
    EXPECT_EQ(out.inner_ip_proto, 1);
}

TEST_F(ErspanParserTest, VLANTaggedOuterFrame) {
    PacketBuilder pb;
    addOuterEthVlan(pb, 100, 0x0800);
    size_t ip_start;
    addOuterIPv4(pb, &ip_start);
    addGRE(pb);
    addERSPANIII(pb);
    addInnerEthIPv4TCP(pb);
    fixupIPv4Length(pb, ip_start);
    EXPECT_EQ(infmon_parse_erspan(pb.data(), pb.size(), &out), INFMON_PARSE_OK);
}

TEST_F(ErspanParserTest, CounterNamesArray) {
    // Verify the array has entries and is non-null
    ASSERT_NE(infmon_parse_counter_names, nullptr);
    for (int i = 0; i < INFMON_PARSE_ERR__COUNT; i++) {
        EXPECT_NE(infmon_parse_counter_names[i], nullptr) << "Counter name at index " << i << " is null";
        EXPECT_GT(strlen(infmon_parse_counter_names[i]), 0u) << "Counter name at index " << i << " is empty";
    }
}
