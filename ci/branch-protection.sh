#!/usr/bin/env bash
# Apply branch-protection settings on `main` per specs/001-ci-and-precommit.md §7.
#
# Requirements: `gh` CLI authenticated as a repo admin.
# Usage:        ci/branch-protection.sh [owner/repo]
#
# Defaults to r12f/InFMon if no argument is supplied. Idempotent — re-running
# overwrites the existing protection block with the canonical settings below.
set -euo pipefail

REPO="${1:-r12f/InFMon}"
BRANCH="${BRANCH:-main}"

if ! command -v gh >/dev/null 2>&1; then
    echo "gh CLI is required but was not found in PATH." >&2
    exit 2
fi

echo "Applying branch protection to ${REPO}@${BRANCH}…"

# NB: required_status_checks.contexts MUST match the `name:` of each workflow
# job exactly. Update both this script and the workflow file together.
#
# `enforce_admins` is intentionally `false`: repo admins can bypass required
# checks for genuine emergency hotfixes (spec 001 §7). Any admin override is
# visible in the PR/commit history, and the team is expected to follow up
# with a normal PR. Flip to `true` if/when we want strict enforcement.
gh api \
    --method PUT \
    -H "Accept: application/vnd.github+json" \
    "/repos/${REPO}/branches/${BRANCH}/protection" \
    --input - <<'JSON'
{
  "required_status_checks": {
    "strict": true,
    "contexts": [
      "lint",
      "rust-test",
      "cpp-test",
      "cross-build",
      "cross-build-cpp",
      "dco"
    ]
  },
  "enforce_admins": false,
  "required_pull_request_reviews": {
    "required_approving_review_count": 1,
    "dismiss_stale_reviews": true,
    "require_code_owner_reviews": true,
    "require_last_push_approval": false
  },
  "restrictions": null,
  "required_linear_history": true,
  "allow_force_pushes": false,
  "allow_deletions": false,
  "block_creations": false,
  "required_conversation_resolution": true,
  "lock_branch": false,
  "allow_fork_syncing": false,
  "required_signatures": false
}
JSON

echo "Done. Verify with: gh api /repos/${REPO}/branches/${BRANCH}/protection"
