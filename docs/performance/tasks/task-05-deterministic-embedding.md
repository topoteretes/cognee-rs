# T5 — Deterministic mock embedding

**Status:** Implemented
**Crate:** `cognee-embedding`
**Depends on:** nothing (independent of T1–T4)
**Unblocks:** stable search results in T6 mock runs

---

## Rationale

Python's mock embedding derives each vector from `sha256(text)` so vectors are
deterministic and content-stable — similar text yields stable neighbors, and
search results don't drift between runs. Rust's current
[`MockEmbeddingEngine`](../../../crates/embedding/src/mock.rs) returns **zero
vectors**, which is fine for timing `add`/`cognify` but makes the `search` phase
degenerate (every vector identical). Porting the deterministic scheme gives the
mock benchmark a meaningful, repeatable search phase.

## Expected output

- `MockEmbeddingEngine` gains a deterministic mode producing SHA-256-derived
  vectors (zero-vector remains the default to preserve existing test behavior).
- Selectable via env: `MOCK_EMBEDDING=deterministic` (in addition to the existing
  truthy values which keep the zero-vector behavior).
- Same text → identical vector across calls and processes; dimensionality honored.

## Step-by-step implementation

1. **Mode field.** Add `enum MockVectorMode { Zero, Deterministic }` and a
   `mode: MockVectorMode` field to `MockEmbeddingEngine` (default `Zero`). Add a
   constructor/builder `deterministic(dimensions) -> Self` and/or
   `with_mode(...)`. Keep `new()`/`with_batch_size()` defaulting to `Zero` so all
   current tests/usages are unchanged.

2. **Port the Python vector scheme.** In `embed`, for `Deterministic`:
   ```text
   h = sha256(text)                       # 32 bytes
   for i in 0..dims:
       offset = (i * 4) % len(h)
       chunk  = h[offset .. offset+4]  (right-pad with 0x00 to 4 bytes)
       val    = f32::from_le_bytes(chunk)
       v[i]   = clamp(val / 1e38, -1.0, 1.0)
   ```
   This mirrors `bench_cognee.py`'s `_MockEmbeddingEngine.embed_text`
   (`struct.unpack("<f", …)`, divide by `1e38`, clamp). Guard against NaN/inf from
   `from_le_bytes` (map non-finite to `0.0`) so cosine math downstream stays sane.

3. **Config wiring.** In
   [`crates/embedding/src/config.rs:194`](../../../crates/embedding/src/config.rs#L194),
   extend the `MOCK_EMBEDDING` parsing: the literal value `deterministic` (and
   maybe `hash`) sets `provider = Mock` **and** records the deterministic mode;
   other truthy values keep the zero-vector mode. Thread the mode into
   `create_engine` where `EmbeddingProvider::Mock` constructs the engine
   (around line 337).

4. **Tests:**
   - Same input twice → identical vector.
   - Two different inputs → different vectors (with high probability).
   - All components are finite and within `[-1.0, 1.0]`; length == `dimensions`.
   - Zero mode still returns zeros (regression guard for existing behavior).
   - `MOCK_EMBEDDING=deterministic` selects `Mock` + deterministic mode (env test,
     following the existing `#[serial]`/env-var test pattern in `config.rs`).

## Acceptance / verification

- `cargo test -p cognee-embedding` passes, old zero-vector tests unchanged.
- A `search` against deterministic mock vectors returns stable, ordered results
  across repeated runs (sanity-checked in T6).
- `cargo clippy -p cognee-embedding -- -D warnings` clean.
