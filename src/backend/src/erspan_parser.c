/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * ERSPAN III / GRE parser implementation.
 * See specs/003-erspan-and-packet-parsing.md for the authoritative spec.
 */

#include "infmon/erspan_parser.h"

#include <string.h>

/* ── Counter names ────────────────────────────────────────────────── */

const char *infmon_parse_counter_names[] = {
    [INFMON_PARSE_OK] = "parsed_ok",
    [INFMON_PARSE_INNER_TRUNCATED_OK] = "inner_truncated_ok",
    [INFMON_PARSE_ERR_OUTER_QINQ_UNSUPPORTED] = "outer_qinq_unsupported",
    [INFMON_PARSE_ERR_OUTER_ETHERTYPE_UNSUPPORTED] = "outer_ethertype_unsupported",
    [INFMON_PARSE_ERR_OUTER_V6_EXT_UNSUPPORTED] = "outer_v6_ext_unsupported",
    [INFMON_PARSE_ERR_OUTER_TRUNCATED] = "outer_truncated",
    [INFMON_PARSE_ERR_MBUF_NOT_CONTIGUOUS] = "mbuf_not_contiguous",
    [INFMON_PARSE_ERR_GRE_UNEXPECTED_FLAGS] = "gre_unexpected_flags",
    [INFMON_PARSE_ERR_GRE_BAD_VERSION] = "gre_bad_version",
    [INFMON_PARSE_ERR_GRE_BAD_PROTO] = "gre_bad_proto",
    [INFMON_PARSE_ERR_ERSPAN_BAD_VERSION] = "erspan_bad_version",
    [INFMON_PARSE_ERR_ERSPAN_TRUNCATED] = "erspan_truncated",
    [INFMON_PARSE_ERR_INNER_ETH_TRUNCATED] = "inner_eth_truncated",
    [INFMON_PARSE_ERR_INNER_L3_TRUNCATED] = "inner_l3_truncated",
    [INFMON_PARSE_ERR_INNER_DOUBLE_ENCAP_DROPPED] = "inner_double_encap_dropped",
};

_Static_assert(sizeof(infmon_parse_counter_names) / sizeof(infmon_parse_counter_names[0]) ==
                   INFMON_PARSE_ERR__COUNT,
               "counter_names out of sync with infmon_parse_result_t");

/* ── Helpers ──────────────────────────────────────────────────────── */

static inline uint16_t read_u16(const uint8_t *p)
{
    return (uint16_t) ((uint16_t) p[0] << 8 | p[1]);
}

static inline uint32_t read_u32(const uint8_t *p)
{
    return (uint32_t) ((uint32_t) p[0] << 24 | (uint32_t) p[1] << 16 | (uint32_t) p[2] << 8 | p[3]);
}

/* ── Extract inner L4 fields (TCP/UDP) ───────────────────────────── */

static void extract_inner_l4(const uint8_t *inner_ptr, uint32_t inner_l4_off, uint32_t inner_len,
                             uint8_t inner_ip_proto, infmon_parsed_packet_t *out)
{
    if (inner_ip_proto == 6 || inner_ip_proto == 17) {
        /* Need at least 4 bytes for ports */
        if (inner_len >= inner_l4_off + 4) {
            out->src_port = read_u16(inner_ptr + inner_l4_off);
            out->dst_port = read_u16(inner_ptr + inner_l4_off + 2);
            out->valid_fields |= INFMON_VALID_PORTS;
        } else {
            out->flow_key_partial = true;
        }

        /* TCP-specific fields */
        if (inner_ip_proto == 6 && (out->valid_fields & INFMON_VALID_PORTS)) {
            if (inner_len >= inner_l4_off + 8) {
                out->tcp_seq = read_u32(inner_ptr + inner_l4_off + 4);
                out->valid_fields |= INFMON_VALID_TCP_SEQ;
            }
            if (inner_len >= inner_l4_off + 12) {
                out->tcp_ack = read_u32(inner_ptr + inner_l4_off + 8);
                out->valid_fields |= INFMON_VALID_TCP_ACK;
            }
            if (inner_len >= inner_l4_off + 14) {
                out->tcp_flags = inner_ptr[inner_l4_off + 13];
                out->valid_fields |= INFMON_VALID_TCP_FLAGS;
            }
            if (inner_len >= inner_l4_off + 16) {
                out->tcp_window = read_u16(inner_ptr + inner_l4_off + 14);
                out->valid_fields |= INFMON_VALID_TCP_WINDOW;
            }
        }
    } else {
        /* Non-TCP/UDP L4: no port extraction */
        out->flow_key_partial = true;
    }
}

/* ── Inner-decap hook ─────────────────────────────────────────────── */

int infmon_inner_decap(infmon_inner_decap_t kind, const uint8_t *in, uint32_t in_len,
                       bool in_truncated, const uint8_t **out_ptr, uint32_t *out_len,
                       bool *out_truncated)
{
    if (kind != INFMON_DECAP_NONE)
        return -1; /* unsupported in v1 */

    *out_ptr = in;
    *out_len = in_len;
    *out_truncated = in_truncated;
    return 0;
}

/* Maximum number of IPv6 extension headers to traverse before giving up. */
#define MAX_IPV6_EXT_HEADERS 8

/* ── Main parser ──────────────────────────────────────────────────── */

infmon_parse_result_t infmon_parse_erspan(const uint8_t *data, uint32_t len,
                                          infmon_parsed_packet_t *out)
{
    memset(out, 0, sizeof(*out));
    uint32_t off = 0;

    /* ── Outer Ethernet ─────────────────────────────────────────── */
    if (len < 14)
        return INFMON_PARSE_ERR_OUTER_TRUNCATED;

    uint16_t ethertype = read_u16(data + 12);
    off = 14;

    /* Single VLAN tag */
    if (ethertype == 0x8100) {
        if (len < off + 4)
            return INFMON_PARSE_ERR_OUTER_TRUNCATED;
        ethertype = read_u16(data + off + 2);
        off += 4;

        /* QinQ check: if after stripping one VLAN we see another 0x8100
         * or 0x88A8, that's stacked VLAN */
        if (ethertype == 0x8100 || ethertype == 0x88A8)
            return INFMON_PARSE_ERR_OUTER_QINQ_UNSUPPORTED;
    } else if (ethertype == 0x88A8) {
        /* S-tag (QinQ outer tag) */
        return INFMON_PARSE_ERR_OUTER_QINQ_UNSUPPORTED;
    }

    if (ethertype != 0x0800 && ethertype != 0x86DD)
        return INFMON_PARSE_ERR_OUTER_ETHERTYPE_UNSUPPORTED;

    /* ── Outer L3 ───────────────────────────────────────────────── */
    uint32_t outer_ip_payload_len = 0; /* bytes after IP header(s) */

    if (ethertype == 0x0800) {
        /* IPv4 */
        if (len < off + 20)
            return INFMON_PARSE_ERR_OUTER_TRUNCATED;

        uint8_t ver_ihl = data[off];
        if ((ver_ihl >> 4) != 4)
            return INFMON_PARSE_ERR_OUTER_TRUNCATED; /* bad version */

        uint8_t ihl = ver_ihl & 0x0F;
        if (ihl < 5)
            return INFMON_PARSE_ERR_OUTER_TRUNCATED;

        uint32_t ip_hdr_len = (uint32_t) ihl * 4;
        if (len < off + ip_hdr_len)
            return INFMON_PARSE_ERR_OUTER_TRUNCATED;

        uint8_t protocol = data[off + 9];
        if (protocol != 47) /* GRE */
            return INFMON_PARSE_ERR_OUTER_ETHERTYPE_UNSUPPORTED;

        uint16_t total_length = read_u16(data + off + 2);

        /* Validate total_length against ip_hdr_len */
        if (total_length < ip_hdr_len)
            return INFMON_PARSE_ERR_OUTER_TRUNCATED;

        /* Extract mirror_src_ip (IPv4 SA at offset 12) */
        out->mirror_src_ip.family = INFMON_AF_V4;
        memcpy(out->mirror_src_ip.addr.v4, data + off + 12, 4);

        outer_ip_payload_len = total_length - ip_hdr_len;
        off += ip_hdr_len;

    } else {
        /* IPv6 */
        if (len < off + 40)
            return INFMON_PARSE_ERR_OUTER_TRUNCATED;

        uint8_t ver = data[off] >> 4;
        if (ver != 6)
            return INFMON_PARSE_ERR_OUTER_TRUNCATED;

        uint16_t payload_length = read_u16(data + off + 4);
        uint8_t next_hdr = data[off + 6];

        /* Extract mirror_src_ip (IPv6 SA at offset 8) */
        out->mirror_src_ip.family = INFMON_AF_V6;
        memcpy(out->mirror_src_ip.addr.v6, data + off + 8, 16);

        off += 40;

        /* Parse allowed extension headers: Hop-by-Hop (0) and
         * Destination Options (60).
         * Cap iterations to avoid pathological chains. */
        uint32_t v6_ext_len = 0;
        int ext_iters = 0;
        while (next_hdr == 0 || next_hdr == 60) {
            if (++ext_iters > MAX_IPV6_EXT_HEADERS)
                return INFMON_PARSE_ERR_OUTER_V6_EXT_UNSUPPORTED;
            if (len < off + 2)
                return INFMON_PARSE_ERR_OUTER_TRUNCATED;
            uint8_t ext_next = data[off];
            uint8_t ext_hdr_len_field = data[off + 1];
            uint32_t ext_len = ((uint32_t) ext_hdr_len_field + 1) * 8;
            if (len < off + ext_len)
                return INFMON_PARSE_ERR_OUTER_TRUNCATED;
            v6_ext_len += ext_len;
            next_hdr = ext_next;
            off += ext_len;
        }

        if (next_hdr != 47) {
            /* If next_hdr is an unsupported extension type vs non-GRE proto */
            /* Extension headers: Routing(43), Fragment(44), AH(51),
             * ESP(50), Mobility(135) */
            if (next_hdr == 43 || next_hdr == 44 || next_hdr == 51 || next_hdr == 50 ||
                next_hdr == 135)
                return INFMON_PARSE_ERR_OUTER_V6_EXT_UNSUPPORTED;
            return INFMON_PARSE_ERR_OUTER_ETHERTYPE_UNSUPPORTED;
        }

        outer_ip_payload_len = payload_length > v6_ext_len ? payload_length - v6_ext_len : 0;
    }

    /* ── GRE ────────────────────────────────────────────────────── */
    if (len < off + 4)
        return INFMON_PARSE_ERR_OUTER_TRUNCATED;

    uint16_t gre_flags_ver = read_u16(data + off);
    uint16_t gre_proto = read_u16(data + off + 2);

    uint8_t gre_version = gre_flags_ver & 0x07;
    if (gre_version != 0)
        return INFMON_PARSE_ERR_GRE_BAD_VERSION;

    /* Flags: bits 15..0 of the first 16 bits.
     * Bit layout (MSB first): C R K S s(recursion) flags ver
     * C=bit15, R=bit14, K=bit13, S=bit12
     * Only S (bit 12) is allowed. */
    uint16_t flags = gre_flags_ver & 0xFFF8; /* mask out version bits */
    bool has_seq = (flags & 0x1000) != 0;
    uint16_t disallowed = flags & ~(uint16_t) 0x1000;
    if (disallowed != 0)
        return INFMON_PARSE_ERR_GRE_UNEXPECTED_FLAGS;

    if (gre_proto != 0x22EB)
        return INFMON_PARSE_ERR_GRE_BAD_PROTO;

    uint32_t gre_len = 4 + (has_seq ? 4 : 0);
    if (len < off + gre_len)
        return INFMON_PARSE_ERR_OUTER_TRUNCATED;
    off += gre_len;

    /* ── ERSPAN III Header (12 bytes) ───────────────────────────── */
    if (len < off + 12)
        return INFMON_PARSE_ERR_ERSPAN_TRUNCATED;

    uint32_t erspan_w0 = read_u32(data + off);
    uint32_t erspan_w2 = read_u32(data + off + 8);

    /* Ver = bits [31:28] of word 0 */
    uint8_t erspan_ver = (erspan_w0 >> 28) & 0x0F;
    if (erspan_ver != 2)
        return INFMON_PARSE_ERR_ERSPAN_BAD_VERSION;

    /* O bit = bit 0 of word 2 (bit 95 of header, i.e. LSB of word 2) */
    bool o_bit = (erspan_w2 & 0x01) != 0;

    uint32_t erspan_hdr_len = 12 + (o_bit ? 8 : 0);
    if (len < off + erspan_hdr_len)
        return INFMON_PARSE_ERR_ERSPAN_TRUNCATED;
    off += erspan_hdr_len;

    /* ── Compute declared_inner_len ─────────────────────────────── */
    uint32_t overhead_after_ip = gre_len + erspan_hdr_len;
    uint32_t declared_inner_len = 0;
    if (outer_ip_payload_len > overhead_after_ip)
        declared_inner_len = outer_ip_payload_len - overhead_after_ip;

    /* Actual inner bytes available in buffer */
    uint32_t actual_inner_len = (len > off) ? (len - off) : 0;

    /* If declared_inner_len is 0 but we have actual data, the outer IP
     * header is malformed (total_length too small).  Treat as truncated
     * rather than silently parsing trailing buffer bytes. */
    if (declared_inner_len == 0 && actual_inner_len > 0)
        return INFMON_PARSE_ERR_OUTER_TRUNCATED;

    /* Use the smaller of declared and actual */
    uint32_t inner_len = actual_inner_len;
    if (declared_inner_len > 0 && declared_inner_len < inner_len)
        inner_len = declared_inner_len;

    bool truncated = (inner_len < declared_inner_len);

    /* ── Inner-decap hook (v1: identity) ────────────────────────── */
    const uint8_t *inner_ptr = data + off;
    uint32_t decap_len = inner_len;
    bool decap_truncated = truncated;

    if (infmon_inner_decap(INFMON_DECAP_NONE, inner_ptr, inner_len, truncated, &inner_ptr,
                           &decap_len, &decap_truncated) != 0)
        return INFMON_PARSE_ERR_INNER_DOUBLE_ENCAP_DROPPED;

    inner_len = decap_len;
    truncated = decap_truncated;

    /* ── Inner Ethernet (14 B required) ─────────────────────────── */
    if (inner_len < 14)
        return INFMON_PARSE_ERR_INNER_ETH_TRUNCATED;

    /* ── Inner L3 ───────────────────────────────────────────────── */
    uint16_t inner_ethertype = read_u16(inner_ptr + 12);
    uint32_t inner_l3_off = 14;

    /* Handle inner VLAN tag */
    if (inner_ethertype == 0x8100) {
        if (inner_len < inner_l3_off + 4)
            return INFMON_PARSE_ERR_INNER_L3_TRUNCATED;
        inner_ethertype = read_u16(inner_ptr + inner_l3_off + 2);
        inner_l3_off += 4;
    }

    if (inner_ethertype == 0x0800) {
        /* Inner IPv4 */
        if (inner_len < inner_l3_off + 20)
            return INFMON_PARSE_ERR_INNER_L3_TRUNCATED;

        out->inner_af = INFMON_AF_V4;
        out->inner_ip_proto = inner_ptr[inner_l3_off + 9];

        uint8_t inner_ihl = inner_ptr[inner_l3_off] & 0x0F;
        if (inner_ihl < 5) {
            /* Malformed inner IP header — skip L4 extraction */
            out->flow_key_partial = true;
        } else {
            uint32_t inner_ip_hdr_len = (uint32_t) inner_ihl * 4;
            uint32_t inner_l4_off = inner_l3_off + inner_ip_hdr_len;
            extract_inner_l4(inner_ptr, inner_l4_off, inner_len, out->inner_ip_proto, out);
        }

    } else if (inner_ethertype == 0x86DD) {
        /* Inner IPv6 */
        if (inner_len < inner_l3_off + 40)
            return INFMON_PARSE_ERR_INNER_L3_TRUNCATED;

        out->inner_af = INFMON_AF_V6;
        /* NOTE: inner_ip_proto is read from the fixed IPv6 next_hdr field.
         * If the inner packet has extension headers (hop-by-hop, destination
         * options, etc.), this will be the extension type rather than the
         * actual L4 protocol, and port extraction will read the wrong bytes.
         * This is a known limitation for v1 with ~128B ERSPAN snaps. */
        out->inner_ip_proto = inner_ptr[inner_l3_off + 6];

        uint32_t inner_l4_off = inner_l3_off + 40;
        extract_inner_l4(inner_ptr, inner_l4_off, inner_len, out->inner_ip_proto, out);
    } else {
        /* Non-IP inner: we still accept it but can't extract L3/L4 */
        out->flow_key_partial = true;
        out->inner_ptr = inner_ptr;
        out->inner_len = inner_len;
        out->inner_truncated = truncated;
        return truncated ? INFMON_PARSE_INNER_TRUNCATED_OK : INFMON_PARSE_OK;
    }

    out->inner_ptr = inner_ptr;
    out->inner_len = inner_len;
    out->inner_truncated = truncated;

    return truncated ? INFMON_PARSE_INNER_TRUNCATED_OK : INFMON_PARSE_OK;
}
