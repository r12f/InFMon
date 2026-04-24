/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * Structured logging for the InFMon VPP plugin.
 *
 * Uses VPP's vlib_log infrastructure so that log lines appear in
 * `show log` and respect per-class level configuration:
 *
 *   set logging class infmon/rule level debug
 *   show log
 *
 * Six subclasses cover the major subsystems:
 *
 *   infmon/general  – plugin lifecycle (init, config)
 *   infmon/api      – binary API handler entry/exit, message-table setup
 *   infmon/rule     – flow-rule add/del operations
 *   infmon/node     – graph-node processing, feature-arc enable/disable
 *   infmon/counter  – snapshot-and-clear, counter-table allocation
 *   infmon/stats    – stats publish/unpublish events
 *
 * Non-VPP builds (unit tests) get silent no-op stubs.
 */

#ifndef INFMON_LOG_H
#define INFMON_LOG_H

#ifdef INFMON_VPP_BUILD

#include <vlib/log.h>
#include <vlib/vlib.h>

/* ── Global log-class handles (defined in infmon_log.c) ────────── */

extern vlib_log_class_t infmon_log_general;
extern vlib_log_class_t infmon_log_api;
extern vlib_log_class_t infmon_log_rule;
extern vlib_log_class_t infmon_log_node;
extern vlib_log_class_t infmon_log_counter;
extern vlib_log_class_t infmon_log_stats;

/* ── Convenience macros — one per (subclass, level) pair ───────── */

/* infmon/general */
#define INFMON_GEN_ERR(...) vlib_log_err(infmon_log_general, __VA_ARGS__)
#define INFMON_GEN_WARN(...) vlib_log_warn(infmon_log_general, __VA_ARGS__)
#define INFMON_GEN_NOTICE(...) vlib_log_notice(infmon_log_general, __VA_ARGS__)
#define INFMON_GEN_INFO(...) vlib_log_info(infmon_log_general, __VA_ARGS__)
#define INFMON_GEN_DEBUG(...) vlib_log_debug(infmon_log_general, __VA_ARGS__)

/* infmon/api */
#define INFMON_API_ERR(...) vlib_log_err(infmon_log_api, __VA_ARGS__)
#define INFMON_API_WARN(...) vlib_log_warn(infmon_log_api, __VA_ARGS__)
#define INFMON_API_NOTICE(...) vlib_log_notice(infmon_log_api, __VA_ARGS__)
#define INFMON_API_INFO(...) vlib_log_info(infmon_log_api, __VA_ARGS__)
#define INFMON_API_DEBUG(...) vlib_log_debug(infmon_log_api, __VA_ARGS__)

/* infmon/rule */
#define INFMON_RULE_ERR(...) vlib_log_err(infmon_log_rule, __VA_ARGS__)
#define INFMON_RULE_WARN(...) vlib_log_warn(infmon_log_rule, __VA_ARGS__)
#define INFMON_RULE_NOTICE(...) vlib_log_notice(infmon_log_rule, __VA_ARGS__)
#define INFMON_RULE_INFO(...) vlib_log_info(infmon_log_rule, __VA_ARGS__)
#define INFMON_RULE_DEBUG(...) vlib_log_debug(infmon_log_rule, __VA_ARGS__)

/* infmon/node */
#define INFMON_NODE_LOG_ERR(...) vlib_log_err(infmon_log_node, __VA_ARGS__)
#define INFMON_NODE_LOG_WARN(...) vlib_log_warn(infmon_log_node, __VA_ARGS__)
#define INFMON_NODE_LOG_NOTICE(...) vlib_log_notice(infmon_log_node, __VA_ARGS__)
#define INFMON_NODE_LOG_INFO(...) vlib_log_info(infmon_log_node, __VA_ARGS__)
#define INFMON_NODE_LOG_DEBUG(...) vlib_log_debug(infmon_log_node, __VA_ARGS__)

/* infmon/counter */
#define INFMON_CTR_ERR(...) vlib_log_err(infmon_log_counter, __VA_ARGS__)
#define INFMON_CTR_WARN(...) vlib_log_warn(infmon_log_counter, __VA_ARGS__)
#define INFMON_CTR_NOTICE(...) vlib_log_notice(infmon_log_counter, __VA_ARGS__)
#define INFMON_CTR_INFO(...) vlib_log_info(infmon_log_counter, __VA_ARGS__)
#define INFMON_CTR_DEBUG(...) vlib_log_debug(infmon_log_counter, __VA_ARGS__)

/* infmon/stats */
#define INFMON_STATS_ERR(...) vlib_log_err(infmon_log_stats, __VA_ARGS__)
#define INFMON_STATS_WARN(...) vlib_log_warn(infmon_log_stats, __VA_ARGS__)
#define INFMON_STATS_NOTICE(...) vlib_log_notice(infmon_log_stats, __VA_ARGS__)
#define INFMON_STATS_INFO(...) vlib_log_info(infmon_log_stats, __VA_ARGS__)
#define INFMON_STATS_DEBUG(...) vlib_log_debug(infmon_log_stats, __VA_ARGS__)

#else /* !INFMON_VPP_BUILD — unit-test / standalone builds */

#define INFMON_GEN_ERR(...) ((void) 0)
#define INFMON_GEN_WARN(...) ((void) 0)
#define INFMON_GEN_NOTICE(...) ((void) 0)
#define INFMON_GEN_INFO(...) ((void) 0)
#define INFMON_GEN_DEBUG(...) ((void) 0)

#define INFMON_API_ERR(...) ((void) 0)
#define INFMON_API_WARN(...) ((void) 0)
#define INFMON_API_NOTICE(...) ((void) 0)
#define INFMON_API_INFO(...) ((void) 0)
#define INFMON_API_DEBUG(...) ((void) 0)

#define INFMON_RULE_ERR(...) ((void) 0)
#define INFMON_RULE_WARN(...) ((void) 0)
#define INFMON_RULE_NOTICE(...) ((void) 0)
#define INFMON_RULE_INFO(...) ((void) 0)
#define INFMON_RULE_DEBUG(...) ((void) 0)

#define INFMON_NODE_LOG_ERR(...) ((void) 0)
#define INFMON_NODE_LOG_WARN(...) ((void) 0)
#define INFMON_NODE_LOG_NOTICE(...) ((void) 0)
#define INFMON_NODE_LOG_INFO(...) ((void) 0)
#define INFMON_NODE_LOG_DEBUG(...) ((void) 0)

#define INFMON_CTR_ERR(...) ((void) 0)
#define INFMON_CTR_WARN(...) ((void) 0)
#define INFMON_CTR_NOTICE(...) ((void) 0)
#define INFMON_CTR_INFO(...) ((void) 0)
#define INFMON_CTR_DEBUG(...) ((void) 0)

#define INFMON_STATS_ERR(...) ((void) 0)
#define INFMON_STATS_WARN(...) ((void) 0)
#define INFMON_STATS_NOTICE(...) ((void) 0)
#define INFMON_STATS_INFO(...) ((void) 0)
#define INFMON_STATS_DEBUG(...) ((void) 0)

#endif /* INFMON_VPP_BUILD */

#endif /* INFMON_LOG_H */
