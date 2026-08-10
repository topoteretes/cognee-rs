#!/usr/bin/env bash
#
# Assert that the PgGraphAdapter suite actually executed against a live
# Postgres, instead of skipping itself into a green no-op.
#
# Every `pggraph_test!` case (crates/graph/tests/pg_graph_integration.rs) and
# both inline `shared_db_migration_tests` cases return early — still reporting
# `ok` — when their Postgres URL is absent, so the runner's exit status proves
# nothing on its own. Run with the URL unset, both binaries print the exact
# same "32 passed" / "24 passed" summaries as a real run, just in 0.00s. This
# script is what separates the two.
#
# Called by the `pggraph-postgres` lane in .github/workflows/ci.yml and the
# mirrored lane in .github/workflows/community.yml. It lives here rather than
# inline in both YAML files so the two copies cannot drift, and so it can be
# tested against captured logs without a CI round-trip.
#
# Both runners are understood. The CI lanes run `cargo nextest`; a developer
# reproducing a failure locally will usually reach for `cargo test`, and the two
# print entirely different summaries. Every pattern below therefore accepts
# either format, so the same script validates either log:
#
#   libtest   test result: ok. 32 passed; 0 failed; ...
#             test <path>::<name> ... ok
#   nextest       Summary [   2.30s] 32 tests run: 32 passed, 0 skipped
#                 PASS [   0.05s] cognee-graph <path>::<name>
#
# Note the nextest lanes pass `--success-output immediate`: nextest captures a
# passing test's output by default, which would swallow the skip line that check
# 1 below exists to find.
#
# Usage: assert_pggraph_suite_ran.sh <integration-log> <lib-log>

set -uo pipefail

INTEGRATION_LOG="${1:?usage: $0 <integration-log> <lib-log>}"
LIB_LOG="${2:?usage: $0 <integration-log> <lib-log>}"

# A floor rather than an exact count, so adding cases does not break the guard.
# The integration suite had 32 cases when this was written.
MIN_INTEGRATION_TESTS="${MIN_INTEGRATION_TESTS:-30}"

# The two cases that live inline in crates/graph/src/pg_graph_adapter.rs rather
# than in the integration file, and so need their own assertion.
INLINE_TESTS=(
  pggraph_coexists_with_relational_migrator_in_shared_db
  pggraph_purges_legacy_row_from_default_table_on_upgrade
)

fail() {
  echo "::error::$*"
  exit 1
}

# ── 0. The logs exist ───────────────────────────────────────────────────────
# Checked up front because `if grep A B; then` treats grep's exit 2 ("can't
# open B") identically to exit 1 ("no match"), i.e. as proof of success. An
# unreadable log means the run produced no evidence, which is a failure, not a
# pass.
for log in "$INTEGRATION_LOG" "$LIB_LOG"; do
  [[ -s "$log" ]] ||
    fail "$log is missing or empty — the test step produced no output, so nothing can be asserted about this run."
done

# ── 1. Nothing announced a skip ─────────────────────────────────────────────
# The variable name is globbed rather than spelled out: renaming
# PGGRAPH_TEST_URL rewrites the Rust-side message too, and a hard-coded pattern
# would silently stop matching in exactly the scenario this guard exists for.
#
# Matched unanchored (libtest may interleave the line after a `test NAME ... `
# prefix under --nocapture), then filtered: `cargo test 2>&1` also captures
# rustc diagnostics, and rustc echoes the offending source line verbatim, so a
# future warning pointing at one of the three `eprintln!` call sites would
# otherwise fail a run in which every test passed. Runtime skip output never
# contains `eprintln!`; a compiler-rendered source snippet always does.
matches="$(grep -hE '[A-Z_]+ not set .* skipping' "$INTEGRATION_LOG" "$LIB_LOG")"
grep_status=$?

case "$grep_status" in
  0) ;;                # candidate skip lines found — filtered below
  1) matches="" ;;     # none present
  *) fail "grep exited $grep_status while scanning the test logs; refusing to report success on unverified output." ;;
esac

# `|| true`: grep -v exits 1 when the filter removes every line, which is the
# all-clear case here, not an error. errexit is deliberately off in this script
# (every check is explicit), but the guard must not depend on that.
skips="$(printf '%s\n' "$matches" | grep -v 'eprintln!' | sed '/^[[:space:]]*$/d' || true)"
if [[ -n "$skips" ]]; then
  printf '%s\n' "$skips"
  fail "PgGraph tests skipped — the suite never reached a live Postgres. The URL is unset or empty in the test process; a broken adapter would panic with its own error instead."
fi

# ── 2. A plausible number of integration cases actually ran ─────────────────
# Absence of a skip marker is not presence of a test. Both targets are behind
# `#[cfg(feature = "postgres")]`, so dropping `--features postgres,testing`
# compiles them down to zero cases: the runner then reports 0 tests, exits 0,
# and emits no skip line — indistinguishable from success without this check.
#
# nextest's whole-run summary is checked FIRST, and the order is load-bearing.
# nextest gives every test its own libtest process, and `--success-output
# immediate` echoes each one's captured stdout — so a nextest log is full of
# per-test `test result: ok. 1 passed;` lines. Reading libtest's pattern first
# picks one of those up and concludes that a single test ran, failing a perfectly
# good 32-case run. `Summary [` is emitted only by nextest, so trying it first is
# unambiguous either way round.
#
# A log carrying neither summary falls through to the emptiness check below,
# which fails closed — the safe direction, and the reason this stayed an
# assertion instead of being relaxed when the lanes moved to nextest.
passed="$(sed -nE 's/^[[:space:]]*Summary \[[^]]*\] [0-9]+ tests run: ([0-9]+) passed.*/\1/p' "$INTEGRATION_LOG" | head -1)"
if [[ -z "$passed" ]]; then
  passed="$(sed -nE 's/^test result: ok\. ([0-9]+) passed;.*/\1/p' "$INTEGRATION_LOG" | head -1)"
fi
[[ -n "$passed" ]] ||
  fail "no libtest or nextest summary line in $INTEGRATION_LOG — the integration binary did not run to completion."
((passed >= MIN_INTEGRATION_TESTS)) ||
  fail "only $passed integration tests ran, expected at least $MIN_INTEGRATION_TESTS — the postgres/testing features have probably stopped applying, which compiles the suite down to nothing."

# ── 3. Both inline cases ran by name ────────────────────────────────────────
# The lib target holds 20-odd unrelated unit tests, so a count floor would not
# notice these two disappearing.
#
# `PASS \[` is matched unanchored rather than at line start: nextest prefixes a
# retried case with `TRY n `, and the leading indentation is cosmetic. The names
# are fixed `[a-z_]` literals from the array above, so embedding them in the
# pattern needs no escaping.
for test_name in "${INLINE_TESTS[@]}"; do
  grep -qE "($test_name \.\.\. ok|PASS \[[^]]*\][[:space:]].*$test_name([[:space:]]|\$))" "$LIB_LOG" ||
    fail "inline test $test_name did not run (or did not pass) — see $LIB_LOG."
done

echo "PgGraph suite verified: $passed integration tests ran against a live Postgres, both inline migration tests passed, no skip markers."
