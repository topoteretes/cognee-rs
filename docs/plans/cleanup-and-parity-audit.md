# Cleanup & Python-Parity Audit

> **Status:** Draft — created 2026-06-14
> **Companion to:** [release-readiness-plan.md](./release-readiness-plan.md)
> **Method:** Repo-wide audit + line-by-line comparison against the Python reference
> (`github.com/topoteretes/cognee`, cloned to `/tmp/cognee-python`).
> **Scope note:** Only operations **declared-supported** in `.claude/CLAUDE.md` are
> treated as parity targets. Explicitly out-of-scope items (S3, partial unstructured
> office extraction, cross-process Ladybug locking, agentic/skills subsystem) are not
> counted as gaps. Highest-impact items (★) were spot-verified against both sources.

This document has two parts:
- **Part A — Cleanup** (Rust code, docs, bindings/examples): hygiene, dead code, drift.
- **Part B — Python-parity gaps**: behavioral divergences in supported operations that
  threaten the project's stated **90%+ correctness-parity** goal.

---

# Part A — Cleanup

## A1. Rust production code

### High value
- **A1.1 — Delete orphaned scaffolding.** `crates/graph/src/ladybug_restored.rs` and
  `ladybug_restored_clean.rs` are incomplete cut-off copies of the Ladybug adapter,
  not declared in `lib.rs`, referenced nowhere (~220 dead lines). Remove.
- **A1.2 — Dedup dataset-name→uuid5 resolution.** The same DB-lookup-then-`uuid5`
  fallback is copy-pasted across 4 routers: `cognify.rs:123`, `memify.rs:67`,
  `remember.rs:208`, `improve.rs:80`. Extract `resolve_dataset_id(name, db)`.
- **A1.3 — Unify truthy env-var parsing.** A correct `parse_env_bool`
  (`true|1|yes|on`, trimmed, lowercased) already exists privately at
  `crates/http-server/src/config.rs:224`, but ~15 sites across 6 crates reinvent a
  weaker `v == "true" || v == "1" || v == "yes"` (no trim, no `on`, case-sensitive):
  `crates/lib/src/config.rs` (~9 sites), `component_manager.rs:431`,
  `embedding/src/config.rs:197`, `observability/src/settings.rs:83`,
  `llm/src/adapters/openai.rs:155`, `http-server/src/{routers/remember.rs:98,auth/context.rs:118}`.
  Promote one into `cognee-utils`. (Duplication **and** a robustness bug.)

### Medium value
- **A1.4 — `log` vs `tracing` inconsistency (records silently dropped).** Two crates
  use the `log` facade while the workspace runs a `tracing` subscriber with **no
  `tracing-log`/`LogTracer` bridge** — so these logs vanish:
  `crates/ontology/src/{loader.rs:6,rdflib.rs:6,builder.rs:8}` and
  `crates/utils/src/retry.rs:150-175`. Convert to `tracing`; drop the `log` dep.
- **A1.5 — Redundant `NAMESPACE_OID`.** `cognee_utils::NAMESPACE_OID`
  (`utils/src/id_generation.rs:13`) is byte-identical to `uuid::Uuid::NAMESPACE_OID`,
  and the codebase mixes both (often in the same file). Standardize on one.
- **A1.6 — Inline uuid5 instead of canonical helpers.** `core/src/pipeline.rs:457`
  and `pipeline_run_registry/default_impl.rs:{326,355,442,701}` recompute IDs inline
  instead of using the canonical `pipeline_run_registry/ids.rs` helpers.
- **A1.7 — Removable dead fields/fns behind `#[allow(dead_code)]`:**
  `pipeline_run_registry/scoped_watcher.rs:20` (`PerRunSink.run_id`, write-only),
  `default_impl.rs:29` (`RunSlot.started_at`, write-only),
  `http-server/src/middleware/tracing.rs:76` (`duration_ms`, zero callers),
  `ontology/src/builder.rs:151` (`extract_local_name`, never called).
- **A1.8 — Forwarding wrapper adds nothing.** `JaccardChunksRetriever`
  (`search/src/retrievers/lexical_retriever.rs:275`) is a pure delegate over
  `LexicalRetriever`; inline at the one call site (`search_execution_builder.rs:221`),
  delete ~50 lines.

### Low value
- **A1.9 — Likely-unused deps** (confirm with a per-crate build): `log` in
  `llm`/`chunking`; `time` in `graph`/`database` (both use chrono); `regex`,
  `email_address`, `tokio-stream`, `http-body-util` in `http-server`; `async-trait`
  in `visualization`; `dotenv`/`chrono` in `cli`; `env_logger` (dev) in `ontology`;
  `cognee-models` duplicated as both dep and dev-dep in `database`.
- **A1.10 — Orphaned feature wiring:** `session`'s `redis` feature is never enabled by
  any aggregating crate (RedisSessionStore effectively unbuildable in normal builds);
  `lib`'s `ort-cuda`/`ort-tensorrt` are pass-throughs not forwarded by cli/bindings.
  Decide: wire or document.
- **A1.11 — Re-export sprawl:** `crates/lib/src/lib.rs` globs
  `cognee_{cognify,delete,search}::*` both inside facade modules and again at crate
  root (lines 51,55,209-211) — ambiguity risk. Also stylistic: some crates declare
  `default = []`, others omit it.

> **Verified clean** (no action): thiserror error enums, single-impl internal traits
> (all serve real decoupling), the few TODOs are legitimately tracked, the
> `#[allow(dead_code)]` on OpenAPI/serde-wire DTOs is justified, feature naming is
> consistent kebab-case.

## A2. Documentation

### High value
- **A2.1 — Python README is stale.** It still describes only the pipeline-engine tier
  (`python/README.md` Quick Start shows only `Pipeline()`), but Python now has the
  full `PyCognee` SDK (40+ ops, T1–T11 done per `docs/python-bindings/STATUS.md`).
  Rewrite to match the JS README's operation coverage.
- **A2.2 — Logging env vars missing from `.env.example`.** README documents 8
  `COGNEE_LOG_*` / `LOG_*` vars (all read by `crates/lib/src/config.rs:197-280`); none
  appear in `.env.example`. Add a logging section.
- **A2.3 — CLAUDE.md CI drift.** Lines 54 & 286 reference non-existent workflows
  (`lib-tests.yml`, `lint.yml`, `capi-check.yml`, `js-check.yml`, `python-check.yml`);
  actual are `ci.yml` + `http-parity.yml`. (Also tracked in the release plan, T6.5.)

### Medium value
- **A2.4 — Internal task-tracking docs would ship to users.** These are engineering
  scratchpads, not user docs, and shouldn't be in a release tarball:
  `docs/python-bindings/{IMPLEMENTATION-PROMPT.md,STATUS.md}`,
  `docs/cognify-compatibility-implementation-plan.md`,
  `docs/cognify-compatibility/IMPLEMENTATION-PROMPTS.md`. Move to `.claude/` or a
  `docs/.internal/` excluded from distribution.
- **A2.5 — Empty `docs/memify-tasks/` directory.** Leftover; remove.

### Low value
- **A2.6 — Env-var redundancy / drift risk.** The same vars are tabulated in root
  README, `.env.example`, CLAUDE.md, and three binding READMEs. Currently consistent;
  designate the root README as canonical and have the others reference it.

> **Verified current** (no action): `docs/not-implemented.md`, `docs/open-questions.md`,
> the document-classification/extraction claims, HTTP-server router docs.

## A3. Bindings & examples

### Medium value
- **A3.1 — Config-setter surface drift across bindings.** JS exposes 44 granular typed
  setters (`js/cognee-neon/src/config.rs`); C and Python expose only generic
  `set`/`set_str` + 4 bulk setters (`capi/.../sdk_config.rs`, `python/src/config.rs:82-167`).
  Documented as intentional sugar (all delegate to the generic setter), but it's a real
  API-shape inconsistency — confirm it's acceptable, or add the sugar to C/Python.
- **A3.2 — JS test coverage has drifted behind Python.** Python: ~180 tests / 22 files
  covering 9 op groups end-to-end. JS: ~118 tests / 13 files covering ~5; **no JS tests
  for** memory/sessions/notebooks/admin/data-ops(forget/update/prune)/cloud/visualization/recall,
  despite those ops being exported (`js/cognee-neon/src/lib.rs:74-126`). Add JS tests.

### Low value
- **A3.3 — No runnable example** for memify/visualization/sessions/datasets in any
  binding (only add/cognify/search). Python has **no `examples/` dir** at all. Add a
  `python/examples/` and a few cross-op examples.
- **A3.4 — Stale local artifact.** `python/cognee_pipeline/_native.cpython-39-darwin.so`
  (16 MB, Apr 11) sits in the source tree — untracked (won't ship) but `clean` it to
  avoid importing a stale binary.
- **A3.5 — Binding versions hardcoded.** `python/pyproject.toml` and `js/package.json`
  both hardcode `0.1.0`, not linked to the Cargo workspace version → drift risk. (Also
  in release plan, T2.5/T2.6.)

> **Verified clean** (no action): no committed build artifacts under js/python/capi; no
> dead code or wire-logic duplication in binding sources; the core op set is consistent
> across all three at the `bindings-common::ops` layer; all examples compile.

---

# Part B — Python-Parity Gaps

> These threaten the **90%+ correctness-parity** goal. Severity = behavioral impact:
> **Critical/High** = changes pipeline output structure, IDs, or which data the LLM
> sees (breaks cross-SDK interop or answer parity); **Medium** = stored-payload or
> ranking differences; **Low** = cosmetic / migration-only.

## B1. ADD / ingestion

| ID | Gap | Sev |
|---|---|---|
| **B1.1** ★ | **ADD never runs loaders.** Python (`tasks/ingestion/ingest_data.py:103`) runs the loader at ingest time and stores the **extracted text** as `text_<hash>.txt`. Rust (`crates/ingestion/src/pipeline.rs:163-193`) streams **raw bytes** and runs loaders only later at cognify. For any non-plain-text input the stored artifact, extension, and mime differ. Text-only tests mask this. | High |
| **B1.2** ★ | **`raw_content_hash` wrong for non-text.** Python: MD5 of extracted-text file (differs from `content_hash`). Rust: always equals `content_hash` (`pipeline.rs:357`; admitted in `models/src/data.rs:37`). | High |
| **B1.3** | **Legacy `get_unique_data_id` fallback not ported** (`ingest_data`/`get_unique_data_id.py:51-68`). Only affects reading pre-tenant Python DBs. | Low |

> **Match (verified):** `content_hash`/`data_id`/`dataset_id` formulas, dedup-by-hash,
> `text_<md5>.txt` naming, `file://` URIs, 22-column Data schema, multi-tenant handling.

## B2. COGNIFY

| ID | Gap | Sev |
|---|---|---|
| **B2.1** ★ | **Chunk token counter defaults to `WordCounter`** (whitespace) when the `tiktoken`/`hf-tokenizer` feature or a tokenizer path is absent (`chunking/src/config.rs:77-163`). Python uses real BPE (tiktoken cl100k for OpenAI). Same `max_chunk_size` → different boundaries → different chunk text → **different uuid5 chunk IDs and counts** → different LLM inputs. | High |
| **B2.2** ★ | **Default `max_chunk_size` diverges.** Rust fixed `1500` (`cognify/src/config.rs:203`); Python auto = `min(8191, 16384/2)` ≈ 8191 (`cognify.py` + `llm/utils.py`). Rust's auto-override also uses wrong quantities (context window / 512 cap). Effective ≈16× smaller chunks. Compounds B2.1. | High |
| **B2.3** ★ | **Graph-extraction prompt diverges.** Rust `DEFAULT_GRAPH_PROMPT` (`fact_extraction/extractor.rs`) drops Python's edge-description paragraph + examples and forces UPPERCASE node types. Changes node casing and edge content. | High |
| **B2.4** ★ | **Summarization prompt diverges.** Rust uses a generic 3-line paraphrase; Python (`summarize_content.txt`) is a structured "categories + ordered facts, ≤200 tokens" prompt. The "based on Python" comment is stale. | High |
| **B2.5** ★ | **`Edge` schema missing `description`.** Python `KnowledgeGraph.Edge` has `description`, used as edge `edge_text` for triplet/edge-type embeddings (`expand_with_nodes_and_edges.py:294`). Rust `Edge` (`fact_extraction/models.rs`) has none → edges carry no fact text. | High |
| **B2.6** ★ | **Missing `TextDocument_name` (+ Document subtype) vector collection, and Document graph nodes.** Python indexes nodes by each model's `index_fields`; Documents declare `index_fields=["name"]` and are stored as graph nodes. Rust hardcodes 6 `index_points` calls (`cognify/src/tasks.rs`), never indexes/stores Documents. Python searches against `TextDocument_name` find nothing on a Rust store. | High |
| **B2.7** | **Hardcoded LLM temperature/max_tokens** (extraction 0.1/2000, summary 0.3/500). Python passes provider defaults (≈0.0) and no completion cap → reproducibility + possible truncation. | Med |
| **B2.8** | **Schema collections not indexed.** Python's `extract_dlt_fk_edges` indexes 4 schema collections; Rust skips (`tasks.rs:1866`). DLT FK **graph** structure is otherwise faithful. | Med |
| **B2.9** | **`use_pipeline_cache` default mismatch** (Rust `false`, Python effectively `true`); config comment inaccurate. `ChunkStrategy::Recursive` defined but inert; `chunk_overlap` never consumed. | Low |

> **Match (verified):** word/sentence/paragraph boundary algorithm, `cut_type` values,
> chunk `index_fields` metadata, collection-name **format** (`{Type}_{field}`), cognify
> batch defaults (chunks_per_batch=100, data_per_batch=20), DLT FK edge graph structure.

## B3. SEARCH

| ID | Gap | Sev |
|---|---|---|
| **B3.1** ★ | **`DEFAULT_TRIPLET_DISTANCE_PENALTY` is wrong: Rust `3.5` vs Python `6.5`** (`brute_force_triplet_search.rs:16` — and its doc comment **falsely** claims it matches Python's 3.5; Python `utils/brute_force_triplet_search.py:56,227` = 6.5). This penalty for no-vector-match nodes changes triplet ranking on **every default graph-completion search**. **One-line fix.** | High |
| **B3.2** ★ | **Brute-force search omits `Triplet_text`** (and other indexed types). Rust hardcodes 5 collections (`brute_force_triplet_search.rs:24-30`); Python enumerates all `DataPoint` subclass collections incl. `Triplet_text`. So after `memify`, Python's graph search uses triplet vectors; Rust never does. | High |
| **B3.3** | **Feedback-detection system prompt gutted** (Rust 6 lines `utils/feedback_detection.rs:5-17` vs Python 31-line `feedback_detection_system.txt` with rules/examples/scoring). Same LLM classifies feedback differently. | High |
| **B3.4** | **Missing search types:** `GRAPH_COMPLETION_DECOMPOSITION` (Python has full retriever + prompt) and `AGENTIC_COMPLETION` (agentic/skills — large optional surface, likely out of scope). Rust adds `FEEDBACK` as a first-class type (architectural divergence — Python detects feedback in the session manager instead). | Med |
| **B3.5** | **Prompt drift guard absent.** Rust hardcodes search prompts as `const &str`; Python loads `.txt` files. The 11 ported search prompts are currently byte-identical (verified) save cosmetic whitespace/apostrophes, but there's no mechanism to catch upstream prompt edits. | Low |

> **Match (verified):** default search type GRAPH_COMPLETION, top_k=10,
> wide_search_top_k=100, cot max_iter=4, context_extension_rounds=4,
> feedback_influence=0.0, and the 11 search-prompt texts.

## B4. MEMORY (recall / remember / improve / memify)

| ID | Gap | Sev |
|---|---|---|
| **B4.1** | **memify triplet node-text differs.** Python builds node text from `index_fields` (Entity → `"Alice"`); Rust always concatenates name+description (`"Alice: engineer"`) (`memify/extract_triplets.rs:117-128`). Same edge → different embedding input → breaks cross-SDK triplet vectors. (Collection name `Triplet_text` and `-›` separator **match**.) | High |
| **B4.2** | **`improve()` missing stages:** no persist-agent-trace-steps stage (Python `improve.py:1141-1163` — the bulk of the agent use case), no `build_global_context_index`, no single-session improve lock (concurrent improves duplicate work). Feedback-weight math (`feedback_alpha=0.1`, `(score-1)/4`, EMA) **matches**. | High |
| **B4.3** | **`recall()` advanced params not plumbed** (system_prompt, node_name filters, wide_search_top_k, triplet_distance_penalty, feedback_influence, neighborhood_*, only_context). Exist lower-level on `SearchRequest`; not exposed on the facade. | Med |
| **B4.4** | **`remember()` skills path missing** — Python handles `content_type="skills"`/`SkillRunEntry`; Rust `MemoryEntry` has only Qa/Trace/Feedback. (Skills subsystem likely out of scope.) | Low |

## B5. SESSIONS

| ID | Gap | Sev |
|---|---|---|
| **B5.1** | **Session→search integration gaps:** Rust never prepends the session's stored graph-context snapshot to history (Python `session_manager.py:435-450`), never persists conversationally-detected feedback to the prior QA entry (`add_feedback`), and never populates `used_graph_element_ids` on save. All three starve downstream memify/improve of provenance. | High |
| **B5.2** | **Stored QA `context` differs.** Rust always stores full retrieved context; Python stores `""` (or a summary only when `summarize_context=True`). Persisted entries diverge cross-SDK. | Med |
| **B5.3** | **FS session store on-disk format not cross-SDK compatible.** Python: diskcache SQLite (`.cognee_fs_cache/sessions_db/`); Rust: plain JSON files. Entry shape matches; container doesn't (acknowledged in Rust comments). | Med |

## B6. DELETE / FORGET / PRUNE / UPDATE

| ID | Gap | Sev |
|---|---|---|
| **B6.1** ★ | **`prune_system(metadata=true)` is a silent no-op** (`lib/src/api/prune.rs:114-119` logs and does nothing). Python drops the relational DB. `PruneTarget::all()` advertises `metadata:true` but won't deliver. | High |
| **B6.2** | **`forget` `memory_only` mode missing.** Python has 6 targets incl. `*_memory_only` (wipe graph+vector, keep files+Data, reset cognify status for re-cognify). Rust `ForgetTarget` has only Item/Dataset/All; zero `memory_only` anywhere. | High |
| **B6.3** ★ | **`forget`/`update` use Hard delete; Python uses Soft.** `forget.rs:166` and `update.rs:78` hardcode `DeleteMode::Hard`; Python defaults soft (and warns hard is dangerous). Rust is more destructive to shared graph state. | High |
| **B6.4** | **Soft-delete orphan cleanup mismatch.** Python soft-delete still removes orphan entities/types (graph traversal); Rust sweeps orphans only under Hard (`delete/src/lib.rs:524`) → Rust soft-delete leaves orphans Python removes. | Med |
| **B6.5** | **`update()` re-derives dataset ID from name** (`update.rs:91`) vs Python's explicit `dataset_id: UUID`; also drops `node_set`/`preferred_loaders`/`incremental_loading` and has no auth gate. | Med |
| **B6.6** | **Custom-graph-model fallback missing.** Python `delete_data` cleans graph/vector even when the relational Data row is absent; Rust errors (`delete/src/lib.rs:1326`). | Low |

> **Match (verified):** delete cascade order (graph→vector→relational→files),
> last-link-wins file deletion. Rust adds `preview()`/dry-run (superset).

## B7. DATASETS / CONFIG

| ID | Gap | Sev |
|---|---|---|
| **B7.1** | **`DatasetManager` has no `create_dataset`/`create_authorized_dataset`** (with deterministic ID + owner/parent ACL grant). Python does. | High |
| **B7.2** ★ | **Embedding default trio + no auto-dimension resolution.** Rust: `onnx`/`BGE-Small-v1.5`/static `384` (`config.rs:662`). Python: `openai`/`text-embedding-3-large`/auto-resolved (3072). Python's `_resolve_embedding_dimensions` auto-adjusts when the model changes; Rust keeps 384 silently → vector-shape mismatch risk. Partly an intentional edge default, but no safety net. | High |
| **B7.3** | **Permission enforcement opt-in vs always-on.** Rust `DatasetManager.acl_db` is `Option`; unset → read/delete checks silently pass (owner-scoped only); `has_data` does no auth check (info leak). Python always enforces the 4-permission model. | Med |
| **B7.4** | **`get_status` single-pipeline only** (`datasets.rs:89-104` queries only `cognify_pipeline`, flat map) vs Python's multi-pipeline nested result. **`list_data` ordering** unset vs Python `data_size desc` (non-deterministic output). | Med |
| **B7.5** | **Config setter allowlists too narrow + missing keys.** Rust narrow allowlists reject valid Python keys; missing `set_relational_db_config`/`set_migration_db_config`, and knobs for features Rust *has* (`transcription_model`, temporal prompt paths, `embedding_api_version`, LLM `fallback_*`). Also missing introspection (`get_settings`, masked `save_*_config`). | Med |
| **B7.6** | **Default-string mismatches** (leak into telemetry/settings output): `llm_model` `openai/gpt-5-mini` vs `gpt-5-mini`; `graph_database_provider` `ladybug` vs `kuzu`; `logs_root_directory` `~/.cognee/logs` vs `./logs`. | Low |

## B8. PERMISSIONS / VISUALIZE / CLOUD / SYNC

| ID | Gap | Sev |
|---|---|---|
| **B8.1** | **Permission revoke endpoints missing (HTTP).** Grant/assign exist; DELETE `/datasets/{principal_id}` (revoke ACL — repo method `revoke_acl` exists, just unwired), DELETE `/roles/{role_id}`, DELETE `/users/{user_id}/roles` (`revoke_role` exists, unwired) are not exposed → no way to revoke a grant. `docs/http-server/routers/permissions.md:319` wrongly claims this is omitted "to match Python." **Mostly wiring.** | High |
| **B8.2** | **`sync` is a stub** (`cloud/src/sync.rs:51-79` ticks progress, moves no data) but the HTTP wire contract looks complete. Document as no-op or implement. (Sync isn't in CLAUDE.md's implemented list.) | Med |
| **B8.3** | **`CloudClient` proxy missing `add`/`cognify`/`search`/`remember_entry`** (only remember/recall/improve/forget proxied) — when connected via `serve()`, those can't reach the cloud. | Med |
| **B8.4** | **Visualization behind Python's multi-view rewrite.** Python now emits Story/Schema/Inspector views; Rust targets the older single-template Graph+Schema. Sub-gaps: schema tab always "No schema configured" (`visualization/src/lib.rs:80` passes `None`); stale node-color map (`colors.rs:19-37`); name-derivation is name-or-id only vs Python's 6-key fallback. Both still emit a self-contained d3.v7 graph. | Med |

> **Solid parity (verified):** serve/disconnect/device-auth/credentials
> (`cloud_credentials.json` byte-compatible, 0o600), users (auth+CRUD), api_keys
> (incl. masking), notebooks CRUD (only `/run` stubbed — known gap), chunk config.

---

# Prioritized fix list (parity, by ROI)

**Tier 1 — one-liners / small, high-impact correctness fixes**
1. **B3.1** `DEFAULT_TRIPLET_DISTANCE_PENALTY` 3.5 → 6.5 (+ fix the false comment). *One line, affects every default graph search.*
2. **B6.1 / B6.3** Stop silent/over-destructive behavior: implement prune-metadata or make `all()` honest; change `forget`/`update` to soft delete.
3. **B8.1** Wire the existing `revoke_acl`/`revoke_role` repo methods to DELETE routes.

**Tier 2 — correctness parity, moderate effort**
4. **B2.3 / B2.4 / B3.3** Sync the graph, summary, and feedback-detection prompts to the Python `.txt` sources (and add a drift check — B3.5).
5. **B2.1 / B2.2** Default the chunk token counter to tiktoken for OpenAI-family; fix `max_chunk_size` auto-calc to `min(8191, llm_max/2)`.
6. **B3.2 + B4.1** Make brute-force search enumerate indexed collections (incl. `Triplet_text`); fix memify node-text to use `index_fields`.
7. **B2.5 / B2.6** Add `Edge.description`; index Document nodes into `TextDocument_name` (ideally drive indexing off `index_fields`, not a hardcoded list).

**Tier 3 — structural / feature gaps**
8. **B1.1 / B1.2** Run loaders at ADD; store extracted text + correct `raw_content_hash`.
9. **B6.2 / B7.1** `forget` `memory_only`; `DatasetManager.create_dataset` + ACL grant.
10. **B7.2** Embedding auto-dimension resolution.
11. **B4.2 / B5.1** `improve()` trace-persist stage + lock; session graph-context/feedback/provenance integration.

---

# How this feeds the release plan

- **Cleanup (Part A)** items fold into the release plan's **Phase 4 (hygiene)** and
  **Phase 5 (docs)** — most are low-risk and can land before release.
- **Parity (Part B)** is the project's core value proposition (90%+ parity). Tier 1 is
  cheap and should be release-blocking; Tiers 2–3 are a tracked parity backlog. A new
  **Phase 7 — Python parity correctness** has been added to
  [release-readiness-plan.md](./release-readiness-plan.md) referencing this audit.
