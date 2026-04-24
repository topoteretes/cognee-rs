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
| `remember()` | Smart ingestion: composes `add` + `cognify` + optional `improve`. Two modes: **permanent memory** (default) and **session memory** (when `session_id` is passed). Returns an awaitable `RememberResult` with status + item metadata. | **Partial** — core composition exists (`crates/lib/src/api/remember.rs`); session-bridging path, `RememberResult` Display/serialization, background-task awaiting, and `token_count` field are missing. | [remember.md](remember.md) | [impl/remember-plan.md](impl/remember-plan.md) |
| `recall()` | Smart search: checks session cache first via keyword lookup, then falls through to `search()` with an auto-selected `SearchType` chosen by a rule-based query router (factual / cypher / coding_rules / lexical / summary / reasoning / relationship / temporal). | **Partial (~95%)** — `crates/lib/src/api/recall.rs` + `crates/search/src/query_router.rs` implement the full routing algorithm and session-first lookup. Missing: global override-tracking counter, telemetry/span parity. Usable today. | [recall.md](recall.md) | [impl/recall-plan.md](impl/recall-plan.md) |
| `improve()` | Bidirectional session ↔ graph bridge in 4 stages: (1) apply feedback to graph node/edge weights, (2) cognify session Q&A text and persist to the permanent graph, (3) enrich existing graph with triplet embeddings (= `memify`), (4) write graph context back into session cache entries. | **Partial** — Stage 3 is fully implemented by `cognee-cognify::memify`. Stages 1, 2, 4 exist only as stubs in `crates/lib/src/api/improve.rs` that log intent but perform no work. Session store lacks feedback fields on the public type. | [improve.md](improve.md) | [impl/improve-plan.md](impl/improve-plan.md) |
| `forget()` | Unified deletion: by item id, by dataset, or everything. Cascades across relational DB, graph DB, vector DB, file storage, and session cache. Supports ACL enforcement and dry-run preview. | **Implemented** (UUID + telemetry polish landed) — `crates/lib/src/api/forget.rs` is a thin facade over `DeleteService` in `crates/delete/`, which covers all three modes including session `prune()` for the `everything` scope. | [forget.md](forget.md) | [impl/forget-plan.md](impl/forget-plan.md) (polish complete) |
| `serve()` / `disconnect()` | Cloud-integration pair. `serve()` runs either **direct mode** (user-provided URL + API key) or **cloud mode** (OAuth2 Device Code flow against Auth0 + Management API tenant provisioning + local credential cache). `disconnect()` tears down the session and wipes cached credentials. | **Not Started** — no equivalent in Rust. Arguably out of scope for a library-level port; best implemented as a separate optional crate (e.g. `cognee-cloud`) behind a feature flag. | [serve-disconnect.md](serve-disconnect.md) | [impl/serve-disconnect-plan.md](impl/serve-disconnect-plan.md) |
| `visualize()` | Generates a single-file interactive HTML5 visualization of the knowledge graph (d3.js v7 force layout, embedded JSON, pan/zoom, search, color-coded provenance filters). Optional helper `start_visualization_server()` serves it over HTTP. | **Not Started** — graph reader (`GraphDBTrait::get_all_nodes/edges`) and file storage (`LocalStorage`) are in place, but the HTML template, color mappers, and JSON serializer bridging graph types to the viz format are missing. | [visualize.md](visualize.md) | [impl/visualize-plan.md](impl/visualize-plan.md) |

### Legend

- **Implemented** — Rust has full functional parity (possibly with minor cosmetic gaps).
- **Partial** — Rust has the entry point and some building blocks, but one or more stages/paths are unimplemented or stubbed.
- **Not Started** — No Rust code exists; all work to be done.

---

## Summary of findings

- **4 of 7 functions have at least partial Rust implementations.** The per-function docs correct several claims from the older v1 gap analysis (in particular, `forget()` is already complete, and `recall()` is ~95% complete — not "missing").
- **The two biggest remaining functional gaps are `improve()` and `serve()/disconnect()`.** The former is core to the V2 memory semantics (feedback loop); the latter is cloud integration that may warrant a separate crate.
- **`visualize()` is small** (S–M effort) and self-contained — a straightforward first target if we want to expand V2 coverage.
- **`remember()` is tantalizingly close** — permanent-memory mode works today; session-memory mode requires session-store feedback fields and background-task plumbing that are also prerequisites for `improve()` stages 2/4.

### Rough effort ordering (easiest → hardest)

| Rank | Function | Approx. effort | Notes |
|---|---|---|---|
| 1 | `forget()` | **Done** | No work; minor cosmetic polish only. |
| 2 | `recall()` | **S** (~1 day) | Override tracking + telemetry spans. |
| 3 | `visualize()` | **S–M** (~2–4 days) | HTML template + color mappers. |
| 4 | `remember()` | **M–L** (~1 week) | Background tasks, session-bridging path, `RememberResult` polish. |
| 5 | `improve()` | **L–XL** (~5–8 weeks) | Feedback fields on session types, batch graph property updates, session-text cognify variant, graph-to-session sync. |
| 6 | `serve()` / `disconnect()` | **XL** (~5–7 weeks) | OAuth2 device flow + Management API + credential store. Consider splitting into `cognee-cloud` crate. |

---

## See also

- **Sequential implementation prompt:** [`IMPLEMENTATION-PROMPT.md`](IMPLEMENTATION-PROMPT.md) — step-by-step orchestration for landing all 6 tasks in the recommended order, one commit per task, with research / implementor / reviewer / doc-update sub-agents per task.
- General v1 API gap analysis: [`../api-gaps/README.md`](../api-gaps/README.md)
- Python reference (cloned locally at `/tmp/cognee-python/cognee/api/v1/`)
- Rust V2 entry points: [`crates/lib/src/api/`](../../crates/lib/src/api/)
