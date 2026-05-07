"""Cross-SDK identity parity for ``send_telemetry``.

Asserts that Python and Rust SDKs, given the same ``LLM_API_KEY`` and
shared ``~/.cognee/``, produce identical ``api_key_tracking_id`` and
``persistent_id`` on the wire.

Run via the ``e2e-telemetry`` compose service. See
``docs/telemetry/02/10-cross-sdk-parity.md`` for the full design.
"""
import json
import os
import subprocess
import time
from pathlib import Path

CAPTURES = Path("/captures/all.jsonl")
PROXY_URL = os.environ.get("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS", "")


def _read_captures():
    if not CAPTURES.exists():
        return []
    return [
        json.loads(line)
        for line in CAPTURES.read_text().splitlines()
        if line.strip()
    ]


def _wait_for_n_captures(n, timeout=15.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        records = _read_captures()
        if len(records) >= n:
            return records
        time.sleep(0.2)
    raise AssertionError(
        f"timed out waiting for {n} captures; got {len(_read_captures())}"
    )


def _python_emit():
    """Drive Python's ``send_telemetry`` via the in-image venv interpreter.

    The harness image installs the Python ``cognee`` package into
    ``/opt/python-venv`` (Dockerfile stage 3). In-process import is
    preferred over the CLI because no production subcommand is
    guaranteed to route to ``send_telemetry``.
    """
    subprocess.check_call(
        [
            "/opt/python-venv/bin/python",
            "-c",
            (
                "from cognee.shared.utils import send_telemetry;"
                "send_telemetry('cognee.forget', user_id='cross-sdk-user',"
                " additional_properties={'target':'everything',"
                " 'cognee_version':'cross-sdk-test'})"
            ),
        ],
        env={**os.environ},
        timeout=30,
    )


def _rust_emit():
    """Drive Rust's ``send_telemetry`` via the harness-only emit binary.

    A dedicated ``/usr/local/bin/cognee-telemetry-emit`` binary calls
    ``cognee_lib::telemetry::send_telemetry`` directly with fixed args.
    We do **not** use ``cognee-cli-rust`` because no production
    subcommand is guaranteed to route to ``send_telemetry``.
    """
    subprocess.check_call(
        ["/usr/local/bin/cognee-telemetry-emit"],
        env={**os.environ, "COGNEE_TELEMETRY_INTEGRATION_TEST": "1"},
        timeout=30,
    )


def _user_props(record):
    """Pull user_properties from either the top level or properties.

    Python's ``send_telemetry`` writes ``user_properties`` at the top
    level of the payload; the Rust serde model mirrors that. If the
    field isn't found, return ``{}`` so the assertion below produces a
    readable failure rather than a ``KeyError``.
    """
    if "user_properties" in record:
        return record["user_properties"]
    return record.get("properties", {}).get("user_properties", {})


def test_cross_sdk_telemetry_identity_parity():
    # Clear any prior captures from previous test invocations so the
    # poll below sees only this test's two records.
    if CAPTURES.exists():
        CAPTURES.unlink()

    _python_emit()
    _rust_emit()

    records = _wait_for_n_captures(2, timeout=15.0)

    # Identify which record came from which SDK. Rust adds
    # ``sdk_runtime: "rust"`` per decision 2; Python may or may not
    # set the field. Fall back to "the one that isn't rust" for the
    # Python record.
    rs = next(
        (r for r in records if r.get("properties", {}).get("sdk_runtime") == "rust"),
        None,
    )
    py = next(
        (r for r in records if r is not rs),
        None,
    )
    assert rs is not None, f"no Rust record in captures: {records!r}"
    assert py is not None, f"no Python record in captures: {records!r}"

    assert py["event_name"] == "cognee.forget"
    assert rs["event_name"] == "cognee.forget"

    py_user = _user_props(py)
    rs_user = _user_props(rs)

    # api_key_tracking_id must be byte-identical (decision 11 — derived
    # from LLM_API_KEY at emission time on both sides).
    py_ak = py_user.get("api_key_tracking_id")
    rs_ak = rs_user.get("api_key_tracking_id")
    assert py_ak and py_ak.startswith("ak_"), f"python ak missing/invalid: {py_ak!r}"
    assert rs_ak and rs_ak.startswith("ak_"), f"rust ak missing/invalid: {rs_ak!r}"
    assert py_ak == rs_ak, (
        f"api_key_tracking_id drift: python={py_ak!r}, rust={rs_ak!r}"
    )

    # persistent_id must be identical (shared ~/.cognee/.persistent_id
    # via the cognee-home docker volume).
    py_pid = py_user.get("persistent_id")
    rs_pid = rs_user.get("persistent_id")
    assert py_pid == rs_pid, (
        f"persistent_id drift: python={py_pid!r}, rust={rs_pid!r}"
    )

    # api_key_hash backward-compat alias must mirror api_key_tracking_id
    # on both SDKs.
    assert py_user.get("api_key_hash") == py_ak
    assert rs_user.get("api_key_hash") == rs_ak

    # Rust adds sdk_runtime: "rust" (decision 2). Python may add
    # sdk_runtime later; tolerate either presence.
    assert rs["properties"]["sdk_runtime"] == "rust"
