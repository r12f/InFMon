/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * ERSPAN III / GRE parser — see specs/003-erspan-and-packet-parsing.md
 *
 * Pure-function parser: no allocations, no syscalls, no cross-packet state.
 * Operates on a single contiguous buffer.
 */

#ifndef INFMON_ERSPAN_PARSER_H
#define INFMON_ERSPAN_PARSER_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Error / counter reason codes ─────────────────────────────────── */

typedef enum {
    INFMON_PARSE_OK = 0,              /* full inner packet present          */
    INFMON_PARSE_INNER_TRUNCATED_OK,  /* inner truncated but usable         */
    INFMON_PARSE_ERR_OUTER_QINQ_UNSUPPORTED,
    INFMON_PARSE_ERR_OUTER_ETHERTYPE_UNSUPPORTED,
    INFMON_PARSE_ERR_OUTER_V6_EXT_UNSUPPORTED,
    INFMON_PARSE_ERR_OUTER_TRUNCATED,
    INFMON_PARSE_ERR_MBUF_NOT_CONTIGUOUS,  /* reserved for VPP integration */
    INFMON_PARSE_ERR_GRE_UNEXPECTED_FLAGS,
    INFMON_PARSE_ERR_GRE_BAD_VERSION,
    INFMON_PARSE_ERR_GRE_BAD_PROTO,
    INFMON_PARSE_ERR_ERSPAN_BAD_VERSION,
    INFMON_PARSE_ERR_ERSPAN_TRUNCATED,
    INFMON_PARSE_ERR_INNER_ETH_TRUNCATED,
    INFMON_PARSE_ERR_INNER_L3_TRUNCATED,
    INFMON_PARSE_ERR_INNER_DOUBLE_ENCAP_DROPPED,
    INFMON_PARSE_ERR__COUNT  /* sentinel */
} infmon_parse_result_t;

/* ── Counter names (indexed by infmon_parse_result_t) ─────────────── */

extern const char *infmon_parse_counter_names[];

/* ── IP address family ────────────────────────────────────────────── */

typedef enum {
    INFMON_AF_V4 = 0,
    INFMON_AF_V6,
} infmon_af_t;

/* ── Mirror source IP (tagged union) ──────────────────────────────── */

typedef struct {
    infmon_af_t family;
    union {
        uint8_t v4[4];
        uint8_t v6[16];
    } addr;
} infmon_mirror_src_ip_t;

/* ── Valid-fields bitmask for inner L4 ────────────────────────────── */

#define INFMON_VALID_PORTS      (1u << 0)
#define INFMON_VALID_TCP_FLAGS  (1u << 1)
#define INFMON_VALID_TCP_SEQ    (1u << 2)
#define INFMON_VALID_TCP_ACK    (1u << 3)
#define INFMON_VALID_TCP_WINDOW (1u << 4)

/* ── Inner-decap hook types (§4.6) ────────────────────────────────── */

typedef enum {
    INFMON_DECAP_NONE = 0,
    INFMON_DECAP_VXLAN,   /* future */
    INFMON_DECAP_GENEVE,  /* future */
    INFMON_DECAP_ROCEV2,  /* future */
} infmon_inner_decap_t;

/* ── Parser output ────────────────────────────────────────────────── */

typedef struct {
    const uint8_t *inner_ptr;       /* pointer into original buffer    */
    uint32_t       inner_len;       /* bytes available                 */
    bool           inner_truncated; /* inner_len < declared_inner_len  */

    infmon_mirror_src_ip_t mirror_src_ip;

    /* Inner L4 extracted fields (valid only when corresponding bit set) */
    uint32_t       valid_fields;    /* bitmask of INFMON_VALID_* */
    uint16_t       src_port;        /* network byte order */
    uint16_t       dst_port;        /* network byte order */
    uint8_t        tcp_flags;
    uint32_t       tcp_seq;         /* network byte order */
    uint32_t       tcp_ack;         /* network byte order */
    uint16_t       tcp_window;      /* network byte order */

    bool           flow_key_partial; /* ports missing for TCP/UDP */

    /* Inner L3 info */
    uint8_t        inner_ip_proto;
    infmon_af_t    inner_af;
} infmon_parsed_packet_t;

/* ── Main parser entry point ──────────────────────────────────────── */

/**
 * Parse an ERSPAN III over GRE packet from a contiguous buffer.
 *
 * @param data   Pointer to the start of the outer Ethernet frame.
 * @param len    Length of the buffer in bytes.
 * @param out    Output struct (populated on PARSE_OK or INNER_TRUNCATED_OK).
 *
 * @return INFMON_PARSE_OK or INFMON_PARSE_INNER_TRUNCATED_OK on success;
 *         an error code otherwise.
 */
infmon_parse_result_t
infmon_parse_erspan(const uint8_t *data, uint32_t len,
                    infmon_parsed_packet_t *out);

/* ── Inner-decap hook (§4.6) ──────────────────────────────────────── */

/**
 * v1: only INFMON_DECAP_NONE is implemented (identity).
 */
int infmon_inner_decap(infmon_inner_decap_t kind,
                       const uint8_t *in, uint32_t in_len, bool in_truncated,
                       const uint8_t **out_ptr, uint32_t *out_len,
                       bool *out_truncated);

#ifdef __cplusplus
}
#endif

#endif /* INFMON_ERSPAN_PARSER_H */
