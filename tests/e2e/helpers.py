"""InFMon interaction helpers for E2E tests.

Wraps infmonctl commands for flow rule management and stats retrieval.
"""

import json
import logging
import subprocess
import time

logger = logging.getLogger(__name__)
from typing import Any, Dict, List, Optional


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


def flow_rule_add(name: str, fields: Dict[str, str], max_keys: int = 0) -> None:
    """Add a flow rule via infmonctl.

    Args:
        name: Rule name.
        fields: Dict of match field name to value.
        max_keys: Maximum number of flow keys (0 = unlimited).
    """
    args = ["flow-rule", "add", "--name", name]
    for field_name, field_value in fields.items():
        args.extend(["--field", f"{field_name}={field_value}"])
    if max_keys > 0:
        args.extend(["--max-keys", str(max_keys)])
    _run_infmonctl(*args)


def flow_rule_rm(name: str) -> None:
    """Remove a flow rule by name."""
    _run_infmonctl("flow-rule", "rm", "--name", name)


def flow_rule_list() -> List[Dict[str, Any]]:
    """List all flow rules, returning parsed JSON."""
    result = _run_infmonctl("flow-rule", "list", "--json")
    parsed = _parse_json_output(result)
    return parsed if isinstance(parsed, list) else []


def get_flow_stats(rule_name: str) -> Optional[Dict[str, Any]]:
    """Get flow statistics for a specific rule, returning parsed JSON."""
    result = _run_infmonctl("stats", "--name", rule_name, "--json", check=False)
    if result.returncode != 0:
        return None
    return _parse_json_output(result)


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
