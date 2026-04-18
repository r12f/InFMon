/* SPDX-License-Identifier: BSD-3-Clause
 * Minimal DPDK environment sanity check.
 * Initialises EAL, prints version + lcore layout, then exits cleanly.
 */
#include <stdio.h>
#include <stdlib.h>
#include <rte_eal.h>
#include <rte_debug.h>
#include <rte_lcore.h>
#include <rte_launch.h>
#include <rte_version.h>

static int
lcore_hello(void *arg)
{
	(void)arg;
	printf("  lcore %u alive on socket %u\n",
	       rte_lcore_id(), rte_socket_id());
	return 0;
}

int
main(int argc, char **argv)
{
	int ret = rte_eal_init(argc, argv);
	if (ret < 0)
		rte_panic("Cannot init EAL\n");

	printf("DPDK %s — EAL initialised, %u lcore(s) enabled\n",
	       rte_version(), rte_lcore_count());

	unsigned lcore_id;
	RTE_LCORE_FOREACH_WORKER(lcore_id)
		rte_eal_remote_launch(lcore_hello, NULL, lcore_id);

	lcore_hello(NULL);
	rte_eal_mp_wait_lcore();

	rte_eal_cleanup();
	return 0;
}
