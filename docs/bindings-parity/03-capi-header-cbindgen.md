# CL-1 — C API: generate the header via cbindgen; add a CI symbol-diff to stop drift

- **Binding:** C API (`capi/cognee-capi/`)
- **Dimension:** Cleanliness / Documentation
- **Priority:** P1
- **Status:** Not started

## Problem

The C headers are hand-maintained, and they have already drifted from the
exported symbols:

- `cg_exec_status_new` is exported at
  [capi/cognee-capi/src/exec_status.rs:175](../../capi/cognee-capi/src/exec_status.rs#L175)
  but is **not declared** in [capi/include/cognee.h](../../capi/include/cognee.h) (which
  declares only `cg_exec_status_noop` and `cg_exec_status_destroy`). A C caller
  cannot use the vtable-based exec-status manager without writing the prototype
  by hand.
- The infrastructure to prevent this already exists but is inert:
  `cbindgen = "0.27"` is a build-dependency, `capi/cognee-capi/cbindgen.toml`
  exists, yet `capi/cognee-capi/build.rs` is a deliberate no-op
  (`fn main() {}`). The headers are written and edited by hand.

There are ~120 exported functions across two headers (`cognee.h`,
`cognee_sdk.h`); manual maintenance does not scale and the drift above is the
predictable result.

## Goal / definition of done

The committed C headers cannot silently drift from the exported ABI. Either the
headers are generated, or CI fails when an exported symbol has no matching
declaration. `cg_exec_status_new` (and any other currently-undeclared symbol) is
present in the header.

## Design decision: generate vs. verify

The headers are hand-written with extensive Doxygen-style documentation
(`cognee_sdk.h` is 1307 lines with a rich preamble documenting the tier rule,
deferred-callback rule, JSON contract, etc.). Fully regenerating them with
cbindgen would **lose that prose** unless every doc comment is moved into Rust
`///` comments and cbindgen is configured to emit them.

Two viable paths:

- **Option A (recommended): keep hand-written headers, add a CI guard.** Add a
  check that diffs the set of `#[no_mangle] extern "C"` exports against the
  function declarations in the headers and fails on any mismatch. Low effort,
  preserves the curated docs, catches drift immediately. Fix the existing
  `cg_exec_status_new` gap by hand.
- **Option B: migrate to generated headers.** Move all header prose into Rust
  doc comments, enable cbindgen in `build.rs`, and commit the generated output
  (or generate at build time and check it in via CI). Higher effort and a
  one-time risk of doc loss, but eliminates drift structurally.

This plan implements **Option A** and leaves Option B as a documented follow-up.

## Implementation plan (Option A)

### Step 1 — Fix the known drift

Add the `cg_exec_status_new` declaration to
[capi/include/cognee.h](../../capi/include/cognee.h) next to `cg_exec_status_noop`,
including the `CgExecStatusManagerVtable` struct and the `state` pointer, with
ownership/`# Safety` documentation matching the Rust doc comment
(`state must be valid until vtable.destroy is called`).

### Step 2 — Add a symbol-vs-header diff script

Create `capi/scripts/check_header_sync.sh` that:

1. Extracts exported symbol names from the source:
   ```bash
   grep -rhoE 'pub (unsafe )?extern "C" fn [a-z_]+' capi/cognee-capi/src/ \
     | sed -E 's/.* ([a-z_]+)$/\1/' | sort -u > /tmp/exports.txt
   ```
   (Exclude test-only exports such as `cg_test_force_panic` via an allowlist.)
2. Extracts declared function names from both headers:
   ```bash
   grep -hoE '\bcg_[a-z_]+\s*\(' capi/include/cognee.h capi/include/cognee_sdk.h \
     | sed -E 's/\s*\($//' | sort -u > /tmp/declared.txt
   ```
3. `comm -23 /tmp/exports.txt /tmp/declared.txt` — any line printed is an
   exported-but-undeclared symbol; exit non-zero if non-empty. Print the offenders.

Keep an explicit allowlist file (`capi/scripts/header_sync_allow.txt`) for
intentionally-internal exports (trampolines that are `#[no_mangle]` for callback
ABI but not part of the public surface — verify which, if any, qualify).

### Step 3 — Wire it into the check pipeline

Add the script to [capi/scripts/check.sh](../../capi/scripts/check.sh) so it runs in
the C API CI job (`.github/workflows/capi-check.yml`) and in
`scripts/check_all.sh`.

### Step 4 — (Optional, follow-up) keep cbindgen as a generator for review

Even under Option A, wire cbindgen in `build.rs` to emit a `cognee_generated.h`
into `OUT_DIR` (not committed). The symbol-diff in Step 2 can then optionally
diff against the generated file too, giving signature-level (not just
name-level) drift detection without replacing the curated headers. If this
proves reliable, graduate to Option B.

## Verification

```bash
# from capi/
bash scripts/check_header_sync.sh   # exits 0 only when every export is declared
bash scripts/check.sh
# from repo root
scripts/check_all.sh
```

Confirm the check **fails** if you temporarily delete the `cg_exec_status_new`
declaration, then passes once Step 1 is applied.

## Risks / notes

- The name-level diff (Option A) catches missing/renamed functions but not
  signature changes (wrong arg types). Option B / the optional Step 4 closes that
  gap; note the limitation in the script header so future maintainers know its
  scope.
- Some `#[no_mangle]` functions are callback trampolines, not public API.
  Curate the allowlist deliberately and comment why each entry is exempt.
