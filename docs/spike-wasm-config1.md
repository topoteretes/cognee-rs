# Spike: compiling cognee-rs to WebAssembly — Config 1 (logic-only)

**Status:** Config 1 **achieved** — the logic-only crate set builds *and runs* on
`wasm32-unknown-unknown`, verified under both Node and a real headless browser.
**Scope:** prove the logic-only crate set cross-compiles to wasm, actually
executes there, and record the exact wall hit at each step. This is a feasibility
spike, not a production WASM SDK. Config 2 (full in-browser pipeline with
in-memory graph + relational backends) remains future work, sized at the end.

**Target chosen:** `wasm32-unknown-unknown` + JS glue (via `wasm-bindgen`) — the
repo owner's *preferred* outcome (browser-capable), not the `wasm32-wasip1`
fallback. See the decision note at the end.

## TL;DR

The following crates now compile to `wasm32-unknown-unknown`:

| Crate | wasm32 | Notes |
|---|---|---|
| `cognee-models` | ✅ | local-filesystem streaming arm cfg'd out; no tokio on wasm |
| `cognee-utils` | ✅ | getrandom 0.2 `js`; uuid `js` (workspace); `futures-timer` for retry (no tokio) |
| `cognee-chunking` (`--features tiktoken`) | ✅ | storage-coupled `cognify_pipeline` cfg'd out |
| `cognee-storage` | ❌ (excluded) | fundamentally filesystem-coupled — see Config 2 |

Native builds of all four crates remain green (verified with `cargo check` on the
host target); every wasm-specific change is behind `cfg(target_arch = "wasm32")`
(or its negation) and leaves the native code path byte-identical.

A `wasm-bindgen-test` smoke test then **runs** the chunking primitives + token
counting inside an actual wasm host — under Node *and* headless Chrome — closing
the last acceptance item (see [Acceptance](#acceptance-running-in-a-wasm-host-node--headless-browser)).

### Reproduce

```bash
# one-time: the wasm target
rustup target add wasm32-unknown-unknown

# logic crates
cargo build -p cognee-models -p cognee-utils \
    --target wasm32-unknown-unknown

# + chunking primitives (tiktoken is pure Rust; do NOT use hf-tokenizer on wasm)
cargo build -p cognee-chunking \
    --no-default-features --features tiktoken \
    --target wasm32-unknown-unknown
```

> Toolchain note: this repo pins its toolchain via `rust-toolchain.toml`, so a
> plain `cargo …` already selects it. (On a Windows host whose `~/.cargo/bin`
> `cargo.exe` proxy is a broken 0-byte shim, either run from a Linux/WSL checkout
> or drive cargo via `rustup run <toolchain> cargo …`.)

## The walls, in the order they were hit

Every wall was in a **transitive dependency or a single filesystem code path** —
none in the core logic. Each was fixed minimally and target-gated.

> **Note on Walls 1–3.** The spike's first pass over-shimmed the dependency
> layer — an extra getrandom 0.4 crate, a rustflag read by nobody, and reduced
> per-crate tokio specs. PR review (checked against the actual manifests and
> `cargo tree --target wasm32-unknown-unknown`) pruned these. The walls below are
> written as **resolved**, with a "Review correction" note where the first
> approach was wrong.

### Wall 1 — `getrandom` (via `rand`) has no default wasm backend

```
error: the wasm*-unknown-unknown targets are not supported by default, you may
       need to enable the "js" feature.
  --> getrandom-0.2.17/src/lib.rs
```

`cognee-utils` pulls `getrandom 0.2.17` transitively through the retry jitter:
`rand 0.8.6` → `rand_core 0.6.4` → `getrandom 0.2.17`. On wasm it has no default
entropy source.

**Fix** (`crates/utils/Cargo.toml`, wasm32 target block) — enable getrandom 0.2's
`js` feature (browser crypto API). A feature-only shim: `rand` already pulls the
crate; this just turns the backend on.

```toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
getrandom = { version = "0.2", features = ["js"] }
```

This is the **only** randomness shim needed. `uuid` does *not* pull getrandom on
wasm32-unknown — it sources its own wasm randomness through its `js` feature,
enabled once on the **workspace** `uuid` dependency (Wall 2). There is no
getrandom 0.4 dependency and no `getrandom_backend` rustflag (`.cargo/config.toml`
carries only `runner = "wasm-bindgen-test-runner"`); the plain `cargo build` path
needs neither.

> **Review correction.** The first pass also added a getrandom **0.4** shim
> (`getrandom_v04`) plus `--cfg getrandom_backend="wasm_js"` in
> `.cargo/config.toml`, assuming `uuid` pulled getrandom 0.4 on wasm. It does
> not: uuid 1.23 gates its `getrandom`/`rand` deps to **non-wasm** targets and
> uses `wasm-bindgen`/`js-sys` on wasm32 (Wall 2). `cargo tree --target
> wasm32-unknown-unknown` shows no getrandom 0.4 in the graph, and the rustflag
> was read by nobody. Both were removed — getrandom 0.2 + `js` is the only
> randomness shim actually needed.

### Wall 2 — `uuid` refuses to build on wasm without a randomness source

```
error: to use `uuid` on `wasm32-unknown-unknown`, specify a source of
       randomness using one of the `js`, `rng-getrandom`, or `rng-rand` features
  --> uuid-1.23.4/src/rng.rs
```

uuid 1.23's `js` feature pulls `wasm-bindgen` + `js-sys` (both target-gated to
wasm inside uuid) and routes `Uuid::new_v4()` through the browser crypto API —
no getrandom involved on wasm.

**Fix** — enable `js` on the **workspace** uuid dependency, not per-crate:

```toml
# Cargo.toml (workspace)
uuid = { version = "1.21", features = ["v4", "v5", "serde", "js"] }
```

`js` is a no-op on native (the wasm-bindgen/js-sys deps don't exist there), so
enabling it workspace-wide leaves native builds byte-identical while every
crate's uuid works on wasm. This also fixes a latent gap: `cognee-models` calls
`Uuid::new_v4()` but does not depend on `cognee-utils`, so a per-crate `js` in
utils only reached models by feature-unification leak when both were compiled
together — a standalone `cargo build -p cognee-models --target wasm32` would have
missed it.

### Wall 3 — `tokio`'s `rt-multi-thread` / `fs` don't compile on wasm

```
error: Only features sync,macros,io-util,rt,time are supported on wasm.
  --> tokio-1.52.3/src/lib.rs
```

The workspace tokio carries `rt-multi-thread` + `fs`, and workspace inheritance
can only *add* features, never drop them — so a wasm crate can't inherit it. The
resolution is not a reduced-feature tokio but **no tokio on wasm at all**:

- **`cognee-models`** — its only tokio use is local-file streaming in the
  `FilePath` arm, already cfg'd off wasm (Wall 4). tokio is gated to non-wasm; the
  wasm build pulls none.
- **`cognee-utils`** — its only non-test tokio use was `tokio::time::sleep` in
  `retry.rs`. tokio's timer doesn't actually fire on wasm32 (no clock, no thread
  parking), so it is replaced with `futures_timer::Delay` — a runtime-agnostic
  timer whose `wasm-bindgen` feature is a native no-op. tokio becomes a
  native-only **dev-dependency** (the `#[tokio::test]` module is gated off wasm).
- **`cognee-chunking`** — only uses tokio in its storage-coupled pipeline, gated
  off wasm (Wall 5).

```toml
# crates/models/Cargo.toml — tokio is non-wasm-only
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
tokio.workspace = true

# crates/utils/Cargo.toml — retry uses a cross-platform timer; no lib tokio
[dependencies]
futures-timer.workspace = true
```

> **Review correction.** The first pass instead gave each crate a reduced wasm
> tokio spec (`["sync","macros","io-util","rt","time"]`). Most features were
> unused and tokio's timer is inert on wasm anyway, so the blocks were removed in
> favour of `futures-timer` + native-only tokio — fewer cfgs, and retry's backoff
> now actually works on wasm.

### Wall 4 — `cognee-models` streams local files via `tokio::fs`

The first **source-level** wall (the earlier static audit had marked the logic
crates as fs-free; this one was missed):

```
error[E0432]: unresolved import `tokio::fs`
  --> crates/models/src/data_input.rs:3
```

`DataInput::process_by_chunks` reads `DataInput::FilePath` from the local
filesystem. wasm32 has no filesystem.

**Fix** — cfg-gate the `FilePath` arm (and its `tokio::fs`/`AsyncReadExt`
imports) off wasm; on wasm it returns `io::ErrorKind::Unsupported`, exactly
mirroring how the existing `Url` and `S3Path` arms already defer resolution.
Callers must resolve a path to `Text`/`Binary` before streaming. The native code
path is unchanged.

### Wall 5 — `cognee-chunking` → `cognee-storage` is filesystem-coupled

`cognee-chunking` depends on `cognee-storage`, but **only** through
`cognify_pipeline::ExtractTextChunksPipeline`, which holds an
`Arc<dyn StorageTrait>`. The core chunking primitives (`chunk_text`, the
`TokenCounter` trait, `TikTokenCounter`, `WordCounter`, the word/sentence/
paragraph/row chunkers) have **zero** storage dependency.

`cognee-storage` cannot currently compile on wasm — and not just `LocalStorage`:

- `storage_trait.rs` exposes `StorageWriter { file: tokio::fs::File }` and
  `get_full_path(..) -> PathBuf` in the public trait surface.
- `local_storage.rs` is entirely `tokio::fs`.
- even `MockStorage` uses `tempfile::NamedTempFile` + `tokio::fs::File::from_std`.

So the trait itself — not just the default backend — is filesystem-shaped.

**Fix for Config 1** — cfg-gate `cognify_pipeline` (and therefore the
`cognee-storage` + `tokio` deps) off wasm in `cognee-chunking`. wasm keeps the
pure chunking primitives, which is exactly the Config-1 / acceptance target:

```toml
# crates/chunking/Cargo.toml
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
cognee-storage = { path = "../storage", version = "0.1.1" }
tokio.workspace = true
```

```rust
// crates/chunking/src/lib.rs
#[cfg(not(target_arch = "wasm32"))]
pub mod cognify_pipeline;
#[cfg(not(target_arch = "wasm32"))]
pub use cognify_pipeline::ExtractTextChunksPipeline;
```

### Wall 6 — `chrono::Utc::now()` traps at runtime (invisible to the compiler)

The only wall **the build could not catch**. After all six crates compiled, the
smoke test still aborted the moment it touched real logic:

```
test chunk_text_runs_in_wasm ... FAILED
  RuntimeError: unreachable executed
```

`chunk_text` → `DocumentChunk::new` → `DataPoint::new` calls `chrono::Utc::now()`,
which reads the system clock. On `wasm32-unknown-unknown` chrono has no clock
source and the call lowers to an `unreachable` instruction that traps at runtime.
A compile-only check (or `cargo build`) sails right past this — it is purely a
**runtime** wall, which is exactly why the acceptance step *runs* the code rather
than only building it.

**Fix** — enable chrono's `wasmbind` feature on wasm, which routes `Utc::now()`
through `js_sys::Date`. The feature is additive, so the base `chrono.workspace`
dep simply gains it via feature unification on wasm only:

```toml
# crates/models/Cargo.toml — wasm32 target block
chrono = { workspace = true, features = ["wasmbind"] }
```

## Correction to the prior dependency audit

The pre-spike audit listed `cognee-storage` as a "light" Config-1 crate
(`async-trait/tokio/uuid`). In practice **storage is fundamentally
filesystem-coupled at the trait level**, and is the first crate that needs real
new code (an in-memory backend + a writer abstraction that isn't `tokio::fs::File`)
rather than a feature/cfg tweak. This makes it the natural boundary between
Config 1 (achieved) and Config 2.

## Files changed

```
Cargo.toml                            workspace: uuid `js` (no-op on native); add futures-timer
.cargo/config.toml                    wasm test runner (getrandom_backend rustflag removed in review)
crates/models/Cargo.toml              tokio gated non-wasm-only; chrono `wasmbind` on wasm (Wall 6)
crates/models/src/data_input.rs       cfg-gate FilePath fs streaming + async test off wasm
crates/utils/Cargo.toml               getrandom 0.2 `js`; futures-timer; tokio dev-dep split off wasm
crates/utils/src/retry.rs             tokio::time::sleep -> futures_timer::Delay; test module off wasm
crates/chunking/Cargo.toml            gate cognee-storage + tokio off wasm; wasm test dev-dep
crates/chunking/src/lib.rs            gate cognify_pipeline off wasm
crates/chunking/tests/wasm.rs         wasm smoke test, Node runner
crates/chunking/tests/wasm_browser.rs same assertions, headless-browser runner
crates/chunking/tests/wasm_smoke/mod.rs shared assertion bodies for both runners
.github/workflows/ci.yml              NEW `wasm` job: build + --no-run drift guard + Node run
scripts/check_all.sh                  build-only wasm drift guard (no runner)
docs/spike-wasm-config1.md            this report
```

> Committed in two steps then revised: `8fa4dde` (build+run), `4f1bff0` (browser
> runner), and a review-response round (dependency pruning + wasm CI). See the
> "Review correction" notes in Walls 1–3.

## Acceptance: running in a wasm host (Node + headless browser)

The crates build *and run*. The acceptance smoke test exercises the pure logic
path end-to-end inside a wasm host — `WordCounter`, `chunk_text` (which drives
`DataPoint::new` → the Wall-6 `Utc::now()`), and (under `--features tiktoken`)
`TikTokenCounter` cl100k BPE encoding. The assertion bodies live in
`tests/wasm_smoke/mod.rs` and are shared by two runners so they can't drift:

| Runner | Config | Default features | `--features tiktoken` |
|---|---|---|---|
| **Node** (`tests/wasm.rs`) | default | ✅ 2 passed | ✅ 3 passed |
| **Headless Chrome** (`tests/wasm_browser.rs`, `run_in_browser`) | real browser | ✅ 2 passed | ✅ 3 passed |

The browser runner is what proves the *owner's target* — the wasm artifact and
its `wasm-bindgen` JS glue executing in a real browser, not merely under Node.

### Reproduce the tests

```bash
# one-time host tooling
cargo install wasm-bindgen-cli          # must match the locked wasm-bindgen version
# Node on PATH for the Node runner.

# Node runner (default + tiktoken)
cargo test -p cognee-chunking --target wasm32-unknown-unknown --test wasm
cargo test -p cognee-chunking --features tiktoken \
    --target wasm32-unknown-unknown --test wasm

# Headless-browser runner — needs a WebDriver + matching browser. With
# Chrome for Testing unpacked locally and `google-chrome` on PATH:
export CHROMEDRIVER=/path/to/chromedriver
cargo test -p cognee-chunking --target wasm32-unknown-unknown --test wasm_browser
cargo test -p cognee-chunking --features tiktoken \
    --target wasm32-unknown-unknown --test wasm_browser
```

> Environment notes (from the spike host):
> - `wasm-bindgen-cli` must match the locked `wasm-bindgen` version (0.2.126
>   here); a mismatch fails the runner.
> - Chrome runs headless via the matched **Chrome for Testing** + `chromedriver`
>   pair; `CHROMEDRIVER` points the runner at the driver, which discovers the
>   browser through a `google-chrome` symlink on `PATH`. Under WSL the browser
>   needs `--no-sandbox` (the runner's default headless args already include it).
> - The Node runner has worked on Node 18 and 20; Node 20+ is recommended (an
>   earlier wasm-bindgen/Node-18 combination crashed V8 on the externref glue).

### CI coverage (drift guard)

Because the wasm test files are `#![cfg(target_arch = "wasm32")]`, the native
lanes compile them to empty crates and never type-check them — a renamed
`DocumentChunk` field or a changed `chunk_text` signature would stay green on
native CI and only surface on a manual wasm run. Two guards close this:

- **`ci.yml` `wasm` job** — builds the logic crates for
  `wasm32-unknown-unknown`, then type-checks the wasm **test** build of every
  crate whose wasm test layer this work gates (`cargo test … --no-run`, no runner
  needed): `cognee-utils` + `cognee-models` (so the tokio dev-dep split and the
  `cfg(not(wasm32))` gates on the retry/data_input test modules can't silently
  regress — `cargo build` only covers the lib), and `cognee-chunking` under
  **both** the default and `--features tiktoken` configs (the shared `wasm_smoke`
  module has a `#[cfg(feature = "tiktoken")]` arm, and the default build of
  `tests/wasm.rs` is otherwise only exercised by the live Node step). It then
  installs a version-matched `wasm-bindgen-cli` (read from the gitignored
  Cargo.lock) and runs the **Node** smoke tests. The headless-browser runner
  stays manual (needs a WebDriver).
- **`scripts/check_all.sh`** — the same build-only `--no-run` drift guards
  locally, with no Node/wasm-bindgen-cli requirement.

## Config 2 — what's left (sized for its own issue)

To run the **full** pipeline (`add → cognify → search`) in-browser, the two
genuine blockers from the audit remain, plus the storage finding above:

1. **In-memory relational store** — a pure-`HashMap` implementation of
   `IngestDb` / `SearchHistoryDb` / `DeleteDb`, replacing the SeaORM/SQLite (C)
   and Postgres (network) backends. Largest new piece.
2. **In-memory graph** — a pure-Rust `GraphDBTrait` impl (harden/promote
   `MockGraphDB`), replacing Ladybug (C++) and PgGraph (network).
3. **wasm-clean storage** — an in-memory `StorageTrait` backend **and**
   decoupling `StorageWriter` from `tokio::fs::File` (e.g. an enum/`Box<dyn>`
   writer, or a `Vec<u8>` buffer on wasm), so `cognify_pipeline` can run on wasm.
4. **Runtime shims already proven viable here** — `getrandom 0.2` `js` backend,
   uuid `js` (workspace), `futures-timer` for delays (tokio's timer is inert on
   wasm). Still needed: `reqwest` Fetch/`wasm-bindgen` transport for the
   OpenAI-compatible embedding + remote-HTTP backends, and — if any async code on
   the wasm path needs an executor — a wasm-compatible runtime
   (e.g. `wasm-bindgen-futures`) rather than tokio's `rt`.
5. **Not blockers** (already pure Rust / HTTP): `BruteForceVectorDB`,
   `OpenAICompatibleEmbeddingEngine`, `TikTokenCounter` / `WordCounter`.

A `wasm` feature set on `cognee-lib` (mirroring the existing `android-default`
set, dropping onnx/pgvector/pggraph/postgres/pdfium/server/gRPC-telemetry and
keeping brute-force vector + tiktoken + HTTP embedding) is the umbrella-crate
entry point for Config 2, with cfg-guards on the native-only deps mirroring the
existing `cfg(not(target_os = "android"))` guard in `crates/vector/Cargo.toml`.

## Decision: `wasm32-unknown-unknown` vs `wasm32-wasip1`

This spike targeted **`wasm32-unknown-unknown`** (browser / `wasm-bindgen`),
which is the harder, more portable target and the one the SDK use-cases want.
`wasm32-wasip1` (WASI) does **not** rescue the heavy backends: the C++ deps
(`lbug`/Ladybug, `ort`, `arrow`) build through `cxx` + `cmake` and don't target
wasm at all, and even where C (`libsqlite3-sys`) could theoretically build with
`wasi-sdk`, the graph backend still can't. So the in-memory backends are required
regardless of which wasm target is chosen; `unknown-unknown` is the right default.
