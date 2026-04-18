#!/usr/bin/env python3
"""
sync-debian-changelog.py — regenerate debian/changelog from CHANGELOG.md.

Per spec 008 §9.2, CHANGELOG.md (Keep a Changelog format) is the
single source of truth for release notes. debian/changelog is a
mechanical projection of it, generated at source-package build time
so the two cannot drift.

Mapping rules
-------------

For every released section in CHANGELOG.md, e.g.

    ## [1.4.2] - 2026-05-01
    ### Added
    - Foo
    ### Fixed
    - Bar

we emit one debian/changelog stanza:

    infmon (1.4.2-1) UNRELEASED; urgency=medium

      * Added: Foo
      * Fixed: Bar

     -- <maintainer>  <RFC2822 timestamp>

The Debian revision suffix defaults to `-1`. To bump it without
changing the upstream version (packaging-only re-roll), edit
DEBIAN_REVISION_OVERRIDES below or pass `--override 1.4.2=2`.

The "[Unreleased]" section, if present, is emitted as a single
stanza with version `<next>~unreleased-1` and distribution UNRELEASED,
where `<next>` is the highest released version with PATCH+1. If no
released version exists yet, version `0.0.1~unreleased-1` is used.

This script is intentionally dependency-free (stdlib only) so it can
run in a minimal Debian build chroot. It is idempotent: running it
twice produces the same output.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import email.utils
import os
import re
import textwrap
import sys
from pathlib import Path
from typing import Dict, List, Tuple

REPO_ROOT = Path(__file__).resolve().parent.parent
CHANGELOG_MD = REPO_ROOT / "CHANGELOG.md"
DEBIAN_CHANGELOG = REPO_ROOT / "debian" / "changelog"

PKG = "infmon"
MAINTAINER = os.environ.get(
    "DEBFULLNAME_EMAIL",
    "InFMon maintainers <r12f.code@gmail.com>",
)
URGENCY = "medium"

# version -> debian-revision override (string like "2"). Use when you
# re-roll packaging without bumping upstream.
DEBIAN_REVISION_OVERRIDES: Dict[str, str] = {}

# Section header regex: "## [VERSION] - DATE"  or  "## [Unreleased]"
SECTION_RE = re.compile(
    r"^##\s+\[(?P<ver>[^\]]+)\](?:\s+-\s+(?P<date>\d{4}-\d{2}-\d{2}))?\s*$"
)
SUBSECTION_RE = re.compile(r"^###\s+(?P<name>.+?)\s*$")
BULLET_RE = re.compile(r"^\s*[-*+]\s+(?P<text>.*\S)\s*$")
SEMVER_RE = re.compile(r"^\d+\.\d+\.\d+(?:[-+~][\w.\-]+)?$")


def _parse_changelog(md: str) -> List[Tuple[str, str | None, List[Tuple[str, str]]]]:
    """
    Return list of (version, iso_date_or_None, [(subsection, bullet)...]).
    Order is preserved from the file (newest first, by convention).
    """
    sections: List[Tuple[str, str | None, List[Tuple[str, str]]]] = []
    cur_ver: str | None = None
    cur_date: str | None = None
    cur_subsection: str = "Changed"
    cur_bullets: List[Tuple[str, str]] = []

    def flush():
        if cur_ver is not None:
            sections.append((cur_ver, cur_date, list(cur_bullets)))

    for line in md.splitlines():
        m = SECTION_RE.match(line)
        if m:
            flush()
            cur_ver = m.group("ver").strip()
            cur_date = m.group("date")
            cur_subsection = "Changed"
            cur_bullets = []
            continue
        if cur_ver is None:
            continue
        sm = SUBSECTION_RE.match(line)
        if sm:
            cur_subsection = sm.group("name").strip()
            continue
        bm = BULLET_RE.match(line)
        if bm:
            cur_bullets.append((cur_subsection, bm.group("text")))
            continue
        # Continuation line: indented text that follows a bullet
        if cur_bullets and line.startswith("  ") and line.strip():
            section, prev = cur_bullets[-1]
            cur_bullets[-1] = (section, prev + " " + line.strip())

    flush()
    return sections


def _next_unreleased_version(released: List[str]) -> str:
    """Pick a synthetic upstream version for the [Unreleased] stanza."""
    semvers = [v for v in released if SEMVER_RE.match(v)]
    if not semvers:
        return "0.0.1"

    def key(v: str):
        core = re.split(r"[-+~]", v, maxsplit=1)[0]
        parts = core.split(".")
        return tuple(int(p) if p.isdigit() else 0 for p in parts[:3])

    top = max(semvers, key=key)
    core = re.split(r"[-+~]", top, maxsplit=1)[0]
    major, minor, patch = (core.split(".") + ["0", "0", "0"])[:3]
    return f"{major}.{minor}.{int(patch) + 1}"


def _deb_version(upstream: str, is_unreleased: bool) -> str:
    rev = DEBIAN_REVISION_OVERRIDES.get(upstream, "1")
    if is_unreleased:
        return f"{upstream}~unreleased-{rev}"
    return f"{upstream}-{rev}"


def _format_stanza(
    upstream_ver: str,
    is_unreleased: bool,
    iso_date: str | None,
    bullets: List[Tuple[str, str]],
    fallback_ts: _dt.datetime,
) -> str:
    deb_ver = _deb_version(upstream_ver, is_unreleased)
    distro = "UNRELEASED" if is_unreleased else "unstable"

    if iso_date:
        d = _dt.datetime.strptime(iso_date, "%Y-%m-%d").replace(
            tzinfo=_dt.timezone.utc
        )
    else:
        d = fallback_ts
    rfc2822 = email.utils.format_datetime(d)

    if not bullets:
        bullet_block = "  * (no changelog entries)\n"
    else:
        lines = []
        for section, text in bullets:
            entry = f"  * {section}: {text}"
            wrapped = textwrap.fill(
                entry, width=78, subsequent_indent="    ",
            )
            lines.append(wrapped + "\n")
        bullet_block = "".join(lines)

    return (
        f"{PKG} ({deb_ver}) {distro}; urgency={URGENCY}\n"
        f"\n"
        f"{bullet_block}"
        f"\n"
        f" -- {MAINTAINER}  {rfc2822}\n"
    )


def render(md_text: str, fallback_ts: _dt.datetime | None = None) -> str:
    sections = _parse_changelog(md_text)
    if fallback_ts is None:
        # Honour SOURCE_DATE_EPOCH for reproducible builds; fall back to
        # a fixed sentinel so repeated runs without it are idempotent.
        sde = os.environ.get("SOURCE_DATE_EPOCH")
        if sde and sde.isdigit():
            fallback_ts = _dt.datetime.fromtimestamp(
                int(sde), tz=_dt.timezone.utc
            )
        else:
            fallback_ts = _dt.datetime(
                2026, 1, 1, 0, 0, 0, tzinfo=_dt.timezone.utc
            )

    if not sections:
        # Fallback: emit a single placeholder stanza so dpkg-parsechangelog
        # does not blow up on an empty CHANGELOG.md.
        return _format_stanza("0.0.1", True, None, [], fallback_ts)

    released_versions = [
        v for (v, _d, _b) in sections if v.lower() != "unreleased"
    ]

    out_parts: List[str] = []
    for ver, iso_date, bullets in sections:
        if ver.lower() == "unreleased":
            synth = _next_unreleased_version(released_versions)
            out_parts.append(
                _format_stanza(synth, True, None, bullets, fallback_ts)
            )
        else:
            out_parts.append(
                _format_stanza(ver, False, iso_date, bullets, fallback_ts)
            )

    return "\n".join(out_parts)


def main(argv: List[str]) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--check", action="store_true",
                   help="exit non-zero if debian/changelog would change")
    p.add_argument("--changelog-md", default=str(CHANGELOG_MD))
    p.add_argument("--debian-changelog", default=str(DEBIAN_CHANGELOG))
    args = p.parse_args(argv)

    md_path = Path(args.changelog_md)
    if not md_path.exists():
        print(f"ERROR: {md_path} not found", file=sys.stderr)
        return 1

    new = render(md_path.read_text(encoding="utf-8"))

    deb_path = Path(args.debian_changelog)
    deb_path.parent.mkdir(parents=True, exist_ok=True)
    old = deb_path.read_text(encoding="utf-8") if deb_path.exists() else ""

    if old == new:
        return 0

    if args.check:
        sys.stderr.write(
            "debian/changelog is out of sync with CHANGELOG.md.\n"
            "Run: python3 tools/sync-debian-changelog.py\n"
        )
        return 2

    deb_path.write_text(new, encoding="utf-8")
    print(f"wrote {deb_path} ({len(new)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
