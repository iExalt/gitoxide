#!/usr/bin/env bash
set -euo pipefail
: "${GH_TOKEN:?Set GH_TOKEN to a token with read access to iExalt/gitoxide}"
# Pass the credential through a descriptor, not process arguments or logs.
exec release-please release-pr --repo-url iExalt/gitoxide \
    --target-branch "${1:-main}" --config-file release-please-config.json \
    --manifest-file .release-please-manifest.json --dry-run \
    --token /dev/fd/3 3<<<"$GH_TOKEN"
