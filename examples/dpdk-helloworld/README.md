# DPDK helloworld

Minimal DPDK sanity check used to verify the DPU development environment.

See [`docs/dev-environment.md`](../../docs/dev-environment.md) for the full
setup. Quickstart:

```bash
make            # build ./dpdk-helloworld
make run        # sudo + /mnt/huge-2M + --no-pci
make clean
```

Expected output: EAL initialises, two lcores print "alive on socket 0", and
the DPDK version is logged.
