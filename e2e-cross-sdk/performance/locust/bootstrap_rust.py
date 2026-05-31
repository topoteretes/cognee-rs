"""Bootstrap helper for Rust HTTP-server load tests.

Mode A (default): no-auth benchmark runs. This script is intentionally lightweight
and just emits metadata consumed by wrappers.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path


def parse_bool(value: str | None, default: bool) -> bool:
    if value is None:
        return default
    return value.strip().lower() in {"1", "true", "yes", "on"}


def main(out_path: str) -> None:
    payload = {
        "require_authentication": parse_bool(
            os.environ.get("REQUIRE_AUTHENTICATION"), False
        ),
        "api_key": os.environ.get("COGNEE_API_KEY", ""),
        "search_type": os.environ.get("COGNEE_SEARCH_TYPE", "GRAPH_COMPLETION"),
    }
    Path(out_path).write_text(json.dumps(payload), encoding="utf-8")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: python -m bootstrap_rust <output-json-path>")
    main(sys.argv[1])
