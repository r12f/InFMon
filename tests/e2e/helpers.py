"""InFMon interaction helpers for E2E tests.

Wraps infmonctl commands for flow rule management and stats retrieval.
"""

import json
import logging
import subprocess
import time
from typing import Any, Dict, List, Optional

logger = logging.getLogger(__name__)


def _run_infmonctl(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    """Run an infmonctl command and return the result."""
    cmd = ["infmonctl"] + list(args)
    return subprocess.run(cmd, capture_output=True, text=True, check=check)


def _parse_json_output(result: subprocess.CompletedProcess) -> Any:
    """Parse JSON from infmonctl stdout."""
    output = result.stdout.strip()
    if not output:
        return None
    return json.loads(output)


def flow_rule_add(name: str, fields: List[str], max_keys: int = 0) -> None:
    """Add a flow rule via infmonctl.

    Uses positional key=value syntax expected by infmonctl:
        infmonctl flow-rule add name=<name> fields=<f1,f2,...> [max_keys=<N>]

    Args:
        name: Rule name.
        fields: List of match field names (e.g. ["src_ip", "dst_ip"]).
        max_keys: Maximum number of flow keys (0 = unlimited).
    """
    if not fields:
        raise ValueError("fields must be non-empty; infmonctl requires at least one match field")
    args = ["flow-rule", "add", f"name={name}", f"fields={','.join(fields)}"]
    if max_keys > 0:
        args.append(f"max_keys={max_keys}")
    _run_infmonctl(*args)


def flow_rule_rm(name: str) -> None:
    """Remove a flow rule by name.

    Uses positional syntax: infmonctl flow-rule rm <name>
    """
    _run_infmonctl("flow-rule", "rm", name)


def flow_rule_list() -> List[Dict[str, Any]]:
    """List all flow rules, returning parsed JSON.

    Falls back to ``stats show --json`` when ``flow-rule list`` is
    broken (protocol deserialization bug on empty list).
    """
    result = _run_infmonctl("flow-rule", "list", "--json", check=False)
    if result.returncode == 0:
        parsed = _parse_json_output(result)
        if isinstance(parsed, list):
            return parsed
    # Fallback: extract rule metadata from stats pull output (forces a
    # fresh snapshot from VPP, avoiding the stale-cache / 0-counter issue).
    result = _run_infmonctl("stats", "pull", "--json", check=False)
    if result.returncode != 0:
        return []
    parsed = _parse_json_output(result)
    if parsed and isinstance(parsed, dict):
        return parsed.get("flow_rules", [])
    return []


def get_flow_stats(rule_name: str) -> Optional[Dict[str, Any]]:
    """Get flow statistics for a specific rule, returning parsed JSON.

    Uses ``stats pull`` which forces a fresh snapshot from VPP, avoiding
    the stale-cache / 0-counter issue that affects ``stats show``.
    """
    result = _run_infmonctl("stats", "pull", "--json", check=False)
    if result.returncode != 0:
        return None
    parsed = _parse_json_output(result)
    if not parsed or not isinstance(parsed, dict):
        return None
    for rule in parsed.get("flow_rules", []):
        if rule.get("name") == rule_name:
            return rule
    return None


def clear_all_flow_rules() -> None:
    """Remove all flow rules."""
    rules = flow_rule_list()
    for rule in rules:
        name = rule.get("name", "")
        if name:
            try:
                flow_rule_rm(name)
            except subprocess.CalledProcessError:
                logger.warning("Failed to remove flow rule %r, continuing", name)


def wait_for_stats(rule_name: str, timeout: float = 30.0, poll_interval: float = 1.0) -> Dict[str, Any]:
    """Wait until flow stats are available for a rule.

    Args:
        rule_name: The flow rule name.
        timeout: Max seconds to wait.
        poll_interval: Seconds between polls.

    Returns:
        Flow stats dict.

    Raises:
        TimeoutError: If stats are not available within the timeout.
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        stats = get_flow_stats(rule_name)
        if stats is not None and stats:
            return stats
        time.sleep(poll_interval)
    raise TimeoutError(f"No stats for rule '{rule_name}' after {timeout}s")
