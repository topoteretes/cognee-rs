"""Cross-SDK no-double-emit assertion (gap 07 decision 13).

This test cannot fail meaningfully until a binding starts emitting
``send_telemetry`` events from the Rust side. Today the Python ``cognee``
SDK fires analytics from its own Python code; the Rust
``cognee_pipeline`` binding exposes only the pipeline surface and
never reaches the ``cognee_lib::api::*`` call sites that emit. The
harness wiring lives here so the test runs automatically the moment a
future gap surfaces those APIs through PyO3.

When that gap lands:

1. Drop the module-level ``pytest.mark.skip`` marker below.
2. Implement the body of ``test_no_double_emit_when_host_sdk_set`` so it:

   * Starts the cross-SDK harness Python container with both ``cognee``
     (the upstream SDK) and ``cognee_pipeline`` (the Rust binding)
     installed.
   * Points ``COGNEE_TELEMETRY_PROXY_URL`` at the mock proxy provided
     by ``e2e-cross-sdk/telemetry-emit``.
   * Sets ``COGNEE_HOST_SDK=python`` so the Rust side suppresses
     emission (decision 10).
   * Triggers an operation that both layers would normally emit for.
   * Asserts the mock proxy received exactly one POST per logical
     event, carrying the Python-SDK identifiers (anon/persistent IDs).
"""
from __future__ import annotations

import pytest

pytestmark = pytest.mark.skip(
    reason=(
        "Pending binding surfacing of cognee_lib::api::* (gap 07 decision 13). "
        "Wired into the harness so the test runs automatically when the "
        "double-emit path becomes reachable from a binding."
    )
)


def test_no_double_emit_when_host_sdk_set() -> None:
    # Skeleton — see module docstring for the activation checklist.
    raise AssertionError(
        "unreachable while the module-level skip marker is active"
    )
