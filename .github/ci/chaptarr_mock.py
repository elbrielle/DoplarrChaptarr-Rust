#!/usr/bin/env python3
"""Small deterministic Chaptarr API used only by the container smoke test."""

from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
FIXTURE_ROOT = REPOSITORY_ROOT / "doplarr" / "tests" / "fixtures" / "chaptarr"
API_KEY = "ci-test-api-key"
ROUTES = {
    "/api/v1/system/status": "system_status.json",
    "/api/v1/rootfolder": "root_folders_nested.json",
    "/api/v1/qualityprofile": "quality_profiles.json",
    "/api/v1/metadataprofile": "metadata_profiles.json",
}


class ChaptarrHandler(BaseHTTPRequestHandler):
    server_version = "ChaptarrSmoke/1.0"

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        path = urlsplit(self.path).path
        if path == "/health":
            self._send_json({"status": "ready"})
            return

        if self.headers.get("X-Api-Key") != API_KEY:
            self._send_json({"message": "invalid CI API key"}, status=401)
            return

        fixture_name = ROUTES.get(path)
        if fixture_name is None:
            self._send_json({"message": f"unexpected CI request: {path}"}, status=404)
            return

        with (FIXTURE_ROOT / fixture_name).open(encoding="utf-8") as fixture:
            self._send_json(json.load(fixture))

    def _send_json(self, value: object, status: int = 200) -> None:
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        print(f"chaptarr-mock: {format % args}", flush=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=18080)
    args = parser.parse_args()

    server = ThreadingHTTPServer(("0.0.0.0", args.port), ChaptarrHandler)
    print(f"chaptarr-mock: listening on {args.port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
