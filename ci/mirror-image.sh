#!/usr/bin/env bash
# Mirror ligato/vpp-base:24.10 to ghcr.io/r12f/infmon-vpp-dev:24.10.
#
# See specs/001-ci-and-precommit.md §5.
#
# CI consumes the ghcr.io mirror so Docker Hub outages / rate-limits
# don't hard-block PRs. Run this periodically (weekly cron or manual)
# to keep the mirror fresh.
#
# Requirements:
#   - docker (with buildx for multi-arch)
#   - Authenticated to ghcr.io:  echo $GHCR_TOKEN | docker login ghcr.io -u <user> --password-stdin
#
# Usage: ci/mirror-image.sh [source_tag] [dest_tag]
set -euo pipefail

SOURCE="${1:-docker.io/ligato/vpp-base:24.10}"
DEST="${2:-ghcr.io/r12f/infmon-vpp-dev:24.10}"

echo "Mirroring ${SOURCE} → ${DEST}"

# Pull the source image (multi-arch manifest if available)
docker pull "${SOURCE}"

# Re-tag
docker tag "${SOURCE}" "${DEST}"

# Push to ghcr.io
docker push "${DEST}"

echo "Done. Verify with: docker manifest inspect ${DEST}"
