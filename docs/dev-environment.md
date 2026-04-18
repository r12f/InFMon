# DPDK Development Environment

This document describes the DPDK toolchain InFMon targets, how to set it up on
an NVIDIA BlueField-3 DPU running Ubuntu 24.04 with DOCA installed, and how to
verify the environment with the `examples/dpdk-helloworld` sanity check.

## Target platform

| Component | Version (verified) |
|-----------|--------------------|
| DPU       | NVIDIA BlueField-3 (`MT43244` ConnectX-7) |
| OS        | Ubuntu 24.04 LTS (`aarch64`) |
| Kernel    | 6.8.0-1016-bluefield |
| DOCA      | 3.3 (`doca-devel` 1-3.3.0109-1) |
| DPDK      | 25.11.0+doca2601.2 (shipped via `/opt/mellanox/dpdk`) |
| GCC       | 13.3.0 |

DPDK is installed by the DOCA stack. Headers, libraries and the `dpdk-*`
helper binaries live under `/opt/mellanox/dpdk/`. Build flags are exposed via
pkg-config:

```
/opt/mellanox/dpdk/lib/aarch64-linux-gnu/pkgconfig/libdpdk.pc
```

## Host packages

DOCA does not pull in the toolchain itself. Install the build essentials once
per machine:

```bash
sudo apt-get update
sudo apt-get install -y \
    build-essential \
    pkg-config \
    meson ninja-build \
    python3-pyelftools \
    libnuma-dev
```

`meson` / `ninja` are only required if you plan to build DPDK itself or any
DPDK-based project that uses meson. The `examples/dpdk-helloworld` sanity check
uses plain `make` + `pkg-config` and works without them.

## Hugepages

DPDK requires hugepages. The BlueField kernel exposes three sizes
(`/sys/kernel/mm/hugepages/hugepages-{2048,524288,16777216}kB`). For
development we use 2 MiB pages because they are easy to reserve at runtime
without a reboot.

Reserve 1024 × 2 MiB pages (2 GiB) and mount a hugetlbfs that DPDK can target:

```bash
echo 1024 | sudo tee /sys/kernel/mm/hugepages/hugepages-2048kB/nr_hugepages
sudo mkdir -p /mnt/huge-2M
sudo mount -t hugetlbfs -o pagesize=2M nodev /mnt/huge-2M
```

> The default `/dev/hugepages` mount on this image uses 512 MiB pages and has
> zero pages reserved, so DPDK falls back to `--no-huge` or fails. Always
> point DPDK at `/mnt/huge-2M` with `--huge-dir /mnt/huge-2M` until persistent
> reservations are configured (`/etc/fstab` + kernel cmdline).

To make the reservation persist across reboots, add to `/etc/fstab`:

```
nodev  /mnt/huge-2M  hugetlbfs  pagesize=2M  0 0
```

and add `default_hugepagesz=2M hugepagesz=2M hugepages=1024` to the kernel
cmdline.

## NIC inventory

Confirm the DPU NICs are visible to DPDK tooling:

```bash
/opt/mellanox/dpdk/bin/dpdk-devbind.py --status-dev net
```

Expected on a BlueField-3 (both ports bound to `mlx5_core`, which is what
DPDK's `mlx5` PMD wants — no `vfio-pci` rebind needed):

```
0000:03:00.0 'BlueField-3 ... ConnectX-7' drv=mlx5_core
0000:03:00.1 'BlueField-3 ... ConnectX-7' drv=mlx5_core
```

## Sanity check

A minimal helloworld lives under [`examples/dpdk-helloworld`](../examples/dpdk-helloworld).
It links against DPDK via `pkg-config`, initialises the EAL on two lcores and
prints the runtime version.

```bash
cd examples/dpdk-helloworld
make
make run        # uses sudo + /mnt/huge-2M + --no-pci
```

Expected output (abridged):

```
EAL: Detected CPU lcores: 16
EAL: Detected NUMA nodes: 1
EAL: Selected IOVA mode 'VA'
DPDK 25.11.0+doca2601 — EAL initialised, 2 lcore(s) enabled
  lcore 1 alive on socket 0
  lcore 0 alive on socket 0
```

If you see this, the development environment is ready.

## Build flag cheat sheet

For projects that aren't bundled with InFMon, the canonical way to consume
DOCA-shipped DPDK is:

```bash
export PKG_CONFIG_PATH=/opt/mellanox/dpdk/lib/aarch64-linux-gnu/pkgconfig:$PKG_CONFIG_PATH
pkg-config --cflags libdpdk
pkg-config --libs   libdpdk
```

Drop those into `CFLAGS` / `LDFLAGS` and you're done.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---------|--------------|-----|
| `EAL: Cannot get hugepage information` | No hugepages reserved or wrong dir | Reserve pages, pass `--huge-dir /mnt/huge-2M` |
| `cannot find -lrte_*` at link time | Wrong pkg-config path | `export PKG_CONFIG_PATH=/opt/mellanox/dpdk/lib/aarch64-linux-gnu/pkgconfig:$PKG_CONFIG_PATH` |
| `undefined reference to rte_panic` | Missing `<rte_debug.h>` include | Include `<rte_debug.h>` (and `<rte_launch.h>` for `rte_eal_remote_launch`) |
| Permission denied on `/dev/hugepages` | DPDK launched as non-root | Run with `sudo` or grant your user `CAP_SYS_NICE` + hugetlbfs group |
