# 17 — Run loaders at ADD + correct `raw_content_hash`

> Wave 4 · P1 should-fix · Track A · Release-blocking: no · Effort: 1d ·
> Depends on: — (interacts with [14 chunking parity](14-chunking-parity.md)) ·
> Source: audit [B1.1](../cleanup-and-parity-audit.md#b1-add--ingestion),
> [B1.2](../cleanup-and-parity-audit.md#b1-add--ingestion)

Back to the [master index](00-INDEX.md).

## Goal

Make Rust `add()` run the document loader **at ingest time** and store the
**extracted text** (Python's `text_<hash>.txt`), exactly like Python — instead of
streaming the raw original bytes — and compute `raw_content_hash` as the MD5 of
that extracted-text file (not a copy of `content_hash`). For plain-text inputs the
two hashes already coincide and the stored bytes are identical, so the text path
must not regress; the change is observable only for non-plain-text inputs (PDF,
CSV, HTML, image, audio).

## Background & why

### Python: loader runs at ingest, stores extracted text — audit B1.1

`ingest_data.py:103` calls `data_item_to_text_file(actual_file_path,
preferred_loaders)`, which (`data_item_to_text_file.py:36-79`) invokes the loader
engine's `load_file`. For text, `TextLoader.load`
(`infrastructure/loaders/core/text_loader.py:53-90`, storage section 73-90):

1. computes the **original** file's `content_hash`,
2. writes the **extracted content** to `text_<content_hash>.txt` in the data root,
3. returns that stored file's full path.

`ingest_data.py` then:
- classifies the **original** file → `content_hash` (B1: MD5 of raw original bytes)
  and `data_id` (`identify(...)`),
- classifies the **stored extracted-text** file →
  `storage_file_metadata["content_hash"]`, stored as `raw_content_hash`
  (`ingest_data.py:194-195`),
- stores `raw_data_location = cognee_storage_file_path` (the extracted-text file),
  `extension`/`mime_type` from the **stored** file (`.txt`/`text/plain`),
  `original_extension`/`original_mime_type` from the **original**.

So for a PDF, the stored artifact is a `.txt` of the extracted text, `extension`
is `txt`, and `raw_content_hash != content_hash`.

### Rust: streams raw bytes, runs loaders only at cognify — audit B1.1/B1.2

`process_input` (`crates/ingestion/src/pipeline.rs:163-229`) streams the **raw
bytes** straight to storage and hashes them. The `LoaderRegistry`
(`crates/ingestion/src/loaders/mod.rs`) is **not used by ADD at all** — cognify
runs loaders later by reading `raw_data_location` and calling `.extract(...)`
(`crates/cognify/src/tasks.rs:237-244`). Consequences:

- For non-text inputs the stored artifact is raw bytes (e.g. PDF binary), the
  `extension`/`mime_type` describe the raw file, and `raw_data_location` is a
  `.pdf`, not `text_<hash>.txt`. Cross-SDK file/metadata reads diverge.
- `raw_content_hash` is hardcoded equal to `content_hash`
  (`pipeline.rs:357`: `.raw_content_hash(processed.content_hash.clone())`), an
  admitted shortcut (`crates/models/src/data.rs:37-38`: "same as content_hash
  when using MD5 mode"). Python's `raw_content_hash` is the hash of the
  **extracted text**, which differs from `content_hash` for any input the loader
  transforms.
- **`ProcessedInput` has no `raw_content_hash` field** — the struct currently
  carries only `content_hash: String`. Step 1 must add it.

Text-only tests mask both gaps because for plain text the extracted text equals
the raw bytes.

> **SACRED — do not change (cross-SDK byte-compat, already matches Python):**
> - `content_hash` = MD5 of the **raw original bytes** (no owner_id) — `pipeline.rs:182-183`.
> - `data_id` = `uuid5` derived from `content_hash + owner_id (+tenant)` — `pipeline.rs:188`.
> - `dataset_id` derivation, `text_<md5>.txt` naming convention, `file://` URIs,
>   the 22-column `Data` schema (including the existing `raw_content_hash` column
>   which is already in `m20260914_000001_baseline.rs:82` — no migration needed,
>   only correct population).
>
> This task changes **only** `raw_content_hash`, the **stored-file content**
> (extracted text instead of raw bytes), and the stored-file
> `extension`/`mime_type`/`raw_data_location`/`loader_engine`. It must NOT alter
> `content_hash` or `data_id`, which are still computed from the raw original bytes.

### Interaction with task 14

Once ADD stores extracted text, cognify will read **already-extracted text** from
`raw_data_location` and should **not** re-run the loader (or it would double-load
a `.txt`). Task [14](14-chunking-parity.md) changes chunking; this task changes
what bytes chunking sees. Coordinate: after this task, the cognify chunk source is
the stored extracted text, so chunk boundaries/IDs depend on the extracted text —
which for cross-SDK parity must match Python's extracted text. Re-verify cognify's
read path (`tasks.rs:180-307`, loader dispatch at `tasks.rs:237-244`) after landing this.

**Note on double-loading:** `classify_documents` in `crates/models/src/document.rs`
derives `document_type` from `data.extension` (the **stored** extension). After
this task sets `extension = "txt"`, all stored documents classify as `"text"` and
will pass through `TextLoader` at cognify time — which is correct and idempotent
(text loader returns the content verbatim). No special cognify changes are needed
to prevent non-text loaders from running on already-extracted text, **as long as**
`data.extension` (stored) is set to `"txt"` correctly by this task (not the
original extension).

### D3 / `DataInput::S3Path` interaction

Per decision D3 (feature-gate `S3Path` + rustdoc note), `DataInput::S3Path` is a
stub that errors at runtime. This task must not change that behavior: if the
loader-dispatch path in `process_input` encounters `S3Path`, it must continue to
surface an error (either `IngestionError` or `UnsupportedDocumentType`). Do not
silently fall back to storing raw bytes for S3 inputs.

## Prerequisites

```bash
git checkout -b task/17-loaders-at-add
```

Read first (both sides):

| Side | File | What to look at |
|---|---|---|
| Rust | `crates/ingestion/src/pipeline.rs` | `process_input` (103-229), `ProcessedInput` struct (70-92), `raw_content_hash` set in `persist_data_with_acl` (357) |
| Rust | `crates/ingestion/src/loaders/mod.rs` | `DocumentLoader` trait, `LoaderOutput`, `LoaderRegistry::default_registry`, `get()` method (131) |
| Rust | `crates/ingestion/src/loaders/text.rs` | `TextLoader` |
| Rust | `crates/ingestion/src/loader_registry.rs` | `get_loader_name(ext)` — extension → loader engine name mapping used by ADD |
| Rust | `crates/cognify/src/tasks.rs` | cognify loader dispatch path (237-244); `classify_documents` input (157) |
| Rust | `crates/models/src/document.rs` | `classify_documents` uses `data.extension` (stored, line 112) for `document_type` |
| Rust | `crates/models/src/data.rs` | `raw_content_hash` doc (37-38) |
| Rust | `crates/database/src/migrator/m20260914_000001_baseline.rs` | `raw_content_hash` column already in schema (line 82) — no migration needed |
| Python | `cognee/tasks/ingestion/ingest_data.py` | loader call (103-106), original vs stored metadata (113-123), Data fields (183-204), `raw_content_hash` (195) |
| Python | `cognee/tasks/ingestion/data_item_to_text_file.py` | `data_item_to_text_file` (36-79) |
| Python | `cognee/infrastructure/loaders/core/text_loader.py` | `load` method (53-90); key storage section (73-90) writes `text_<orig_content_hash>.txt` (line 76) |

## Files to change

| Path | Change |
|---|---|
| `crates/ingestion/src/pipeline.rs` | In `process_input`: after computing the original `content_hash` from raw bytes, run the loader on the raw bytes, store the **extracted text** as `text_<content_hash>.txt`, set stored `extension=txt`/`mime_type=text/plain`/`raw_data_location` to that file, keep `original_*` from the source. Compute `raw_content_hash` = MD5 of the extracted-text bytes. |
| `crates/ingestion/src/pipeline.rs` | In `persist_data_with_acl`: stop forcing `raw_content_hash = content_hash`; use the value carried on `ProcessedInput`. |
| `crates/ingestion/src/pipeline.rs` (`ProcessedInput`) | Add a `raw_content_hash: String` field. |
| `crates/cognify/src/tasks.rs` | Ensure cognify treats `raw_data_location` as already-extracted text (text loader on a `.txt`), so non-text loaders aren't re-applied to extracted text. |
| `crates/ingestion` (loaders) | The ingest pipeline must depend on `LoaderRegistry`; thread a registry into `process_input` (or build the default registry inside, behind the same features). |

## Python reference

| Behavior | Python file:line |
|---|---|
| Run loader at ingest, store text | `cognee/tasks/ingestion/ingest_data.py:103-106` |
| `data_item_to_text_file` dispatches to loader | `cognee/tasks/ingestion/data_item_to_text_file.py:57-75` |
| Text loader writes `text_<content_hash>.txt`, returns path | `cognee/infrastructure/loaders/core/text_loader.py:76` (naming), 88 (store) |
| `content_hash` from **original** file (`original_file_metadata["content_hash"]`) | `ingest_data.py:118 (get_metadata call), 194 (new Data)` |
| `raw_content_hash` from **stored extracted-text** file (`storage_file_metadata["content_hash"]`) | `ingest_data.py:163 (update path), 195 (new Data path)` |
| stored `extension`/`mime_type` from stored file; `original_*` from original | `ingest_data.py:156-159 (update), 188-191 (new)` |
| `raw_data_location = cognee_storage_file_path` (extracted text) | `ingest_data.py:154 (update), 186 (new)` |
| `loader_engine = loader_engine.loader_name` | `ingest_data.py:160 (update), 192 (new)` |

## Implementation steps

1. **Add `raw_content_hash` to `ProcessedInput`** (`pipeline.rs`). It already holds
   `content_hash`. Add:

   ```rust
   /// MD5 of the EXTRACTED-text file (Python parity, ingest_data.py:195).
   /// Equals `content_hash` only when extracted text == raw bytes (plain text).
   pub raw_content_hash: String,
   ```

2. **Make the ingest pipeline loader-aware.** `process_input` must have access to a
   `LoaderRegistry`. Options (pick the lowest-churn that compiles):
   - Add a `loaders: &LoaderRegistry` parameter to `process_input` and thread it
     from `AddPipeline` callers; or
   - Build `LoaderRegistry::default_registry()` inside `process_input` (matches
     Python's implicit `get_loader_engine()`), gated by the same feature flags that
     gate the loaders.

   Confirm the loader dispatch key: Rust loaders are keyed by `document_type`
   (see `LoaderRegistry::get()` in `mod.rs:131`), while ingest has only an
   extension/mime. The extension → document type mapping already exists in
   `crates/models/src/document.rs` (`extension_to_doc_type`, line 36), which is
   the same mapping `classify_documents` uses. Use it or duplicate the same logic
   in `process_input`. Alternatively, `LoaderRegistry::get_for_extension(ext)`
   helper can be added to keep dispatch centralized.

3. **Run the loader and store extracted text** in `process_input`. Today the code
   streams raw bytes to a writer and hashes them (`pipeline.rs:152-229`). Restructure
   so that:

   - Raw bytes are still **buffered and hashed** to produce `content_hash` and
     `data_id` (UNCHANGED — `pipeline.rs:179-188`).
   - The loader runs on the buffered raw bytes + a `Document`-like descriptor to
     produce extracted text (`LoaderOutput::Text` / `Rows` / `SingleChunk`).
     Normalize to a single `String` the same way cognify already does (see
     `tasks.rs:247-268` for the `match output { ... }` block). For `Rows`, join
     with `"\n\n"` (matches `LoaderOutput::Rows` doc in `mod.rs:86-90`).
   - The **extracted text** is written to storage as `text_<content_hash>.txt`
     (the Python name uses the ORIGINAL file's content hash — see
     `text_loader.py:76`). Use the already-computed raw `content_hash`.
   - Set `stored_extension = "txt"`, `stored_mime_type = "text/plain"`,
     `raw_data_uri` / `raw_data_location` = the stored extracted-text file,
     `loader_engine` = the chosen loader's `engine_name()`. Keep
     `original_extension`/`original_mime_type`/`original_location` from the source.

   Before (current streaming-of-raw-bytes, `pipeline.rs:163-193`):

   ```rust
   effective_input.process_by_chunks(move |chunk| { /* writes raw bytes to writer + buffer */ }).await?;
   let collected = /* raw bytes */;
   let content_hash = ContentHasher::hash_content(&collected, hash_algorithm);
   let data_id = generate_data_id(&content_hash, owner_id, tenant_id);
   let storage_location = writer.finish().await?; // raw bytes file
   ```

   After (sketch — buffer raw, hash raw, then load + store text):

   ```rust
   // Buffer raw bytes (do NOT write them as the stored artifact).
   let collected: Vec<u8> = /* collect via process_by_chunks into a Vec */;

   // content_hash + data_id from RAW bytes — UNCHANGED (sacred).
   let content_hash = ContentHasher::hash_content(&collected, hash_algorithm);
   let data_size = collected.len() as i64;
   let data_id = generate_data_id(&content_hash, owner_id, tenant_id);

   // Run the loader at ingest (Python parity, ingest_data.py:103).
   let loader = registry.for_extension(&original_extension, &original_mime_type);
   let extracted_text: String = match loader.extract(&collected, &descriptor).await? {
       LoaderOutput::Text(t) => t,
       LoaderOutput::Rows(rows) => rows.join("\n\n"),
       LoaderOutput::SingleChunk { text, .. } => text,
   };
   let extracted_bytes = extracted_text.into_bytes();

   // Store EXTRACTED text as text_<content_hash>.txt (Python text_loader.py:76).
   let stored_name = format!("text_{content_hash}.txt");
   let storage_location = storage.store(&extracted_bytes, &stored_name).await?;

   // raw_content_hash = MD5 of the extracted-text file (ingest_data.py:195).
   let raw_content_hash = ContentHasher::hash_content(&extracted_bytes, hash_algorithm);

   // Stored-file metadata is now text/plain/.txt; originals keep their values.
   let stored_extension = "txt".to_string();
   let stored_mime_type = "text/plain".to_string();
   let loader_engine = loader.engine_name().to_string();
   ```

   Populate `ProcessedInput { content_hash, raw_content_hash, data_size, ... }`.

   > Keep streaming where it still pays off: you may stream raw bytes through the
   > hasher without holding the *stored* writer open, but the loader needs the full
   > content, so buffering the raw input is acceptable (Python buffers too).

4. **Stop forcing `raw_content_hash = content_hash`** in `persist_data_with_acl`
   (`pipeline.rs:357`). Before:

   ```rust
   .raw_content_hash(processed.content_hash.clone())
   ```

   After:

   ```rust
   .raw_content_hash(processed.raw_content_hash.clone())
   ```

5. **Preserve the plain-text path.** For `DataInput::Text` and `.txt`/`.md` inputs,
   the `TextLoader` returns the content verbatim, so `extracted_bytes == raw bytes`,
   `raw_content_hash == content_hash`, and the stored file is byte-identical to
   today. Add an explicit test asserting this (see Verification) so the text path
   provably does not regress.

6. **Adjust cognify to not double-load.** After ADD stores extracted text,
   `raw_data_location` points at a `text_<hash>.txt`. Cognify calls
   `classify_documents` (`tasks.rs:157-158`) which uses `data.extension` (the
   **stored** extension, `document.rs:112`) to determine `document_type`. Once
   this task sets the stored `extension = "txt"`, `classify_documents` will always
   produce `document_type = "text"`, and the text loader will re-read the
   already-extracted text — correct and idempotent. **Confirm** by verifying the
   test at `document.rs` line ~260 which already asserts `extension="txt"` → `document_type="text"`.
   No cognify code changes should be required as long as this task correctly sets
   `data.extension` (stored) to `"txt"` for all non-text inputs.

7. **Update the `raw_content_hash` doc** in `crates/models/src/data.rs:37` to drop
   the "same as content_hash when using MD5 mode" claim and describe the
   extracted-text semantics. The field is at line 38 (`pub raw_content_hash: Option<String>`);
   the doc comment is line 37.

## Verification

```bash
cargo check -p cognee-ingestion -p cognee-cognify --all-targets
cargo test -p cognee-ingestion --features testing
# Loader-dependent paths (enable the relevant loader features):
cargo test -p cognee-ingestion --features "testing,csv-loader,html-loader"
scripts/check_all.sh
# End-to-end add+cognify on a non-text file (needs OpenAI + embed model):
bash scripts/run_tests_with_openai.sh add
```

### Tests to add

- **Text path no-regression** (`crates/ingestion`): `add` a plain-text input;
  assert the stored file content equals the input bytes, `raw_content_hash ==
  content_hash`, `extension == "txt"`, and `data_id`/`content_hash` are unchanged
  vs the pre-change values (hardcode the known UUID5/MD5 for a fixed string —
  reuse the existing Python-compat ID test fixtures).
- **CSV / HTML path** (feature-gated): `add` a small CSV/HTML file; assert the
  stored file is the **extracted text** (e.g. CSV rows formatted as Python does,
  HTML stripped of tags), `extension == "txt"`, `raw_data_location` ends in
  `text_<hash>.txt`, `original_extension == "csv"`/`"html"`, and
  `raw_content_hash != content_hash`.
- **Cross-SDK (if parity CI from task 12 is available):** extend
  `e2e-cross-sdk/test_add_parity.py` to add a non-text fixture and assert
  `content_hash`, `data_id`, stored extracted text, and `raw_content_hash` match
  Python.

### Expected outcomes

- Plain text: identical stored bytes + hashes as before (no regression).
- PDF/CSV/HTML: stored artifact is extracted `.txt`; `raw_content_hash` differs
  from `content_hash`; `original_*` retain the source type.
- `content_hash` and `data_id` byte-identical to Python for the same input.

## Acceptance criteria

- [ ] ADD runs the loader at ingest time (LoaderRegistry used by the ingest pipeline).
- [ ] Stored artifact is the extracted text named `text_<content_hash>.txt`.
- [ ] `raw_content_hash` = MD5 of the extracted-text file (not a copy of `content_hash`).
- [ ] Stored `extension`/`mime_type` = `txt`/`text/plain`; `original_*` keep the source values; `loader_engine` set from the loader.
- [ ] `content_hash` and `data_id` (raw-bytes-derived) UNCHANGED; text path byte-identical to before.
- [ ] Cognify reads the stored extracted text and does not double-load non-text loaders.
- [ ] `crates/models/src/data.rs:37` doc updated.
- [ ] New tests cover text no-regression + at least one non-text loader path.
- [ ] `scripts/check_all.sh` passes.

## Gotchas / do-not

- **DO NOT change `content_hash` or `data_id`.** They are MD5 / UUID5 over the
  **raw original bytes** and already match Python. Keep hashing the buffered raw
  input *before* loading. Only `raw_content_hash` (extracted text) and the stored
  artifact change.
- **Stored file name uses the ORIGINAL content hash**, not the extracted-text hash
  (`text_loader.py:76` hashes the original then names the file with it). Use the
  already-computed raw `content_hash` for the `text_<...>.txt` name.
- **Plain text must not regress.** Extracted text == raw bytes for text, so all
  hashes and stored bytes stay identical. Pin this with a test using a known fixed
  string and its known MD5/UUID5.
- **Cross-SDK extracted-text parity:** the extracted text is now an input to
  chunking → chunk IDs → embeddings. A Rust PDF/HTML/CSV extraction that differs
  from Python's produces different chunk IDs and vectors. Where the Rust loader's
  output can't byte-match Python (e.g. different PDF text extractor), call it out
  as a residual parity gap rather than assuming equality.
- **Interaction with task 14:** chunking now reads extracted text. Land/retest the
  two together; don't assume cognify's old raw-byte read path still applies.
- **Feature gating:** non-text loaders are feature-gated (`pdf-*`, `csv-loader`,
  `html-loader`, `image-loader`, `audio-loader`, `unstructured`). With a loader's
  feature off, ADD must fall back gracefully (today cognify errors
  `UnsupportedDocumentType`); preserve equivalent behavior at ADD — do not store
  raw bytes silently for an unsupported type without a clear error path.
- **D3 / `DataInput::S3Path`:** Per decision D3 (`docs/plans/release/01-decisions.md`),
  `S3Path` is a stub that must continue to error at ingest time. The new loader-dispatch
  path must not accidentally store raw S3 bytes silently — ensure the S3 stub arm
  surfaces a clear `IngestionError` (as it does today via the placeholder metadata path).
- **`raw_content_hash` column already exists** in the baseline schema
  (`m20260914_000001_baseline.rs:82`) — do not add a new migration. This task
  is about correct population only.
- Avoid `unwrap()` in the new ingest code — propagate via `?`/`map_err` per project rules.

## Rollback

Revert the branch. The change alters stored-artifact content and `raw_content_hash`
for non-text inputs, so stores written with the new code are NOT mixed-compatible
with old Rust stores for those inputs — re-`add` affected non-text data after
reverting if needed. `content_hash`/`data_id` are unchanged, so relational rows and
dedup keys remain stable across the revert.
