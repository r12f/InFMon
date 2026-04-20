"""Session-scoped fixtures for InFMon E2E tests.

Sets up networking (VPP RX port, Linux/remote TX port), verifies
connectivity, and optionally pushes replay assets to a remote host.
"""

import os
import shutil
import subprocess
import time

import pytest

# ---------------------------------------------------------------------------
# Environment defaults
# ---------------------------------------------------------------------------

_DEFAULTS = {
    "INFMON_E2E_TX_MODE": "local",
    "INFMON_E2E_TX_IFACE": "p1",
    "INFMON_E2E_TX_HOST": "",
    "INFMON_E2E_TX_HOST_IFACE": "",
    "INFMON_E2E_RX_VPP_IFACE": "TwoHundredGigabitEthernet3/0/0",
    "INFMON_E2E_RX_IP": "10.123.0.1/24",
    "INFMON_E2E_TX_IP": "10.123.0.2/24",
}


def _env(key: str) -> str:
    return os.environ.get(key, _DEFAULTS.get(key, ""))


def _run(cmd: str, check: bool = True, capture: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        cmd, shell=True, check=check, capture_output=capture, text=True
    )


# ---------------------------------------------------------------------------
# Networking helpers
# ---------------------------------------------------------------------------

def _assign_rx_ip() -> None:
    """Assign IP to the VPP RX interface via vppctl."""
    iface = _env("INFMON_E2E_RX_VPP_IFACE")
    ip = _env("INFMON_E2E_RX_IP")
    _run(f"vppctl set interface ip address {iface} {ip}", check=False)
    _run(f"vppctl set interface state {iface} up", check=False)


def _assign_tx_ip_local() -> None:
    """Assign IP to the local Linux TX interface."""
    iface = _env("INFMON_E2E_TX_IFACE")
    ip = _env("INFMON_E2E_TX_IP")
    # Flush existing addresses first to avoid duplicates
    _run(f"ip addr flush dev {iface}", check=False)
    _run(f"ip addr add {ip} dev {iface}")
    _run(f"ip link set {iface} up")


def _assign_tx_ip_remote() -> None:
    """Assign IP on the remote TX host via SSH."""
    host = _env("INFMON_E2E_TX_HOST")
    iface = _env("INFMON_E2E_TX_HOST_IFACE")
    ip = _env("INFMON_E2E_TX_IP")
    _run(f"ssh {host} 'ip addr flush dev {iface}; ip addr add {ip} dev {iface}; ip link set {iface} up'")


def _push_replay_assets(remote_host: str) -> None:
    """SCP replay_traffic.py and scenario directories to the remote host."""
    e2e_dir = os.path.dirname(__file__)
    replay_script = os.path.join(e2e_dir, "replay_traffic.py")
    scenarios_dir = os.path.join(e2e_dir, "scenarios")
    _run(f"scp {replay_script} {remote_host}:/tmp/replay_traffic.py")
    if os.path.isdir(scenarios_dir):
        _run(f"scp -r {scenarios_dir} {remote_host}:/tmp/e2e_scenarios")


def _verify_ping() -> None:
    """Ping the RX IP from the TX side to verify connectivity."""
    rx_ip = _env("INFMON_E2E_RX_IP").split("/")[0]
    mode = _env("INFMON_E2E_TX_MODE")
    if mode == "remote":
        host = _env("INFMON_E2E_TX_HOST")
        cmd = f"ssh {host} 'ping -c 3 -W 2 {rx_ip}'"
    else:
        cmd = f"ping -c 3 -W 2 {rx_ip}"
    result = _run(cmd, check=False)
    if result.returncode != 0:
        pytest.fail(
            f"Ping to RX IP {rx_ip} failed — check physical connectivity and IP config."
        )


def _ensure_infmon_running() -> None:
    """Start InFMon services if they are not already running."""
    result = _run("systemctl is-active infmon", check=False)
    if result.stdout.strip() != "active":
        _run("systemctl start infmon", check=False)
        time.sleep(2)


# ---------------------------------------------------------------------------
# Session fixture
# ---------------------------------------------------------------------------

@pytest.fixture(scope="session")
def infmon_env():
    """Set up networking, verify connectivity, and yield config dict."""
    mode = _env("INFMON_E2E_TX_MODE")

    # 1. Assign IPs
    _assign_rx_ip()
    if mode == "remote":
        host = _env("INFMON_E2E_TX_HOST")
        if not host:
            pytest.fail("INFMON_E2E_TX_HOST must be set in remote mode")
        _assign_tx_ip_remote()
        _push_replay_assets(host)
    else:
        _assign_tx_ip_local()

    # 2. Verify connectivity
    _verify_ping()

    # 3. Ensure InFMon is running
    _ensure_infmon_running()

    # 4. Yield config for tests
    yield {
        "tx_mode": mode,
        "tx_iface": _env("INFMON_E2E_TX_IFACE"),
        "tx_host": _env("INFMON_E2E_TX_HOST"),
        "tx_host_iface": _env("INFMON_E2E_TX_HOST_IFACE"),
        "rx_vpp_iface": _env("INFMON_E2E_RX_VPP_IFACE"),
        "rx_ip": _env("INFMON_E2E_RX_IP"),
        "tx_ip": _env("INFMON_E2E_TX_IP"),
    }
