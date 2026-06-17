# T1 — Cassette format & hashing

**Status:** Not implemented
**Crate:** `cognee-llm` (new `mock` feature)
**Depends on:** nothing
**Unblocks:** T2, T3

---

## Rationale

Both new LLM behaviors — the recorder (T2) and the replay mock (T3) — need a
shared on-disk format and a shared rule for turning an LLM *call* into a stable
key. Defining these once, first, keeps record and replay symmetric: a value
written by the recorder must be found by the mock using the identical hash.

We deliberately key on content (`sha256(user input + canonical schema)`) instead
of Python's title-substring matching. Content addressing is unambiguous, requires
no per-corpus tuning, and matches the repo's existing UUID5 philosophy.

## Expected output

- A `mock` feature in `crates/llm/Cargo.toml` (no default), pulling in `sha2`.
- A new module `crates/llm/src/mock/mod.rs` + `crates/llm/src/mock/cassette.rs`
  exposing:
  - `struct LlmCassette { version: u32, model: String, entries: BTreeMap<String, CassetteEntry> }`
  - `struct CassetteEntry { method: CassetteMethod, user_input_preview: String, schema_name: Option<String>, response: serde_json::Value }`
  - `enum CassetteMethod { Generate, StructuredOutput, TranscribeImage }`
  - `fn input_hash(messages: &[Message], schema: Option<&Value>) -> String`
  - `fn vision_hash(image_bytes: &[u8], mime_type: &str) -> String`
  - `LlmCassette::load(path)` / `save(path)` (pretty JSON).
- Unit tests proving hash stability and serde round-trip.
- `cassette.json` is human-readable and hand-editable (so a recorded fixture can
  be tweaked like Python's `mock_memories.json`).

## Step-by-step implementation

1. **Feature + dependency.** In `crates/llm/Cargo.toml` add:
   ```toml
   [features]
   mock = ["dep:sha2"]
   ```
   and `sha2 = { workspace = true, optional = true }`. `sha2` is already a
   workspace dependency (`sha2 = "0.10"` in `[workspace.dependencies]`, consumed
   via `sha2.workspace = true` by `cognee-ingestion`, `cognee-telemetry`, and
   `cognee-http-server`), so no version addition is needed.

2. **Module skeleton.** Create `crates/llm/src/mock/mod.rs`:
   ```rust
   //! Record/replay LLM support (cassette-based mock). Feature: `mock`.
   mod cassette;
   pub use cassette::{CassetteEntry, CassetteMethod, LlmCassette, input_hash, vision_hash};
   ```
   Gate it in `crates/llm/src/lib.rs`: `#[cfg(feature = "mock")] pub mod mock;`.

3. **Serde types.** In `cassette.rs` define the structs/enum above with
   `#[derive(Debug, Clone, Serialize, Deserialize)]`. Use `BTreeMap` (not
   `HashMap`) for `entries` so the file is deterministically ordered and diffs
   cleanly. `CassetteMethod` → `#[serde(rename_all = "snake_case")]`.

4. **Hashing.** Implement `input_hash`:
   - Concatenate, in order, each message as `"{role}:{content}\n"` (role via its
     serde representation so it's stable).
   - If `schema` is `Some`, append the schema canonicalized: `serde_json` does not
     guarantee key order, so serialize through a `BTreeMap`-backed canonical form
     (write a small `canonicalize(&Value) -> String` that recursively sorts object
     keys), and append that string.
   - Feed the buffer to `Sha256`, return lowercase hex.
   `vision_hash` = `sha256(mime_type bytes + image bytes)`.

5. **Load/save.** `save` writes `serde_json::to_string_pretty`; `load` reads +
   parses. `LlmError` ([`crates/llm/src/error.rs`](../../../crates/llm/src/error.rs))
   has **no `Io` variant**, so map filesystem/load failures to
   `LlmError::ConfigError(String)` and JSON-parse failures to
   `LlmError::DeserializationError(String)` (both exist). Do not invent a new
   variant.

6. **Tests** (`#[cfg(test)] mod tests` in `cassette.rs`):
   - `input_hash` is identical for two calls with the same messages/schema, and
     differs when content differs.
   - `canonicalize` is order-independent: two `Value`s with the same keys in
     different insertion order hash equal.
   - `LlmCassette` round-trips through `save`→`load` (use `tempfile`).

## Acceptance / verification

- `cargo test -p cognee-llm --features mock` passes the new tests.
- `cargo check -p cognee-llm` (no `mock`) still compiles — the module is fully
  gated and adds nothing to the default build.
- `cargo clippy -p cognee-llm --features mock -- -D warnings` is clean.
