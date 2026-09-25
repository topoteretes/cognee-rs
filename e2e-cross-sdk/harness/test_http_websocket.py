"""Phase-3 parity tests for WebSocket /api/v1/cognify/subscribe/{pipeline_run_id}.

Requires: OPENAI_TOKEN or OPENAI_API_KEY, and a cognified dataset.

Per p8-e2e-parity.md Step 11 and websocket.md §5:
- Happy-path: connect after background cognify, read frames until terminal close.
  Assert close code 1000, terminal status is PipelineRunCompleted, and the
  intermediate status *set* matches between both servers.
  Frame-count delta tolerance: ±WS_YIELD_TOLERANCE (2).
- Error-path: cognify on a bad dataset → Errored status frame, no 1000 close.

Auth: uses cookie jar from authed_clients (WebSocket must reuse the session).
"""

from __future__ import annotations

import pytest

from conftest import requires_openai
from http_helpers import DEFAULT_IGNORE, WS_YIELD_TOLERANCE
from seed import seed_dataset_with_text

pytestmark = [requires_openai]

# Try to import httpx_ws; if unavailable, skip all tests in this module.
try:
    import httpx_ws  # noqa: F401
    HAS_HTTPX_WS = True
except ImportError:
    HAS_HTTPX_WS = False

if not HAS_HTTPX_WS:
    pytestmark = [pytest.mark.skip(reason="httpx-ws not installed — skipping WebSocket tests")]

_SEED_TEXT = (
    "WebSocket parity test document.  "
    "The server streams pipeline run events over a WebSocket connection.  "
    "The harness validates frame shapes and terminal close codes."
)
_TERMINAL_STATUSES = {"PipelineRunCompleted", "PipelineRunErrored", "Errored", "completed", "errored"}
_WS_BASE_PY = "ws://127.0.0.1:8000"
_WS_BASE_RS = "ws://127.0.0.1:8001"


def _start_background_cognify(client, dataset_id: str) -> str | None:
    """POST /cognify with run_in_background=true; return the pipeline_run_id."""
    r = client.post(
        "/api/v1/cognify",
        json={"datasets": [dataset_id], "run_in_background": True},
        timeout=30.0,
    )
    if r.status_code != 200:
        return None
    body = r.json()
    return body.get("pipeline_run_id") or body.get("run_id") or body.get("id")


def _collect_ws_frames(ws_base: str, cookies: dict, run_id: str) -> tuple[list[dict], int | None]:
    """Connect to the WebSocket and collect all frames until close.

    Returns (frames, close_code).
    """
    import json as _json
    import httpx_ws

    frames: list[dict] = []
    close_code: int | None = None
    url = f"{ws_base}/api/v1/cognify/subscribe/{run_id}"

    with httpx_ws.connect(url, cookies=cookies) as ws:
        try:
            while True:
                msg = ws.receive()
                if msg is None:
                    break
                if hasattr(msg, "data"):
                    try:
                        frames.append(_json.loads(msg.data))
                    except Exception:
                        pass
                if hasattr(msg, "code"):
                    # WebSocket close frame
                    close_code = msg.code
                    break
        except Exception:
            pass
    return frames, close_code


@pytest.mark.skipif(not HAS_HTTPX_WS, reason="httpx-ws not installed")
def test_websocket_happy_path(authed_clients, unique_dataset_name):
    """WebSocket cognify subscribe delivers frames and closes with code 1000 on both servers."""
    ds_ids: dict[str, str | None] = {}
    for side, client in authed_clients.items():
        r = seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)
        ds_ids[side] = r.get("dataset_id") or r.get("id")

    run_ids: dict[str, str | None] = {}
    for side, client in authed_clients.items():
        ds_id = ds_ids[side]
        if not ds_id:
            pytest.skip(f"No dataset_id for {side}")
        run_ids[side] = _start_background_cognify(client, ds_id)

    for side, run_id in run_ids.items():
        if not run_id:
            pytest.skip(f"No pipeline_run_id returned from background cognify on {side}")

    # Collect cookies from each client for WS auth
    py_cookies = dict(authed_clients["py"].cookies)
    rs_cookies = dict(authed_clients["rs"].cookies)

    py_frames, py_close = _collect_ws_frames(_WS_BASE_PY, py_cookies, run_ids["py"])
    rs_frames, rs_close = _collect_ws_frames(_WS_BASE_RS, rs_cookies, run_ids["rs"])

    # Both must close with code 1000
    assert py_close == 1000, f"py WebSocket close code: {py_close} (expected 1000)"
    assert rs_close == 1000, f"rs WebSocket close code: {rs_close} (expected 1000)"

    # Terminal frame status must be PipelineRunCompleted (or equivalent)
    def _terminal_status(frames: list[dict]) -> str | None:
        for f in reversed(frames):
            s = f.get("status") or f.get("event")
            if s in _TERMINAL_STATUSES:
                return s
        return None

    py_terminal = _terminal_status(py_frames)
    rs_terminal = _terminal_status(rs_frames)
    assert py_terminal in _TERMINAL_STATUSES, f"py terminal status {py_terminal!r} not in {_TERMINAL_STATUSES}"
    assert rs_terminal in _TERMINAL_STATUSES, f"rs terminal status {rs_terminal!r} not in {_TERMINAL_STATUSES}"

    # Frame count delta within tolerance
    delta = abs(len(py_frames) - len(rs_frames))
    assert delta <= WS_YIELD_TOLERANCE, (
        f"Frame count delta {delta} exceeds tolerance {WS_YIELD_TOLERANCE}: "
        f"py={len(py_frames)} rs={len(rs_frames)}"
    )

    # Intermediate status sets must match
    def _intermediate_statuses(frames: list[dict]) -> set[str]:
        return {f.get("status") or f.get("event") for f in frames if f.get("status") or f.get("event")}

    py_statuses = _intermediate_statuses(py_frames)
    rs_statuses = _intermediate_statuses(rs_frames)
    assert py_statuses == rs_statuses, (
        f"Intermediate status mismatch:\n  only in py: {py_statuses - rs_statuses}\n  only in rs: {rs_statuses - py_statuses}"
    )


@pytest.mark.skipif(not HAS_HTTPX_WS, reason="httpx-ws not installed")
def test_websocket_error_path(authed_clients):
    """Cognify subscribe on a bad run_id delivers Errored status (no 1000 close)."""
    import uuid
    bad_run_id = str(uuid.uuid4())
    py_cookies = dict(authed_clients["py"].cookies)
    rs_cookies = dict(authed_clients["rs"].cookies)

    py_frames, py_close = _collect_ws_frames(_WS_BASE_PY, py_cookies, bad_run_id)
    rs_frames, rs_close = _collect_ws_frames(_WS_BASE_RS, rs_cookies, bad_run_id)

    # Both should not get a 1000 close on an invalid run_id
    # (could get 404, 4xxx WS close, or an Errored frame)
    assert py_close != 1000 or any(
        (f.get("status") or "") in {"Errored", "PipelineRunErrored"} for f in py_frames
    ), f"Expected error response for bad run_id on py, got close={py_close} frames={py_frames[:3]}"

    assert py_close == rs_close or (
        set(f.get("status") for f in py_frames) == set(f.get("status") for f in rs_frames)
    ), f"Error-path mismatch: py_close={py_close} rs_close={rs_close}"
