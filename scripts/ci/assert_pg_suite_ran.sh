#!/usr/bin/env bash
#
# Assert that a Postgres-gated test suite actually executed against a live
# Postgres, instead of skipping itself into a green no-op.
#
# The repo has a recurring failure mode this script exists to make impossible.
# Every Postgres suite gates itself on an environment variable and returns early
# — still reporting `ok` — when that variable is absent, so the runner's exit
# status proves nothing on its own: run with the URL unset and the binaries print
# the same "24 passed" summaries as a real run, just in 0.00s. Worse, a suite
# behind `#![cfg(feature = "...")]` built without that feature compiles down to
# zero cases and reports `running 0 tests` — exit 0, no skip line, nothing to
# notice. The `pgvector-spans` lane shipped in exactly that state and reported
# green for its entire life. Both are checked below.
#
# Called by the `pgvector-postgres`, `pggraph-postgres` and
# `pg-single-db-postgres` lanes in .github/workflows/ci.yml and their mirrors in
# .github/workflows/community.yml.
#
# Also called by the `test` job's retrieval step, which is not Postgres-gated at
# all — `search_live_smoke` gates on an LLM key and (formerly) a local ONNX
# model. Nothing here is Postgres-specific: the skip-marker pattern is
# variable-agnostic by design, and both the test-count floor and the required
# test names are parameters. The name is kept for its three original callers
# rather than churning six YAML references. It lives here rather than inline in six YAML
# blocks so the copies cannot drift, and so it can be tested against captured
# logs without a CI round-trip.
#
# Generalized from the pggraph-only `assert_pggraph_suite_ran.sh` (PR #128) when
# the third and fourth call sites arrived: its skip-marker matching was already
# variable-agnostic, and only the test-count floor and the inline test names were
# hard-coded, so parameterising those two was a smaller change than a third copy.
#
# Both runners are understood. Most CI lanes run `cargo test` (the Postgres
# suites share one database and rely on `#[serial]`, which only serializes within
# a process); the `pggraph-postgres` lane runs `cargo nextest`, whose per-test
# processes are safe there because each case provisions its own database. A
# developer reproducing a failure locally may reach for either. The two print
# entirely different summaries, so every pattern below accepts both formats:
#
#   libtest   test result: ok. 32 passed; 0 failed; ...
#             test <path>::<name> ... ok
#   nextest       Summary [   2.30s] 32 tests run: 32 passed, 0 skipped
#                 PASS [   0.05s] cognee-graph <path>::<name>
#
# Note the nextest lanes must pass `--success-output immediate`: nextest captures
# a passing test's output by default, which would swallow the skip line that
# check 1 below exists to find. The `cargo test` lanes pass `--nocapture` for the
# same reason.
#
# Usage:
#   assert_pg_suite_ran.sh --label <NAME>
#                          (--log <FILE> --min-tests <N>)...
#                          [--require-test <TEST_NAME>]...
#
#   --label        Suite name used in the failure messages (e.g. PgVector).
#   --log          A captured test log. Repeatable; each --log must be
#                  immediately followed by its own --min-tests.
#   --min-tests    Floor on the passing-test count reported by the preceding
#                  --log. A floor rather than an exact count, so adding cases
#                  does not break the guard.
#   --require-test A test name that must appear as *passed* in at least one of
#                 the logs. Repeatable. Use for cases a count floor cannot
#                 notice going missing — typically inline `--lib` tests sharing
#                 a target with dozens of unrelated unit tests.

set -uo pipefail

LABEL=""
LOGS=()
MIN_TESTS=()
REQUIRE_TESTS=()

fail() {
  echo "::error::$*"
  exit 1
}

usage() {
  fail "usage: $0 --label <NAME> (--log <FILE> --min-tests <N>)... [--require-test <NAME>]... ($*)"
}

while (($# > 0)); do
  case "$1" in
    --label)
      [[ -n "${2:-}" ]] || usage "--label needs a value"
      LABEL="$2"
      shift 2
      ;;
    --log)
      [[ -n "${2:-}" ]] || usage "--log needs a value"
      LOGS+=("$2")
      shift 2
      ;;
    --min-tests)
      [[ "${2:-}" =~ ^[0-9]+$ ]] || usage "--min-tests needs a number"
      # Positionally bound to the most recent --log: the counts array must stay
      # the same length as the logs array, so a misordered invocation is a usage
      # error rather than a silently unchecked log.
      ((${#LOGS[@]} == ${#MIN_TESTS[@]} + 1)) ||
        usage "--min-tests must directly follow its --log"
      MIN_TESTS+=("$2")
      shift 2
      ;;
    --require-test)
      [[ -n "${2:-}" ]] || usage "--require-test needs a value"
      REQUIRE_TESTS+=("$2")
      shift 2
      ;;
    *) usage "unknown argument '$1'" ;;
  esac
done

[[ -n "$LABEL" ]] || usage "--label is required"
((${#LOGS[@]} > 0)) || usage "at least one --log is required"
((${#LOGS[@]} == ${#MIN_TESTS[@]})) || usage "every --log needs a --min-tests"

# ── 0. The logs exist ───────────────────────────────────────────────────────
# Checked up front because `if grep A B; then` treats grep's exit 2 ("can't
# open B") identically to exit 1 ("no match"), i.e. as proof of success. An
# unreadable log means the run produced no evidence, which is a failure, not a
# pass.
for log in "${LOGS[@]}"; do
  [[ -s "$log" ]] ||
    fail "$log is missing or empty — the test step produced no output, so nothing can be asserted about this run."
done

# ── 1. Nothing announced a skip ─────────────────────────────────────────────
# The variable name is globbed rather than spelled out: renaming
# PGVECTOR_TEST_URL / TEST_POSTGRES_URL / PGGRAPH_TEST_URL rewrites the
# Rust-side message too, and a hard-coded pattern would silently stop matching
# in exactly the scenario this guard exists for. It also means one pattern
# covers all three lanes.
#
# Matched unanchored (libtest may interleave the line after a `test NAME ... `
# prefix under --nocapture — observed: `test test_add_nodes_batch ...
# okPGGRAPH_TEST_URL not set — skipping test_get_graph_data`), then filtered:
# `cargo test 2>&1` also captures rustc diagnostics, and rustc echoes the
# offending source line verbatim, so a future warning pointing at one of the
# `eprintln!` call sites would otherwise fail a run in which every test passed.
# Runtime skip output never contains `eprintln!`; a compiler-rendered source
# snippet always does.
matches="$(grep -hE '[A-Z_]+ not set .* skipping' "${LOGS[@]}")"
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
  # The message names no specific cause. The marker lines printed just above say
  # which variable the test process found missing, and it is not always a Postgres
  # URL — `pg_full_stack_e2e` also skips on an absent LLM key. Asserting "the URL
  # is unset" here would send whoever reads the log to debug a healthy service
  # container. What IS safe to assert is the category: every suite behind this
  # guard panics with the underlying error when a configured backend misbehaves,
  # so a *skip* means an environment variable never arrived.
  fail "$LABEL tests skipped — a case returned early instead of running. See the marker line(s) above for which environment variable the test process found missing or empty. These suites panic with the underlying error when a configured backend fails, so a skip is a missing variable, not a broken backend."
fi

# ── 2. A plausible number of cases actually ran, per log ────────────────────
# Absence of a skip marker is not presence of a test. These targets are behind
# `#[cfg(feature = "...")]` gates, so dropping the feature flags compiles them
# down to zero cases: the runner then reports 0 tests, exits 0, and emits no
# skip line — indistinguishable from success without this check. That is not
# hypothetical; it is what `pgvector-spans` did from the day it was added.
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
# which fails closed — the safe direction.
total_passed=0
for i in "${!LOGS[@]}"; do
  log="${LOGS[$i]}"
  floor="${MIN_TESTS[$i]}"

  passed="$(sed -nE 's/^[[:space:]]*Summary \[[^]]*\] [0-9]+ tests run: ([0-9]+) passed.*/\1/p' "$log" | head -1)"
  if [[ -z "$passed" ]]; then
    passed="$(sed -nE 's/^test result: ok\. ([0-9]+) passed;.*/\1/p' "$log" | head -1)"
  fi
  [[ -n "$passed" ]] ||
    fail "no libtest or nextest summary line in $log — the test binary did not run to completion."
  ((passed >= floor)) ||
    fail "only $passed tests passed in $log, expected at least $floor — the feature flags have probably stopped applying, which compiles the suite down to nothing."

  total_passed=$((total_passed + passed))
done

# ── 3. Named cases ran, by name ────────────────────────────────────────────
# A `--lib` target holds dozens of unrelated unit tests, so a count floor would
# not notice a specific case disappearing.
#
# `PASS \[` is matched unanchored rather than at line start: nextest prefixes a
# retried case with `TRY n `, and the leading indentation is cosmetic. The names
# are `[a-z0-9_]` literals from the command line, so embedding them in the
# pattern needs no escaping — but reject anything else up front rather than let
# it reach grep as a pattern.
for test_name in ${REQUIRE_TESTS[@]+"${REQUIRE_TESTS[@]}"}; do
  [[ "$test_name" =~ ^[A-Za-z0-9_]+$ ]] ||
    fail "--require-test '$test_name' is not a bare test name; refusing to use it as a pattern."
  grep -qhE "($test_name \.\.\. ok|PASS \[[^]]*\][[:space:]].*$test_name([[:space:]]|\$))" "${LOGS[@]}" ||
    fail "required test $test_name did not run (or did not pass) in any of: ${LOGS[*]}."
done

echo "$LABEL suite verified: $total_passed tests passed against a live backend across ${#LOGS[@]} log(s), ${#REQUIRE_TESTS[@]} named case(s) confirmed, no skip markers."
