# Roadmap — work to do

This folder is the single home for every doc that describes **outstanding work**
in cognee-rust: known gaps, deferred features, unresolved design decisions, and
active implementation plans. Docs that describe *already-shipped* components live
elsewhere (`../http-server/`, `../observability/`, `../cli/`, `../performance/`,
etc.) — this folder is only for things still to be done.

## Gaps & open decisions

| Doc | What it tracks |
|-----|----------------|
| [not-implemented.md](not-implemented.md) | Capabilities intentionally deferred, out of scope, or stubbed (S3, legacy binary office formats, partial `improve()` stages, multi-replica WS fan-out, …). Verified against the code. |
| [open-questions.md](open-questions.md) | Cross-cutting design *decisions* that are still open (auth params, tenancy/RBAC scope, sampling, …) — choices to be made, not missing features. |

## Implementation plans

Each plan has an index doc plus one sub-document per remaining work item.

### Fully-compatible cognify (COG-4457)

| Doc | Role |
|-----|------|
| [cognify-compatibility-plan.md](cognify-compatibility-plan.md) | Index + decision log. Items 1, 2, 4, 5 have landed; only **Item 3** remains. |
| [pghybrid-full-adapter.md](pghybrid-full-adapter.md) | Item 3 — full `PgHybridAdapter` + unified-engine wiring (the one remaining milestone). |

## Conventions

- When a planned item lands, delete its sub-document (the code is the record) and
  flip its status in the parent index to ✅ Implemented.
- When a whole plan is complete, move whatever durable design rationale is worth
  keeping into the relevant component docs and drop the plan from this folder.
- New gaps go in `not-implemented.md`; new open design choices go in
  `open-questions.md`; a new multi-step effort gets its own index + sub-docs here.
