#!/usr/bin/env bash
set -euo pipefail

image=${1:?usage: smoke-image.sh IMAGE}
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
mock_log=${RUNNER_TEMP:-/tmp}/doplarr-chaptarr-mock.log
report_file=$(mktemp "${RUNNER_TEMP:-/tmp}/doplarr-preflight.XXXXXX.json")

python3 "$repo_root/.github/ci/chaptarr_mock.py" --port 18080 >"$mock_log" 2>&1 &
mock_pid=$!

cleanup() {
    kill "$mock_pid" 2>/dev/null || true
    wait "$mock_pid" 2>/dev/null || true
    cat "$mock_log"
    rm -f "$report_file"
}
trap cleanup EXIT

for _ in $(seq 1 30); do
    if curl --fail --silent http://127.0.0.1:18080/health >/dev/null 2>&1; then
        break
    fi
    sleep 0.2
done
curl --fail --silent --show-error http://127.0.0.1:18080/health >/dev/null

user=$(docker image inspect "$image" --format '{{.Config.User}}')
if [[ "$user" != "65532:65532" ]]; then
    echo "Expected image user 65532:65532, got: $user" >&2
    exit 1
fi

entrypoint=$(docker image inspect "$image" --format '{{json .Config.Entrypoint}}')
if [[ ! "$entrypoint" =~ /bin/doplarr\"\]$ ]]; then
    echo "Unexpected image entrypoint: $entrypoint" >&2
    exit 1
fi

command=$(docker image inspect "$image" --format '{{json .Config.Cmd}}')
if [[ "$command" != '["/config.toml"]' ]]; then
    echo "Unexpected image command: $command" >&2
    exit 1
fi

docker run --rm --pull never \
    --read-only \
    --cap-drop ALL \
    --security-opt no-new-privileges:true \
    --tmpfs /tmp:size=16m,mode=1777 \
    --add-host host.docker.internal:host-gateway \
    --env RUST_LOG=error \
    --mount "type=bind,source=$repo_root/.github/ci/smoke-config.toml,target=/config.toml,readonly" \
    "$image" --check /config.toml >"$report_file"

python3 - "$report_file" <<'PY'
import json
import sys
from pathlib import Path

report_path = Path(sys.argv[1])
raw = report_path.read_text(encoding="utf-8")
report = json.loads(raw)

assert report["status"] == "ok", report
assert report["discord"] == "not_contacted", report
assert {
    (
        backend["media"],
        backend["provider"],
        backend.get("version"),
        backend.get("compatibility"),
    )
    for backend in report["backends"]
} == {
    ("book", "Chaptarr", "0.9.720.0", "tested"),
    ("audiobook", "Chaptarr", "0.9.720.0", "tested"),
}, report

for forbidden in (
    "host.docker.internal",
    "ci-test-api-key",
    "/library/",
    "Ebook Standard",
    "Audiobook Standard",
):
    assert forbidden not in raw, f"preflight report leaked {forbidden!r}"

print(raw, end="")
PY
