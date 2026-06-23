# T6 — S5 bindings reuse seam audit

**Result:** `S5-fix` set is **empty**. The bindings reuse seam is already
structurally in place per plan §6.1; `ops/cloud.rs` is cleanly liftable
as-is. T6 lands no source-code changes; it ratifies the seam so the closed
cdylib (Phase 4) can depend on `cognee-bindings-common` and add a sibling
cloud-ops module without any OSS-side churn.

## What S5 requires

From [oss-split-plan.md §4 S5](../oss-split-plan.md#4-seams-that-must-exist-before-the-split):

> **S5 — Bindings reuse seam.** The reuse is *already structurally present*:
> the Python (PyO3) and JS (Neon) bindings both call
> `cognee_bindings_common::ops::*` directly (thin language-marshaling glue, no
> duplicated logic), and cloud is isolated to a single `ops/cloud.rs`
> (verified — it is the only cloud-coupled file). So the closed cdylib just
> **depends on the OSS `bindings-common` crate and adds a cloud-ops module**
> — no function-pointer registry needed (see §6.1). S5's only real work is
> making `ops/cloud.rs` cleanly liftable to the closed side.

The §6.1 reviewer addendum makes the same point: a function-pointer registry
is **unnecessary** because cloud features are selected statically via Cargo
features, not at runtime. The closed-side bindings simply re-link
`cognee-bindings-common` with `cloud` enabled and provide their own
sibling module for any closed-only ops — full type safety, zero dynamic
dispatch.

## bindings-common cloud surface inventory

> **Post-T7:** imports went from `cognee_lib::*` re-exports to direct
> `cognee_cloud::*`; `bindings-common` gained an optional `cognee-cloud`
> path-dep (and the `cloud` feature flipped from forwarding through
> `cognee-lib` to enabling that optional dep directly).

| Aspect | Location | Finding |
|---|---|---|
| `cloud` removed from `default` in T3-pre | `crates/bindings-common/Cargo.toml:25-34` | `default` lists `visualization, ladybug, onnx, hf-tokenizer, tiktoken, sqlite, testing`; no `cloud` |
| `cloud` feature gates an optional path-dep | `crates/bindings-common/Cargo.toml:36` | `cloud = ["dep:cognee-cloud"]` — direct optional dep on the closed `cognee-cloud` crate, no longer forwarded through `cognee-lib` |
| `cognee-lib` is `default-features = false` | `crates/bindings-common/Cargo.toml:45` | Forwards strictly through the OSS umbrella; no closed-side leakage via transitive defaults |
| Optional path-dep on closed `cognee-cloud` | `crates/bindings-common/Cargo.toml:46` | `cognee-cloud = { path = "../../../cognee-cloud-rust/crates/cognee-cloud", optional = true }` — only compiled when `cloud` is enabled |
| `pub mod cloud;` is unconditional | `crates/bindings-common/src/ops/mod.rs:9` | Intentional — see §"Design rationale" below |
| `ops/cloud.rs` imports | `crates/bindings-common/src/ops/cloud.rs:25, 41-42, 85, 119` | `crate::SdkError` + direct `cognee_cloud::{ServeConfig, serve, disconnect}` imports under `#[cfg(feature = "cloud")]`; the OSS `cognee-lib` cloud re-export block was removed in T7 |
| No `pub use ops::cloud::*` leak | `crates/bindings-common/src/lib.rs:21-32` | Cloud surface accessed only via the canonical `cognee_bindings_common::ops::cloud::*` path |

## Binding callsites — three-row table

| Binding | Callsite | Cargo feature forwarding |
|---|---|---|
| Python (PyO3) | `python/src/sdk_cloud.rs:16` — `use cognee_bindings_common::ops::cloud;` | `python/Cargo.toml:54` — `cloud = ["cognee-lib/cloud", "cognee-bindings-common/cloud"]` |
| JS (Neon) | `js/cognee-neon/src/sdk_cloud.rs:36` — `use cognee_bindings_common::ops::cloud;` | `js/cognee-neon/Cargo.toml:42` |
| C API | `capi/cognee-capi/src/sdk_cloud.rs:38` — `use cognee_bindings_common::ops::cloud;` | `capi/cognee-capi/Cargo.toml:41` |

All three bindings call `cognee_bindings_common::ops::cloud::{run_serve,
run_disconnect}` with `serde_json::Value` and convert errors via OSS
`SdkError`. This is already the §6.1 ideal: "thin language-marshaling glue,
no duplicated logic".

## ops/cloud.rs liftability verdict

- **Direct `cognee_cloud::*` imports under `#[cfg(feature = "cloud")]`.**
  Post-T7, `ops/cloud.rs` references `cognee_cloud::ServeConfig` (line 41-42),
  `cognee_cloud::serve` (line 85), and `cognee_cloud::disconnect` (line 119)
  directly via the optional path-dep declared at
  `crates/bindings-common/Cargo.toml:46`. The OSS `cognee-lib` umbrella
  re-export block was removed in T7, so there is no longer an umbrella seam
  to traverse — bindings-common reaches into the closed `cognee-cloud` crate
  itself when (and only when) the `cloud` feature is on.
- **No `pub(crate)` items in `bindings-common/src/`.** Verified by grep — no
  privacy promotions are needed for closed-side reuse.
- **Only OSS-staying types referenced:** `crate::SdkError`
  (`crates/bindings-common/src/error.rs:34, 55`) — both `Runtime` and
  `FeatureNotBuilt` variants are `pub`. `build_serve_config` is already `pub`
  (`crates/bindings-common/src/ops/cloud.rs:41`), so a closed caller can
  reuse it directly if it needs fine-grained control over `ServeConfig`
  assembly.

## Design rationale — why `pub mod cloud;` is unconditional

`run_serve` / `run_disconnect` are `pub async fn` defined unconditionally
(`crates/bindings-common/src/ops/cloud.rs:82, 116`), with their bodies
`cfg`-split: the cloud-enabled branch performs the call, the not-built
branch returns `SdkError::FeatureNotBuilt`. This is a deliberate UX
decision:

- Binding wrappers stay un-gated — Python `py_serve`, Neon `cogneeServe`, and
  C `cg_sdk_serve` don't have to repeat a not-built arm of their own. They
  call the same shared function regardless of feature state.
- Error handling is centralized at `crates/bindings-common/src/ops/cloud.rs:101`
  and `:137` (the `FeatureNotBuilt` return). Every binding converts that one
  variant to its native feature-not-built code
  (`FEATURE_NOT_BUILT` / `CG_ERR_FEATURE_NOT_BUILT` / `CogneeFeatureNotBuiltError`).
- Adding `#[cfg(feature = "cloud")]` to `pub mod cloud;` would force each
  binding (Python, Neon, C) to repeat the not-built branch with their own
  error code — duplicating exactly the logic the seam was designed to
  eliminate.

**Conclusion:** do NOT "fix" the unconditional `pub mod cloud;`. The design
is intentional and matches §6.1's "thin language-marshaling glue, no
duplicated logic" outcome.

## Closed-side consequence

Per plan §3 target topology and Phase 4, the closed `cognee-cloud-rust`
cdylib bindings will depend on OSS `cognee-bindings-common` and add a
sibling cloud-ops module (e.g. `cognee-bindings-common-cloud-ext` or an
in-crate module under the closed binding crate). No separate
`cognee-bindings-common-cloud` crate is needed: there is no dynamic
dispatch, no function-pointer registry, and no need to abstract the OSS
↔ closed boundary at runtime. Cargo's feature-flag selection plus the
trait-and-feature-flag pattern from S1 provide full type safety at compile
time.

## Surviving closed-named selections — out of scope for S5

The removal of the `cognee-lib::cloud::*` re-exports and the `cloud`-named
arms in workspace `default` sets was **§4 S6 / S7 (T7) scope** and has
since landed: `crates/lib/src/lib.rs` no longer contains a cloud re-export
block, and `bindings-common`'s `cloud` feature now gates an optional
`cognee-cloud` path-dep directly (see Cargo.toml:36, 46). T6 ratified the
seam; T7 finished the feature-default hygiene. Cross-reference
[`T1-s1-audit.md:63-69`](./T1-s1-audit.md) which already lists this deferral
under "T7 (S6 / S7 — cloud re-exports + default-feature hygiene)".
