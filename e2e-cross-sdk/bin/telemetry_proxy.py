#!/usr/bin/env python3
"""Tiny HTTP proxy that captures POST bodies to /captures/all.jsonl.

Used by the cross-SDK telemetry parity test
(docs/telemetry/02/10-cross-sdk-parity.md). The Python and Rust SDKs
each fire one `send_telemetry` POST against this proxy; the test then
reads the captured records and asserts identity parity
(`api_key_tracking_id`, `persistent_id`).

For each request, append the JSON body to /captures/all.jsonl (one
record per line). A bare `GET /_health` returns 200 for the
docker-compose health check.
"""
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

CAPTURE_DIR = Path("/captures")
CAPTURE_DIR.mkdir(parents=True, exist_ok=True)
JSONL = CAPTURE_DIR / "all.jsonl"


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802 (BaseHTTPRequestHandler API)
        if self.path == "/_health":
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b"ok")
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):  # noqa: N802 (BaseHTTPRequestHandler API)
        try:
            ln = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            ln = 0
        body = self.rfile.read(ln) if ln > 0 else b""
        try:
            obj = json.loads(body) if body else {}
        except Exception:
            obj = {"_raw": body.decode("utf-8", errors="replace")}
        # Line-buffered append: one JSON object per line. Newline
        # terminator means the test's _wait_for_n_captures poll never
        # sees a half-written record.
        with JSONL.open("a") as f:
            f.write(json.dumps(obj) + "\n")
            f.flush()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(b"{}")

    def log_message(self, format, *args):  # noqa: A002 (stdlib signature)
        # Quiet logs; the captures file is the source of truth.
        sys.stderr.write("proxy: " + (format % args) + "\n")


def main():
    port = int(os.environ.get("PORT", "9090"))
    HTTPServer(("0.0.0.0", port), Handler).serve_forever()


if __name__ == "__main__":
    main()
