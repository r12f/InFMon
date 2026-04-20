"""E2E test: packet replay and flow counter verification.

Discovers scenario directories under tests/e2e/scenarios/, replays traffic,
and compares flow counters against expected baselines.
"""

import json
import os
import pathlib
from typing import List

import pytest

from helpers import clear_all_flow_rules, flow_rule_add, wait_for_stats
from replay_traffic import replay

# ---------------------------------------------------------------------------
# Scenario discovery
# ---------------------------------------------------------------------------

_SCENARIOS_DIR = pathlib.Path(__file__).parent / "scenarios"


def discover_scenarios() -> List[str]:
    """Find all scenario directories that have both input.pcap and expected_flows.json."""
    scenarios = []
    if not _SCENARIOS_DIR.is_dir():
        return scenarios
    for entry in sorted(_SCENARIOS_DIR.iterdir()):
        if not entry.is_dir():
            continue
        has_pcap = (entry / "input.pcap").exists()
        has_expected = (entry / "expected_flows.json").exists()
        if has_pcap and has_expected:
            scenarios.append(entry.name)
    return scenarios


# ---------------------------------------------------------------------------
# Test
# ---------------------------------------------------------------------------

_DISCOVERED = discover_scenarios()
if not _DISCOVERED:
    pytest.skip("No E2E scenarios found under scenarios/", allow_module_level=True)


@pytest.mark.parametrize("scenario", _DISCOVERED, ids=lambda s: s)
def test_packet_replay(scenario: str, infmon_env: dict) -> None:
    """Replay a pcap and verify flow counters match the expected baseline.

    Args:
        scenario: Name of the scenario directory under scenarios/.
        infmon_env: Fixture (from conftest.py) providing environment config.
            Expected keys: tx_iface, tx_mode, rx_ip, and optionally tx_host.
    """
    scenario_dir = _SCENARIOS_DIR / scenario
    pcap_path_raw = scenario_dir / "input.pcap"
    resolved = pcap_path_raw.resolve()
    if not resolved.exists():
        pytest.fail(f"input.pcap symlink target missing: {resolved}")
    pcap_path = str(resolved)
    expected_path = scenario_dir / "expected_flows.json"

    refresh_mode = os.environ.get("INFMON_E2E_TEST_REFRESH_BASELINE", "0") == "1"

    # 1. Clear all existing flow rules
    clear_all_flow_rules()

    # 2. Create a flow rule for this scenario.
    #    Use the scenario name as the rule name.
    #    A config file (scenario.json) can override fields/max_keys;
    #    if absent, use a sensible default that captures all traffic.
    config_path = scenario_dir / "scenario.json"
    if config_path.exists():
        with open(config_path) as f:
            config = json.load(f)
        fields = config.get("fields", {})
        max_keys = config.get("max_keys", 0)
    else:
        # Default: match all traffic on the RX interface
        fields = {}
        max_keys = 0  # 0 = unlimited keys (sentinel value in InFMon API)

    flow_rule_add(name=scenario, fields=fields, max_keys=max_keys)

    try:
        # 3. Replay traffic
        tx_iface = infmon_env["tx_iface"]
        tx_mode = infmon_env["tx_mode"]
        tx_host = infmon_env.get("tx_host", "")

        # The dst_ip is the RX side IP (without prefix length)
        dst_ip = infmon_env["rx_ip"].split("/")[0]

        replay(
            pcap_path=pcap_path,
            dst_ip=dst_ip,
            iface=tx_iface,
            remote_host=tx_host if tx_mode == "remote" else "",
        )

        # 4. Pull flow counters
        stats = wait_for_stats(rule_name=scenario, timeout=30.0, poll_interval=1.0)

        # 5. Compare or refresh baseline
        if refresh_mode:
            with open(expected_path, "w") as f:
                json.dump(stats, f, indent=2, sort_keys=True)
                f.write("\n")
            pytest.skip(f"Baseline refreshed for {scenario}")
        else:
            with open(expected_path) as f:
                expected = json.load(f)

            if not expected:
                pytest.fail(
                    f"Expected baseline for {scenario!r} is empty. "
                    f"Run with INFMON_E2E_TEST_REFRESH_BASELINE=1 to populate it."
                )

            assert stats == expected, (
                f"Flow stats mismatch for {scenario!r}.\n"
                f"Expected: {json.dumps(expected, indent=2)}\n"
                f"Actual:   {json.dumps(stats, indent=2)}"
            )
    finally:
        # Ensure flow rules are cleaned up even on failure
        clear_all_flow_rules()
