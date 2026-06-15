# Contributing to cognee-rust

Thanks for contributing! cognee-rust is a Rust port of the Python
[cognee](https://github.com/topoteretes/cognee) AI-memory pipeline, with C, JS, and Python
bindings. The headline goal is **90%+ behavioral parity with Python cognee** — keep that in
mind when changing pipeline output, IDs, schema, or ranking.

## Getting started

```bash
git clone https://github.com/topoteretes/cognee-rust
cd cognee-rust
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

## Language bindings

Each binding has its own check script (also invoked by `scripts/check_all.sh`):

| Binding | Source | Check | Notes |
|---|---|---|---|
| **C API** (`capi/`) | separate Cargo workspace | `bash capi/scripts/check.sh` | FFI must never panic across the boundary — sanitize/propagate, never `unwrap()` caller data. Headers + built lib are the artifact. |
| **JavaScript** (`js/`) | Neon (`js/cognee-neon/`, standalone crate) | `bash js/scripts/check.sh` | Return JS errors instead of panicking into the V8 runtime. |
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

By contributing you agree your contributions are licensed under the project's
**Apache-2.0** license (see [`LICENSE`](LICENSE)).
