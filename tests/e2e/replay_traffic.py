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
import tempfile

from scapy.all import IP, IPv6, rdpcap, wrpcap


def rewrite_dst_ip(packets, dst_ip: str, dst_ip6: str = ""):
    """Rewrite destination IP in all packets.

    For IPv4 packets, uses *dst_ip*.  For IPv6 packets, uses *dst_ip6*
    if provided; otherwise leaves the IPv6 dst unchanged (the outer
    tunnel address is usually not routed anyway).

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
            if dst_ip6:
                pkt[IPv6].dst = dst_ip6
                pkt = pkt.__class__(bytes(pkt))
            # else: leave dst unchanged
        rewritten.append(pkt)
    return rewritten


def replay(
    pcap_path: str,
    dst_ip: str,
    iface: str,
    count: int = 1,
    remote_host: str = "",
    dst_ip6: str = "",
) -> None:
    """Read a pcap, rewrite dst IP, and replay via tcpreplay.

    Args:
        pcap_path: Path to the input pcap file.
        dst_ip: Destination IPv4 to rewrite to.
        iface: Network interface for replay.
        count: Number of times to replay (default: 1).
        remote_host: If set, replay on this host via SSH.
        dst_ip6: Destination IPv6 to rewrite to (optional).
    """
    with tempfile.NamedTemporaryFile(suffix=".pcap", delete=False) as tmp:
        tmp_path = tmp.name

    try:
        packets = rdpcap(pcap_path)
        rewritten = rewrite_dst_ip(packets, dst_ip, dst_ip6=dst_ip6)
        wrpcap(tmp_path, rewritten)

        loop_arg = f"--loop={count}" if count > 1 else ""
        if remote_host:
            # Copy rewritten pcap to remote and replay there
            remote_tmp = f"/tmp/replay_{os.getpid()}.pcap"
            subprocess.run(
                f"scp {shlex.quote(tmp_path)} {shlex.quote(remote_host)}:{shlex.quote(remote_tmp)}",
                shell=True, check=True, capture_output=True,
            )
            remote_cmd = (
                f"tcpreplay {loop_arg} -i {shlex.quote(iface)} {shlex.quote(remote_tmp)}; "
                f"rm -f {shlex.quote(remote_tmp)}"
            )
            subprocess.run(
                f"ssh {shlex.quote(remote_host)} {shlex.quote(remote_cmd)}",
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
