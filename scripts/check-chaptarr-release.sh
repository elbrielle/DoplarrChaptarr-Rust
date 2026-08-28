#!/usr/bin/env bash
# Triage a new Chaptarr release in minutes: clone the tag, extract its route
# inventory, diff it against the vendored extract, and run the route-inventory
# contract test against the fresh one. One command:
#
#   scripts/check-chaptarr-release.sh <git-ref>
#
# Set CHAPTARR_REPO to clone from somewhere other than the canonical repo (a
# local mirror, a fork). Route inventory ONLY: the spec mistypes command bodies
# and parameter optionality, so route names are the single thing consumed from
# it - nothing else may ever be generated from the spec.
set -euo pipefail

ref="${1:-}"
if [[ -z "$ref" ]]; then
    echo "usage: scripts/check-chaptarr-release.sh <git-ref>" >&2
    exit 2
fi

repo="${CHAPTARR_REPO:-https://github.com/Chaptarr/chaptarr.git}"
root="$(cd "$(dirname "$0")/.." && pwd)"
vendored="$root/doplarr/tests/fixtures/chaptarr/openapi_paths.json"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Cloning $repo at $ref ..."
git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$ref" "$repo" "$tmp/clone"

spec="$tmp/clone/src/Chaptarr.Api.V1/openapi.json"
if [[ ! -f "$spec" ]]; then
    echo "No openapi.json at src/Chaptarr.Api.V1/openapi.json in $ref - the spec moved or the ref is wrong." >&2
    exit 1
fi

jq '.paths | keys' "$spec" > "$tmp/openapi_paths.json"

jq -r '.[]' "$tmp/openapi_paths.json" | LC_ALL=C sort > "$tmp/fresh.txt"
jq -r '.[]' "$vendored" | LC_ALL=C sort > "$tmp/vendored.txt"

echo
echo "Route inventory: $ref has $(wc -l < "$tmp/fresh.txt" | tr -d ' ') routes, vendored extract has $(wc -l < "$tmp/vendored.txt" | tr -d ' ')."

gained="$(comm -13 "$tmp/vendored.txt" "$tmp/fresh.txt")"
lost="$(comm -23 "$tmp/vendored.txt" "$tmp/fresh.txt")"

if [[ -z "$gained" && -z "$lost" ]]; then
    echo "No route drift: $ref matches the vendored extract exactly."
else
    if [[ -n "$gained" ]]; then
        echo
        echo "GAINED (in $ref, not vendored):"
        echo "$gained"
    fi
    if [[ -n "$lost" ]]; then
        echo
        echo "LOST (vendored, gone in $ref):"
        echo "$lost"
    fi
fi

echo
echo "Running the route-inventory contract test against the $ref extract ..."
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$root"
CHAPTARR_OPENAPI_PATHS="$tmp/openapi_paths.json" cargo test -p doplarr --test chaptarr_contract \
    depended_on_routes_exist_in_the_vendored_openapi_extract

echo
echo "Verdict: depended-on routes intact in $ref."
