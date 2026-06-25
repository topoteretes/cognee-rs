#!/usr/bin/env python3
"""Regenerate the Rust <-> Python PBKDF2 byte-parity fixtures.

Run from the repo root:
    python3 scripts/generate_python_fixtures.py \
        > crates/telemetry/tests/fixtures/pbkdf2_vectors.json

Output is a JSON array of test vectors:
    [{"key": "<utf8>", "salt": "<utf8>", "iter": 100000,
      "dklen": 16, "expected_hex": "<lowercase 32-char hex>"}, ...]
"""
import hashlib
import json
import sys

VECTORS = [
    {"key": "sk-test-key-12345",
     "salt": "cognee.telemetry.api-key-tracking.v1"},
    {"key": "sk-proj-abcdefghijklmnopqrstuvwxyz0123456789",
     "salt": "cognee.telemetry.api-key-tracking.v1"},
    {"key": "sk-test-key-12345",
     "salt": "deployment-private-namespace-2026"},
    # Visible-tail-vs-full-key: two keys sharing the last 4 chars.
    {"key": "sk-aaaaaaaaaaaaaa1234",
     "salt": "cognee.telemetry.api-key-tracking.v1"},
    {"key": "sk-bbbbbbbbbbbbbb1234",
     "salt": "cognee.telemetry.api-key-tracking.v1"},
]

out = []
for v in VECTORS:
    derived = hashlib.pbkdf2_hmac(
        "sha256", v["key"].encode("utf-8"),
        v["salt"].encode("utf-8"), 100_000, 16,
    )
    out.append({**v, "iter": 100_000, "dklen": 16,
                "expected_hex": derived.hex()})
json.dump(out, sys.stdout, indent=2)
print()
