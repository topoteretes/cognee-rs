# Implementation Prompts — COG-4454 Cognee Core

**Parent:** [../cog-4454-core-implementation-plan.md](../cog-4454-core-implementation-plan.md)

This document holds a ready-to-run prompt per gap. Each prompt drives the same
**3-step scheme**:

1. **Implement** — a sub-agent re-validates the gap doc against current code (the
   plan may be stale), fixes the doc if needed, then implements per the plan.
2. **Review** — a fresh sub-agent reviews the diff for correctness, security,
   consistency, and fidelity to the gap doc; runs formatting/build/clippy/tests;
   fixes anything it finds.
3. **Land** — flip the gap status to ☑ Implemented (in the sub-doc header **and**
   the root index table) and commit with a meaningful, gap-matching message.

## How to use

Work the gaps **strictly in order: 1 → 2 → 3** — this is not optional. Gap 1
creates `crates/core/src/sentinels.rs` and the `&dyn Value` downcast helper that
Gap 2 reuses; Gap 3 touches the executor retry loop / `ExecEnv` and must come
last. Paste the chosen gap's prompt to the orchestrator; it spawns the sub-agents
for steps 1–2 and performs step 3 itself.

### Conventions every step must honor

- **Build/test policy** (per repo CLAUDE.md): `cargo check --all-targets` for
  compilation; run tests in **debug** mode (no `--release`), scoped with
  `cargo test -p cognee-core`; finish with `scripts/check_all.sh` (fmt, check,
  clippy `-D warnings`, and the C/Python/JS binding checks).
- **No `unwrap()` in non-test code** — use `expect("why it cannot fail")` or
  proper `?`/error propagation. `Mutex/RwLock` lock `unwrap()` is allowed with a
  `// lock poison is unrecoverable` comment.
- **Object-safety** — `cognee-core` traits are `Send + Sync` + `async_trait`. Any
  new trait (e.g. `RateLimiter` in Gap 3) must stay object-safe and be used as
  `Arc<dyn …>`.
- **Python parity** — the Python reference is the source of truth. If absent,
  clone it: `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python`.
- **Commit message footer** — end the commit body with:
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **Branch** — work on `feature/cog-4454-cognee-core` (the issue branch); if on
  another branch (e.g. `main`), create/switch to it first. Do not push unless
  asked.
- **Status flip (step 3)** — set `**Status:** ☑ Implemented (<short-sha>)` in the
  sub-doc header and change the matching row's Status cell in the root index from
  `☐ Not started` to `☑ Implemented`.

---

## Reusable step templates

The per-gap sections below fill in `{{GAP_DOC}}`, `{{GAP_TITLE}}`, and
`{{COMMIT_SCOPE}}`.

### Step 1 prompt (Implementation sub-agent — general-purpose)

> You are implementing **{{GAP_TITLE}}** in the cognee-rust repo
> (`/home/dmytro/dev/cognee/cognee-rust`), crate `cognee-core` (`crates/core/`).
> The gap spec is `{{GAP_DOC}}`.
>
> 1. **Re-validate the spec against the current code first.** The plan was written
>    earlier and may be stale. For every file/line/symbol the doc references
>    (`tasks.rs`, `execute_from`, `TaskInfo`, `ExecEnv`, `call_with_retry`,
>    `RetryPolicy`, `sentinels.rs`, etc.), open it and confirm it still exists and
>    says what the doc claims. Check the Python reference too (clone to
>    `/tmp/cognee-python` if missing). If anything is wrong (moved code, changed
>    signatures, an already-done sub-step, a mismatched Python reference), **fix
>    the gap doc** to match reality before coding, and note what you changed.
> 2. **Implement the gap** exactly per the (corrected) spec. Follow repo
>    conventions: no `unwrap()` in non-test code, `thiserror` for errors,
>    object-safe `Send + Sync` traits used via `Arc<dyn …>`, async via tokio. Add
>    or extend unit + integration tests for `cognee-core` as the spec's acceptance
>    criteria require.
> 3. **Verify locally**: `cargo check --all-targets` must pass; run
>    `cargo test -p cognee-core` in debug mode (no `--release`). Do **not** commit
>    — leave the working tree dirty for review.
> 4. Report: what you changed in the doc (if anything), the files you touched, the
>    commands you ran with their results, and any acceptance criterion you could
>    not satisfy and why.

### Step 2 prompt (Review sub-agent — general-purpose, fresh context)

> You are reviewing an **uncommitted** change implementing **{{GAP_TITLE}}** in
> `/home/dmytro/dev/cognee/cognee-rust` (crate `cognee-core`). The spec is
> `{{GAP_DOC}}`; the diff is in the working tree (`git diff` / `git status`).
>
> Review for:
> - **Correctness** — does the code do what the spec describes? Trace the executor
>   path; check the sentinel downcast/identity logic (Gaps 1–2) or the limiter
>   acquire-inside-retry placement (Gap 3); check edge cases, cancellation
>   interaction, and Python parity where claimed.
> - **Spec fidelity** — does the implemented state match the gap doc and its
>   acceptance criteria? If the code is right but the doc is now inaccurate, fix
>   the doc.
> - **Security** — no `unwrap()`/panics on the hot path, no unbounded growth, no
>   secrets logged; for rate limiting, no deadlock/starvation and correct behavior
>   under cancellation.
> - **Consistency** — naming, error types (`thiserror`), object-safe trait usage,
>   `Arc<dyn …>` plumbing, and that Gap 2 actually reuses Gap 1's `sentinels.rs`
>   rather than duplicating it.
> - **Checks** — run `scripts/check_all.sh` (fmt, `cargo check --all-targets`,
>   clippy `-D warnings`, binding checks) and `cargo test -p cognee-core` in debug
>   mode. Capture the actual output.
>
> **Fix** anything you find (code, tests, or doc) directly. Do not commit. Report
> a verdict (approve / changes-made), the issues found, the fixes applied, and the
> final `scripts/check_all.sh` result. If a check genuinely cannot run here, say
> so explicitly rather than claiming it passed.

### Step 3 (orchestrator — status flip + commit)

After the review sub-agent approves and `scripts/check_all.sh` is green:

1. Set `**Status:** ☑ Implemented (<short-sha-after-commit>)` in `{{GAP_DOC}}` and
   flip the matching Status cell in
   [../cog-4454-core-implementation-plan.md](../cog-4454-core-implementation-plan.md) from `☐ Not started` to
   `☑ Implemented`. (Stage the doc edits in the same commit; backfill the SHA via
   `--amend` or note it in the row — pick one and stay consistent.)
2. Commit on `feature/cog-4454-cognee-core` with a message matching the gap, e.g.
   `{{COMMIT_SCOPE}}`. Include a short body summarizing what changed and any
   deferred acceptance criteria. End with the `Co-Authored-By` footer. Do **not**
   push unless asked.

---

## Gap 1 — Drop / filter sentinel

- `{{GAP_DOC}}` = [01-drop-sentinel.md](./01-drop-sentinel.md)
- `{{GAP_TITLE}}` = "drop/filter sentinel — let a task signal 'discard this item' (creates the shared sentinels.rs module)"
- `{{COMMIT_SCOPE}}` = `feat(core): add drop/filter sentinel for pipeline item discarding`

Run **Step 1** → **Step 2** → **Step 3**. This gap creates `sentinels.rs` and the
`&dyn Value` downcast helper — get the module shape right, since Gap 2 builds on
it. **Must be done before Gap 2.**

## Gap 2 — Enrichment mode (`enriches` flag)

- `{{GAP_DOC}}` = [02-enrichment-mode.md](./02-enrichment-mode.md)
- `{{GAP_TITLE}}` = "enrichment mode — enriching task returns input unchanged via a PassthroughSentinel + enriches flag on TaskInfo"
- `{{COMMIT_SCOPE}}` = `feat(core): add enrichment mode (enriches flag) with passthrough sentinel`

Run only **after Gap 1 is committed.** Step 1's re-validation must confirm Gap 1's
`sentinels.rs` and the `execute_from` insertion point landed as the doc describes,
and that this gap **reuses** that module (adds `PassthroughSentinel` to it) rather
than duplicating logic. Step 1 → Step 2 → Step 3.

## Gap 3 — Rate limiting

- `{{GAP_DOC}}` = [03-rate-limiting.md](./03-rate-limiting.md)
- `{{GAP_TITLE}}` = "rate limiting — token-bucket / concurrency limiter for LLM & HTTP tasks, acquired inside the retry loop"
- `{{COMMIT_SCOPE}}` = `feat(core): add token-bucket rate limiter threaded through ExecEnv/call_with_retry`

Run **last** — it is the only medium-effort gap and the only one that changes
executor plumbing (new `rate_limiter.rs`, `Option<Arc<dyn RateLimiter>>` through
`Pipeline → ExecEnv → call_with_retry`, acquire inside the retry loop). Step 1's
re-validation must confirm the current `call_with_retry`/`ExecEnv`/`RetryPolicy`
shapes before threading the limiter. Pay attention in Step 2 to limiter behavior
under cancellation and retry (acquire must not double-count or deadlock). Step 1 →
Step 2 → Step 3.

---

## Notes for the orchestrator

- Spawn step-1 and step-2 agents as **separate** sub-agents so the reviewer has
  fresh context and does not rubber-stamp its own work.
- If a step-1 agent reports it changed the gap doc, tell the step-2 reviewer to
  validate against the **updated** doc.
- Each gap is one Step-1/2/3 cycle. Do not start a gap until the previous one is
  committed — the ordering (shared `sentinels.rs`, then executor plumbing) is a
  hard dependency, not a preference.
- If `scripts/check_all.sh` cannot fully run (e.g. binding toolchains absent),
  record exactly which sub-checks ran and which were skipped — never report a
  skipped check as passed.
