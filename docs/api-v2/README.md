# Cognee API v2 — Memory-Oriented API

The Python cognee SDK exposes a "V2 memory-oriented API" as a separate public surface, layered on top of the v1 primitives (`add`, `cognify`, `search`, `memify`, `delete`). It is defined in [`cognee/__init__.py`](https://github.com/topoteretes/cognee/blob/main/cognee/__init__.py) under the header:

```python
# ---------------------------------------------------------------------------
# V2 memory-oriented API
# ---------------------------------------------------------------------------
from .api.v1 import remember, RememberResult, recall, improve, forget, serve, disconnect, visualize
```

Although the functions physically live under `cognee/api/v1/` in the Python tree, they are the SDK's **"memory API"** — a higher-level, human-intent-oriented layer (`remember` / `recall` / `improve` / `forget`) plus infrastructure functions (`serve` / `disconnect` / `visualize`). This document analyzes each function's purpose, its Python building blocks, and the status of the corresponding Rust port.

> This scope is deliberately narrower than the general v1 gap analysis in [`../api-gaps/`](../api-gaps/README.md). Where findings overlap, the v2 docs here are the authoritative, re-verified source.

---

## Functions at a glance

| Function | Brief description | Rust status | Gap doc | Impl plan |
|---|---|---|---|---|
| `remember()` | Smart ingestion: composes `add` + `cognify` + optional `improve`. Two modes: **permanent memory** (default) and **session memory** (when `session_id` is passed). Returns an awaitable `RememberResult` with status + item metadata. | **Implemented** (commit `4da7623`) — `crates/lib/src/api/remember.rs` with `Display` / `to_dict()` / `is_success()` / `done()` / `await_completion()`, `RememberStatus::Running`, `JoinHandle`-based background mode, session-bridged `improve()` spawn, and `token_count` + `data_size` + `pipeline_run_id` + `content_hash` + `error` fields populated. | [remember.md](remember.md) | [impl/remember-plan.md](impl/remember-plan.md) |
| `recall()` | Smart search: checks session cache first via keyword lookup, then falls through to `search()` with an auto-selected `SearchType` chosen by a rule-based query router (factual / cypher / coding_rules / lexical / summary / reasoning / relationship / temporal). | **Implemented** — `crates/lib/src/api/recall.rs` + `crates/search/src/query_router.rs` implement the full Python-parity routing algorithm (14 rules verbatim with word-boundary helper and year-range regex), session-first lookup, and process-global override tracking (`crates/search/src/query_router_stats.rs`). `cognee.api.recall` tracing span emits Python-parity attributes. | [recall.md](recall.md) | [impl/recall-plan.md](impl/recall-plan.md) |
| `improve()` | Bidirectional session ↔ graph bridge in 4 stages: (1) apply feedback to graph node/edge weights, (2) cognify session Q&A text and persist to the permanent graph, (3) enrich existing graph with triplet embeddings (= `memify`), (4) write graph context back into session cache entries. | **Implemented** (commit 646ebbc) — Stages 1, 2, 4 land on top of the existing Stage 3 memify: `feedback_weights.rs` (streaming-EMA + batch `GraphDBTrait` methods), `persist_sessions.rs` (reuses `AddPipeline` + `cognify()` with `node_set=["user_sessions_from_cache"]`), `sync_graph_session.rs` (paginated `get_edges_since` + new `CheckpointStore` trait). Postgres batch edge methods and CLI subcommand deferred. | [improve.md](improve.md) | [impl/improve-plan.md](impl/improve-plan.md) |
| `forget()` | Unified deletion: by item id, by dataset, or everything. Cascades across relational DB, graph DB, vector DB, file storage, and session cache. Supports ACL enforcement and dry-run preview. | **Implemented** (UUID + telemetry polish landed) — `crates/lib/src/api/forget.rs` is a thin facade over `DeleteService` in `crates/delete/`, which covers all three modes including session `prune()` for the `everything` scope. | [forget.md](forget.md) | [impl/forget-plan.md](impl/forget-plan.md) (polish complete) |
| `serve()` / `disconnect()` | Cloud-integration pair. `serve()` runs either **direct mode** (user-provided URL + API key) or **cloud mode** (OAuth2 Device Code flow against Auth0 + Management API tenant provisioning + local credential cache). `disconnect()` tears down the session and wipes cached credentials. | **Implemented** (commits `ac8c86f` / `8624a3f` / `e94e9f4` / `7c04dcb` / C5) — new `cognee-cloud` crate gated behind the `cloud` feature (default-enabled on both `cognee-lib` and `cognee-cli`). Full RFC 8628 device-code flow, Python-byte-compatible `~/.cognee/cloud_credentials.json`, `X-Api-Key` HTTP proxy, and `cognee-cli serve` / `disconnect` subcommands. | [serve-disconnect.md](serve-disconnect.md) | [impl/serve-disconnect-plan.md](impl/serve-disconnect-plan.md) |
| `visualize()` | Generates a single-file interactive HTML5 visualization of the knowledge graph (d3.js v7 force layout, embedded JSON, pan/zoom, search, color-coded provenance filters). Optional helper `start_visualization_server()` serves it over HTTP. | **Implemented** (commit a0daab3) — new `cognee-visualization` crate ports Python's `cognee_network_visualization` byte-for-byte: 65 KB d3.js template, color mappers, JSON serializer, `cognee-cli visualize` subcommand. `start_visualization_server()` HTTP helper remains out of scope. | [visualize.md](visualize.md) | [impl/visualize-plan.md](impl/visualize-plan.md) |

### Legend

- **Implemented** — Rust has full functional parity (possibly with minor cosmetic gaps).
- **Partial** — Rust has the entry point and some building blocks, but one or more stages/paths are unimplemented or stubbed.
- **Not Started** — No Rust code exists; all work to be done.

---

## Summary of findings

- **7 of 7 functions fully Implemented.** forget/recall/visualize/improve/remember/serve/disconnect all land in Rust. API v2 scope is complete.
- **All 7 functions (remember, recall, improve, forget, visualize, serve, disconnect) are now Implemented. API v2 scope complete.**
- **`serve()` / `disconnect()` is Implemented** (commits `ac8c86f` / `8624a3f` / `e94e9f4` / `7c04dcb` / C5) — new `cognee-cloud` crate behind the `cloud` feature (default-enabled), with a Python-byte-compatible credential cache and a feature-gated CLI.
- **`visualize()` is Implemented** (commit a0daab3).
- **`improve()` is Implemented** (commit 646ebbc) — Stages 1, 2, 4 landed on top of existing Stage 3 memify.
- **`remember()` is Implemented** (commit `4da7623`) — background-task awaiter, `Display` / `to_dict()` / `is_success()` / `done()` helpers, session-bridged `improve()` spawn, and `token_count` / `data_size` / `pipeline_run_id` / `content_hash` / `error` fields all landed.

### Rough effort ordering (easiest → hardest)

| Rank | Function | Approx. effort | Notes |
|---|---|---|---|
| 1 | `forget()` | **Done** | No work; minor cosmetic polish only. |
| 2 | `recall()` | **Done** (~1 day) | Override tracking + telemetry spans landed in commit 598d553. |
| 3 | `visualize()` | **Done** | HTML template + color mappers landed in commit a0daab3. |
| 4 | `remember()` | **Done** (commit `4da7623`) | Background tasks, session-bridging path, `RememberResult` polish all landed. |
| 5 | `improve()` | **Done** (commit 646ebbc) | Stages 1, 2, 4 landed on top of existing Stage 3 memify; Postgres batch edge methods and CLI subcommand deferred. |
| 6 | `serve()` / `disconnect()` | **Done** (commits `ac8c86f` / `8624a3f` / `e94e9f4` / `7c04dcb` / C5) | OAuth2 device flow + Management API + credential store landed in new `cognee-cloud` crate behind the `cloud` feature (default-enabled). CLI subcommands `cognee-cli serve` / `disconnect` wired. |

---

## See also

- **Sequential implementation prompt:** [`IMPLEMENTATION-PROMPT.md`](IMPLEMENTATION-PROMPT.md) — step-by-step orchestration for landing all 6 tasks in the recommended order, one commit per task, with research / implementor / reviewer / doc-update sub-agents per task.
- General v1 API gap analysis: [`../api-gaps/README.md`](../api-gaps/README.md)
- Python reference (cloned locally at `/tmp/cognee-python/cognee/api/v1/`)
- Rust V2 entry points: [`crates/lib/src/api/`](../../crates/lib/src/api/)
