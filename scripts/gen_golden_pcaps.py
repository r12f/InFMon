#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Generate golden PCAP test vectors for ERSPAN III parser tests.
# See specs/003-erspan-and-packet-parsing.md §8.1

import os
import struct
from scapy.all import (
    Ether, IP, IPv6, GRE, Raw, TCP, wrpcap,
    Dot1Q,
)

OUTDIR = os.path.join(os.path.dirname(__file__), "..", "tests", "pcaps", "erspan")
os.makedirs(OUTDIR, exist_ok=True)

# ERSPAN III header builder (scapy's ERSPAN support is limited)
def erspan3_hdr(o_bit=False, ver=2, session_id=0):
    """Build raw ERSPAN III header bytes."""
    # Word 0: ver(4) | vlan(12) | cos(3) | bso(2) | t(1) | session_id(10)
    w0 = (ver << 28) | (session_id & 0x3FF)
    # Word 1: timestamp
    w1 = 0x12345678
    # Word 2: sgt(16) | p(1) | ft(5) | hw_id(6) | d(1) | gra(2) | o(1)
    w2 = 1 if o_bit else 0
    hdr = struct.pack("!III", w0, w1, w2)
    if o_bit:
        hdr += struct.pack("!II", 0xDEADBEEF, 0xCAFEBABE)
    return hdr

# Standard inner TCP packet (Eth + IPv4 + TCP SYN)
def inner_tcp(payload_len=0):
    pkt = (
        Ether(dst="aa:bb:cc:dd:ee:ff", src="11:22:33:44:55:66", type=0x0800) /
        IP(src="192.168.1.1", dst="192.168.1.2", ttl=64) /
        TCP(sport=12345, dport=80, flags="S", seq=0xAABBCCDD, window=65535)
    )
    if payload_len > 0:
        pkt = pkt / Raw(load=b"\x41" * payload_len)
    return pkt

def build_pkt(outer_ip, gre_seq=False, erspan_o=False, erspan_ver=2, inner=None):
    """Build a full ERSPAN III packet."""
    if inner is None:
        inner = bytes(inner_tcp())
    erspan = erspan3_hdr(o_bit=erspan_o, ver=erspan_ver)
    payload = erspan + inner

    gre_flags = 0
    gre_hdr = b""
    if gre_seq:
        gre_flags = 0x1000  # S bit
        gre_hdr = struct.pack("!HH", gre_flags, 0x22EB) + struct.pack("!I", 42)
    else:
        gre_hdr = struct.pack("!HH", gre_flags, 0x22EB)

    gre_payload = gre_hdr + payload
    pkt = Ether(dst="00:11:22:33:44:55", src="66:77:88:99:aa:bb") / outer_ip / Raw(load=gre_payload)
    return pkt

def save(name, pkt):
    path = os.path.join(OUTDIR, name)
    wrpcap(path, [pkt])
    print(f"  {name}: {len(bytes(pkt))} bytes")

print("Generating golden PCAPs...")

# 1. erspan3_full.pcap — full inner packet, no truncation
save("erspan3_full.pcap", build_pkt(IP(src="10.0.0.1", dst="10.0.0.2", proto=47)))

# 2. erspan3_with_seq.pcap — GRE S flag set
save("erspan3_with_seq.pcap", build_pkt(IP(src="10.0.0.1", dst="10.0.0.2", proto=47), gre_seq=True))

# 3. erspan3_o_bit.pcap — Platform-Specific Sub-Header present
save("erspan3_o_bit.pcap", build_pkt(IP(src="10.0.0.1", dst="10.0.0.2", proto=47), erspan_o=True))

# 4. erspan3_o_bit_truncated.pcap — O=1 with mbuf truncated inside the 8-byte sub-header
pkt4 = build_pkt(IP(src="10.0.0.1", dst="10.0.0.2", proto=47), erspan_o=True)
raw4 = bytes(pkt4)
# Truncate 4 bytes into the 8-byte platform sub-header
# Outer Eth(14) + IPv4(20) + GRE(4) + ERSPAN(12) = 50, then sub-header starts
# We need to cut inside the sub-header, so keep up to offset 50 + 4 = 54
trunc_len = 14 + 20 + 4 + 12 + 4  # 54 bytes - mid sub-header
save("erspan3_o_bit_truncated.pcap",
     Ether(raw4[:trunc_len]))

# 5. erspan3_ipv6_full.pcap — IPv6 outer transport, full inner packet
save("erspan3_ipv6_full.pcap",
     build_pkt(IPv6(src="2001:db8::1", dst="2001:db8::2", nh=47)))

# 6. erspan3_ipv6_trunc128.pcap — IPv6 outer with BF-3-style 128 B snap
# Need a large inner to make 128B be a truncation. IPv6 overhead: 14+40+4+12=70
pkt6 = build_pkt(IPv6(src="2001:db8::1", dst="2001:db8::2", nh=47),
                 inner=bytes(inner_tcp(payload_len=200)))
raw6 = bytes(pkt6)
assert len(raw6) > 128, f"pkt6 is only {len(raw6)} bytes"
save("erspan3_ipv6_trunc128.pcap", Ether(raw6[:128]))

# 7. erspan3_trunc128.pcap — BF-3-style 128 B snap (IPv4 outer)
# IPv4 overhead: 14+20+4+12=50
pkt7 = build_pkt(IP(src="10.0.0.1", dst="10.0.0.2", proto=47),
                 inner=bytes(inner_tcp(payload_len=200)))
raw7 = bytes(pkt7)
assert len(raw7) > 128, f"pkt7 is only {len(raw7)} bytes"
save("erspan3_trunc128.pcap", Ether(raw7[:128]))

# 8. erspan3_trunc_outer.pcap — outer-header truncation (must drop)
# Truncate in the middle of outer IPv4 header
pkt8 = build_pkt(IP(src="10.0.0.1", dst="10.0.0.2", proto=47))
raw8 = bytes(pkt8)
save("erspan3_trunc_outer.pcap", Ether(raw8[:24]))  # Only 14 + 10 bytes of IPv4

# 9. erspan3_bad_version.pcap — Ver=1 (must drop)
save("erspan3_bad_version.pcap",
     build_pkt(IP(src="10.0.0.1", dst="10.0.0.2", proto=47), erspan_ver=1))

# 10. erspan3_qinq.pcap — outer QinQ (must drop)
# Build manually with QinQ outer
inner_raw = bytes(inner_tcp())
erspan = erspan3_hdr()
gre_hdr = struct.pack("!HH", 0, 0x22EB)
ip_payload = gre_hdr + erspan + inner_raw
ip_total = 20 + len(ip_payload)
ip_hdr = struct.pack("!BBHHHBBH4s4s",
    0x45, 0, ip_total, 0, 0x4000, 64, 47, 0,
    b"\x0a\x00\x00\x01", b"\x0a\x00\x00\x02")
raw_after_eth = struct.pack("!HH", 0x88A8, 100) + struct.pack("!HH", 0x8100, 200) + \
    struct.pack("!H", 0x0800) + ip_hdr + ip_payload
qinq_pkt = b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb" + raw_after_eth
save("erspan3_qinq.pcap", Ether(qinq_pkt))

# 11. erspan3_gre_keyed.pcap — GRE K flag set (must drop)
inner_raw = bytes(inner_tcp())
erspan = erspan3_hdr()
# GRE with K flag: flags=0x2000, proto=0x22EB, plus 4-byte key
gre_hdr = struct.pack("!HH", 0x2000, 0x22EB) + struct.pack("!I", 0x12345678)
ip_payload = gre_hdr + erspan + inner_raw
ip_total = 20 + len(ip_payload)
ip_hdr = struct.pack("!BBHHHBBH4s4s",
    0x45, 0, ip_total, 0, 0x4000, 64, 47, 0,
    b"\x0a\x00\x00\x01", b"\x0a\x00\x00\x02")
eth = b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb" + struct.pack("!H", 0x0800)
keyed_pkt = eth + ip_hdr + ip_payload
save("erspan3_gre_keyed.pcap", Ether(keyed_pkt))

print("Done!")
