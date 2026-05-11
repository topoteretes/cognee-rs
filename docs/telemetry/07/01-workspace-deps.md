# Task 07-01 — Workspace + binding manifests for auto-init

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 07-02 — PyO3 bridge](02-pyo3-bridge.md) (PyO3 binding uses `pyo3-log` + `tracing-log`).
- [Task 07-03 — Neon default subscriber](03-neon-default-subscriber.md) (Neon needs `tracing-subscriber` exposed as a dep — it already comes transitively, but the binding declares it explicitly).
- [Task 07-05 — Binding OTLP setup](05-binding-otlp-setup.md) (all three binding crates need the `telemetry` feature enabled on `cognee-observability` / `cognee-lib`).
- [Task 07-06 — Host-SDK sentinel](06-host-sdk-sentinel.md) (binding crates need `cognee-telemetry` as a direct dep).

**Parent doc**: [07 — Bindings auto-init for tracing & telemetry](../07-bindings-auto-init.md)
**Locked decisions**: 3 (`telemetry` cargo feature on by default in bindings), 5 (`pyo3-log` is the canonical Python event sink).

---

## 1. Goal

Add the dependency lines that unblock the rest of gap 07. Three
separate manifest edits, no code changes:

1. **PyO3 binding** ([`python/Cargo.toml`](../../../python/Cargo.toml)) —
   add `pyo3-log = "0.4"` and `tracing-log = "0.2"`; add
   `cognee-observability` and `cognee-telemetry` as direct deps with
   the `telemetry` feature enabled.
2. **Neon binding** ([`js/cognee-neon/Cargo.toml`](../../../js/cognee-neon/Cargo.toml)) —
   add `tracing-subscriber` (workspace dep), `cognee-observability`,
   and `cognee-telemetry` direct deps with the `telemetry` feature
   enabled.
3. **C API binding** ([`capi/cognee-capi/Cargo.toml`](../../../capi/cognee-capi/Cargo.toml)) —
   add `cognee-observability` and `cognee-telemetry` direct deps with
   the `telemetry` feature enabled.

No `use` statements yet. Later tasks pull these into binding source
files.

## 2. Rationale

- Decision 3 places binding crates on parity with `cognee-cli`, which
  ships the `telemetry` feature on by default. Enabling at the
  manifest level means task 07-05 can call
  `cognee_observability::init_telemetry` without `#[cfg(feature = "telemetry")]`
  gates inside every binding.
- Decision 5 makes `pyo3-log` the bridge between Rust `tracing` and
  Python's stdlib `logging` module. The companion crate `tracing-log`
  installs `LogTracer`, which forwards `log::Record`s into `tracing`
  (and vice versa, depending on which init pattern is selected). The
  Python sub-doc 07-02 uses the `tracing → log → pyo3-log → Python`
  flow.
- `pyo3-log = "0.12"` is the line that pairs with `pyo3 = "0.23"`
  (current pin at [`python/Cargo.toml:16`](../../../python/Cargo.toml#L16)).
  Earlier drafts of this sub-doc referenced `pyo3-log = "0.4"`, but
  `pyo3-log` was renumbered to track PyO3's major version: `0.4.x`
  paired with `pyo3 0.16`, while `pyo3-log 0.12.x` is the line that
  supports `pyo3 0.23`. The implementor pinned `0.12` accordingly.
- Landing the manifest changes as a standalone commit lets later tasks
  do pure-source additions with focused `Cargo.lock` deltas.

## 3. Pre-conditions

- Clean `cargo check --all-targets` on `main`.
- No outstanding edits under `python/`, `js/cognee-neon/`,
  `capi/cognee-capi/`.
- `cognee-observability` already exposes the `telemetry` feature
  ([`crates/observability/Cargo.toml:9-17`](../../../crates/observability/Cargo.toml#L9-L17)).
- `cognee-telemetry` already exposes the `telemetry` feature
  ([`crates/telemetry/Cargo.toml:13-24`](../../../crates/telemetry/Cargo.toml#L13-L24)).

## 4. Step-by-step

### 4.1 Python binding manifest

Edit [`python/Cargo.toml`](../../../python/Cargo.toml). Append to
`[dependencies]`:

```toml
# Bridge Rust `tracing` events into Python `logging` (gap 07 decision 5).
tracing-log = "0.2"
pyo3-log    = "0.12"

# Optional opentelemetry / send_telemetry surfaces. Both have noop
# bodies when the `telemetry` feature is off, so the binding can
# still build under `--no-default-features` once feature wiring is
# in place (out of scope for v1 — feature is on by default here).
cognee-observability = { path = "../crates/observability", features = ["telemetry"] }
cognee-telemetry     = { path = "../crates/telemetry",     features = ["telemetry"] }
```

Confirm `tracing-subscriber` is already pulled in transitively via
`cognee-logging` and `cognee-observability`. If `cargo tree -e
features -p cognee-python | grep tracing-subscriber` shows it as a
non-direct edge after the change, that's expected.

### 4.2 Neon binding manifest

[`js/cognee-neon/Cargo.toml`](../../../js/cognee-neon/Cargo.toml) is
**not part of the parent workspace** (see line 7 `[workspace]` empty
section in the existing manifest). It pins its own dep versions and
patches. Append to `[dependencies]`:

```toml
# Bridge Rust `tracing` events to stderr for Node hosts (gap 07
# decision 1 / Option A). Workspace version pin would be ideal but
# this crate is outside the workspace, so pin explicitly to the
# version `cognee-logging` consumes — verify by running
# `cargo tree -p cognee-logging | grep tracing-subscriber` from the
# workspace root before committing.
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }

cognee-observability = { path = "../../crates/observability", features = ["telemetry"] }
cognee-telemetry     = { path = "../../crates/telemetry",     features = ["telemetry"] }
```

### 4.3 C API binding manifest

Edit [`capi/cognee-capi/Cargo.toml`](../../../capi/cognee-capi/Cargo.toml).
Append to `[dependencies]`:

```toml
cognee-observability = { path = "../../crates/observability", features = ["telemetry"] }
cognee-telemetry     = { path = "../../crates/telemetry",     features = ["telemetry"] }
```

The C binding does not need `pyo3-log` or `tracing-log` — task 07-04
uses only `std::panic::set_hook`.

### 4.4 Refresh the lockfile

```bash
cargo update -p tracing-log
cargo update -p pyo3-log
cargo check --all-targets
```

Expect new entries in `Cargo.lock` for `pyo3-log` and `tracing-log`
plus their `log` transitive (already in the graph).

The Neon binding has its own `Cargo.lock` ([`js/cognee-neon/`](../../../js/cognee-neon/)
isolated workspace). Run `cd js/cognee-neon && cargo check` to
update its lockfile in the same commit.

## 5. Verification

```bash
# 1. Workspace compiles end-to-end with the new deps.
cargo check --all-targets

# 2. The Neon binding compiles (separate workspace).
cd js/cognee-neon && cargo check && cd -

# 3. pyo3-log resolves alongside pyo3 0.23.
cargo tree -p cognee-python -e features | grep -E "pyo3-log|tracing-log" | head

# 4. cognee-observability is pulled with the telemetry feature on
#    in each binding.
cargo tree -p cognee-python   -e features | grep -E "cognee-observability/telemetry"
cargo tree -p cognee-neon     -e features | grep -E "cognee-observability/telemetry"
cargo tree -p cognee-capi     -e features | grep -E "cognee-observability/telemetry"

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`python/Cargo.toml`](../../../python/Cargo.toml) — four new deps.
- [`js/cognee-neon/Cargo.toml`](../../../js/cognee-neon/Cargo.toml) —
  three new deps (tracing-subscriber, cognee-observability,
  cognee-telemetry).
- [`capi/cognee-capi/Cargo.toml`](../../../capi/cognee-capi/Cargo.toml) —
  two new deps.
- `Cargo.lock` — automatic.
- `js/cognee-neon/Cargo.lock` — automatic (separate isolated
  workspace).

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `pyo3-log 0.4` pin is wrong for `pyo3 = 0.23` | Materialised — `pyo3-log` major versions track PyO3 major versions; the original `0.4` reference was stale. Resolved by pinning `pyo3-log = "0.12"` (the line that declares `pyo3 0.23` compat). | Implementor verified `cargo check` passes with `pyo3-log = "0.12"`; doc updated to reflect the actual pin. |
| Adding `cognee-observability` to the C binding bloats the cdylib | Medium — opentelemetry SDK + tonic add ~MB. | Accepted (decision 3). Document in 07-08 README; embedders that care opt-out by building with `--no-default-features` on the binding once feature wiring exists. |
| Neon binding has a separate `Cargo.lock`; manifest deltas don't propagate | Low — explicit `cd js/cognee-neon && cargo check` in the verification step refreshes it. | Implementor commits both lockfiles. |
| The Neon binding pins `tonic`/`hyper`/`tar` via `[patch.crates-io]`. Adding `cognee-observability` (which depends on `tonic 0.14`) may conflict. | Medium — the existing Neon `[patch.crates-io]` block patches `tonic` to a qdrant fork at v0.11. cognee-observability needs tonic 0.14. | Sub-agent C must run the Neon `cargo check` and surface any patch conflict; if it occurs, escalate to the user (this would change the design of the patch table, which is shared with the qdrant integration). |
| `Cargo.lock` churn unrelated to the dep change | Low | Commit the lockfile in the same commit; `cargo update -p <crate>` keeps the diff focused. |

## 8. Out of scope

- Adding any code that imports `pyo3_log`, `tracing_log`,
  `cognee_observability`, or `cognee_telemetry`. That belongs in
  tasks 07-02 through 07-06.
- Adding a binding-level cargo feature `telemetry = []` to allow
  embedders to opt-out at build time. That would be a follow-up if
  binary size complaints arise — for v1 the feature is forced on by
  the binding's manifest (decision 3 explicitly accepts the cost).
- Bumping `pyo3`, `neon`, or `cbindgen` versions. Gap 07 piggybacks
  on existing pins.
