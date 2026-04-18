# SPDX-License-Identifier: BSD-3-Clause
# Minimal DPDK helloworld build.
# Uses pkg-config to pull DPDK CFLAGS/LDFLAGS from the DOCA-installed libdpdk.pc.
#
# Usage:
#   make            # build ./dpdk-helloworld
#   make run        # run the sanity check (needs hugepages + sudo)
#   make clean

PKG_CONFIG ?= pkg-config
DPDK_PC_PATH ?= /opt/mellanox/dpdk/lib/aarch64-linux-gnu/pkgconfig
export PKG_CONFIG_PATH := $(DPDK_PC_PATH):$(PKG_CONFIG_PATH)

APP := dpdk-helloworld
SRCS := main.c

CFLAGS  += -O2 -Wall -Wextra
CFLAGS  += $(shell $(PKG_CONFIG) --cflags libdpdk)
LDFLAGS += $(shell $(PKG_CONFIG) --libs   libdpdk)

$(APP): $(SRCS)
	$(CC) $(CFLAGS) $^ -o $@ $(LDFLAGS)

.PHONY: run clean
run: $(APP)
	sudo ./$(APP) -l 0-1 --huge-dir /mnt/huge-2M --no-pci

clean:
	rm -f $(APP)
