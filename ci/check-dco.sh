#!/usr/bin/env bash
# Reject commit messages missing a `Signed-off-by:` trailer.
# Invoked by the pre-commit `commit-msg` stage; the commit message file
# path is passed as $1 by pre-commit/git.
set -euo pipefail

msg_file="${1:-}"
if [[ -z "$msg_file" || ! -f "$msg_file" ]]; then
    echo "check-dco.sh: missing commit message file argument" >&2
    exit 2
fi

if grep -qE '^Signed-off-by: .+ <.+@.+>$' "$msg_file"; then
    exit 0
fi

cat >&2 <<'EOF'
DCO check failed: commit message is missing a `Signed-off-by:` trailer.

Add it automatically with:
    git commit -s ...

Or amend the current commit with:
    git commit --amend -s --no-edit

See specs/001-ci-and-precommit.md §3 for the project's DCO policy.
EOF
exit 1
