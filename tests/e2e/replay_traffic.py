#!/usr/bin/env python3
"""Replay pcap traffic with destination IP rewriting.

Can be used as a standalone script or imported as a module.

Standalone usage:
    python3 replay_traffic.py <pcap_file> <dst_ip> <iface> [--count N]

Module usage:
    from replay_traffic import replay
    replay("input.pcap", "10.123.0.1", "p1", count=1)
"""

import argparse
import os
import shlex
import subprocess
import sys
import tempfile

from scapy.all import IP, IPv6, rdpcap, wrpcap


def rewrite_dst_ip(packets, dst_ip: str):
    """Rewrite destination IP in all packets.

    Handles both IPv4 and IPv6 based on the dst_ip format.
    Returns a new list of modified packets.
    """
    rewritten = []
    for pkt in packets:
        pkt = pkt.copy()
        if pkt.haslayer(IP):
            pkt[IP].dst = dst_ip
            del pkt[IP].chksum
            # Force full checksum recalculation for all layers
            pkt = pkt.__class__(bytes(pkt))
        elif pkt.haslayer(IPv6):
            pkt[IPv6].dst = dst_ip
        rewritten.append(pkt)
    return rewritten


def replay(
    pcap_path: str,
    dst_ip: str,
    iface: str,
    count: int = 1,
    remote_host: str = "",
) -> None:
    """Read a pcap, rewrite dst IP, and replay via tcpreplay.

    Args:
        pcap_path: Path to the input pcap file.
        dst_ip: Destination IP to rewrite to.
        iface: Network interface for replay.
        count: Number of times to replay (default: 1).
        remote_host: If set, replay on this host via SSH.
    """
    with tempfile.NamedTemporaryFile(suffix=".pcap", delete=False) as tmp:
        tmp_path = tmp.name

    try:
        packets = rdpcap(pcap_path)
        rewritten = rewrite_dst_ip(packets, dst_ip)
        wrpcap(tmp_path, rewritten)

        loop_arg = f"--loop={count}" if count > 1 else ""
        if remote_host:
            # Copy rewritten pcap to remote and replay there
            remote_tmp = f"/tmp/replay_{os.getpid()}.pcap"
            subprocess.run(
                f"scp {shlex.quote(tmp_path)} {shlex.quote(remote_host)}:{shlex.quote(remote_tmp)}",
                shell=True, check=True, capture_output=True,
            )
            subprocess.run(
                f"ssh {shlex.quote(remote_host)}"
                f" 'tcpreplay {loop_arg} -i {shlex.quote(iface)} {shlex.quote(remote_tmp)};"
                f" rm -f {shlex.quote(remote_tmp)}'",
                shell=True, check=True, capture_output=True,
            )
        else:
            cmd = f"tcpreplay {loop_arg} -i {shlex.quote(iface)} {shlex.quote(tmp_path)}"
            subprocess.run(cmd, shell=True, check=True, capture_output=True)
    finally:
        if os.path.exists(tmp_path):
            os.unlink(tmp_path)


def main():
    parser = argparse.ArgumentParser(description="Replay pcap with dst IP rewriting")
    parser.add_argument("pcap", help="Path to input pcap file")
    parser.add_argument("dst_ip", help="Destination IP to rewrite to")
    parser.add_argument("iface", help="Network interface for replay")
    parser.add_argument("--count", type=int, default=1, help="Replay count (default: 1)")
    parser.add_argument("--remote", default="", help="Remote host for replay via SSH")
    args = parser.parse_args()

    replay(args.pcap, args.dst_ip, args.iface, count=args.count, remote_host=args.remote)


if __name__ == "__main__":
    main()
