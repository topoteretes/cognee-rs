# 19 — Embedding auto-dimension resolution

> Wave 4 · Priority P1 (should-fix) · Track A · Release-blocking: no · Effort: 0.5d ·
> Depends on: — · Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) B7.2 · [index](00-INDEX.md)

## Goal

Add a safety net that resolves the embedding **vector dimension** from the configured
provider+model instead of trusting a single static default. When the model changes (e.g.
ONNX BGE-Small → OpenAI `text-embedding-3-large`) but `EMBEDDING_DIMENSIONS` is not
explicitly set, Rust must derive the correct dimension (384 vs 3072) rather than silently
keeping `384` and producing vector-shape mismatches on the first write.

End state: a known-model → dimension lookup table + explicit override precedence
(`EMBEDDING_DIMENSIONS` env > known-model table > documented fallback with a warning),
applied in `EmbeddingConfig::from_env()` so every engine reports the right size and the
vector collections are created with matching dimensions.

## Background & why

Python resolves dimensions automatically. `_resolve_embedding_dimensions`
(`/tmp/cognee-python/cognee/infrastructure/databases/vector/embeddings/config.py:19–59`)
queries the `fastembed` and `litellm` registries to map a provider+model to its output
vector size, and `EmbeddingConfig.model_post_init` (lines 62–102) only falls back to a
hard default (`3072`) **with a warning** when the model is unknown. `embedding_dimensions`
defaults to `None` precisely so it gets auto-derived — the code comment notes the previous
hard `3072` default "silently broke every non-OpenAI embedder."

Rust does **none of this** (audit B7.2). The default trio is
`onnx` / `BGE-Small-v1.5` / static **384** (`crates/lib/src/config.rs:662`), and
`EmbeddingConfig::from_env()` (`crates/embedding/src/config.rs`) reads
`EMBEDDING_DIMENSIONS` verbatim or uses a provider default with **no model lookup**. There
is **no known-model dimension table anywhere** in the embedding crate. So if a user sets
`EMBEDDING_MODEL=text-embedding-3-large` but forgets `EMBEDDING_DIMENSIONS`, Rust keeps the
old/default dimension; the Qdrant adapter then either creates the collection at the wrong
size or — because it infers size from the first vector batch — errors later with
`VectorDBError::DimensionMismatch` (`crates/vector/src/qdrant_adapter.rs:275–286`) when a
second engine/batch disagrees. Either way it's a confusing, late failure.

### Python vs Rust

| Aspect | Python | Rust (current) |
|---|---|---|
| dims default | `None` → auto-resolved | static `384` (lib) / `1536` (embedding crate non-onnx default) |
| resolution source | `fastembed` + `litellm` registries | **none** |
| unknown-model behavior | warn + fallback `3072` | silently keep configured/default |
| explicit override | `EMBEDDING_DIMENSIONS` wins | `EMBEDDING_DIMENSIONS` wins ✓ |
| mismatch surfaced | at first vector write (shape error) | `DimensionMismatch` at write ✓ (but late) |

### On the default trio (do we change it?)

The divergence `onnx/BGE-Small/384` (Rust edge default) vs
`openai/text-embedding-3-large/3072` (Python cloud default) is **partly intentional** —
Rust targets edge devices with a bundled local ONNX model, so defaulting to an OpenAI
cloud model would be wrong for the project's primary use case.

**Recommendation: keep the Rust edge default trio, do NOT switch to OpenAI.** The real bug
is the missing *auto-resolution safety net*, not the default itself. Cross-SDK interop on
the **vector store** already requires the same embedder on both sides (different models
produce incomparable vectors regardless of dimension), so matching Python's default model
would not buy interop — only matching the *user's chosen* model does. Document the default
clearly and make `from_env` resolve correctly for whatever model the user picks. (If the
release owner wants strict default parity, that is a separate decision in task 01; this
task implements the safety net either way.)

## Prerequisites

```bash
git checkout -b task/19-embedding-dimension-resolution
```

Read first:

- Rust: `crates/embedding/src/config.rs` (`EmbeddingConfig`, `Default`, `from_env`),
  `crates/lib/src/config.rs:~660–670` (lib-level defaults),
  `crates/embedding/src/{onnx,openai_compatible,ollama}.rs` (`fn dimension`),
  `crates/vector/src/qdrant_adapter.rs:~118–158, ~223–286` (collection size + mismatch).
- Python: `/tmp/cognee-python/cognee/infrastructure/databases/vector/embeddings/config.py`
  (lines 19–102), and the dim assertions in
  `/tmp/cognee-python/cognee/tests/unit/infrastructure/databases/vector/test_embedding_config.py`.

Re-grep current locations:

```bash
grep -n "embedding_dimensions\|384\|1536\|3072\|fn from_env\|impl Default for EmbeddingConfig" crates/embedding/src/config.rs
grep -n "embedding_dimensions\|embedding_model_name\|384" crates/lib/src/config.rs
grep -n "fn dimension" crates/embedding/src/onnx.rs crates/embedding/src/openai_compatible.rs crates/embedding/src/ollama.rs
grep -n "DimensionMismatch\|VectorDataConfig\|size:" crates/vector/src/qdrant_adapter.rs
```

## Python reference

`/tmp/cognee-python/cognee/infrastructure/databases/vector/embeddings/config.py`

- **`_resolve_embedding_dimensions(provider, model)` (lines 19–59):** strips a provider
  prefix (`openai/text-embedding-3-large` → `text-embedding-3-large`), builds candidate
  keys, then looks up `fastembed.TextEmbedding.list_supported_models()` (`dim`/`embed_dim`)
  and `litellm.model_cost[candidate]["output_vector_size"]`. Returns `None` if unknown.
  Never raises.
- **`EmbeddingConfig` (lines 62–102):** `embedding_provider="openai"`,
  `embedding_model="openai/text-embedding-3-large"`, `embedding_dimensions: Optional[int]=None`.
  `model_post_init` resolves dims when `None`; on failure logs a warning and uses
  `_FALLBACK_DIMENSIONS = 3072`.
- **Known-model dims (verified via tests):**

  | provider/model | dims |
  |---|---|
  | openai / text-embedding-3-large | 3072 |
  | openai / text-embedding-3-small | 1536 |
  | openai / text-embedding-ada-002 | 1536 |
  | fastembed / BAAI/bge-small-en-v1.5 (BGE-Small) | 384 |
  | fastembed / BAAI/bge-large-en-v1.5 | 1024 |

  (Python derives these from external registries; Rust will hardcode a small table — see
  Gotchas on the prefix-normalization parity.)

## Implementation steps

1. **Add a known-model dimension table** in `crates/embedding/src/config.rs` (or a new
   `dimensions.rs` module in the embedding crate). Keep it small, normalized, and
   provider-aware:

   ```rust
   /// Best-effort known embedding model → output vector dimension.
   /// Mirrors the dims Python resolves via litellm/fastembed registries.
   /// Returns `None` for unknown models (caller then falls back with a warning).
   pub fn known_model_dimensions(provider: EmbeddingProvider, model: &str) -> Option<usize> {
       // Strip a provider prefix: "openai/text-embedding-3-large" -> "text-embedding-3-large"
       let bare = model.rsplit('/').next().unwrap_or(model);
       let key = bare.to_ascii_lowercase();
       let dim = match key.as_str() {
           "text-embedding-3-large" => 3072,
           "text-embedding-3-small" => 1536,
           "text-embedding-ada-002" => 1536,
           "bge-small-v1.5" | "bge-small-en-v1.5" | "baai/bge-small-en-v1.5" => 384,
           "all-minilm-l6-v2" => 384,
           "bge-large-en-v1.5" => 1024,
           "bge-base-en-v1.5" => 768,
           "nomic-embed-text" => 768,        // common Ollama model
           "mxbai-embed-large" => 1024,      // common Ollama model
           _ => return None,
       };
       let _ = provider; // provider currently unused; keep for future provider-scoped dims
       Some(dim)
   }
   ```

   > The `unwrap_or` is on `rsplit('/').next()`, which is infallible for any `&str`
   > (always yields at least one element); no `unwrap()` rule concern.

2. **Define the fallback constant** matching the chosen edge default (NOT Python's 3072 —
   Rust's primary model is BGE-Small):

   ```rust
   /// Fallback dimension when the model is unknown AND EMBEDDING_DIMENSIONS is unset.
   /// 384 matches the default ONNX BGE-Small edge model. If you switch to a cloud
   /// model without setting EMBEDDING_DIMENSIONS, you'll get this + a warning.
   const FALLBACK_DIMENSIONS: usize = 384;
   ```

3. **Apply resolution precedence in `from_env()`.** Locate where `dimensions` is currently
   assigned in `EmbeddingConfig::from_env()` and replace the static/default assignment with:

   ```rust
   let dimensions = match std::env::var("EMBEDDING_DIMENSIONS").ok()
       .and_then(|v| v.trim().parse::<usize>().ok())
   {
       // 1. explicit override always wins (parity with Python)
       Some(d) => d,
       None => match known_model_dimensions(provider, &model) {
           // 2. known model → derived dimension
           Some(d) => d,
           // 3. unknown → fallback + warning (parity with Python model_post_init)
           None => {
               tracing::warn!(
                   provider = ?provider, model = %model, fallback = FALLBACK_DIMENSIONS,
                   "Could not auto-derive embedding dimensions; set EMBEDDING_DIMENSIONS \
                    explicitly if your embedder produces a different vector size, \
                    otherwise the first vector write will fail with a shape mismatch."
               );
               FALLBACK_DIMENSIONS
           }
       },
   };
   ```

   Keep ONNX behavior intact: when `provider == Onnx`, the ONNX config already carries an
   authoritative `dimensions` (the model file dictates it). Prefer the ONNX config's
   dimension over the table for ONNX so a custom ONNX model still works:

   ```rust
   #[cfg(feature = "onnx")]
   let dimensions = if matches!(provider, EmbeddingProvider::Onnx) {
       std::env::var("EMBEDDING_DIMENSIONS").ok()
           .and_then(|v| v.trim().parse().ok())
           .unwrap_or(onnx_cfg.dimensions)
   } else { dimensions };
   ```

4. **Align the `Default` impl** (`crates/embedding/src/config.rs`) so the non-onnx default
   dimension comes from `known_model_dimensions` for the default model rather than a magic
   literal. This removes the lone `1536` literal and keeps one source of truth.

5. **Sync the lib-level default** if needed. `crates/lib/src/config.rs:~662` sets
   `embedding_dimensions: 384` for the ONNX trio — that stays correct. Add a doc comment
   pointing at `known_model_dimensions` so future model changes don't reintroduce a stale
   literal.

6. **(Optional, recommended) Guard collection creation.** In
   `crates/vector/src/qdrant_adapter.rs`, the `DimensionMismatch` error already fires at
   write time. No change required, but verify the error message includes the collection
   name so the failure points the user at the dims mismatch. If it does not, enrich the
   error context (still surfacing the same error type).

## Verification

```bash
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test -p cognee-embedding dimension
```

### Tests to add (in `crates/embedding/src/config.rs` `#[cfg(test)]` or a tests file)

1. **`known_dims_openai_large`** — `known_model_dimensions(OpenAi, "text-embedding-3-large") == Some(3072)`.
2. **`known_dims_prefix_stripped`** — `known_model_dimensions(OpenAi, "openai/text-embedding-3-small") == Some(1536)`.
3. **`known_dims_bge_small`** — both `"BGE-Small-v1.5"` and `"BAAI/bge-small-en-v1.5"` → `Some(384)`.
4. **`from_env_explicit_override_wins`** — set `EMBEDDING_DIMENSIONS=999`, `EMBEDDING_MODEL=text-embedding-3-large`; assert resolved dims `== 999`.
5. **`from_env_model_change_resolves`** — unset `EMBEDDING_DIMENSIONS`, set
   `EMBEDDING_PROVIDER=openai` + `EMBEDDING_MODEL=text-embedding-3-large`; assert resolved
   dims `== 3072` (the regression this task fixes — previously would stay 384).
6. **`from_env_unknown_falls_back`** — unset dims, set `EMBEDDING_MODEL=some-unknown-model`;
   assert resolved dims `== 384` (fallback). (Capturing the warning is optional.)

> Use `std::env::set_var`/`remove_var` inside `#[serial_test::serial]` tests to avoid env
> races with other tests in the crate.

Expected: all pass; the previously-broken case (test 5) now returns 3072.

## Acceptance criteria

- [ ] A `known_model_dimensions(provider, model)` lookup exists, with provider-prefix
      stripping and case-insensitive matching.
- [ ] `EmbeddingConfig::from_env()` resolves dims with precedence
      **explicit env > known-model table > fallback(+warning)**.
- [ ] Changing `EMBEDDING_MODEL` to a known model **without** setting
      `EMBEDDING_DIMENSIONS` yields that model's correct dimension (no silent 384).
- [ ] Unknown model logs a `warn!` and uses the documented fallback.
- [ ] ONNX provider still derives its dimension from the ONNX model config.
- [ ] `cargo check --all-targets`, `cargo clippy -- -D warnings`, and the new tests pass.
- [ ] A short note in the README / `.env.example` documents the resolution order and that
      `EMBEDDING_DIMENSIONS` overrides everything.

## Gotchas / do-not

- **Do NOT switch the default model/provider to OpenAI** as part of this task — that is a
  task-01 decision and would regress the edge-device use case. This task only adds the
  resolution safety net.
- **Explicit `EMBEDDING_DIMENSIONS` must always win** (parity with Python). Never let the
  table override a user-set value.
- **Prefix normalization parity:** Python strips one leading `provider/` segment
  (`model.split("/")[-1]`). Use `rsplit('/').next()` (last segment) so
  `openai/text-embedding-3-large` and `azure/text-embedding-3-large` both resolve. Keep
  matching **case-insensitive** because Rust models are sometimes `BGE-Small-v1.5`.
- **Cross-SDK vectors require the same embedder, not just the same dimension.** Two models
  with the same dimension (e.g. ada-002 and 3-small both 1536) produce incomparable
  vectors. This task only prevents *shape* errors; it does not make different models
  interoperable. Note this in the README so users don't expect dim-match to imply
  interop.
- **The fallback is 384, not Python's 3072** — deliberate, because Rust's default model is
  BGE-Small. Do not copy Python's 3072 fallback or you'll mis-size the common edge path.
- **Do not change the Qdrant collection-name format or distance metric** — only the size
  is in scope here.

## Rollback

Self-contained. Revert by restoring the original `dimensions` assignment in
`from_env()`/`Default` and deleting `known_model_dimensions` + `FALLBACK_DIMENSIONS`:
`git checkout main -- crates/embedding/src/config.rs` (and the new `dimensions.rs` if
added). No schema, ID, or collection-format changes were made, so rollback is risk-free.
