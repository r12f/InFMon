/*
 * SPDX-License-Identifier: Apache-2.0
 * Copyright 2026 Riff
 *
 * Log class registration for the InFMon VPP plugin.
 */

#ifdef INFMON_VPP_BUILD

#include <vlib/log.h>
#include <vlib/vlib.h>

vlib_log_class_t infmon_log_general;
vlib_log_class_t infmon_log_api;
vlib_log_class_t infmon_log_rule;
vlib_log_class_t infmon_log_node;
vlib_log_class_t infmon_log_counter;
vlib_log_class_t infmon_log_stats;

static clib_error_t *infmon_log_init(CLIB_UNUSED(vlib_main_t *vm))
{
    infmon_log_general = vlib_log_register_class("infmon", "general");
    infmon_log_api = vlib_log_register_class("infmon", "api");
    infmon_log_rule = vlib_log_register_class("infmon", "rule");
    infmon_log_node = vlib_log_register_class("infmon", "node");
    infmon_log_counter = vlib_log_register_class("infmon", "counter");
    infmon_log_stats = vlib_log_register_class("infmon", "stats");

    vlib_log_notice(infmon_log_general, "logging subsystem initialized");
    return 0;
}

/*
 * Run early so log classes are available before other init functions.
 * .runs_before ensures infmon_log_init completes before any future
 * infmon_*_init that registers a dependency on it.
 */
VLIB_INIT_FUNCTION(infmon_log_init) = {
    .runs_before = VLIB_INITS("infmon_api_init", "infmon_node_init"),
};

#endif /* INFMON_VPP_BUILD */
