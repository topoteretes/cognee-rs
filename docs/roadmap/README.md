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

### AWS Bedrock provider (issue #17)

| Doc | Role |
|-----|------|
| [bedrock-provider-plan.md](bedrock-provider-plan.md) | The last tier of issue #17. **All of the Rust work (R1–R8) and the in-repo parity work (P2, P3, P4) have landed**; the doc stays for the one item that has not — **§5 P1**, adding `Literal["bedrock"]` to Python's `LLMConfigInputDTO.provider` in `topoteretes/cognee`, which cannot land from this repository — plus the optional **§5 P6**. Its §1 (wire spec) and §6 (decisions/caveats) remain the reference that the Bedrock source comments, `docs/http-server/routers/settings.md` and the cross-SDK parity test link into, so it is not deleted until P1 lands and those references are re-pointed. |

## Conventions

- When a planned item lands, delete its sub-document (the code is the record) and
  flip its status in the parent index to ✅ Implemented.
- When a whole plan is complete, move whatever durable design rationale is worth
  keeping into the relevant component docs and drop the plan from this folder.
- New gaps go in `not-implemented.md`; new open design choices go in
  `open-questions.md`; a new multi-step effort gets its own index + sub-docs here.
