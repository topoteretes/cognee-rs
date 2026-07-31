# Contributing to cognee-rust

Thanks for contributing! cognee-rust is a Rust port of the Python
[cognee](https://github.com/topoteretes/cognee) AI-memory pipeline, with C, JS, and Python
bindings. The headline goal is **90%+ behavioral parity with Python cognee** — keep that in
mind when changing pipeline output, IDs, schema, or ranking.

## Getting started

```bash
git clone https://github.com/topoteretes/cognee-rs
cd cognee-rs
cargo build
```

See [`.claude/CLAUDE.md`](.claude/CLAUDE.md) for an architecture overview and the crate map.

## Branching & PRs

- **Branch off `main`.** Never commit directly to `main`.
- One logical change per branch / PR. Don't batch unrelated work.
- Suggested branch name: `task/<short-slug>` or `<type>/<short-slug>`.
- Open the PR against `main`; ensure CI (`ci.yml`) is green before requesting review.

## Commit messages

We use **[Conventional Commits](https://www.conventionalcommits.org/)** with an optional
scope:

```
<type>(<optional scope>): <imperative summary>

<optional body, wrapped at ~72 cols, explaining the *why*>
```

- **Types:** `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`, `ci`, `build`.
- **Scope** is usually a crate/binding (e.g. `python`, `cognify`, `capi`, `js`).
- Examples from this repo:
  - `fix(python): disable Rust test harness for the PyO3 extension module`
  - `feat(search): add triplet-completion retriever`
  - `docs: add release runbook`
- **AI-assisted commits** must include a co-author trailer, e.g.:
  ```
  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
  ```

## Coding conventions

- Run `cargo fmt` before committing.
- **`unwrap()` is forbidden in non-test code.** Use `expect("why it cannot fail")` with a
  reason, or propagate the error (`?` / `map_err` / `ok_or`). `Mutex/RwLock` lock guards may
  `.unwrap()` with a `// lock poison is unrecoverable` comment.
- Use `thiserror` for library error enums, `anyhow` in binaries/examples.
- Prefer `Arc<dyn Trait>` abstractions; keep public traits `Send + Sync`.
- **Parity is sacred:** do not change on-disk DB columns, content-hash inputs, UUID5
  namespaces/inputs, vector collection name formats, or stored-file naming unless the change
  is explicitly intended and called out — these stay byte-compatible with Python cognee.

## Testing

Before pushing, run the full gate:

```bash
scripts/check_all.sh
# fmt --check → cargo check --all-targets → clippy -D warnings → C/Python/JS binding checks
```

For tests that exercise the LLM / embedding path (cognify, search, fact extraction), use:

```bash
# Downloads BGE-Small embedding models if missing; runs single-threaded for LLM isolation.
bash scripts/run_tests_with_openai.sh                 # full workspace
bash scripts/run_tests_with_openai.sh <test_name>     # a single test
```

These need an OpenAI-compatible endpoint. Configure via `.env` at the repo root:
`OPENAI_URL`, `OPENAI_TOKEN`, and optionally `OPENAI_MODEL`
(see the "Running Integration & E2E Tests" section in `.claude/CLAUDE.md`).

Plain unit tests that don't touch the LLM run with `cargo test`.

### MSRV & lockfiles

The MSRV is **1.89**, declared via `rust-version` in `[workspace.package]` and pinned for
local builds by `rust-toolchain.toml`. (On x86_64 the embedded qdrant `quantization` crate
uses AVX-512 intrinsics stabilized in Rust 1.89; the aarch64 build skips that path, so a Mac
may build on an older toolchain while CI on x86_64 will not.) `Cargo.lock` is intentionally
**not committed** (see `.gitignore`); the edition-2024 MSRV-aware resolver (`resolver = "3"`)
picks dependency versions compatible with 1.89 on a fresh resolve.

If `scripts/check_all.sh` fails with an error like
`roaring@x.y.z requires rustc 1.90.0` (or any other "requires rustc > 1.89"), you have a
**stale local lockfile** that pinned a version published with a higher MSRV — it is not a
real breakage (a fresh resolve under the pinned toolchain selects a 1.89-compatible version).
Recover by regenerating the lock or pinning the offending crate down, e.g.:

```bash
# Regenerate fresh (uses the 1.89-aware resolver), or pin the specific crate:
cargo update                                            # whole workspace
cargo update -p roaring@0.11.4 --precise 0.11.3         # one transitive dep
# capi/ is its own workspace — run the same from inside capi/ if it drifts there.
```

## Cleaning build artifacts

This repo contains **four** Cargo workspaces, so a plain `cargo clean` at the root leaves
most of the disk usage behind (the Neon `target/` alone routinely exceeds 20 GB). Use:

```bash
bash scripts/clean_all.sh                   # cargo clean all 4 workspaces
bash scripts/clean_all.sh --all             # ...plus non-Cargo build outputs
bash scripts/clean_all.sh --dry-run --all   # report sizes, delete nothing
bash scripts/clean_all.sh --help            # authoritative flag/artifact list
```

| Workspace / crate | Cleaned by root `cargo clean`? |
|---|---|
| `Cargo.toml` (root — `crates/`, `examples/`, `python/`) | yes |
| `capi/Cargo.toml` (separate workspace, decision D10) | **no** |
| `ts/cognee-ts-neon/Cargo.toml` (standalone crate) | **no** |
| `java/cognee-java-jni/Cargo.toml` (standalone crate) | **no** |

`--all` adds the generated non-Cargo outputs (npm, tsc, the Neon `.node` binaries, Maven,
`capi/build*`, `ios/.build`, wheels, `__pycache__`); run `--dry-run --all` for the exact
list with sizes rather than trusting a copy of it here. Two things are deliberately **not**
in `--all`, because nothing in `scripts/check_all.sh` or the per-binding check scripts
brings them back:

- **`target/models`** — downloaded ONNX/tokenizer files, the default `EMBEDDING_MODEL_PATH`
  and the source `scripts/android-build-and-deploy.sh` stages models from. It is preserved
  across the root `cargo clean` (stashed and restored); pass `--include-models` to drop it.
- **`capi/CogneeSDK.xcframework`** — ~6.6 GB, 20–30 min to rebuild via
  `capi/scripts/build_xcframework.sh`, and `ios/Package.swift` cannot resolve without it.
  Pass `--xcframework` to drop it.

Local databases and data stores (`cognee.db`, `.data_storage/`, `.cognee_system/`) are never
removed — they can hold a developer's own knowledge graph.

For Docker, `--docker` removes **only the images this repo builds** — today just the
~5 GB `cognee-e2e-cross-sdk` image from the `e2e-cross-sdk/` harness. It is not part of
`--all` because rebuilding it recompiles both SDKs from scratch, and it is a no-op when
docker is absent or the daemon is down.

Everything else Docker holds is left alone on purpose. A dev machine typically also carries
unrelated images (kind node images, other services' `:local` tags) and a BuildKit cache
shared across every project, so `clean_all.sh` prints the daemon-wide reclaimable total for
information and never runs `docker system prune` itself. Two details worth knowing if you
extend `DOCKER_IMAGES`: removal is by `repo:tag` rather than image ID, because several tags
routinely share one ID and `docker rmi <id>` would delete all of them; and the freed total
counts each image once no matter how many tags point at it.

### Which profile settings drive the size

Debug info is the dominant term, and it reaches further than the Rust artifacts: cargo derives
the `OPT_LEVEL`/`DEBUG` env vars it passes to build scripts from the profile, and the `cmake`
crate maps those onto the `CMAKE_BUILD_TYPE` used for lbug's bundled Ladybug C++ tree. Setting
`opt-level >= 1` **and** `debug = 0` together takes that tree from 2.9 GiB to 213 MiB; either
one alone does nothing for it (`debug = "line-tables-only"` counts as "wants debug info").
Both binding workspaces set the pair in `[profile.dev]` — a debug build of
`ts/cognee-ts-neon` went from 12.1 GiB to 3.3 GiB. See
[docs/build/lbug-rebuilds.md](docs/build/lbug-rebuilds.md) for the full table, and use
`CARGO_PROFILE_DEV_DEBUG=2 cargo build` when you do need full debug info.

## Language bindings

Each binding has its own check script (also invoked by `scripts/check_all.sh`):

| Binding | Source | Check | Notes |
|---|---|---|---|
| **C API** (`capi/`) | separate Cargo workspace | `bash capi/scripts/check.sh` | FFI must never panic across the boundary — sanitize/propagate, never `unwrap()` caller data. Headers + built lib are the artifact. |
| **JavaScript/TypeScript** (`ts/`) | Neon (`ts/cognee-ts-neon/`, standalone crate) | `bash ts/scripts/check.sh` | Return JS errors instead of panicking into the V8 runtime. |
| **Python** (`python/`) | PyO3 (`cognee-python`, workspace member) | `bash python/scripts/check.sh` | Exercised by pytest (the Rust test harness is disabled for the extension module — it has no libpython at link time). |

When you change core crate behavior, check whether the bindings expose it and update them
(and their tests) to keep the SDK surfaces in sync.

## Cross-SDK parity

Parity with Python cognee is verified by the `e2e-cross-sdk/` Docker harness:

```bash
cd e2e-cross-sdk && docker compose up --build
```

If your change could affect IDs, schema, chunking, prompts, or vector collections, run it.

## License

By contributing you agree your contributions are dual-licensed under the project's
**MIT OR Apache-2.0** license (see [`LICENSE-MIT`](LICENSE-MIT) and
[`LICENSE-APACHE`](LICENSE-APACHE)).
