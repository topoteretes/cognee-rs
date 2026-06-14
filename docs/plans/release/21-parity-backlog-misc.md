# 21 — Parity backlog (config / datasets / cloud / viz / recall)

> Wave 4 · Priority P2 (nice-to-have) · Track A · Release-blocking: no · Effort: 2d ·
> Depends on: — · Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md)
> B4.3, B5.2, B5.3, B6.5, B6.6, B7.3, B7.4, B7.5, B7.6, B8.2, B8.3, B8.4 · [index](00-INDEX.md)

## Goal

A **consolidated backlog** of twelve smaller Python-parity gaps that don't each warrant a
standalone task doc but together raise the parity percentage. Each sub-section is
self-contained: Python ref, Rust location, the gap, recommended fix, severity, and
acceptance criteria. Pick items independently; **one item = one small PR** unless trivially
grouped (the config items B7.5/B7.6 naturally group).

## How to use this doc

- Items are ordered by the priority table below, not by audit ID.
- Each item is verified against current code (re-grep before editing — line numbers drift).
- These are deliberately lighter than the P0/P1 task docs: enough to implement, not
  exhaustive step-by-step. If an item grows, promote it to its own numbered doc.
- Branch per item: `git checkout -b task/21-<item-slug>`.

## Prioritized mini-table

| # | Item | Audit | Sev | Effort | Cross-SDK risk |
|---|------|-------|-----|--------|----------------|
| 1 | Permission enforcement always-on (`has_data` auth) | B7.3 | Med | 0.25d | info-leak |
| 2 | `update()` takes explicit dataset_id + plumb fields + auth | B6.5 | Med | 0.25d | none |
| 3 | Custom-graph-model delete fallback (missing Data row) | B6.6 | Low | 0.25d | none |
| 4 | `get_status` multi-pipeline + `list_data` ordering | B7.4 | Med | 0.25d | determinism |
| 5 | `recall()` advanced params plumbed | B4.3 | Med | 0.25d | none |
| 6 | Config setter allowlists + missing keys + introspection | B7.5 | Med | 0.5d | none |
| 7 | Default-string mismatches (3 values) | B7.6 | Low | 0.1d | telemetry/settings output |
| 8 | Stored QA `context` payload (`""`/summary) | B5.2 | Med | 0.25d | persisted-entry parity |
| 9 | `CloudClient` proxy add/cognify/search/remember_entry | B8.3 | Med | 0.5d | none |
| 10 | `sync` stub — document or implement | B8.2 | Med | 0.1d (doc) | none |
| 11 | FS session store on-disk format (diskcache SQLite) | B5.3 | Med | 1d+ | session file interop |
| 12 | Visualization multi-view + name fallback | B8.4 | Med | 1d+ | none (HTML output) |

> Quick wins first (items 1–5, 7, 10) are all ≤0.25d. Items 11 and 12 are the heavy ones —
> defer to post-release if time-boxed.

---

## Item 1 — Permission enforcement always-on (B7.3)

- **Rust:** `crates/lib/src/api/datasets.rs`. `acl_db: Option<Arc<dyn AclDb>>` (~line 29);
  `list_datasets` skips the ACL check entirely when `acl_db` is `None` (~lines 52–64);
  `has_data` does **no** auth check at all (~lines 80–83): `count_dataset_data` then
  `Ok(count > 0)`.
- **Python:** always enforces the 4-permission model (read/write/delete/share) on every
  dataset op — there is no "ACL off" path.
- **Gap:** when `acl_db` is unset, reads/deletes silently pass (owner-scoped only) and
  `has_data` leaks existence regardless of caller. Info-leak + weaker-than-Python default.
- **Recommended fix:** add a `check_read_permission` call to `has_data` (the method already
  exists and is used by `list_data`). Decide the unset-`acl_db` policy as a project default
  (coordinate with task 10): either (a) make ACL mandatory in the facade constructor used by
  the HTTP server, or (b) keep `Option` but document that owner-scoping is the security
  boundary when unset and ensure `has_data` is at least owner-scoped. Prefer (a) for HTTP.
- **Severity:** Med.
- **Acceptance:** `has_data` returns an auth error for a non-owner without the `read`
  permission; a test covers unset-ACL behavior matching the chosen policy; no method silently
  bypasses ACL when `acl_db` is `Some`.

## Item 2 — `update()` explicit dataset_id + dropped fields + auth (B6.5)

- **Rust:** `crates/lib/src/api/update.rs`. Signature takes `dataset_name: &str` and
  **re-derives** the id at ~line 91: `let dataset_id =
  cognee_ingestion::generate_dataset_id(dataset_name, owner_id, tenant_id);`. Does not accept
  `node_set` / `preferred_loaders` / `incremental_loading`, and has no auth gate.
- **Python:** `/tmp/cognee-python/cognee/api/v1/update/update.py:12–22` — takes explicit
  `dataset_id: UUID` and accepts `node_set`, `preferred_loaders`, `incremental_loading`,
  forwarding them to `add()`.
- **Gap:** name-derivation breaks updating a dataset whose row exists under a different
  derivation (e.g. created with a different tenant), and three power-user knobs are silently
  dropped; no permission check on the target dataset.
- **Recommended fix:** add an overload / change the signature to accept
  `dataset_id: Uuid` directly (keep a `_by_name` convenience if needed for the CLI). Plumb
  `node_set`, `preferred_loaders`, `incremental_loading` through to the underlying `add`
  call. Add a `write` permission check when `acl_db` is present.
- **Severity:** Med.
- **Acceptance:** `update()` accepts an explicit `dataset_id`; the three fields reach the add
  path; an unauthorized caller is rejected; existing CLI/HTTP callers updated.

## Item 3 — Custom-graph-model delete fallback (B6.6)

- **Rust:** `crates/delete/src/lib.rs` ~lines 1326–1331 — fetches the `Data` row and errors
  if absent: `data.ok_or_else(|| DeleteError::Validation(format!("Data {data_id} was not
  found")))?`.
- **Python:** `/tmp/cognee-python/cognee/api/v1/datasets/datasets.py:165–176` — when the
  Data row is absent ("user is using a custom graph model"), it still calls
  `delete_data_nodes_and_edges(dataset_id, data_id, user.id)` and optionally deletes the
  empty dataset, then returns success.
- **Gap:** Rust cannot clean graph/vector for data that was written without a relational row
  (custom graph models), forcing an error instead of best-effort cleanup.
- **Recommended fix:** when the `Data` row is missing, fall back to deleting graph
  nodes/edges + vector points for `(dataset_id, data_id)` directly (the cascade already has
  these primitives), then optionally delete the dataset if empty. Return success instead of
  the validation error.
- **Severity:** Low.
- **Acceptance:** deleting a `data_id` with no relational row removes its graph/vector
  footprint and returns success; a test seeds graph-only data and asserts cleanup.

## Item 4 — `get_status` multi-pipeline + `list_data` ordering (B7.4)

- **Rust:** `crates/lib/src/api/datasets.rs`. `get_status` (~89–104) queries only
  `"cognify_pipeline"` and returns a flat `HashMap<Uuid, PipelineRunStatus>`. `list_data`
  (~lines around the read-permission check) returns `self.db.get_dataset_data(dataset_id)`
  in unspecified order.
- **Python:** returns a **nested** multi-pipeline status per dataset, and orders dataset
  data by `data_size` descending.
- **Gap:** Rust hides non-cognify pipeline status and emits data in non-deterministic order
  (test flakiness + cross-SDK output mismatch).
- **Recommended fix:** (a) extend `get_status` to return per-pipeline statuses
  (`HashMap<Uuid, HashMap<String, PipelineRunStatus>>` or a small struct) by querying all
  pipelines registered for the dataset, keeping a backward-compatible accessor for the
  cognify status if callers depend on it; (b) sort `list_data` by `data_size` descending
  (add an `ORDER BY` to the query or sort in Rust).
- **Severity:** Med.
- **Acceptance:** `get_status` reports both `add` and `cognify` pipeline statuses;
  `list_data` output is deterministically ordered by size desc; tests assert ordering.

## Item 5 — `recall()` advanced params plumbed (B4.3)

- **Rust:** `crates/lib/src/api/recall.rs:65–77` — `recall()` exposes `query_text`,
  `query_type`, `datasets`, `top_k`, `auto_route`, `session_id`, `user_id`, `scope`. The
  advanced knobs exist on `SearchRequest`
  (`crates/search/src/types/search_request.rs:16–54`) but are not on the facade.
- **Python:** `/tmp/cognee-python/cognee/api/v1/recall/recall.py:314–337` — exposes
  `system_prompt`, `system_prompt_path`, `node_name`, `node_name_filter_operator`,
  `only_context`, `wide_search_top_k` (default 100), `triplet_distance_penalty` (default 6.5),
  `feedback_influence` (0.0), `neighborhood_depth`, `neighborhood_seed_top_k`, plus
  `dataset_ids`.
- **Gap:** SDK users can't tune these from the facade despite full lower-level support.
- **Recommended fix:** add the missing parameters to `recall()` (default them to the same
  values Python uses, and to the same defaults `SearchRequest` already carries — note
  `triplet_distance_penalty` default must be **6.5** after task 08) and forward them onto
  the `SearchRequest` it builds. Consider a `RecallOptions` struct to avoid an unwieldy
  argument list (and to dodge clippy `too_many_arguments`).
- **Severity:** Med.
- **Acceptance:** each advanced param is settable via the facade and reaches
  `SearchRequest`; a test sets `triplet_distance_penalty`/`node_name` and asserts they reach
  the request; defaults match Python.

## Item 6 — Config setter allowlists + missing keys + introspection (B7.5)

- **Rust:** `crates/lib/src/config.rs` (setters ~806–1073). Narrow allowlists reject valid
  Python keys; **missing** `set_relational_db_config` / `set_migration_db_config`; no
  introspection (`get_settings`, masked `save_*_config`); missing knobs for features Rust
  *has* (`transcription_model`, temporal prompt paths, `embedding_api_version`, LLM
  `fallback_*`).
- **Python:** `/tmp/cognee-python/cognee/api/v1/config/config.py:207–308` — full setter +
  save/load surface across all config sections.
- **Gap:** SDK config parity is incomplete; users can't set or introspect several supported
  knobs.
- **Recommended fix:** widen the allowlists to accept the Python key spellings; add
  `set_relational_db_config` / `set_migration_db_config`; add the missing individual knobs
  (`transcription_model`, `embedding_api_version`, LLM `fallback_*`, temporal prompt paths);
  add a `get_settings()` returning a serializable view with secrets **masked**, and
  `save_*_config` mirroring Python's masking. Cross-check the binding setter surface (A3.1).
- **Severity:** Med.
- **Acceptance:** the previously-rejected Python keys are accepted; the new setters exist and
  round-trip; `get_settings()` masks API keys; tests cover masking + each new key.

## Item 7 — Default-string mismatches (B7.6)

- **Rust:** `crates/lib/src/config.rs` defaults (~605–625): `logs_root_directory:
  "./logs"`, `llm_model: "gpt-5-mini"`, `graph_database_provider: "kuzu"`.
- **Python:** `base_config.py:15` `logs_root_directory` → `~/.cognee/logs`;
  `infrastructure/llm/config.py:45` `llm_model = "openai/gpt-5-mini"`;
  `infrastructure/databases/graph/config.py:45` `graph_database_provider = "ladybug"`.
- **Gap:** three default strings differ. These leak into telemetry/settings output and cause
  confusing config diffs cross-SDK.

  | key | Rust now | Python | Recommended Rust value |
  |---|---|---|---|
  | `llm_model` | `gpt-5-mini` | `openai/gpt-5-mini` | `openai/gpt-5-mini` (provider-qualified) |
  | `graph_database_provider` | `kuzu` | `ladybug` | `ladybug` (Rust uses Ladybug; `kuzu` is wrong) |
  | `logs_root_directory` | `./logs` | `~/.cognee/logs` | discuss — see below |

- **Recommended fix:** set `llm_model` default to `openai/gpt-5-mini` and
  `graph_database_provider` to `ladybug` (the `kuzu` value is simply wrong — Rust's graph
  backend is Ladybug). For `logs_root_directory`, prefer Python's `~/.cognee/logs` for
  parity **unless** the edge/Android target needs a relative path — if so, keep `./logs` and
  document the intentional divergence in the README + `docs/not-implemented.md`.
- **Severity:** Low.
- **Acceptance:** `llm_model` and `graph_database_provider` defaults match Python; the
  `logs_root_directory` decision is recorded; a settings-output test pins the values.

## Item 8 — Stored QA `context` payload (B5.2)

- **Rust:** `crates/search/src/orchestration/search_orchestrator.rs:400–412` — always stores
  the full retrieved context: `let ctx_json = context.as_ref().and_then(|c|
  serde_json::to_string(c).ok());` then `save_qa(..., ctx_json.as_deref())`.
- **Python:** `/tmp/cognee-python/cognee/infrastructure/session/session_manager.py` (~339,
  ~518–521) — `context_to_store` is `""` by default, or a **summary** only when
  `summarize_context=True`; never the full context.
- **Gap:** persisted QA entries diverge cross-SDK — Rust bloats entries with full context;
  Python stores empty/summary.
- **Recommended fix:** add a `summarize_context: bool` (default `false`) to the search/session
  save path. When `false`, store `""`; when `true`, store a summary of the context. Mirror
  Python's `generate_session_completion_with_optional_summary` behavior.
- **Severity:** Med.
- **Acceptance:** default save stores empty context; with `summarize_context=true` it stores
  a summary; a cross-SDK test (or a unit test asserting the stored field) confirms parity.
  (Coordinate with [task 20](20-improve-and-session-integration.md), which adds
  `used_graph_element_ids` to the same save path but deliberately leaves context-payload to
  this item.)

## Item 9 — `CloudClient` proxy add/cognify/search/remember_entry (B8.3)

- **Rust:** `crates/cloud/src/cloud_client.rs:137–262` — proxies `health_check`, `remember`,
  `recall`, `improve`, `forget`. **Missing** `add`, `cognify`, `search`, `remember_entry`.
- **Python:** the cloud client / `serve()` path proxies the full operation set.
- **Gap:** when connected via `serve()`, `add`/`cognify`/`search`/`remember_entry` can't
  reach the cloud — they fall back to local or error.
- **Recommended fix:** add the four proxy methods following the existing
  `remember`/`recall` pattern (same request signing, same `CloudResult<Value>` return,
  matching the cloud HTTP routes). Verify the cloud-side route paths against the Python
  client.
- **Severity:** Med.
- **Acceptance:** `CloudClient` exposes `add`/`cognify`/`search`/`remember_entry`; each posts
  to the correct route; a mock-server test asserts the request shape for at least `add` and
  `search`.

## Item 10 — `sync` stub: document or implement (B8.2)

- **Rust:** `crates/cloud/src/sync.rs:57–79` — `run_background` marks started, ticks
  progress `[0,80,90,95,100]`, marks completed with zeros `(0,0,0,0)` for
  records/bytes. **Moves no data.** The HTTP wire contract (`POST /api/v1/sync`) looks
  complete.
- **Python:** sync performs an actual diff/upload/download.
- **Gap:** the wire contract advertises a working sync, but it's a progress-ticker no-op.
  `sync` is not in CLAUDE.md's implemented list.
- **Recommended fix (0.1.0):** **document it as a no-op** — add a prominent rustdoc note on
  `run_background` and the HTTP route, and list it under `docs/not-implemented.md`. Make the
  completion payload honest (it already reports zero records, which is accurate). Full
  implementation is a separate, larger task — defer.
- **Severity:** Med (honesty of the contract).
- **Acceptance:** the no-op status is documented in rustdoc + `docs/not-implemented.md`; the
  HTTP response/log makes clear no data was transferred; (optional) the route returns a clear
  "not implemented" signal if the project decides against advertising it.

## Item 11 — FS session store on-disk format (B5.3)

- **Rust:** `crates/session/src/fs_store.rs` — plain JSON files at
  `{base_dir}/{user_id}/{session_id}.json` (one JSON array of entries per session). The entry
  **shape** matches Python; the **container** does not.
- **Python:** diskcache **SQLite** backend at `.cognee_fs_cache/sessions_db/` (a SQLite DB,
  not per-session JSON files). Uses the `diskcache` library.
- **Gap:** the on-disk container differs, so a Rust FS session store and a Python FS session
  store can't read each other's files (already acknowledged in Rust comments).
- **Recommended fix:** this is the heavy one. Either (a) **accept the divergence** for 0.1.0
  and document it clearly (the entry shape is compatible; only the FS container differs), or
  (b) implement a diskcache-compatible SQLite reader/writer matching Python's table/key
  layout. Recommend **(a) document for 0.1.0**, file (b) as a follow-up issue. If pursuing
  (b), pin the exact diskcache schema (Cache table, key hashing) from the `diskcache` source
  to stay byte-compatible.
- **Severity:** Med.
- **Acceptance (option a):** the divergence is documented in `docs/not-implemented.md` + the
  module rustdoc, including a note that the SeaOrm/Redis stores are the recommended
  cross-process backends. (Option b acceptance: a Python-written `sessions_db` round-trips
  through the Rust reader.)

## Item 12 — Visualization multi-view + name fallback (B8.4)

- **Rust:** `crates/visualization/src/lib.rs` `render` (~77–80) passes `None` for schema →
  the schema tab always shows "No schema configured" (`html.rs` substitutes `"null"`).
  `colors.rs:19–37` is a stale flat node-color map. Name derivation is name-or-id only.
- **Python:** `/tmp/cognee-python/cognee/modules/visualization/cognee_network_visualization.py:52–80`
  emits **Story / Schema / Inspector** views (4 JS modules: `ui_chrome`, `schema_view`,
  `story_view`, `inspector`). `preprocessor.py:223–237` derives schema-node display via an
  8-key fallback: `database_type`, `primary_key`, `source_table`, `source_column`,
  `target_table`, `target_column`, `relationship_type`, `row_count_estimate`.
- **Gap:** Rust is behind Python's multi-view rewrite; schema tab is always empty; node-color
  map and name fallback are narrower. Both still emit a self-contained d3.v7 graph, so the
  core view works.
- **Recommended fix:** scope to the highest-value sub-gaps for 0.1.0: (a) implement the
  multi-key schema-node name/field fallback (the 8 keys above) so schema-typed nodes render
  meaningfully; (b) refresh `colors.rs` to cover the current node-type set; (c) optionally
  thread schema data through `render` instead of `None`. The full Story/Inspector view rewrite
  is a large effort — defer to post-release and track as an issue.
- **Severity:** Med.
- **Acceptance:** schema-typed nodes show derived names/fields via the 8-key fallback; the
  color map covers all current node types; the Story/Inspector rewrite is tracked as a
  follow-up issue with the gap documented.

---

## Cross-cutting verification

After each item:

```bash
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
# item-specific tests, e.g.:
cargo test -p cognee-lib config_defaults
cargo test -p cognee-cloud cloud_client
bash scripts/run_tests_with_openai.sh recall_advanced_params   # if it touches the search/LLM path
```

## Acceptance criteria (doc-level)

- [ ] Each item above is either implemented (with a test) **or** explicitly deferred with a
      tracked issue + a `docs/not-implemented.md` entry.
- [ ] No item changes content-hash inputs, UUID5 namespaces/inputs, vector collection-name
      formats, or stored-file naming (none of these require that — confirm before merge).
- [ ] The default-string fixes (item 7) and any new config keys (item 6) are reflected in the
      README env-var table.

## Gotchas / do-not

- **Determinism — do NOT touch ID/hash inputs.** None of these items need to; if one seems
  to, stop and reconsider. `list_data` ordering (item 4) changes *output order only*, never
  IDs.
- **`triplet_distance_penalty` default is 6.5** (item 5) — after task 08. Don't reintroduce
  3.5 when plumbing the recall facade default.
- **Mask secrets in introspection** (item 6) — `get_settings`/`save_*_config` must redact API
  keys; reuse `cognee_utils::redact`.
- **Don't silently change persisted-entry shape** (item 8) — the QA `context` field is
  cross-SDK; gate the change behind the `summarize_context` flag exactly as Python does.
- **`graph_database_provider` default is `ladybug`, not `kuzu`** (item 7) — `kuzu` is a Python
  default that doesn't apply to the Rust backend; use `ladybug` to reflect reality.
- **Sync honesty** (item 10) — do not let the wire contract imply data moved; the completion
  payload must report zero transfer while it's a no-op.
- **Heavy items (11, 12) are deferral candidates** — prefer documenting the divergence for
  0.1.0 over a rushed partial that breaks the working core (FS JSON store and single-view
  graph both work today).

## Rollback

Every item is an independent small change. Revert per item with
`git checkout main -- <touched file>`. None alter schema, content hashes, IDs, collection
formats, or stored-file naming, so rollbacks are isolated and risk-free.
