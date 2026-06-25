# cognee-bindings-common

Shared, neon/FFI-free SDK facade for the cognee language bindings. It bundles the engine and services (`CogneeServices`), the shareable handle state (`HandleState`), a portable error type (`SdkError`), JSON wire helpers, and the `ops/` pipeline implementations into one place — so the JavaScript (Neon), C-API, and Python binding crates need only add the thin language-specific glue on top.

## Modules

- `error` — `SdkError` enum and its portable error `code()`s (no neon/FFI imports).
- `handle` — `HandleState` and `DefaultUserBootstrap` for the shareable inner handle state.
- `services` — `CogneeServices`, the fully-wired engine + service bundle.
- `wire` — neon-free JSON marshalling helpers.
- `redact` — `redact_config_json` for safe config logging.
- `ops` — pipeline operations consumed by the bindings: `admin`, `data`, `datasets`, `memory`, `pipeline`, `retrieval`, `sessions`, and `visualization`.

Binding-specific types that require `neon` or C-FFI glue live in the consuming crates (e.g. `cognee-ts-neon`, `cognee-capi`, and the Python binding), not here.

Part of [cognee-rs](https://github.com/topoteretes/cognee-rs) — see the [project README](../../README.md) for an architecture overview and how the pieces fit together.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
