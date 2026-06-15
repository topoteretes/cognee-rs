# 14 — Chunking Parity (tiktoken default + max_chunk_size)

> Wave 3 · Priority P1 · Track A · Release-blocking: no · Effort: 0.5d ·
> Depends on: — · Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) B2.1, B2.2; [release-readiness-plan.md](../release-readiness-plan.md) Phase 7 T8.5. See [index](00-INDEX.md).

## Goal

Make Rust chunking produce the **same chunk boundaries** as Python cognee for the
default (OpenAI-family) configuration. Two fixes:

1. **Default token counter (B2.1):** when no explicit override is set, Rust falls back to
   the whitespace `WordCounter` (the default provider is `onnx` with no tokenizer file).
   Python's default tokenizer is **tiktoken cl100k_base** (the default provider is
   `openai`). Align the default so an out-of-box OpenAI-family setup counts tokens with
   BPE, not whitespace.
2. **Default `max_chunk_size` (B2.2):** Rust hardcodes `1500` and its auto-calc uses the
   wrong quantities (LLM *context window* ÷ 2). Python auto-calculates
   `min(embedding_engine.max_completion_tokens, llm_max_completion_tokens // 2)` — the
   **completion-token** budget, ≈ **512** out of the box (embedding side dominates). Fix
   the default and the auto-calc formula.

> **Loud, intentional consequence:** chunk boundaries feed `uuid5` chunk IDs
> (`uuid5(NAMESPACE_OID, "{document_id}-{chunk_index}")`) and chunk *counts*. Changing
> token counting or `max_chunk_size` **changes which chunks exist and therefore their
> IDs** — see Gotchas. This is the parity goal, but it is a determinism-affecting change.

## Background & why

### Fact 1 — token counter default (verified, both sides)

**Rust** (`crates/chunking/src/config.rs`):
- `TokenCounterKind::from_env()` (lines 51-104) defaults `EMBEDDING_PROVIDER` to `"onnx"`
  when unset (line ~78). For `onnx`/`fastembed` it only picks a real tokenizer if
  `EMBEDDING_TOKENIZER_PATH` points to an existing file; otherwise it returns
  `TokenCounterKind::Word` (whitespace). It returns `TikToken` **only** for
  `EMBEDDING_PROVIDER=openai|openai_compatible` or `COGNEE_TOKEN_COUNTER=tiktoken`.
- A *second* fallback exists in `build()` (lines 112-165): even if `TikToken`/`HuggingFace`
  is selected, if the `tiktoken` / `hf-tokenizer` Cargo feature is **off**, it silently
  falls back to `WordCounter` with an `eprintln!` warning.
- Features: `crates/chunking/Cargo.toml` `default = []`; `tiktoken = ["dep:tiktoken-rs"]`,
  `hf-tokenizer = ["dep:tokenizers", "tokenizers/http"]`. **Good news:** `cognee-lib`
  (`Cargo.toml` default list) and `cognee-cli` (`Cargo.toml` default list) **both enable
  `tiktoken` and `hf-tokenizer` by default**, so a normal build *can* construct
  `TikTokenCounter::cl100k_base()` (`token_counter.rs:88-95`). The feature-off fallback
  only bites `--no-default-features` builds.
- So the real default gap is **selection**: with no env vars, `from_env()` returns `Word`
  (because provider defaults to `onnx`), not tiktoken.

**Python** (`/tmp/cognee-python/cognee/infrastructure/databases/vector/embeddings/LiteLLMEmbeddingEngine.py:255-301`):
- The tokenizer is a property of the embedding engine, chosen by provider:
  `"openai" in provider` → `TikTokenTokenizer`; `"gemini"` → TikToken; `"mistral"` →
  Mistral; else → HuggingFace, falling back to `TikTokenTokenizer(model=None)` =
  **cl100k_base** on any error.
- The default embedding engine is `openai/text-embedding-3-large` (`provider="openai"`),
  so the **out-of-box Python tokenizer is tiktoken cl100k_base**. There is **no
  whitespace counter** anywhere in Python; the universal fallback is cl100k_base.

### Fact 2 — `max_chunk_size` default + auto-calc (verified, both sides)

**Rust** (`crates/cognify/src/config.rs`):
- Field `pub max_chunk_size: usize` (line 33); `Default` sets it to **`1500`** (line 203;
  doc at lines 30-32 says "Python default: 1500").
- `auto_chunk_size` (lines 382-386):
  ```rust
  pub fn auto_chunk_size(embedding_engine: &dyn EmbeddingEngine, llm: &dyn Llm) -> usize {
      let llm_cutoff = (llm.max_context_length() / 2) as usize;   // ← WRONG: context window, not completion tokens
      let embed_max = embedding_engine.max_sequence_length();
      llm_cutoff.min(embed_max).max(1)
  }
  ```
  Doc (lines 373-381) **incorrectly** claims it "Matches Python's `get_max_chunk_tokens()`".
- Auto-calc fires only when the caller left `max_chunk_size` at the default 1500
  (sentinel-equality check in `crates/cognify/src/tasks.rs:2004-2017`):
  ```rust
  let effective_config = if config.max_chunk_size == CognifyConfig::default().max_chunk_size {
      config.clone().with_auto_chunk_size(embedding_engine.as_ref(), llm.as_ref())
  } else { config.clone() };
  ```
- Available Rust trait surface (relevant): `EmbeddingEngine::max_sequence_length() -> usize`
  (`crates/embedding/src/engine.rs:52`, ≈512 for BGE); `Llm::max_context_length() -> u32`
  (`crates/llm/src/llm_trait.rs:75`, default 4096; OpenAI adapter
  `crates/llm/src/adapters/openai.rs:641` returns per-model **context windows**, e.g.
  gpt-4 → 8192). **There is no `max_completion_tokens` method on either trait** — see the
  implementation note.

**Python** (`/tmp/cognee-python/cognee/api/v1/cognify/cognify.py:321-322`):
- `Task(extract_chunks_from_documents, max_chunk_size=chunk_size or get_max_chunk_tokens(), ...)`
  and `chunk_size` defaults to `None` (line 48), so Python **always auto-calculates** when
  the user passes nothing — there is no fixed `1500` at the cognify entry point.
- `get_max_chunk_tokens` (`/tmp/cognee-python/cognee/infrastructure/llm/utils.py:17-44`):
  ```python
  llm_cutoff_point = llm_client.max_completion_tokens // 2
  max_chunk_tokens = min(embedding_engine.max_completion_tokens, llm_cutoff_point)
  return max_chunk_tokens
  ```
  Quantities:
  - `embedding_engine.max_completion_tokens` default = **512** (`LiteLLMEmbeddingEngine.py:69`).
  - `llm_client.max_completion_tokens` = `min(model_max_completion_tokens, llm_max_completion_tokens)`
    where the config default `llm_max_completion_tokens = 16384` (`infrastructure/llm/config.py:51`);
    for a model unknown to litellm it is just `16384`.
  - So out-of-box: `llm_cutoff = 16384 // 2 = 8192`; `max_chunk_tokens = min(512, 8192) = 512`.

> **The audit's "≈8191" figure is imprecise.** `8191` is the tokenizer's *internal*
> default `max_completion_tokens`, but the engine overrides it with **512**, and
> `get_max_chunk_tokens` reads the *engine's* value (512). The true Python default
> `max_chunk_size` ≈ **512**, dominated by the embedding side. The two terms are also
> **completion-token** limits, not context windows.

### Net divergence

| | Rust (current) | Python (default) |
|---|---|---|
| Token counter (no env) | `Word` (whitespace) | tiktoken cl100k_base |
| Default `max_chunk_size` | fixed 1500 | auto ≈ 512 |
| Auto-calc LLM term | `max_context_length()/2` (context window ÷2 → e.g. 2048) | `llm_max_completion_tokens // 2` (8192) |
| Auto-calc embedding term | `max_sequence_length()` (≈512) | `max_completion_tokens` (512) |

Same input text → Rust whitespace-counts into ~1500-"word" chunks; Python BPE-counts into
~512-token chunks → wildly different chunk boundaries, counts, IDs, and LLM inputs.

## Prerequisites

```bash
git checkout -b task/14-chunking-parity
[ -d /tmp/cognee-python ] || git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python
```

Read first (re-grep line numbers):
- `crates/chunking/src/config.rs` (`TokenCounterKind`, `from_env`, `build`)
- `crates/chunking/Cargo.toml` (features); `crates/lib/Cargo.toml`, `crates/cli/Cargo.toml` (default feature lists)
- `crates/chunking/src/token_counter.rs:88-96` (`TikTokenCounter::cl100k_base`)
- `crates/cognify/src/config.rs:33,203,382-398` (default + auto-calc)
- `crates/cognify/src/tasks.rs:2004-2017` (auto-calc trigger)
- `crates/embedding/src/engine.rs:52`; `crates/llm/src/llm_trait.rs:75`; `crates/llm/src/adapters/openai.rs:641`
- Python: `cognify.py:48,321-322`; `utils.py:17-44`; `LiteLLMEmbeddingEngine.py:69,255-301`; `infrastructure/llm/config.py:51`

## Files to change

| Path | Change |
|---|---|
| `crates/chunking/src/config.rs` | `from_env()`: default to tiktoken for OpenAI-family (and when no provider is set, if the default embedding provider is openai-family); keep onnx/ollama behavior. |
| `crates/cognify/src/config.rs` | Change `Default::max_chunk_size` to a parity sentinel/auto default; fix `auto_chunk_size` formula + doc to use completion-token semantics. |
| `crates/cognify/src/tasks.rs` | (Only if the sentinel approach changes) keep the "auto when default" trigger correct. |

## Python reference (exact)

- Auto-calc: `/tmp/cognee-python/cognee/infrastructure/llm/utils.py:17-44`
  → `min(embedding_engine.max_completion_tokens, llm_client.max_completion_tokens // 2)`.
- Default invocation: `/tmp/cognee-python/cognee/api/v1/cognify/cognify.py:321-322`
  (`chunk_size or get_max_chunk_tokens()`, `chunk_size` default `None` at line 48).
- Embedding default `max_completion_tokens = 512`: `LiteLLMEmbeddingEngine.py:69`.
- LLM default `llm_max_completion_tokens = 16384`: `infrastructure/llm/config.py:51`.
- Tokenizer selection: `LiteLLMEmbeddingEngine.py:255-301` (openai → TikToken cl100k).

## Implementation steps

### Fix 1 — token counter default (B2.1)

The cleanest parity change is: when `EMBEDDING_PROVIDER` is **unset**, treat the default
as OpenAI-family (matching Python's default `openai` provider) and select `TikToken`,
instead of defaulting to `onnx` → `Word`.

1. In `crates/chunking/src/config.rs::from_env()`, change the provider default. Before:
   ```rust
   let provider = std::env::var("EMBEDDING_PROVIDER")
       .unwrap_or_else(|_| "onnx".to_string())
       .to_lowercase();
   ```
   After (Python parity — default provider is openai):
   ```rust
   // Python's default embedding provider is `openai`, whose default tokenizer is
   // tiktoken cl100k_base. Match that when EMBEDDING_PROVIDER is unset so an
   // out-of-box OpenAI-family setup counts BPE tokens, not whitespace.
   let provider = std::env::var("EMBEDDING_PROVIDER")
       .unwrap_or_else(|_| "openai".to_string())
       .to_lowercase();
   ```
   This routes the no-env case to the existing `"openai" | "openai_compatible" => TokenCounterKind::TikToken` arm.
2. **Verify the onnx path still works for the actual ONNX engine.** The ONNX engine sets
   `EMBEDDING_PROVIDER=onnx` (or the embedding config does) and provides
   `EMBEDDING_TOKENIZER_PATH`, so the `onnx`/`fastembed` arm still selects
   `HuggingFaceFile`. Confirm the embedding crate / `EmbeddingConfig::from_env()` exports
   `EMBEDDING_PROVIDER=onnx` when ONNX is configured (grep `crates/embedding/src/config.rs`
   for where it sets the provider env or how the chunking counter is selected in the
   pipeline). If the ONNX engine does **not** export `EMBEDDING_PROVIDER`, changing the
   default to `openai` would make a no-env ONNX user get tiktoken — acceptable for parity,
   but note it.
3. **Feature-off safety:** the `build()` fallback to `WordCounter` when `tiktoken` is
   compiled out already exists and is correct. Confirm the `eprintln!` there is converted
   to `tracing::warn!` (this is a separate cleanup, release-plan T5.1 — do not duplicate;
   just don't regress it). Leave the fallback in place: a `--no-default-features` build
   genuinely cannot run tiktoken.

### Fix 2 — `max_chunk_size` default + auto-calc (B2.2)

Python auto-calculates by default (no fixed 1500). Two viable Rust approaches; **prefer
(A)** (keeps the existing sentinel mechanism, minimal blast radius):

**(A) Keep the sentinel, but make the auto-calc correct and make the default mean "auto".**

4. Keep `Default::max_chunk_size` as a sentinel the pipeline interprets as "auto". The
   current sentinel is `1500`; that is a *legitimate user value*, which is the bug
   (a user who explicitly wants 1500 gets silently auto-overridden). Change the default to
   match Python's behavior of "no fixed value → auto":
   - Simplest: keep `1500` as the documented default **but** correct the auto-calc formula
     so that when it fires it yields the Python value (≈512). This preserves the existing
     `tasks.rs:2004-2017` trigger. Update the doc comment at `config.rs:30-32` to stop
     claiming 1500 is the effective default — note the pipeline auto-calculates.
   - Better (removes the "explicit 1500 collides with sentinel" ambiguity): document that
     `max_chunk_size` is auto-derived unless overridden via `with_chunk_size`, and have the
     pipeline auto-calc unconditionally when the config was constructed via `default()`.
     If that requires plumbing an `Option`, it is a larger change — keep it minimal and
     stay with the sentinel unless review asks otherwise.

5. **Fix `auto_chunk_size`** (`crates/cognify/src/config.rs:382-386`) to use
   completion-token semantics. The Rust LLM trait has **no `max_completion_tokens`** — it
   exposes `max_context_length()` (a context window). Python divides the *completion*
   budget (default 16384) by 2 → 8192, then `min`s with the embedding's 512, so the
   embedding term dominates and the LLM term is rarely binding. Mirror this faithfully:

   Before:
   ```rust
   pub fn auto_chunk_size(embedding_engine: &dyn EmbeddingEngine, llm: &dyn Llm) -> usize {
       let llm_cutoff = (llm.max_context_length() / 2) as usize;
       let embed_max = embedding_engine.max_sequence_length();
       llm_cutoff.min(embed_max).max(1)
   }
   ```
   After (Option 1 — minimal, no trait change; document the divergence honestly):
   ```rust
   /// Auto-calculate `max_chunk_size`, mirroring Python's `get_max_chunk_tokens()`:
   /// `min(embedding_engine.max_completion_tokens, llm_max_completion_tokens // 2)`.
   ///
   /// Python's LLM term is the *completion-token* budget (config default 16384 → 8192),
   /// not the context window. The Rust `Llm` trait does not expose a completion-token
   /// limit, so we use the Python default completion budget (16384) directly here; the
   /// embedding term (≈512 for BGE / `max_completion_tokens` for OpenAI) dominates in
   /// practice, matching Python's effective ≈512 default.
   pub fn auto_chunk_size(embedding_engine: &dyn EmbeddingEngine, _llm: &dyn Llm) -> usize {
       const PY_LLM_MAX_COMPLETION_TOKENS: usize = 16384; // infrastructure/llm/config.py:51
       let llm_cutoff = PY_LLM_MAX_COMPLETION_TOKENS / 2; // == 8192
       let embed_max = embedding_engine.max_sequence_length();
       llm_cutoff.min(embed_max).max(1)
   }
   ```
   > Rationale: `embedding_engine.max_sequence_length()` (≈512 for BGE; the OpenAI-
   > compatible engine returns its configured value) is the Rust analog of Python's
   > `embedding_engine.max_completion_tokens`, and it is the dominant term. Using the LLM
   > *context window* ÷ 2 (current code) is the actual bug — for gpt-4 (8192) that is 4096,
   > and `min(4096, 512) = 512` would *coincidentally* match, but for a 4096-context model
   > it is `min(2048, 512) = 512` too — so the embedding term saves it numerically, but the
   > formula and doc are still wrong and brittle. Prefer the explicit Python constant.

   (Option 2 — if review prefers a real LLM completion-token method: add
   `fn max_completion_tokens(&self) -> u32 { 16384 }` to the `Llm` trait with per-adapter
   overrides, and divide that. Larger surface; only do if asked.)

6. **Update the auto-calc trigger doc** in `tasks.rs:2004-2017` if you change the sentinel.
   If you keep `1500` as the sentinel, no code change is needed there — just ensure the
   `info!("Auto-calculated max_chunk_size: {}", …)` log still fires.

7. **Update the stale doc comment** at `config.rs:30-32` and `373-381` to describe the
   corrected semantics (completion-token budget, ≈512 effective default, embedding term
   dominant).

## Verification

```bash
# Compiles.
cargo check -p cognee-chunking -p cognee-cognify --all-targets

# Default token counter is tiktoken when no env is set (add this unit test to
# crates/chunking/src/config.rs #[cfg(test)]; clear the relevant env vars first).
cargo test -p cognee-chunking from_env

# Auto chunk size yields the Python-parity value with a BGE-like embedding (512).
cargo test -p cognee-cognify auto_chunk_size

# Full chunking/cognify suite + parity (LLM/embedding-gated tests).
bash scripts/run_tests_with_openai.sh   # or a named test, e.g. extract_chunks

# Full gate.
scripts/check_all.sh
```

Add unit tests:

```rust
// crates/chunking/src/config.rs  (#[cfg(test)] mod tests)
#[test]
fn from_env_defaults_to_tiktoken_for_openai_family() {
    // SAFETY: tests run single-threaded under the project test harness; clear env.
    unsafe {
        std::env::remove_var("EMBEDDING_PROVIDER");
        std::env::remove_var("COGNEE_TOKEN_COUNTER");
        std::env::remove_var("HUGGINGFACE_TOKENIZER");
        std::env::remove_var("EMBEDDING_TOKENIZER_PATH");
    }
    assert!(matches!(TokenCounterKind::from_env(), TokenCounterKind::TikToken));
}
```

```rust
// crates/cognify/src/config.rs  (#[cfg(test)] mod tests, using a mock embedding/LLM)
#[test]
fn auto_chunk_size_matches_python_default() {
    // Embedding with max_sequence_length() == 512 (BGE-like) and any LLM.
    let embed = /* MockEmbeddingEngine with 512 */;
    let llm = /* MockLlm */;
    assert_eq!(CognifyConfig::auto_chunk_size(&embed, &llm), 512);
}
```

> Re-grep `crates/test-utils` / `cognee-test-utils` for an existing mock embedding engine
> whose `max_sequence_length()` you can set to 512; reuse it rather than writing a new mock.

Expected: tiktoken selected with no env; `auto_chunk_size` returns 512 for a 512-token
embedding engine; existing chunking tests still pass (whitespace counter still selectable
via `COGNEE_TOKEN_COUNTER=word` / onnx-with-tokenizer-path).

> **Heads-up — existing `auto_chunk_size` tests in `config.rs` will break after Fix 2.**
> The current tests at lines 680–717 exercise the **wrong** formula (llm context-window ÷ 2).
> After applying Option 1 (hardcoded Python constant 16384), the LLM arg becomes unused and
> cases like `test_auto_chunk_size_llm_cutoff_is_smaller` (llm_ctx=256, result=128) will
> return 512 instead. Update those tests to reflect the corrected semantics: the LLM term
> is now always 8192 (Python default constant), so the embedding term dominates for any
> embedding model with `max_sequence_length() ≤ 8192`.

## Acceptance criteria

- [ ] With no `EMBEDDING_PROVIDER`/`COGNEE_TOKEN_COUNTER` set, `TokenCounterKind::from_env()` returns `TikToken` (OpenAI-family parity).
- [ ] The `onnx`/`fastembed` path (with `EMBEDDING_TOKENIZER_PATH`) and `ollama`/`word` overrides are unchanged.
- [ ] `auto_chunk_size` uses completion-token semantics and returns ≈512 for a 512-token embedding engine (Python parity); doc comments corrected (no false "matches Python" claim).
- [ ] `max_chunk_size` default/auto behavior matches Python's "auto when unspecified" (no silently-wrong fixed 1500 in the effective path); stale doc at `config.rs:30-32` fixed.
- [ ] New unit tests pass; existing chunking/cognify tests pass.
- [ ] `scripts/check_all.sh` passes.

## Gotchas / do-not

- **DETERMINISM / cross-SDK IDs (loud):** chunk boundaries determine `chunk_index`, which
  determines `uuid5(NAMESPACE_OID, "{document_id}-{chunk_index}")`. Changing the token
  counter and `max_chunk_size` **changes the number of chunks and their IDs**, and the
  text each chunk holds (the LLM's input). This is the **intended** parity correction —
  it makes Rust chunk like Python — but it is a breaking change for any store cognified
  with the old whitespace/1500 behavior. Call this out prominently in the PR. It also
  means cross-SDK structural-cognify CI tolerances (task 12) should *improve*, not
  regress; if they regress, the formula is still off.
- **Feature-flag implication:** `tiktoken` is behind a Cargo feature. `cognee-lib` and
  `cognee-cli` enable it by default, so default builds are fine. But a downstream consumer
  building `cognee-chunking` directly with `--no-default-features` (or a binding that does
  not forward `tiktoken`) will hit the `build()` whitespace fallback and **silently
  diverge again**. Ensure every shipping artifact (lib, cli, and the C/JS/Python bindings)
  forwards `tiktoken` in its default feature set — grep the bindings' `Cargo.toml`
  `[features] default` lists and add `tiktoken` if missing. Document the fallback clearly.
- **Do not remove the `build()` whitespace fallback.** It is the only correct behavior when
  tiktoken is genuinely compiled out; removing it would make `--no-default-features` builds
  fail to construct a counter.
- **`max_completion_tokens` ≠ context window.** Do not "fix" the auto-calc by dividing
  `max_context_length()` — that is the original bug. Mirror Python's completion-token
  budget (16384) or add a real completion-token trait method (only if review wants it).
- **The embedding term must use the right field.** Rust's `max_sequence_length()` (≈512
  for BGE) is the analog of Python's embedding `max_completion_tokens` (512). For the
  OpenAI-compatible engine, confirm `max_sequence_length()` returns the configured value,
  not a hardcoded 384/512 mismatch (cross-check task 19, embedding dimension resolution —
  separate, but adjacent).
- **Env-var mutation in tests is `unsafe` on edition 2024.** Wrap `set_var`/`remove_var`
  in `unsafe { … }` and rely on the harness's `--test-threads=1` (project default) for
  isolation; clear all four relevant vars before asserting `from_env()`.

## Rollback

```bash
git checkout main -- crates/chunking/src/config.rs crates/cognify/src/config.rs crates/cognify/src/tasks.rs
```
Reverts to whitespace-default + fixed-1500 behavior. No schema impact; only affects future
chunking output (and thus newly-generated chunk IDs).
