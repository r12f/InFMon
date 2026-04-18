#!/usr/bin/env bash
# Wrapper for the clippy pre-commit hook.
#
# Skips cleanly with a NOTE when no Cargo workspace exists yet, so
# `pre-commit run --all-files` (and therefore `make lint` and the `lint`
# CI job) stays green on the bootstrap repo state. Once any Cargo.toml
# lands, this becomes a hard gate exactly like running cargo clippy directly.
set -euo pipefail

if [ ! -f Cargo.toml ] && [ -z "$(find . -maxdepth 4 -name Cargo.toml -not -path './target/*' -print -quit 2>/dev/null)" ]; then
    echo "clippy: no Cargo.toml in tree, skipping (bootstrap state)."
    exit 0
fi

exec cargo clippy --workspace --all-targets --all-features -- -D warnings
