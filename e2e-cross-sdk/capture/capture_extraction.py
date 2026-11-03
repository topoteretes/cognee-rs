#!/usr/bin/env python3
"""Capture the graph-extraction LLM request and answer of both SDKs (SDK-1039, P18).

Runs ``add`` + default ``cognify`` on the temporal biography N times per SDK with
a logging reverse proxy between each CLI and the OpenAI API, and records per run:

  (a) the extraction request: system-prompt sha256, ``response_format`` kind,
      ``strict`` flag and a sha256 of the schema (bodies are written to --out,
      they are not meant to be committed);
  (b) the raw nodes typed ``Timestamp`` in the model's answers;
  (c) the Timestamp nodes that reached the graph.

That separates "the model did not emit Timestamp nodes" from "the SDK dropped
them". Run it inside the harness image::

    docker run --rm --env-file <.env> \
      -v $PWD/e2e-cross-sdk/capture:/capture -v $OUT:/out \
      --entrypoint python3 cognee-e2e-cross-sdk:latest \
      /capture/capture_extraction.py --runs 5 --out /out

It needs OPENAI_TOKEN (and optionally OPENAI_MODEL) in the environment.
"""

import argparse
import hashlib
import json
import os
import sys
import tempfile
import threading
import urllib.request
import urllib.error
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, "/harness")
from helpers import (  # noqa: E402
    TEST_DATA_DIR,
    open_db,
    python_graph,
    query_nodes_by_type,
    run_python_cli,
    run_rust_cli,
    rust_db_path,
    write_rust_config,
)

UPSTREAM = os.environ.get("CAPTURE_UPSTREAM", "https://api.openai.com/v1")
DATASET = "temporal_capture"
BIOGRAPHY = TEST_DATA_DIR / "biography_temporal.txt"

LOCK = threading.Lock()
CAPTURED: dict[str, list[dict]] = {"py": [], "rs": []}


class Proxy(BaseHTTPRequestHandler):
    def log_message(self, *args):  # silence
        pass

    def do_POST(self):
        side, _, rest = self.path.lstrip("/").partition("/")
        body = self.rfile.read(int(self.headers.get("content-length", 0)))
        req = urllib.request.Request(
            f"{UPSTREAM}/{rest.removeprefix('v1/')}",
            data=body,
            method="POST",
            headers={
                k: v
                for k, v in self.headers.items()
                if k.lower() not in ("host", "content-length", "accept-encoding")
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=300) as resp:
                status, out = resp.status, resp.read()
        except urllib.error.HTTPError as err:
            status, out = err.code, err.read()
        try:
            entry = {"request": json.loads(body), "status": status, "response": json.loads(out)}
        except ValueError:
            entry = {"request": None, "status": status, "response": None}
        with LOCK:
            CAPTURED.setdefault(side, []).append(entry)
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)


def request_shape(req: dict):
    """Return ``(mode, strict, schema)`` of an extraction request, or ``None``.

    ``json_schema``: ``response_format`` (Python's default path); ``tools``: a
    forced function call (Rust's default ``auto`` cascade starts there).
    """
    fmt = req.get("response_format") or {}
    js = fmt.get("json_schema") or {}
    if "KnowledgeGraph" in (js.get("name") or ""):
        return "json_schema", bool(js.get("strict")), js.get("schema")
    for tool in req.get("tools") or []:
        fn = tool.get("function") or {}
        params = fn.get("parameters") or {}
        if "nodes" in (params.get("properties") or {}) and "edges" in (
            params.get("properties") or {}
        ):
            return "tools", bool(fn.get("strict")), params
    return None


def is_extraction(entry: dict) -> bool:
    return bool(entry.get("request")) and request_shape(entry["request"]) is not None


def answer_nodes(entry: dict) -> list:
    message = entry["response"]["choices"][0]["message"]
    calls = message.get("tool_calls") or []
    raw = calls[0]["function"]["arguments"] if calls else message.get("content") or "{}"
    return json.loads(raw).get("nodes", [])


def canon(value) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def summarize(entries: list[dict]) -> dict:
    calls, raw_ts, strict_flags, prompts, schemas = 0, 0, set(), set(), set()
    for entry in entries:
        if not is_extraction(entry):
            continue
        calls += 1
        req = entry["request"]
        mode, strict, schema = request_shape(req)
        strict_flags.add(f"{mode}/strict={strict}")
        schemas.add(hashlib.sha256(canon(schema).encode()).hexdigest()[:12])
        for message in req["messages"]:
            if message["role"] == "system":
                prompts.add(hashlib.sha256(message["content"].encode()).hexdigest()[:12])
        try:
            nodes = answer_nodes(entry)
        except (KeyError, TypeError, ValueError, IndexError):
            nodes = []
        raw_ts += sum(1 for n in nodes if str(n.get("type", "")).lower() == "timestamp")
    return {
        "extraction_calls": calls,
        "raw_timestamp_nodes": raw_ts,
        "request": sorted(strict_flags),
        "prompt_sha": sorted(prompts),
        "schema_sha": sorted(schemas),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument(
        "--direct",
        action="store_true",
        help="no proxy: talk to OPENAI_URL directly (only graph counts are recorded)",
    )
    ap.add_argument("--sdk", choices=["py", "rs"], action="append", help="default: both")
    ap.add_argument(
        "--rust-env",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help="extra environment for the Rust cognify call, e.g. LLM_STRUCTURED_OUTPUT_MODE=json_schema",
    )
    ap.add_argument(
        "--rust-setting",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help="extra Rust config.json setting, e.g. llm_structured_output_mode=json_schema",
    )
    args = ap.parse_args()
    rust_settings = dict(kv.split("=", 1) for kv in args.rust_setting)
    extra_env = dict(kv.split("=", 1) for kv in args.rust_env)
    args.out.mkdir(parents=True, exist_ok=True)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()

    rows = []
    for run in range(1, args.runs + 1):
        for side in args.sdk or ("py", "rs"):
            with LOCK:
                CAPTURED[side] = []
            ws = Path(tempfile.mkdtemp(prefix=f"cap-{side}-"))
            src = ws / "biography_temporal.txt"
            src.write_text(BIOGRAPHY.read_text())
            endpoint = (
                os.environ.get("OPENAI_URL", UPSTREAM)
                if args.direct
                else f"http://127.0.0.1:{port}/{side}/v1"
            )
            if side == "py":
                env = {"LLM_ENDPOINT": endpoint}
                run_python_cli(ws, ["add", str(src), "-d", DATASET], env=env)
                run_python_cli(ws, ["cognify", "-d", DATASET], env=env)
                nodes, _ = python_graph(ws)
                graph_ts = len({n["graph_id"] for n in nodes if n.get("type") == "Timestamp"})
            else:
                write_rust_config(ws, extra={"llm_endpoint": endpoint, **rust_settings})
                run_rust_cli(ws, ["add", str(src), "-d", DATASET])
                res = run_rust_cli(
                    ws,
                    ["cognify", "-d", DATASET],
                    env={"COGNEE_DEBUG_LLM_REQUEST": "1", **extra_env},
                )
                # `llm.debug_requests` dumps each request/response body to stderr:
                # the way to see what Rust sends to api.openai.com itself.
                (args.out / f"run{run}-rs.stderr").write_text(res.stderr)
                db = open_db(rust_db_path(ws))
                graph_ts = len({n["graph_id"] for n in query_nodes_by_type(db, "Timestamp")})
            with LOCK:
                entries = list(CAPTURED[side])
            (args.out / f"run{run}-{side}.json").write_text(json.dumps(entries, indent=1))
            row = {"run": run, "sdk": side, "graph_timestamp_nodes": graph_ts, **summarize(entries)}
            rows.append(row)
            print(json.dumps(row), flush=True)
    (args.out / "summary.json").write_text(json.dumps(rows, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
