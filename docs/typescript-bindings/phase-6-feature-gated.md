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

Both `visualization` and `cloud` are in `cognee-lib`'s **default** feature set (verified in
`crates/lib/Cargo.toml` — both listed under `[features] default = [...]`), and both appear in
`cognee-cli`'s defaults. Follow the same convention: **both features are in the `default` list
of `cognee-neon`**, forwarding directly to `cognee-lib`.

Add to `js/cognee-neon/Cargo.toml`:
```toml
[features]
default = ["visualization", "cloud"]   # plus existing: qdrant, ladybug, onnx, hf-tokenizer, tiktoken, sqlite, testing
visualization = ["cognee-lib/visualization"]
cloud         = ["cognee-lib/cloud"]
```

`cognee-neon` currently has no `[features]` section at all — this phase adds one.

### Visualization API: exact signatures (verified)

`cognee_visualization` exposes two async free functions:

```rust
// Writes HTML to disk; returns the absolute PathBuf of the file.
pub async fn visualize(
    graph_db: &dyn GraphDBTrait,
    output_path: Option<&Path>,
) -> Result<PathBuf, VisualizationError>

// Returns the HTML string without writing to disk.
pub async fn render(
    graph_db: &dyn GraphDBTrait,
) -> Result<String, VisualizationError>
```

Both are re-exported through `cognee-lib`:
- `cognee_lib::visualize` (top-level re-export — `visualize` only)
- `cognee_lib::visualization::render` (via `pub use cognee_visualization::*` inside the
  `visualization` module)

**Important:** `cognee_lib::visualize` writes to a file and returns a `PathBuf`, **not** an HTML
string. The binding must choose one of two approaches:

**Recommended:** expose two binding functions:
- `cogneeVisualize(handle, opts?) -> Promise<string>` — calls `render()` and returns the HTML
  string directly (no disk I/O in the binding layer; caller decides what to do with it).
- `cogneeVisualizeToFile(handle, opts?) -> Promise<string>` — calls `visualize()` with an
  optional `opts.destinationPath`; returns the path string as written.

The CLI uses `visualize()` (file path returned); for the Node SDK, `render()` returning the HTML
string is more composable. **Update the plan:** the primary TS binding should return HTML string
via `render()`; a secondary `visualizeToFile` binding may also be provided.

`render` takes `&dyn GraphDBTrait` — obtain via `handle.services().await?.graph_db` (an
`Arc<dyn GraphDBTrait>`; dereference with `&*`). No `ServeConfig` or session involvement.

### Cloud API: exact signatures (verified)

`cognee-lib` re-exports under `#[cfg(feature = "cloud")]`:
```rust
pub use cognee_cloud::{
    CloudClient, CloudCredentials, CloudError, CloudResult,
    ServeConfig, disconnect, serve, serve_cloud, serve_url,
};
```

Key function signatures:
```rust
pub async fn serve(config: ServeConfig) -> CloudResult<Arc<CloudClient>>
pub async fn serve_url(url: impl Into<String>, api_key: Option<impl Into<String>>) -> CloudResult<Arc<CloudClient>>
pub async fn serve_cloud() -> CloudResult<Arc<CloudClient>>
pub async fn disconnect(wipe_credentials: bool) -> CloudResult<()>
```

`ServeConfig` fields:
- `url: Option<String>` — direct mode when set
- `api_key: Option<String>`
- `cloud_url: Option<String>` — management API URL override
- `auth0_domain / auth0_client_id / auth0_audience: Option<String>`

`CloudClient` return value: the binding **does not** need to expose the `CloudClient` handle to
JS — `serve()` installs the client process-wide via `set_client()` internally. Return a simple
success JSON `{ "serviceUrl": "...", "email": "..." }` by reading `client.service_url` and the
credential file, or just `{ "connected": true, "serviceUrl": "..." }`.

Note: `serve()` / `disconnect()` do **not** take a `CogneeHandle` — they operate on a
process-wide singleton (`state::set_client`). The `handle` parameter in the binding signatures
is only needed if config or component initialization must happen before serving. For the
minimal binding, pass just the JSON config, no handle required.

### Native functions (cfg-gated)

New source file: `js/cognee-neon/src/sdk_visualization.rs` (cfg `visualization`)
- `cogneeVisualize(handle, opts?) -> Promise<string>` — calls `render(&*graph_db)`, returns HTML
  string. `handle` is needed to get `graph_db`. `opts` is unused / reserved for future extension.
- `cogneeVisualizeToFile(handle, opts?) -> Promise<string>` — calls `visualize(&*graph_db,
  destination_path.as_deref())`, returns the written file path as a string.

New source file: `js/cognee-neon/src/sdk_cloud.rs` (cfg `cloud`)
- `cogneeServe(opts?) -> Promise<object>` — deserializes JSON opts into `ServeConfig` fields,
  calls `serve(config)`, returns `{ "connected": true, "serviceUrl": "..." }`.
- `cogneeDisconnect(opts?) -> Promise<void>` — calls `disconnect(wipe_credentials)` where
  `wipe_credentials` defaults to `false`.

Registration in `lib.rs` is `#[cfg(feature = "...")]`-gated; absent features simply don't
export the functions (they remain `undefined` in the JS module object).

### Error propagation

Wrap `VisualizationError` and `CloudError` into `SdkError::ServiceBuild(err.to_string())` (or
add dedicated variants). The `throw_sdk_error` helper already exists in `errors.rs`.

## Functionalities

- `cogneeVisualize` produces the d3.js HTML graph view (force-directed, Canvas) for the current
  graph, returning the HTML as a string.
- `cogneeVisualizeToFile` writes the HTML to disk and returns the path.
- `cogneeServe` / `cogneeDisconnect` connect/disconnect the local instance to the cognee cloud.
- **TS guard:** the Phase 7 layer detects when a native export is `undefined` and throws a clear
  `CogneeFeatureNotBuiltError("visualization")` rather than a cryptic "not a function".

## Dependencies & ordering

Needs Phase 1 (handle/services for `graph_db` access). `visualize` benefits from Phases 3–5 (a
populated graph) but is independent code-wise. Cloud functions don't need the `CogneeHandle`
beyond optional component init. Can be done in parallel with Phase 5.

## Risks

- **`serve` / `disconnect` are process-wide singletons**, not scoped to a `CogneeHandle`.
  Cloud mode runs an interactive OAuth2 device-code flow (TTY prompt) — document that this
  requires a terminal and cannot run headless unless env vars provide the service URL + API key.
- **`visualize` vs `render` naming:** the crate exposes both. The plan previously said
  `cogneeVisualize` returns "HTML string or path" — that conflated two different functions.
  Corrected above: `render()` → HTML string; `visualize()` → file path.
- Feature combinations multiply the build/prebuild matrix — both features are default, so the
  standard prebuild covers them; a `--no-default-features` build strips them.
- `cloud` pulls reqwest + Auth0 + management API deps — already in `cognee-lib`'s defaults.

## Done when

- In a `visualization` build, `cogneeVisualize` returns valid HTML string for a non-empty graph.
- In a `visualization` build, `cogneeVisualizeToFile` writes the file and returns the path.
- In a `cloud` build, `cogneeServe` and `cogneeDisconnect` are callable (direct mode with URL +
  API key works without a TTY; cloud mode is callable and documented as TTY-required).
- In builds without a feature, the TS layer throws a clear, typed "feature not built" error.
- `js/scripts/check.sh` passes (compile + lint, no LLM required).
