/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * VPP binary API handler implementations.
 *
 * This file bridges VPP's binary API messages (generated from infmon.api)
 * to the portable infmon_api_ctx_t operations defined in api_handler.h.
 *
 * The generated infmon.api.c references handler function symbols by name
 * (e.g. vl_api_infmon_flow_rule_add_t_handler); we define them here.
 *
 * Only compiled when INFMON_VPP_BUILD is defined (plugin build).
 */

#ifdef INFMON_VPP_BUILD

#include <vlib/vlib.h>
#include <vlib/unix/plugin.h>
#include <vlibapi/api.h>
#include <vlibmemory/api.h>
#include <vnet/vnet.h>

/* For the REPLY macros */
#include <vlibapi/api_helper_macros.h>

#include "infmon/api_handler.h"
#include "infmon/counter_table.h"
#include "infmon/flow_rule.h"
#include "infmon/graph_node.h"
#include "infmon/snapshot.h"
#include "infmon/stats_segment.h"

/* Generated API types, enums, endian/calcsize/print helpers.
 * Must appear after the VPP/infmon headers and before any handler code.
 * Suppress -Wpedantic around the generated headers: vppapigen emits
 * zero-length arrays (VLA markers) and GCC statement expressions that
 * -Wpedantic rejects. */
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
#include "infmon.api_enum.h"
#include "infmon.api_types.h"
#pragma GCC diagnostic pop

/* VPP's REPLY_MACRO* uses GCC statement expressions which -Wpedantic
 * rejects.  Suppress for the rest of the file since every handler
 * uses these macros. */
#pragma GCC diagnostic ignored "-Wpedantic"

/* ── Shared state ────────────────────────────────────────────────── */

extern infmon_plugin_main_t infmon_plugin_main;

/* The global API context, shared between binary API handlers and CLI.
 * Initialised lazily on first use.  The rule_set, stats_reg, and
 * snap_mgr are created once and live for the plugin's lifetime. */
static infmon_api_ctx_t infmon_vpp_api_ctx;
static int infmon_vpp_api_ctx_ready = 0;

/* The flow-rule-set-ref used for atomic publish to data plane. */
static infmon_flow_rule_set_ref_t infmon_vpp_rule_set_ref;

/* msg_id_base returned by setup_message_id_table */
static u16 infmon_msg_id_base;
#undef REPLY_MSG_ID_BASE
#define REPLY_MSG_ID_BASE infmon_msg_id_base

/* ── Helpers ─────────────────────────────────────────────────────── */

/**
 * Publish the current flow rules + counter tables to the data plane
 * by updating infmon_plugin_main atomically.
 */
static void
infmon_vpp_publish_rules (void)
{
  infmon_plugin_main_t *pm = &infmon_plugin_main;
  infmon_api_ctx_t *ctx = &infmon_vpp_api_ctx;

  uint32_t n =
    ctx->rule_set ? infmon_flow_rule_count (ctx->rule_set) : 0;

  infmon_vpp_rule_set_ref.rules =
    (n > 0) ? infmon_flow_rule_get (ctx->rule_set, 0) : NULL;
  infmon_vpp_rule_set_ref.count = n;

  __atomic_store_n (&pm->flow_rule_set, &infmon_vpp_rule_set_ref,
		    __ATOMIC_RELEASE);

  /* Sync counter-table pointers */
  for (uint32_t i = 0; i < INFMON_MAX_ACTIVE_FLOW_RULES; i++)
    __atomic_store_n (&pm->tables[i], ctx->tables[i], __ATOMIC_RELEASE);
}

/**
 * Ensure the API context is initialised (lazy, idempotent).
 */
static void
infmon_vpp_api_ctx_ensure (void)
{
  if (infmon_vpp_api_ctx_ready)
    return;

  infmon_flow_rule_set_t *rs =
    infmon_flow_rule_set_create (INFMON_FLOW_RULE_MAX_KEYS_BUDGET);

  /* Stats registry — lives in-process, segment_base=0 (no shared mem). */
  static infmon_stats_registry_t stats_reg;
  infmon_stats_registry_init (&stats_reg, 0);

  infmon_api_ctx_init (&infmon_vpp_api_ctx, rs, &stats_reg);

  /* Create a snapshot manager. */
  static infmon_snapshot_mgr_t snap_mgr;
  infmon_snapshot_mgr_init (&snap_mgr,
                            vlib_num_workers () + 1,
                            /* grace_ns */ 1000000000ULL,
                            /* clock_ns */ NULL);
  infmon_vpp_api_ctx.snap_mgr = &snap_mgr;

  infmon_vpp_api_ctx_ready = 1;
}

/**
 * Map infmon_api_result_t → VPP-style retval (negative on error).
 */
static i32
infmon_api_result_to_retval (infmon_api_result_t r)
{
  switch (r)
    {
    case INFMON_API_OK:
      return 0;
    case INFMON_API_ERR_INVALID_RULE:
      return VNET_API_ERROR_INVALID_VALUE;
    case INFMON_API_ERR_NAME_EXISTS:
      return VNET_API_ERROR_ENTRY_ALREADY_EXISTS;
    case INFMON_API_ERR_NOT_FOUND:
      return VNET_API_ERROR_NO_SUCH_ENTRY;
    case INFMON_API_ERR_BUDGET_EXCEEDED:
      return VNET_API_ERROR_EXCEEDED_NUMBER_OF_RANGES_CAPACITY;
    case INFMON_API_ERR_SET_FULL:
      return VNET_API_ERROR_TABLE_TOO_BIG;
    case INFMON_API_ERR_ALLOC_FAILED:
      return VNET_API_ERROR_INIT_FAILED;
    default:
      return VNET_API_ERROR_UNSPECIFIED;
    }
}

/* ── Handler: flow_rule_add ──────────────────────────────────────── */

static void
vl_api_infmon_flow_rule_add_t_handler (vl_api_infmon_flow_rule_add_t *mp)
{
  vl_api_infmon_flow_rule_add_reply_t *rmp;
  i32 rv = 0;

  infmon_vpp_api_ctx_ensure ();

  /* Convert API message to infmon_flow_rule_t */
  infmon_flow_rule_t rule;
  clib_memset (&rule, 0, sizeof (rule));

  /* Copy name (null-terminated, 32 bytes in API) */
  clib_memcpy_fast (rule.name, mp->name,
		    sizeof (rule.name) < sizeof (mp->name)
		      ? sizeof (rule.name)
		      : sizeof (mp->name));
  rule.name[INFMON_FLOW_RULE_NAME_MAX] = '\0';

  rule.field_count = clib_net_to_host_u32 (mp->field_count);
  if (rule.field_count > INFMON_FLOW_RULE_FIELDS_MAX)
    rule.field_count = INFMON_FLOW_RULE_FIELDS_MAX;

  for (uint32_t i = 0; i < rule.field_count; i++)
    rule.fields[i] = (infmon_field_t) mp->fields[i];

  rule.max_keys = clib_net_to_host_u32 (mp->max_keys);
  if (rule.max_keys == 0)
    rule.max_keys = 65536;

  rule.eviction_policy = (infmon_eviction_policy_t) mp->eviction_policy;

  /* Generate a UUID for this rule */
  infmon_flow_rule_id_t id;
  /* Use VPP's random + timestamp for a simple unique ID */
  id.hi = clib_cpu_time_now () ^ (u64) mp->client_index;
  id.lo = clib_cpu_time_now () ^ 0xdeadbeefcafebabeULL;

  infmon_api_result_t result =
    infmon_api_flow_rule_add_with_id (&infmon_vpp_api_ctx, &rule, id);

  rv = infmon_api_result_to_retval (result);

  if (result == INFMON_API_OK)
    infmon_vpp_publish_rules ();

  REPLY_MACRO2 (VL_API_INFMON_FLOW_RULE_ADD_REPLY, ({
		  if (result == INFMON_API_OK)
		    {
		      rmp->flow_rule_id.hi = clib_host_to_net_u64 (id.hi);
		      rmp->flow_rule_id.lo = clib_host_to_net_u64 (id.lo);
		    }
		  else
		    {
		      rmp->flow_rule_id.hi = 0;
		      rmp->flow_rule_id.lo = 0;
		    }
		}));
}

/* ── Handler: flow_rule_del ──────────────────────────────────────── */

static void
vl_api_infmon_flow_rule_del_t_handler (vl_api_infmon_flow_rule_del_t *mp)
{
  vl_api_infmon_flow_rule_del_reply_t *rmp;
  i32 rv = 0;

  infmon_vpp_api_ctx_ensure ();

  /* Find the rule by ID */
  infmon_flow_rule_id_t id;
  id.hi = clib_net_to_host_u64 (mp->flow_rule_id.hi);
  id.lo = clib_net_to_host_u64 (mp->flow_rule_id.lo);

  /* Search for the rule with this ID to get its name */
  const char *name = NULL;
  infmon_api_ctx_t *ctx = &infmon_vpp_api_ctx;
  for (uint32_t i = 0; i < INFMON_FLOW_RULE_SET_MAX; i++)
    {
      if (infmon_flow_rule_id_eq (ctx->flow_rule_ids[i], id))
	{
	  const infmon_flow_rule_t *r =
	    infmon_flow_rule_get (ctx->rule_set, i);
	  if (r)
	    name = r->name;
	  break;
	}
    }

  if (name)
    {
      infmon_api_result_t result =
	infmon_api_flow_rule_del (&infmon_vpp_api_ctx, name);
      rv = infmon_api_result_to_retval (result);
      if (result == INFMON_API_OK)
	infmon_vpp_publish_rules ();
    }
  else
    {
      rv = VNET_API_ERROR_NO_SUCH_ENTRY;
    }

  REPLY_MACRO (VL_API_INFMON_FLOW_RULE_DEL_REPLY);
}

/* ── Handler: flow_rule_list_dump ────────────────────────────────── */

typedef struct
{
  vl_api_registration_t *rp;
  u32 context;
  u16 msg_id_base;
  infmon_api_ctx_t *ctx;
} infmon_list_walk_ctx_t;

static void
infmon_flow_rule_list_cb (const infmon_flow_rule_t *rule, uint32_t index,
			  void *user)
{
  infmon_list_walk_ctx_t *wctx = (infmon_list_walk_ctx_t *) user;

  vl_api_infmon_flow_rule_list_details_t *rmp =
    vl_msg_api_alloc (sizeof (*rmp));
  clib_memset (rmp, 0, sizeof (*rmp));

  rmp->_vl_msg_id =
    htons (VL_API_INFMON_FLOW_RULE_LIST_DETAILS + wctx->msg_id_base);
  rmp->context = wctx->context;

  /* Fill in flow_rule details */
  vl_api_infmon_flow_rule_details_t *d = &rmp->flow_rule;

  d->flow_rule_id.hi =
    clib_host_to_net_u64 (wctx->ctx->flow_rule_ids[index].hi);
  d->flow_rule_id.lo =
    clib_host_to_net_u64 (wctx->ctx->flow_rule_ids[index].lo);
  d->flow_rule_index = clib_host_to_net_u32 (index);
  clib_memcpy_fast (d->name, rule->name, sizeof (d->name));
  d->field_count = clib_host_to_net_u32 (rule->field_count);
  for (uint32_t i = 0; i < rule->field_count && i < 8; i++)
    d->fields[i] = (vl_api_infmon_api_field_type_t) rule->fields[i];
  d->max_keys = clib_host_to_net_u32 (rule->max_keys);
  d->eviction_policy =
    (vl_api_infmon_api_eviction_policy_t) rule->eviction_policy;
  d->key_width = clib_host_to_net_u32 (rule->key_width);

  vl_api_send_msg (wctx->rp, (u8 *) rmp);
}

static void
vl_api_infmon_flow_rule_list_dump_t_handler (
  vl_api_infmon_flow_rule_list_dump_t *mp)
{
  vl_api_registration_t *rp =
    vl_api_client_index_to_registration (mp->client_index);
  if (!rp)
    return;

  infmon_vpp_api_ctx_ensure ();

  infmon_list_walk_ctx_t wctx = {
    .rp = rp,
    .context = mp->context,
    .msg_id_base = infmon_msg_id_base,
    .ctx = &infmon_vpp_api_ctx,
  };

  infmon_api_flow_rule_list (&infmon_vpp_api_ctx, infmon_flow_rule_list_cb,
			     &wctx);
}

/* ── Handler: flow_rule_get ──────────────────────────────────────── */

static void
vl_api_infmon_flow_rule_get_t_handler (vl_api_infmon_flow_rule_get_t *mp)
{
  vl_api_infmon_flow_rule_get_reply_t *rmp;
  i32 rv = 0;

  infmon_vpp_api_ctx_ensure ();

  infmon_flow_rule_id_t id;
  id.hi = clib_net_to_host_u64 (mp->flow_rule_id.hi);
  id.lo = clib_net_to_host_u64 (mp->flow_rule_id.lo);

  /* Find rule by ID */
  const infmon_flow_rule_t *found = NULL;
  uint32_t found_index = 0;
  infmon_api_ctx_t *ctx = &infmon_vpp_api_ctx;

  for (uint32_t i = 0; i < INFMON_FLOW_RULE_SET_MAX; i++)
    {
      if (infmon_flow_rule_id_eq (ctx->flow_rule_ids[i], id))
	{
	  found = infmon_flow_rule_get (ctx->rule_set, i);
	  found_index = i;
	  break;
	}
    }

  if (!found)
    rv = VNET_API_ERROR_NO_SUCH_ENTRY;

  REPLY_MACRO2 (VL_API_INFMON_FLOW_RULE_GET_REPLY, ({
		  if (found)
		    {
		      vl_api_infmon_flow_rule_details_t *d = &rmp->flow_rule;
		      d->flow_rule_id.hi = clib_host_to_net_u64 (id.hi);
		      d->flow_rule_id.lo = clib_host_to_net_u64 (id.lo);
		      d->flow_rule_index =
			clib_host_to_net_u32 (found_index);
		      clib_memcpy_fast (d->name, found->name,
					sizeof (d->name));
		      d->field_count =
			clib_host_to_net_u32 (found->field_count);
		      for (uint32_t i = 0;
			   i < found->field_count && i < 8; i++)
			d->fields[i] = (vl_api_infmon_api_field_type_t)
			  found->fields[i];
		      d->max_keys =
			clib_host_to_net_u32 (found->max_keys);
		      d->eviction_policy =
			(vl_api_infmon_api_eviction_policy_t)
			  found->eviction_policy;
		      d->key_width =
			clib_host_to_net_u32 (found->key_width);
		    }
		  else
		    {
		      clib_memset (&rmp->flow_rule, 0,
				   sizeof (rmp->flow_rule));
		    }
		}));
}

/* ── Handler: snapshot_and_clear ──────────────────────────────────── */

static void
vl_api_infmon_snapshot_and_clear_t_handler (
  vl_api_infmon_snapshot_and_clear_t *mp)
{
  vl_api_infmon_snapshot_and_clear_reply_t *rmp;
  i32 rv = 0;

  infmon_vpp_api_ctx_ensure ();

  infmon_flow_rule_id_t id;
  id.hi = clib_net_to_host_u64 (mp->flow_rule_id.hi);
  id.lo = clib_net_to_host_u64 (mp->flow_rule_id.lo);

  infmon_api_snap_reply_t snap_reply;
  clib_memset (&snap_reply, 0, sizeof (snap_reply));

  infmon_api_result_t result =
    infmon_api_snapshot_and_clear (&infmon_vpp_api_ctx, id, &snap_reply);

  rv = infmon_api_result_to_retval (result);

  if (result == INFMON_API_OK)
    infmon_vpp_publish_rules ();

  REPLY_MACRO2 (VL_API_INFMON_SNAPSHOT_AND_CLEAR_REPLY, ({
		  if (result == INFMON_API_OK)
		    {
		      infmon_stats_descriptor_t *desc =
			&snap_reply.descriptor;
		      vl_api_infmon_table_descriptor_t *td =
			&rmp->descriptor;

		      td->flow_rule_id.hi =
			clib_host_to_net_u64 (desc->flow_rule_id.hi);
		      td->flow_rule_id.lo =
			clib_host_to_net_u64 (desc->flow_rule_id.lo);
		      td->flow_rule_index =
			clib_host_to_net_u32 (desc->flow_rule_index);
		      td->generation =
			clib_host_to_net_u64 (desc->generation);
		      td->epoch_ns =
			clib_host_to_net_u64 (desc->epoch_ns);
		      td->slots_offset =
			clib_host_to_net_u64 (desc->slots_offset);
		      td->slots_len =
			clib_host_to_net_u32 (desc->slots_len);
		      td->key_arena_offset =
			clib_host_to_net_u64 (desc->key_arena_offset);
		      td->key_arena_capacity =
			clib_host_to_net_u32 (desc->key_arena_capacity);
		      td->key_arena_used =
			clib_host_to_net_u32 (desc->key_arena_used);
		      td->insert_failed =
			clib_host_to_net_u64 (desc->insert_failed);
		      td->table_full =
			clib_host_to_net_u64 (desc->table_full);
		    }
		  else
		    {
		      clib_memset (&rmp->descriptor, 0,
				   sizeof (rmp->descriptor));
		    }
		}));
}

/* ── Handler: status_dump ────────────────────────────────────────── */

static void
vl_api_infmon_status_dump_t_handler (vl_api_infmon_status_dump_t *mp)
{
  vl_api_registration_t *rp =
    vl_api_client_index_to_registration (mp->client_index);
  if (!rp)
    return;

  infmon_vpp_api_ctx_ensure ();

  infmon_api_status_reply_t status;
  infmon_api_result_t result =
    infmon_api_status (&infmon_vpp_api_ctx, &status);

  if (result != INFMON_API_OK)
    return;

  u32 wc = status.worker_count;
  u32 msg_size =
    sizeof (vl_api_infmon_status_details_t) +
    wc * sizeof (vl_api_infmon_worker_status_t);

  vl_api_infmon_status_details_t *rmp = vl_msg_api_alloc (msg_size);
  clib_memset (rmp, 0, msg_size);

  rmp->_vl_msg_id =
    htons (VL_API_INFMON_STATUS_DETAILS + infmon_msg_id_base);
  rmp->context = mp->context;
  rmp->worker_count = clib_host_to_net_u32 (wc);

  for (u32 i = 0; i < wc; i++)
    {
      const infmon_worker_counters_t *wk = &status.workers[i];
      vl_api_infmon_worker_status_t *ws = &rmp->workers[i];

      ws->worker_id = clib_host_to_net_u32 (wk->worker_id);
      ws->packets_seen =
	clib_host_to_net_u64 (
	  __atomic_load_n (&wk->packets_seen, __ATOMIC_RELAXED));
      ws->erspan_unknown_proto =
	clib_host_to_net_u64 (
	  __atomic_load_n (&wk->erspan_unknown_proto, __ATOMIC_RELAXED));
      ws->erspan_truncated =
	clib_host_to_net_u64 (
	  __atomic_load_n (&wk->erspan_truncated, __ATOMIC_RELAXED));
      ws->inner_parse_failed =
	clib_host_to_net_u64 (
	  __atomic_load_n (&wk->inner_parse_failed, __ATOMIC_RELAXED));
      ws->flow_rule_no_match =
	clib_host_to_net_u64 (
	  __atomic_load_n (&wk->flow_rule_no_match, __ATOMIC_RELAXED));
      ws->counter_insert_retry_exhausted =
	clib_host_to_net_u64 (
	  __atomic_load_n (&wk->counter_insert_retry_exhausted,
			   __ATOMIC_RELAXED));
      ws->counter_table_full =
	clib_host_to_net_u64 (
	  __atomic_load_n (&wk->counter_table_full, __ATOMIC_RELAXED));
    }

  vl_api_send_msg (rp, (u8 *) rmp);
}

/* ── API init ────────────────────────────────────────────────────── */

/* Include the generated API registration code.
 * This defines setup_message_id_table() which references all the
 * handler symbols defined above. */
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
#pragma GCC diagnostic ignored "-Woverlength-strings"
#pragma GCC diagnostic ignored "-Waddress-of-packed-member"
#pragma GCC diagnostic ignored "-Wunused-parameter"
#pragma GCC diagnostic ignored "-Wpointer-arith"
#pragma GCC diagnostic ignored "-Wsign-compare"
#define my_api_main (vlibapi_get_main())
#include "infmon.api.c"
#undef my_api_main
#pragma GCC diagnostic pop

static clib_error_t *
infmon_api_init (CLIB_UNUSED (vlib_main_t *vm))
{
  infmon_msg_id_base = setup_message_id_table ();
  return 0;
}

#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
VLIB_API_INIT_FUNCTION (infmon_api_init);
#pragma GCC diagnostic pop

/* ── Accessor for CLI integration ────────────────────────────────── */

/**
 * Return the shared API context, lazily initialised.
 * CLI commands in infmon_nodes.c should call this instead of
 * maintaining their own rule set.
 */
infmon_api_ctx_t *
infmon_vpp_get_api_ctx (void)
{
  infmon_vpp_api_ctx_ensure ();
  return &infmon_vpp_api_ctx;
}

/**
 * Publish current rules to the data plane.
 * Called by CLI after modifications to keep data plane in sync.
 */
void
infmon_vpp_publish (void)
{
  infmon_vpp_publish_rules ();
}

#endif /* INFMON_VPP_BUILD */
