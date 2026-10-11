#!/usr/bin/env bash
# check_env_boundary.sh — ratchet for the configuration-boundary rule.
#
# The root clippy.toml bans direct process-env access (std::env::var & co.,
# dotenv::*, cognee_config_core::ProcessEnv::new). Code that predates the rule
# carries
#
#     #[allow(clippy::disallowed_methods,
#             reason = "config-unification: legacy direct env read, ...")]
#
# This script counts those legacy markers and fails if the count differs from
# BASELINE below. The count may only go DOWN: a PR that migrates a site lowers
# BASELINE in the same change; a PR that adds a site fails here. New code must
# not add a legacy marker. It belongs behind a config boundary instead. See
# docs/configuration.md § "Configuration model" and § "Adding a setting".
#
# Usage: scripts/check_env_boundary.sh   (no arguments; run from anywhere)
set -euo pipefail

BASELINE=39

cd "$(dirname "$0")/.."

MARKER='reason = "config-unification: legacy direct env read'
count=$(grep -rF --include='*.rs' "$MARKER" \
  crates capi python ts java examples e2e-cross-sdk 2>/dev/null | wc -l | tr -d ' ')

if [[ "$count" -gt "$BASELINE" ]]; then
  echo "error: $count legacy direct-env-read allows, baseline is $BASELINE." >&2
  echo "New code must not read the process environment directly; add a field to a" >&2
  echo "config struct and read it at the config boundary instead:" >&2
  echo "  docs/configuration.md#adding-a-setting" >&2
  grep -rnF --include='*.rs' "$MARKER" crates capi python ts java examples e2e-cross-sdk >&2 || true
  exit 1
fi
if [[ "$count" -lt "$BASELINE" ]]; then
  echo "error: $count legacy direct-env-read allows, baseline is $BASELINE." >&2
  echo "Nice, a site was migrated. Lower BASELINE in $0 to $count to lock that in." >&2
  exit 1
fi
echo "env boundary ok ($count legacy allows, baseline $BASELINE)"
