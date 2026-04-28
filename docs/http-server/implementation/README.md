# HTTP Server — Implementation Guide

This directory holds the **step-by-step implementation tracking** for the `cognee-http-server` work. One document per phase, structured as numbered actions with explicit file paths, function names, and test cases.

Audience: an implementor (human or model) who has read the design docs and now needs to execute. The design **why** lives in the design docs ([../plan.md](../plan.md), [../architecture.md](../architecture.md), [../pipelines.md](../pipelines.md), etc.); these implementation docs cover **how**, with references to the design docs whenever a decision needs justification.

**Driver prompt for an autonomous implementor**: [IMPLEMENTATION-PROMPT.md](IMPLEMENTATION-PROMPT.md) — a self-contained meta-prompt that walks a less-powerful model through every task in this directory using a four-agent pipeline (investigate → implement → review → update docs). Read it once at session start; follow its instructions per task.

## Doc structure (every phase doc follows this)

1. **Goal** — one paragraph.
2. **References** — links to the design docs that own the rationale (no duplication).
3. **Prerequisites** — phases that must land before this one.
4. **Step-by-step** — numbered, atomic actions. Each step lists the file(s) it touches, the spec it implements, and how to verify.
5. **Tests** — concrete file list with what each test covers.
6. **Acceptance criteria** — checkboxes for "phase Done".
7. **Files touched** — a short index of every file the phase creates or modifies.

## Status legend

- **Not started** — doc not yet written.
- **Draft** — implementation doc landed, ready for review.
- **Approved** — doc reviewed; ready to implement against.
- **In Progress** — implementation underway.
- **Done** — phase shipped, all tests pass, status reflected in [../routers/README.md](../routers/README.md) status table.

## Status table

| # | Doc | Scope | Effort | Status |
|---|---|---|---|---|
| P0 | [p0-foundation.md](p0-foundation.md) | New `cognee-http-server` crate skeleton, standalone binary, `AppState`, `ApiError`, CORS, OpenAPI bootstrap, root `/`, health router, integration test scaffold. | 1 day | **Done** (commit 323e3e1) |
| P1 | [p1-auth.md](p1-auth.md) | JWT + cookie + `X-Api-Key` auth, login / logout / me, `AuthenticatedUser` extractor, register / reset / verify (email-stubbed), `users` CRUD, `users/get-user-id`, api-keys CRUD. SeaORM migration for `users`, `user_api_key`. | 2 days | **Done** (commit 0459963) |
| P2 | [p2-write-path.md](p2-write-path.md) | `/add`, `/update`, `/datasets` (CRUD + graph + raw download + schema), `/ontologies`, `/delete` (deprecated), `/forget`. Multipart streaming. | 3 days | **Done** (commit 3b4ae9e) |
| P3-pre | [p3-prereq-library-refactor.md](p3-prereq-library-refactor.md) | Library refactor: drop `run_in_background` parameter from `cognee_lib::api::remember()` and `cognee_lib::api::improve()`. Adds the `pipeline_watcher` slot to `TaskContext`. Lands `cognee_core::PipelineRunRegistry` and the `PipelineRunRepository` trait. | 2 days | **Done** (commit 2425f19) |
| P3 | [p3-pipelines-and-websocket.md](p3-pipelines-and-websocket.md) | `/cognify` (POST + WebSocket), `/memify`, `/remember`, `/improve`. Wires the HTTP-side dispatcher onto the `PipelineRunRegistry` from the prereq. | 3 days | **Done** (commit 53b1da0) |
| P4 | [p4-read-path.md](p4-read-path.md) | `/search`, `/recall`, `/llm`, `/visualize`. | 2 days | **Done** (commit 3e10c70) |
| P5 | [p5-admin.md](p5-admin.md) | `/permissions` (13 endpoints), `/settings`, `/configuration`. SeaORM migration for `principals`, `tenants`, `roles`, `user_roles`, `user_tenants`, `permissions`, `acls`, default-permission tables. | 3 days | **Draft** |
| P6 | [p6-observability.md](p6-observability.md) | `/activity` (5 endpoints, span buffer, markdown export), `/sync`, `/checks`. `SpanBufferLayer` tracing layer. | 2 days | **Draft** |
| P7 | [p7-advanced.md](p7-advanced.md) | `/notebooks` (storage only — `/run` returns 501), `/responses` (501 stub), email flows for register / reset / verify with pluggable `Mailer` (default no-op). | 2 days | **Draft** |
| P8 | [p8-e2e-parity.md](p8-e2e-parity.md) | Cross-SDK HTTP parity harness — uvicorn ↔ `cognee-http-server` side-by-side in one Docker container, pytest + `httpx` clients, ~27 test files mapped to phases. | 2 days | **Draft** |

**Total**: ~22 engineer-days for feature-complete; ~12 days to "core pipeline via HTTP works end-to-end" (P0–P3 + a slice of P4).

## How to use this directory

1. **Implementor** (per phase): start at the phase's doc; follow the steps in order; do not skip steps; mark a checkbox in §6 once the step lands.
2. **Reviewer**: each phase PR touches only the files in §7 of its phase doc. Diffs outside that list are out of scope and should be split.
3. **Doc maintainer**: when a step lands, update its checkbox; when the phase is fully landed, flip the status table above and the status row in [../routers/README.md](../routers/README.md).

## Invariants for every phase doc

- **No design rationale**. Link to the design doc instead. If a step requires a decision that's not yet pinned in a design doc, mark it `TBD` in §6 and add an open question to [../plan.md §7](../plan.md#7-open-questions).
- **Atomic steps**. Each step is one commit. If a step would produce a >300-line diff, split it.
- **Verifiable**. Every step has a `Verify:` clause — usually `cargo check`, `cargo test --test <name>`, or a curl example.
- **Strict-Python-parity**. Every wire-visible behavior matches Python verbatim. The two acknowledged divergences (registry eviction default; shutdown writes ERRORED rows) are documented in [../pipelines.md §1](../pipelines.md#1-goals--non-goals) and never extended.

## See also

- Root index: [../plan.md](../plan.md).
- Snapshot audit (closed): [../audit-findings.md](../audit-findings.md).
- Per-router specs: [../routers/](../routers/).
