/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * Thin VAPI wrapper for infmon_snapshot_inline_dump.
 *
 * This file is compiled by the Rust `cc` crate and provides a simple
 * C-callable interface for the Rust frontend to call VPP binary API.
 */

#include <stdlib.h>
#include <string.h>
#include <vapi/vapi.h>

/* Pull in the generated VAPI header for our plugin API. */
/* We need the generated header — built by CMake or vapi_c_gen.py.
 * The build.rs passes -I to cc so this resolves. */
#include "infmon.api.vapi.h"

DEFINE_VAPI_MSG_IDS_INFMON_API_JSON;

/* ── Public types (match Rust FFI) ─────────────────────────────────── */

typedef struct {
    uint64_t flow_rule_id_hi;
    uint64_t flow_rule_id_lo;
    uint64_t generation;
    uint64_t epoch_ns;
    uint64_t insert_failed;
    uint64_t table_full;
    uint64_t key_hash;
    uint64_t packets;
    uint64_t bytes;
    uint64_t last_update;
    uint16_t key_len;
    const uint8_t *key_data; /* points into caller-owned buffer */
} infmon_ffi_flow_entry_t;

/**
 * Callback invoked for each flow entry in the snapshot.
 * Return 0 to continue, non-zero to stop iteration.
 */
typedef int (*infmon_ffi_entry_cb)(const infmon_ffi_flow_entry_t *entry, void *ctx);

/* ── Internal state for dump callback ──────────────────────────────── */

typedef struct {
    infmon_ffi_entry_cb cb;
    void *cb_ctx;
    int error;
} infmon_dump_ctx_t;

/* VAPI callback for each details message. */
static vapi_error_e
infmon_snapshot_inline_details_cb(vapi_ctx_t vapi_ctx, void *callback_ctx, vapi_error_e rv,
                                  bool is_last, vapi_payload_infmon_snapshot_inline_details *reply)
{
    (void) vapi_ctx;
    (void) is_last;

    infmon_dump_ctx_t *dctx = (infmon_dump_ctx_t *) callback_ctx;
    if (rv != VAPI_OK || !reply) {
        /* End of dump or error — just return. */
        return VAPI_OK;
    }

    /* If a previous callback already flagged an error, skip remaining entries. */
    if (dctx->error)
        return VAPI_OK;

    infmon_ffi_flow_entry_t entry;
    memset(&entry, 0, sizeof(entry));
    entry.flow_rule_id_hi = reply->flow_rule_id.hi;
    entry.flow_rule_id_lo = reply->flow_rule_id.lo;
    entry.generation = reply->generation;
    entry.epoch_ns = reply->epoch_ns;
    entry.insert_failed = reply->insert_failed;
    entry.table_full = reply->table_full;
    entry.key_hash = reply->key_hash;
    entry.packets = reply->packets;
    entry.bytes = reply->bytes;
    entry.last_update = reply->last_update;
    entry.key_len = reply->key_len;
    entry.key_data = reply->key_data;

    if (dctx->cb) {
        int ret = dctx->cb(&entry, dctx->cb_ctx);
        if (ret != 0)
            dctx->error = ret;
    }

    return VAPI_OK;
}

/* ── VAPI reply callbacks ──────────────────────────────────────────── */

struct flow_rule_add_reply_ctx {
    int32_t retval;
    uint64_t id_hi;
    uint64_t id_lo;
    int got_reply;
};

static vapi_error_e flow_rule_add_reply_cb(vapi_ctx_t vapi_ctx, void *callback_ctx, vapi_error_e rv,
                                           bool is_last,
                                           vapi_payload_infmon_flow_rule_add_reply *reply)
{
    (void) vapi_ctx;
    (void) is_last;
    struct flow_rule_add_reply_ctx *ctx = callback_ctx;
    if (rv == VAPI_OK && reply) {
        ctx->retval = reply->retval;
        ctx->id_hi = reply->flow_rule_id.hi;
        ctx->id_lo = reply->flow_rule_id.lo;
        ctx->got_reply = 1;
    }
    return VAPI_OK;
}

struct flow_rule_del_reply_ctx {
    int32_t retval;
    int got_reply;
};

static vapi_error_e flow_rule_del_reply_cb(vapi_ctx_t vapi_ctx, void *callback_ctx, vapi_error_e rv,
                                           bool is_last,
                                           vapi_payload_infmon_flow_rule_del_reply *reply)
{
    (void) vapi_ctx;
    (void) is_last;
    struct flow_rule_del_reply_ctx *ctx = callback_ctx;
    if (rv == VAPI_OK && reply) {
        ctx->retval = reply->retval;
        ctx->got_reply = 1;
    }
    return VAPI_OK;
}

/* ── Public API ────────────────────────────────────────────────────── */

/**
 * Connect to VPP API.
 * Returns an opaque vapi_ctx_t handle, or NULL on failure.
 */
void *infmon_vapi_connect(const char *name)
{
    vapi_ctx_t ctx;
    vapi_error_e rv = vapi_ctx_alloc(&ctx);
    if (rv != VAPI_OK)
        return NULL;

    rv = vapi_connect(ctx, name, NULL, 256,      /* max outstanding requests */
                      128,                       /* response queue depth */
                      VAPI_MODE_BLOCKING, true); /* is_nonblocking = true for the read path */
    if (rv != VAPI_OK) {
        vapi_ctx_free(ctx);
        return NULL;
    }

    return (void *) ctx;
}

/**
 * Disconnect from VPP API.
 */
void infmon_vapi_disconnect(void *handle)
{
    if (!handle)
        return;
    vapi_ctx_t ctx = (vapi_ctx_t) handle;
    vapi_disconnect(ctx);
    vapi_ctx_free(ctx);
}

/**
 * Perform snapshot_inline_dump for a given flow_rule_id.
 * Calls `cb` for each flow entry.
 * Returns 0 on success, -1 on error.
 */
int infmon_vapi_snapshot_inline(void *handle, uint64_t flow_rule_id_hi, uint64_t flow_rule_id_lo,
                                infmon_ffi_entry_cb cb, void *cb_ctx)
{
    if (!handle)
        return -1;

    vapi_ctx_t ctx = (vapi_ctx_t) handle;

    vapi_msg_infmon_snapshot_inline_dump *msg = vapi_alloc_infmon_snapshot_inline_dump(ctx);
    if (!msg)
        return -1;

    msg->payload.flow_rule_id.hi = flow_rule_id_hi;
    msg->payload.flow_rule_id.lo = flow_rule_id_lo;

    infmon_dump_ctx_t dctx = {
        .cb = cb,
        .cb_ctx = cb_ctx,
        .error = 0,
    };

    vapi_error_e rv =
        vapi_infmon_snapshot_inline_dump(ctx, msg, infmon_snapshot_inline_details_cb, &dctx);

    if (rv != VAPI_OK)
        return -1;

    return dctx.error;
}

/**
 * List all flow rule IDs.
 * Calls `cb` for each entry with hi/lo IDs.
 * Returns 0 on success, -1 on error.
 */
typedef struct {
    void (*cb)(uint64_t hi, uint64_t lo, void *ctx);
    void *ctx;
} infmon_list_ctx_t;

static vapi_error_e infmon_flow_rule_list_cb(vapi_ctx_t vapi_ctx, void *callback_ctx,
                                             vapi_error_e rv, bool is_last,
                                             vapi_payload_infmon_flow_rule_list_details *reply)
{
    (void) vapi_ctx;
    (void) is_last;

    if (rv != VAPI_OK || !reply)
        return VAPI_OK;

    infmon_list_ctx_t *lctx = (infmon_list_ctx_t *) callback_ctx;
    if (lctx->cb)
        lctx->cb(reply->flow_rule.flow_rule_id.hi, reply->flow_rule.flow_rule_id.lo, lctx->ctx);

    return VAPI_OK;
}

int infmon_vapi_list_flow_rules(void *handle, void (*cb)(uint64_t hi, uint64_t lo, void *ctx),
                                void *ctx)
{
    if (!handle)
        return -1;

    vapi_ctx_t vctx = (vapi_ctx_t) handle;

    vapi_msg_infmon_flow_rule_list_dump *msg = vapi_alloc_infmon_flow_rule_list_dump(vctx);
    if (!msg)
        return -1;

    infmon_list_ctx_t lctx = {
        .cb = cb,
        .ctx = ctx,
    };

    vapi_error_e rv = vapi_infmon_flow_rule_list_dump(vctx, msg, infmon_flow_rule_list_cb, &lctx);

    return (rv == VAPI_OK) ? 0 : -1;
}

/* ── flow_rule_add ────────────────────────────────────────────────── */

/**
 * Add a flow rule to the VPP backend.
 * On success, writes the assigned flow_rule_id into *out_id_hi / *out_id_lo.
 * Returns 0 on success, negative retval on error.
 */
int infmon_vapi_flow_rule_add(void *handle, const char *name, const uint8_t *fields,
                              uint32_t field_count, uint32_t max_keys, uint8_t eviction_policy,
                              uint64_t *out_id_hi, uint64_t *out_id_lo)
{
    if (!handle || !name)
        return -1;

    vapi_ctx_t ctx = (vapi_ctx_t) handle;

    vapi_msg_infmon_flow_rule_add *msg = vapi_alloc_infmon_flow_rule_add(ctx);
    if (!msg)
        return -1;

    /* Fill payload */
    if (strlen(name) >= sizeof(msg->payload.name)) {
        vapi_msg_free(ctx, msg);
        return -1;
    }
    memset(msg->payload.name, 0, sizeof(msg->payload.name));
    strncpy((char *) msg->payload.name, name, sizeof(msg->payload.name) - 1);

    msg->payload.field_count = field_count;
    if (field_count > 8) {
        vapi_msg_free(ctx, msg);
        return -1;
    }
    for (uint32_t i = 0; i < field_count; i++)
        msg->payload.fields[i] = fields[i];

    msg->payload.max_keys = max_keys;
    msg->payload.eviction_policy = eviction_policy;

    /* Context struct to capture the reply in the blocking callback. */
    struct flow_rule_add_reply_ctx reply_ctx = {.retval = -1, .got_reply = 0};

    vapi_error_e rv = vapi_infmon_flow_rule_add(ctx, msg,
                                                /* callback */ flow_rule_add_reply_cb, &reply_ctx);
    if (rv != VAPI_OK)
        return -1;

    if (!reply_ctx.got_reply)
        return -1;

    if (reply_ctx.retval != 0)
        return reply_ctx.retval;

    if (out_id_hi)
        *out_id_hi = reply_ctx.id_hi;
    if (out_id_lo)
        *out_id_lo = reply_ctx.id_lo;

    return 0;
}

/* ── flow_rule_del ────────────────────────────────────────────────── */

/**
 * Delete a flow rule from the VPP backend by its ID.
 * Returns 0 on success, negative retval on error.
 */
int infmon_vapi_flow_rule_del(void *handle, uint64_t id_hi, uint64_t id_lo)
{
    if (!handle)
        return -1;

    vapi_ctx_t ctx = (vapi_ctx_t) handle;

    vapi_msg_infmon_flow_rule_del *msg = vapi_alloc_infmon_flow_rule_del(ctx);
    if (!msg)
        return -1;

    msg->payload.flow_rule_id.hi = id_hi;
    msg->payload.flow_rule_id.lo = id_lo;

    struct flow_rule_del_reply_ctx reply_ctx = {.retval = -1, .got_reply = 0};

    vapi_error_e rv = vapi_infmon_flow_rule_del(ctx, msg, flow_rule_del_reply_cb, &reply_ctx);
    if (rv != VAPI_OK)
        return -1;

    if (!reply_ctx.got_reply)
        return -1;

    return reply_ctx.retval;
}
