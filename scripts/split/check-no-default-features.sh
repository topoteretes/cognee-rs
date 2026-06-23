#!/usr/bin/env bash
# Generalises the per-crate `cargo check --no-default-features` gate that
# .github/workflows/ci.yml runs for `cognee-lib` alone. Plan §5 Phase 0 step 8.
#
# Skipped crates (rationale):
#   examples              — example targets only, not a library surface
#   cognee-bench          — bench harness, publish = false
#   cognee-python         — PyO3 cdylib; covered by ci.yml's python-check
#   cognee-telemetry-emit — harness binary; depends on cognee-lib
#                           --no-default-features --features telemetry directly
set -euo pipefail

CRATES=(
  cognee-models
  cognee-storage
  cognee-database
  cognee-ingestion
  cognee-chunking
  cognee-cognify
  cognee-core
  cognee-lib
  cognee-logging
  cognee-cli
  cognee-utils
  cognee-llm
  cognee-graph
  cognee-embedding
  cognee-vector
  cognee-ontology
  cognee-search
  cognee-session
  cognee-delete
  cognee-test-utils
  cognee-visualization
  cognee-http-server
  cognee-observability
  cognee-telemetry
  cognee-bindings-common
)
for crate in "${CRATES[@]}"; do
  echo "::group::cargo check -p ${crate} --no-default-features"
  cargo check -p "${crate}" --no-default-features
  echo "::endgroup::"
done
echo "OK: ${#CRATES[@]} crates pass --no-default-features"
