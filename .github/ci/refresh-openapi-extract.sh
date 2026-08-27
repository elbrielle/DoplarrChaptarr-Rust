#!/usr/bin/env bash
# Refresh the vendored Chaptarr openapi route extract that the contract
# tests assert against. One command:
#
#   .github/ci/refresh-openapi-extract.sh [path-to-chaptarr-clone]
#
# The clone defaults to /tmp/chaptarr-ref (see docs/chaptarr/COMPATIBILITY.md
# for the pinned tag). Route inventory ONLY: the spec mistypes command bodies
# and parameter optionality and omits controller 400s, so nothing else may be
# generated from it.
set -euo pipefail

clone="${1:-/tmp/chaptarr-ref}"
spec="$clone/src/Chaptarr.Api.V1/openapi.json"
out="$(cd "$(dirname "$0")/../.." && pwd)/doplarr/tests/fixtures/chaptarr/openapi_paths.json"

if [[ ! -f "$spec" ]]; then
    echo "No openapi.json at $spec - clone Chaptarr at the pinned tag first." >&2
    exit 1
fi

jq '.paths | keys' "$spec" > "$out"
echo "Wrote $(jq 'length' "$out") routes to $out"
