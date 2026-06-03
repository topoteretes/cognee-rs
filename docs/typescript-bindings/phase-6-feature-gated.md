# Phase 6 — Feature-gated surfaces: visualize, serve/disconnect

← [Index](../typescript-bindings-plan.md)

**Goal:** complete the parity checklist with the two surfaces that sit behind cargo features.
Surfaces **#18 `visualize`** (feature `visualization`) and **#19 `serve` / `disconnect`**
(feature `cloud`).

## Scope

- **In:** conditional compilation + export of these functions, their JSON shapes, and a TS-side
  "feature not built" guard.
- **Out:** the HTTP server surface itself (out of scope for the SDK bindings).

## Structures

### Cargo features (`js/cognee-neon/Cargo.toml`)
- Define `visualization` and `cloud` features on `cognee-neon` that forward to the matching
  `cognee-lib` features. Decide the **default** set: per the project feature strategy,
  non-platform features are on by default in the umbrella/CLI — so `visualization` likely
  defaults on; `cloud` may stay opt-in. Align with `cognee-cli`'s defaults.

### Native functions (cfg-gated)
- `cogneeVisualize(handle, opts?) -> Promise<string>` (cfg `visualization`)
  - Calls `visualization::visualize` over `svc.graph_db`; returns the self-contained HTML as a
    string, or writes to `opts.destinationPath` and returns the path.
- `cogneeServe(handle, opts?) -> Promise<...>` and `cogneeDisconnect(handle) -> Promise<...>`
  (cfg `cloud`)
  - Map to `cloud::serve` / `serve_url` / `serve_cloud` / `disconnect`; marshal `ServeConfig` and
    `CloudCredentials` as JSON. Preserve on-disk-format compatibility (the Rust cloud crate
    already guarantees this).
- Registration in `lib.rs` is `#[cfg(feature = ...)]`-gated; absent features simply don't export.

## Functionalities

- `visualize` produces the d3.js HTML graph view (force-directed, Canvas) for the current graph.
- `serve` / `disconnect` connect/disconnect the local instance to the cognee cloud.
- **TS guard:** the Phase 7 layer detects when a native export is `undefined` and throws a clear
  `CogneeFeatureNotBuiltError("visualization")` rather than a cryptic "not a function".

## Dependencies & ordering

Needs Phase 1 (handle/services). `visualize` benefits from Phases 3–5 (a populated graph) but is
independent code-wise. Can be done in parallel with Phase 5.

## Risks

- Feature combinations multiply the build/prebuild matrix — keep the default set small and
  documented; ensure the npm prebuilds state which features they include.
- `cloud` pulls additional dependencies; keep it opt-in unless there's demand.

## Done when

- In a `visualization` build, `cogneeVisualize` returns valid HTML for a non-empty graph.
- In a `cloud` build, `serve` / `disconnect` are callable.
- In builds without a feature, the TS layer throws a clear, typed "feature not built" error.
