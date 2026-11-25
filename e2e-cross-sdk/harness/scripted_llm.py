"""A scripted OpenAI-compatible endpoint both SDKs can be pointed at.

Real LLM extraction is not deterministic, so a cross-SDK case that asserts an
exact type set or edge count cannot use one. This server answers every
``chat/completions`` call with a canned result instead:

* a knowledge-graph request (its schema has ``nodes`` and ``edges``) gets the
  graph scripted for the first marker found in the request's messages, or an
  empty graph when none matches;
* any other structured request (the chunk summary, …) gets a value filled in
  from its own JSON schema;
* ``embeddings`` returns a deterministic vector per input text.

It speaks the three shapes the SDKs send: Rust's ``response_format:
json_schema`` (answered in ``content``), and Python instructor's ``tools`` /
legacy ``functions`` calls (answered as a tool / function call). Both SDKs see
the same graphs, so any difference in what they store comes from the
pipelines, not the model.

Every request is recorded (``calls``) so a test can assert the scripted
graphs were actually served and the SDK did not take some other path.
"""

from __future__ import annotations

import base64
import hashlib
import json
import struct
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Optional

EMBEDDING_DIMENSIONS = 1536


def _resolve(schema: dict, root: dict) -> dict:
    """Follow a local ``$ref`` (``#/$defs/X`` or ``#/definitions/X``)."""
    seen = 0
    while isinstance(schema, dict) and "$ref" in schema and seen < 16:
        path = schema["$ref"].lstrip("#/").split("/")
        node = root
        for part in path:
            node = node.get(part, {})
        schema = node
        seen += 1
    return schema


def _fill(schema: dict, root: dict, depth: int = 0):
    """A minimal value that satisfies *schema*."""
    schema = _resolve(schema, root)
    if depth > 8 or not isinstance(schema, dict):
        return None
    for key in ("anyOf", "oneOf", "allOf"):
        if key in schema:
            options = [_resolve(o, root) for o in schema[key]]
            options = [o for o in options if o.get("type") != "null"] or options
            return _fill(options[0], root, depth + 1)
    if "enum" in schema:
        return schema["enum"][0]
    kind = schema.get("type")
    if isinstance(kind, list):
        kind = next((k for k in kind if k != "null"), "string")
    if kind == "object" or "properties" in schema:
        props = schema.get("properties", {})
        return {name: _fill(sub, root, depth + 1) for name, sub in props.items()}
    if kind == "array":
        return []
    if kind == "integer":
        return 0
    if kind == "number":
        return 0.0
    if kind == "boolean":
        return False
    return "scripted"


def _is_graph_schema(schema: dict, root: dict) -> bool:
    props = _resolve(schema, root).get("properties", {})
    return "nodes" in props and "edges" in props


def _vector(text: str, dims: int) -> list[float]:
    """A deterministic unit-ish vector for *text*."""
    out: list[float] = []
    counter = 0
    while len(out) < dims:
        digest = hashlib.sha256(f"{counter}:{text}".encode()).digest()
        out.extend((b - 127.5) / 127.5 for b in digest)
        counter += 1
    out = out[:dims]
    norm = sum(x * x for x in out) ** 0.5 or 1.0
    return [x / norm for x in out]


def _message_text(body: dict) -> str:
    parts = []
    for message in body.get("messages", []):
        content = message.get("content")
        if isinstance(content, str):
            parts.append(content)
        elif isinstance(content, list):
            parts.extend(p.get("text", "") for p in content if isinstance(p, dict))
    return "\n".join(parts)


class ScriptedLlm:
    """Run the scripted endpoint on a free localhost port.

    ``graphs`` maps a marker string to the KnowledgeGraph JSON returned for a
    request whose messages contain it. Use as a context manager; ``url`` is
    the ``/v1`` base both SDKs take as their endpoint.
    """

    def __init__(self, graphs: dict[str, dict]):
        self.graphs = graphs
        self.calls: list[dict] = []
        self._lock = threading.Lock()
        self._server: Optional[ThreadingHTTPServer] = None

    # ── lifecycle ────────────────────────────────────────────────────────

    def __enter__(self) -> "ScriptedLlm":
        handler = self._handler()
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=self._server.serve_forever, daemon=True).start()
        return self

    def __exit__(self, *exc) -> None:
        if self._server is not None:
            self._server.shutdown()
            self._server.server_close()

    @property
    def url(self) -> str:
        assert self._server is not None, "use ScriptedLlm as a context manager"
        return f"http://127.0.0.1:{self._server.server_address[1]}/v1"

    def reset(self) -> None:
        with self._lock:
            self.calls.clear()

    def graph_markers_served(self) -> list[str]:
        """Markers of the graph requests served so far, in arrival order."""
        with self._lock:
            return [c["marker"] for c in self.calls if c["kind"] == "graph" and c["marker"]]

    def unscripted(self) -> list[dict]:
        """Requests this server could not answer (unknown path)."""
        with self._lock:
            return [c for c in self.calls if c["kind"] == "unknown"]

    # ── responses ────────────────────────────────────────────────────────

    def _record(self, **call) -> None:
        with self._lock:
            self.calls.append(call)

    def _graph_for(self, text: str) -> tuple[Optional[str], dict]:
        for marker, graph in self.graphs.items():
            if marker in text:
                return marker, graph
        return None, {"nodes": [], "edges": []}

    def _chat(self, body: dict) -> dict:
        text = _message_text(body)
        name, schema = "Response", None
        tools = body.get("tools") or []
        functions = body.get("functions") or []
        response_format = body.get("response_format") or {}
        if tools:
            fn = tools[0].get("function", {})
            name, schema = fn.get("name", name), fn.get("parameters")
        elif functions:
            name, schema = functions[0].get("name", name), functions[0].get("parameters")
        elif response_format.get("type") == "json_schema":
            js = response_format.get("json_schema", {})
            name, schema = js.get("name", name), js.get("schema")

        if schema is None:
            # Plain-text completion (no schema): nothing in cognify should ask
            # for one, but answer rather than hang the SDK.
            self._record(kind="text", marker=None, name=None)
            payload = "scripted"
        elif _is_graph_schema(schema, schema):
            marker, graph = self._graph_for(text)
            self._record(kind="graph", marker=marker, name=name)
            filled = _fill(schema, schema)
            filled.update(graph)
            payload = json.dumps(filled)
        else:
            self._record(kind="structured", marker=None, name=name)
            payload = json.dumps(_fill(schema, schema))

        message: dict = {"role": "assistant", "content": None}
        finish = "stop"
        if tools:
            message["tool_calls"] = [
                {
                    "id": "call_scripted",
                    "type": "function",
                    "function": {"name": name, "arguments": payload},
                }
            ]
            finish = "tool_calls"
        elif functions:
            message["function_call"] = {"name": name, "arguments": payload}
            finish = "function_call"
        else:
            message["content"] = payload
        return {
            "id": "chatcmpl-scripted",
            "object": "chat.completion",
            "created": 0,
            "model": body.get("model", "scripted"),
            "choices": [{"index": 0, "message": message, "finish_reason": finish}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        }

    def _embeddings(self, body: dict) -> dict:
        inputs = body.get("input")
        if isinstance(inputs, str) or (inputs and isinstance(inputs[0], int)):
            inputs = [inputs]
        dims = int(body.get("dimensions") or EMBEDDING_DIMENSIONS)
        as_base64 = body.get("encoding_format") == "base64"
        data = []
        for i, item in enumerate(inputs or []):
            vec = _vector(json.dumps(item), dims)
            if as_base64:
                emb = base64.b64encode(struct.pack(f"<{dims}f", *vec)).decode()
            else:
                emb = vec
            data.append({"object": "embedding", "index": i, "embedding": emb})
        self._record(kind="embedding", marker=None, name=None)
        return {
            "object": "list",
            "data": data,
            "model": body.get("model", "scripted"),
            "usage": {"prompt_tokens": 1, "total_tokens": 1},
        }

    def _handler(self):
        llm = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):  # keep pytest output readable
                pass

            def _send(self, status: int, payload: dict) -> None:
                raw = json.dumps(payload).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def do_GET(self):  # noqa: N802 — http.server API
                if self.path.rstrip("/").endswith("/models"):
                    self._send(200, {"object": "list", "data": [{"id": "scripted"}]})
                    return
                llm._record(kind="unknown", marker=None, name=f"GET {self.path}")
                self._send(404, {"error": {"message": f"unscripted GET {self.path}"}})

            def do_POST(self):  # noqa: N802 — http.server API
                length = int(self.headers.get("Content-Length") or 0)
                body = json.loads(self.rfile.read(length) or b"{}")
                path = self.path.split("?")[0].rstrip("/")
                if path.endswith("/chat/completions"):
                    self._send(200, llm._chat(body))
                elif path.endswith("/embeddings"):
                    self._send(200, llm._embeddings(body))
                else:
                    llm._record(kind="unknown", marker=None, name=f"POST {self.path}")
                    self._send(404, {"error": {"message": f"unscripted POST {self.path}"}})

        return Handler
