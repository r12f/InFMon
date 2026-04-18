#!/usr/bin/env python3
"""
Unit tests for tools/sync-debian-changelog.py.

Run from repo root:

    python3 -m unittest discover -s tools/tests -v
"""

import datetime as _dt
import importlib.util
import sys
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "tools" / "sync-debian-changelog.py"

spec = importlib.util.spec_from_file_location("sync_changelog", SCRIPT)
sync = importlib.util.module_from_spec(spec)  # type: ignore[arg-type]
sys.modules["sync_changelog"] = sync
spec.loader.exec_module(sync)  # type: ignore[union-attr]

TS = _dt.datetime(2026, 4, 18, 9, 0, 0, tzinfo=_dt.timezone.utc)


class RenderTests(unittest.TestCase):
    def test_unreleased_only(self) -> None:
        md = (
            "# Changelog\n"
            "\n"
            "## [Unreleased]\n"
            "### Added\n"
            "- Debian packaging tree.\n"
        )
        out = sync.render(md, fallback_ts=TS)
        self.assertIn("infmon (0.0.1~unreleased-1) UNRELEASED;", out)
        self.assertIn("* Added: Debian packaging tree.", out)
        self.assertIn("Sat, 18 Apr 2026 09:00:00 +0000", out)

    def test_released_then_unreleased_bumps_patch(self) -> None:
        md = (
            "## [Unreleased]\n"
            "### Added\n"
            "- New shiny.\n"
            "\n"
            "## [1.4.2] - 2026-05-01\n"
            "### Fixed\n"
            "- Bug.\n"
        )
        out = sync.render(md, fallback_ts=TS)
        # The unreleased synthetic version must be 1.4.3 (patch+1).
        self.assertIn("infmon (1.4.3~unreleased-1) UNRELEASED;", out)
        self.assertIn("infmon (1.4.2-1) unstable;", out)
        # Released stanza uses the section date, not the fallback.
        self.assertIn("Fri, 01 May 2026 00:00:00 +0000", out)

    def test_idempotent(self) -> None:
        md = (
            "## [1.0.0] - 2026-01-01\n"
            "### Added\n"
            "- Initial release.\n"
        )
        out1 = sync.render(md, fallback_ts=TS)
        out2 = sync.render(md, fallback_ts=TS)
        self.assertEqual(out1, out2)

    def test_empty_changelog_emits_placeholder(self) -> None:
        md = "# Changelog\n(no entries)\n"
        out = sync.render(md, fallback_ts=TS)
        self.assertIn("infmon (0.0.1~unreleased-1) UNRELEASED;", out)
        self.assertIn("(no changelog entries)", out)

    def test_next_unreleased_version_picks_highest(self) -> None:
        self.assertEqual(
            sync._next_unreleased_version(["1.4.2", "1.10.0", "1.9.9"]),
            "1.10.1",
        )
        self.assertEqual(
            sync._next_unreleased_version([]),
            "0.0.1",
        )


if __name__ == "__main__":
    unittest.main()
