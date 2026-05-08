# Task 05-02 — Audit `Data.content_hash` propagation through Rust ingestion

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 05-03 — Provenance core](03-provenance-core.md) (`extract_content_hash_from_value` reads `Data.content_hash`).

**Parent doc**: [05 — DataPoint Provenance Stamping](../05-datapoint-provenance.md)
**Locked decisions**: #7 — content-hash propagation has its own audit task; fix any gap found, narrowly.

---

## 1. Goal

Confirm — and fix if necessary — that
[`cognee_models::Data::content_hash`](../../crates/models/src/data.rs#L22)
is **non-empty for every `Data` row** that the cognify pipeline can see,
across every ingestion entry point. Provenance task 05-03 will read this
field via `extract_content_hash_from_value` and propagate it onto every
DataPoint emitted downstream; if any path leaves `content_hash` empty,
the entire chain silently regresses.

This is an **audit task with a narrow fix budget**: if the audit confirms
populated, output a one-paragraph confirmation and stop. If the audit
finds a write-path gap, fix the specific path so the field is populated,
**without** redesigning the hashing strategy or the column type.

After this task we have a written assurance that:

1. Every `add()` entry point produces `Data` rows with non-empty
   `content_hash`.
2. Every code path that **clones** a `Data` row (or reconstructs one from
   the DB) preserves `content_hash`.
3. Cognify-pipeline tasks that re-emit `Data` (e.g. as `DataItem` inputs
   to the `extract_chunks_from_documents` task) carry the hash forward.

## 2. Rationale

- The proposed `extract_content_hash_from_value` helper in
  [`05-03-provenance-core.md`](03-provenance-core.md) walks the input
  `Arc<dyn Value>` and returns the first non-empty `Data.content_hash`
  it finds. If the field is empty (Rust `Data::content_hash` is `String`,
  not `Option<String>`), the helper has to treat empty as "no hash"
  semantically, which is fragile.
- Locking down ingestion behaviour first lets 05-03 safely assume "if I
  see a `Data`, its `content_hash` is the truth" rather than building in
  defensive empty-string checks at every read site.
- Locked decision 7 explicitly requires this audit run **before** the
  core machinery lands.

## 3. Pre-conditions

- Task [05-01](01-source-content-hash-field.md) is **not** required —
  this task is purely about `Data.content_hash`, not the new
  `DataPoint.source_content_hash`.
- Clean `cargo check --all-targets` on `main`.

## 4. Step-by-step

### 4.1 Map all `Data` construction sites

Run a targeted grep to find every place that constructs a `Data`:

```bash
grep -rn "Data::builder\|Data {" crates/ --include="*.rs" \
  | grep -v "/tests/" | grep -v "/test_utils/" | grep -v "tests.rs"
```

Expected production sites (flag any extras):

- [`crates/ingestion/src/pipeline.rs:417-425`](../../crates/ingestion/src/pipeline.rs#L417-L425) — the canonical construction point at the end of `process_data_input`.
- [`crates/database/src/ops/data.rs`](../../crates/database/src/ops/data.rs) — DB read path (verify `content_hash` is selected, not skipped, in `find_data_by_*` and `list_data` ops).
- Any place that reconstructs `Data` from a JSON or DB row.

For each site, record (in scratch, not in committed code):

| Site | Source of `content_hash` | Default if missing |
|---|---|---|
| `process_data_input` | `ContentHasher::hash_content(...)` | n/a — required |
| `crates/database/src/ops/data.rs::find_data_by_id` | DB column | should be the stored value |
| … | … | … |

If any production site sets `content_hash: String::new()` or omits it from
a SeaORM column projection, **that's the gap**.

### 4.2 Verify the SQLite/Postgres schema enforces non-null content_hash

Open the SeaORM migration file(s) for the `data` table:

```bash
grep -rn "content_hash" crates/database/src/ | head -20
```

Confirm that `content_hash` is declared `NOT NULL`. If it is `NULL`-able,
the audit must capture that and either:

1. Add a migration to enforce `NOT NULL` (only if there is unanimous
   agreement that legacy rows always have a value), **or**
2. Document that we permit empty `content_hash` and the new
   `extract_content_hash_from_value` helper must fall back to "no hash"
   in that case.

The fix path depends on what the schema says; do not change the schema
without first surfacing the question to the user.

### 4.3 Verify `Data` clones preserve the hash

The cognify pipeline passes `Vec<Data>` into `CognifyInput`, which is
cloned at chunk-extraction time. Confirm that `Data: Clone` is a
field-for-field clone (no manual `Clone` impl that drops `content_hash`):

```bash
grep -B 2 -A 10 "impl Clone for Data" crates/models/src/data.rs
grep -B 1 "#\[derive.*Clone.*\]" crates/models/src/data.rs
```

If `Data` derives `Clone`, the clone is automatic and there is nothing
to fix. If a manual impl exists, audit each cloned field.

### 4.4 Verify `DataInput::DataItem` preserves the hash

[`DataInput::DataItem { data, .. }`](../../crates/models/src/data_input.rs)
is the cognify pipeline's way of feeding pre-existing `Data` rows back
through the chunk-extraction path. Confirm the variant carries the full
`Data`, not a stripped subset:

```bash
grep -B 1 -A 8 "DataItem" crates/models/src/data_input.rs
```

If the variant is `DataItem { content: String, mime_type: String, … }`
(a field-by-field copy), audit each construction site to confirm
`content_hash` is propagated. If it is `DataItem { data: Data, … }`, the
audit is a one-liner.

### 4.5 Verify `process_by_chunks` and the DB-read paths

Search for places that re-emit `Data` from a stream:

```bash
grep -rn "Data {" crates/ingestion/src/ crates/cognify/src/ crates/lib/src/ \
  --include="*.rs" | grep -v "/tests/"
```

For each, confirm `content_hash` is set to a real value (either
`hash_content(...)` or carried from upstream).

### 4.6 If a gap is found: fix narrowly

For any write site that omits or zeroes out `content_hash`:

1. Identify whether the upstream `Data` already has a hash (carry it
   forward) or whether the content is available to hash (compute one
   on the fly via `ContentHasher::hash_content`).
2. Make the **minimal** edit that populates the field. Do not rename
   columns, change algorithms, or introduce a feature flag.
3. Add a one-line regression test in
   `crates/ingestion/tests/` (or the appropriate sibling crate) that
   asserts `Data.content_hash` is non-empty after that path executes.

### 4.7 Document the audit outcome

Append a short paragraph to this file's "Status" header (after sub-agent
D commits) summarising what was checked and any fix applied. Example
forms:

- `**Status**: ✅ implemented in commit <SHA> — audit confirmed all 3 production write sites populate content_hash; no fix required.`
- `**Status**: ✅ implemented in commit <SHA> — audit found that `crates/foo/src/bar.rs:234` constructed Data with `content_hash: String::new()` for the URL-crawler stub path; fixed to call `ContentHasher::hash_content` on the fetched body.`

This is the contract that 05-03 reads to decide whether
`extract_content_hash_from_value` can return early on empty strings.

## 5. Verification

```bash
# 1. Compile (only if a fix landed; otherwise skip).
cargo check --all-targets

# 2. Existing ingestion tests still pass.
cargo test -p cognee-ingestion

# 3. Any new regression test from §4.6 passes.
# (specific test name depends on the fix path)

# 4. Clippy.
cargo clippy --all-targets -- -D warnings

# 5. Full check (only if a fix landed).
scripts/check_all.sh
```

If the audit was a no-op (no fix), §1, §4, and §5 are skipped — only the
status note is committed.

## 6. Files modified

Worst case (if a fix is required):

- One or two source files in
  [`crates/ingestion/src/`](../../crates/ingestion/src/) or
  [`crates/database/src/ops/`](../../crates/database/src/ops/) —
  whichever path is found to drop `content_hash`.
- One regression test under the corresponding `tests/` directory.

Best case (audit passes):

- This file (`docs/telemetry/05/02-data-content-hash-audit.md`) — the
  status note.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Audit identifies a much larger gap (e.g. URL crawler never hashes) | Low — the URL crawler's hashing flow already shows up in `pipeline.rs:252-253`. | Surface to the user; do not over-scope this task. Open a follow-up gap if the fix exceeds two files. |
| Schema gap (e.g. `content_hash` is nullable in SQLite but non-null in Postgres) | Low | Document the divergence in the status note and decide with the user whether to align both. |
| Audit-only commit produces no diff | Expected if everything is fine. | The status-note edit on this doc is the diff. |

## 8. Out of scope

- Changing the hash algorithm (MD5 vs SHA256) or the column type
  (`String` vs `Option<String>`).
- Backfilling `content_hash` on legacy rows in production databases.
- Adding new hash invariants (e.g. uniqueness constraints across
  tenants).
