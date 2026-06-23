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

| Aspect | Location | Finding |
|---|---|---|
| `cloud` removed from `default` in T3-pre | `crates/bindings-common/Cargo.toml:25-34` | `default` lists `visualization, ladybug, onnx, hf-tokenizer, tiktoken, sqlite, testing`; no `cloud` |
| `cloud` feature is a thin forward | `crates/bindings-common/Cargo.toml:36` | `cloud = ["cognee-lib/cloud"]` — no direct `cognee-cloud` dep in this crate |
| `cognee-lib` is `default-features = false` | `crates/bindings-common/Cargo.toml:45` | Forwards strictly through the OSS umbrella; no closed-side leakage via transitive defaults |
| `pub mod cloud;` is unconditional | `crates/bindings-common/src/ops/mod.rs:9` | Intentional — see §"Design rationale" below |
| `ops/cloud.rs` imports | `crates/bindings-common/src/ops/cloud.rs:25, 42, 85, 119` | Only `crate::SdkError` + `cognee_lib::{ServeConfig, serve, disconnect}` re-exports (the latter are `#[cfg(feature = "cloud")]`-gated in `crates/lib/src/lib.rs:149-153`) |
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

- **Zero direct `cognee_cloud::*` imports.** All closed-side references go
  through the OSS `cognee_lib::*` re-exports at `crates/lib/src/lib.rs:149-153`
  (which are `#[cfg(feature = "cloud")]`-gated and forward to
  `cognee_cloud::*`). When the cloud re-exports move out wholesale in S6/T7,
  `ops/cloud.rs` follows trivially — its imports rewrite from `cognee_lib::…`
  to `cognee_cloud::…` (or stay on `cognee_lib::…` if the closed bindings
  re-add the umbrella re-export).
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

The removal of the `cognee-lib::cloud::*` re-exports at
`crates/lib/src/lib.rs:141-153` and the `cloud` opt-in feature itself
(plus the `cloud`-named arms in every `default` set across the
workspace) is **§4 S6 / S7 (T7) scope**. T6 ratifies the seam; T7 finishes
the feature-default hygiene. Cross-reference
[`T1-s1-audit.md:63-69`](./T1-s1-audit.md) which already lists this deferral
under "T7 (S6 / S7 — cloud re-exports + default-feature hygiene)".
