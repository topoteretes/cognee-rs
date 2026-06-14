# Bindings Parity & Maturity Plan

This directory tracks the work needed to bring the three language bindings —
**Python (PyO3)**, **C API (FFI)**, and **JavaScript/TypeScript (Neon)** — to the
same maturity level across five dimensions:

1. **Correctness** — no reachable panics, no silently-wrong results.
2. **Code idiomaticity** — the API feels native to the target language.
3. **Cleanliness** — no duplicated logic, no rule violations (`unwrap()`), generated artifacts not hand-maintained.
4. **Documentation** — README, reference docs, and inline docs are complete and accurate.
5. **Example coverage** — runnable examples cover at least the core `add → cognify → search` flow and the main ops.

It is the follow-up to the binding review of 2026-06-13. Each task below has a
dedicated subdocument with a step-by-step implementation plan, acceptance
criteria, and verification steps. Update the **Status** column here as work lands.

To **implement** these tasks with an automated, sub-agent-driven workflow (a
4-step validate → implement → review → commit scheme per task, suitable for a
Sonnet-class model), follow [EXECUTION-PROMPT.md](EXECUTION-PROMPT.md).

## Maturity baseline (review of 2026-06-13)

| Dimension | Python (PyO3) | C API (FFI) | JS/TS (Neon) |
|---|---|---|---|
| Functionality coverage | Strong vs Rust; **weak as drop-in `cognee`** | Strong (one broken engine path) | Strong |
| Security & correctness | Strong | **Adequate** (reachable `unwrap`, no `catch_unwind`) | Strong |
| Idiomaticity | Adequate | Strong | Strong |
| Examples | **Missing** | Strong | Adequate |
| Documentation | Adequate | Strong (header drift) | Strong |

All three share `crates/bindings-common` (portable op bodies + `SdkError` →
stable `code()` mapping), which is why their SDK surfaces line up 1:1. The
target end-state is: **C API correctness and cleanliness raised to match Python's
safety engineering; Python idiomaticity, typing, examples, and SDK parity raised
to match JS; example and documentation coverage uniform across all three.**

## Task list

Priority: **P0** = correctness/safety (do first), **P1** = idiomaticity/cleanliness/distribution, **P2** = docs/examples/polish.
Status: `Not started` · `In progress` · `Blocked` · `Done`.

| ID | Task | Binding | Dimension | Prio | Status | Plan |
|----|------|---------|-----------|------|--------|------|
| CR-1 | Eliminate reachable `unwrap()` on FFI paths; enforce panic safety (`catch_unwind` + `panic = "abort"`) | C API | Correctness | P0 | Done | [01-capi-panic-safety.md](01-capi-panic-safety.md) |
| CR-2 | Fix empty-task pipeline in `execute_async`/`execute_in_background` | C API | Correctness | P0 | Done | [02-capi-pipeline-async-tasks.md](02-capi-pipeline-async-tasks.md) |
| CL-1 | Generate C header via cbindgen; add CI symbol-diff to stop drift | C API | Cleanliness/Docs | P1 | Done | [03-capi-header-cbindgen.md](03-capi-header-cbindgen.md) |
| ID-1 | Drop-in `cognee` SDK API parity (module-level functions, package alias, `SearchType`) | Python | Idiomaticity/Functionality | P1 | Done | [04-python-sdk-parity.md](04-python-sdk-parity.md) |
| ID-2 | Typed inputs & options (TypedDict + kwargs), result-key convention | Python | Idiomaticity | P1 | Done | [05-python-typing-stubs.md](05-python-typing-stubs.md) |
| DOC-1 | Ship `.pyi` type stubs (or remove the misleading `py.typed`) | Python | Documentation | P1 | Done | [05-python-typing-stubs.md](05-python-typing-stubs.md) |
| PKG-1 | Declare test/dev deps (`pytest-asyncio`) and example deps in `pyproject.toml` | Python | Cleanliness | P2 | Done | [06-python-packaging-tests.md](06-python-packaging-tests.md) |
| PKG-2 | Prebuild matrix / source-build fallback for the native `.node` addon | JS | Distribution | P1 | Done | [07-js-distribution.md](07-js-distribution.md) |
| ID-3 | Surface notebooks/users/pipeline-run ops as `Cognee` sub-objects | JS | Idiomaticity/Functionality | P1 | Not started | [08-js-types-and-surface.md](08-js-types-and-surface.md) |
| ID-4 | Replace `any` result types with typed interfaces | JS | Idiomaticity | P1 | Not started | [08-js-types-and-surface.md](08-js-types-and-surface.md) |
| CR-3 | Remove stray `unwrap()` in Neon `task.rs`; annotate lock poisoning | JS | Correctness/Cleanliness | P2 | Not started | [08-js-types-and-surface.md](08-js-types-and-surface.md) |
| EX-1 | Example parity: Python example scripts + JS example expansion + npm scripts | Python, JS | Examples | P2 | Not started | [09-examples-parity.md](09-examples-parity.md) |
| CL-2 | Hoist `SECRET_FIELDS` redaction into `bindings-common` (dedup ×3) | All | Cleanliness | P2 | Not started | [10-shared-cleanliness.md](10-shared-cleanliness.md) |
| DOC-2 | Documentation parity (README core-flow, docstring/header parity, parity matrix) | All | Documentation | P2 | Not started | [11-documentation-parity.md](11-documentation-parity.md) |

## Suggested sequencing

1. **Wave 1 — correctness (P0):** CR-1, CR-2. These are shippable bug fixes with no API impact; land them first and independently.
2. **Wave 2 — idiomaticity & distribution (P1):** ID-1/ID-2/DOC-1 (Python), ID-3/ID-4 (JS), CL-1 (C API), PKG-2 (JS). These are the bulk of the "raise to parity" effort and can run in parallel per binding.
3. **Wave 3 — cleanliness, examples, docs (P2):** CL-2, EX-1, CR-3, PKG-1, DOC-2. Polish that depends on the Wave-2 surfaces being settled.

## Definition of "at parity"

The bindings are at equal maturity when, for every dimension in the baseline
table, all three columns read **Strong** (or the gap is explicitly documented as
a shared-core limitation, e.g. `SearchType` types that the Rust core does not yet
implement). Each task doc lists its own acceptance criteria; this index is done
when every row is `Done` and the baseline table has been re-scored.
