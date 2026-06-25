# OSS / closed split — implementation orchestrator prompt

> Paste this whole file as the opening prompt of a fresh session whose working
> directory is the **parent** of the cognee-rust workspace (i.e. the directory
> that contains `cognee-rust/`). You are the **orchestrator**: you do not write
> production code yourself — you launch sub-agents per the 5-step protocol below,
> hold the git state, and decide go/no-go between steps.

---

## 0. Mission

Execute the repository split described in
[`cognee-rust/docs/roadmap/oss-split-plan.md`](oss-split-plan.md) — the **single
source of truth**. Read it in full before doing anything. It defines the
rationale, locked decisions, the seams S1–S7, the phased plan, the risk register,
and the clean-birth algorithm (§8). This prompt defines only the *execution
mechanics*; when this prompt and the plan disagree, the plan wins (and you fix
this prompt's task list to match — see Step 1).

The work happens in **two trees**, never in the original checkout:

- **OSS tree** — a git **worktree** of `cognee-rust` on a new branch `oss-split`.
  All open-source work (seams, removals, crates.io readiness) happens here.
- **Closed tree** — a **new sibling repo** `cognee-cloud-rust` with its own fresh
  git history. Holds the cloud/auth/premium-adapter crates and depends on the OSS
  tree via a local `[patch]` during development.

**The original `cognee-rust` checkout on `main` is the safety net: never touch
it, never check out another branch in it, never commit to it.** Until **T17**
(registry publish, gated on explicit human approval), every action is reversible
by deleting the two new GitHub repos / force-pushing / `flip-oss-source.sh dev`.
T16 ships the reversible push-and-pin groundwork (private repos, tag, CI, leak
audit, closed flip to rev pin); T17 is the irreversible `cargo publish` /
`npm publish` step that burns names on public registries.

---

## 1. Paths (session root = parent dir)

```
<session-root>/
├── cognee-rust/          # EXISTING mixed repo, stays on `main` — DO NOT TOUCH
├── cognee-rust-oss/      # OSS worktree (branch oss-split) — create in T0
└── cognee-cloud-rust/    # CLOSED repo (fresh git) — create in T0
```

Resolve these to absolute paths once at start and pass them explicitly to every
sub-agent. Sub-agents operate **directly on these real directories** — they must
NOT create their own worktrees or isolation copies (the steps are sequential and
share state on purpose).

---

## 2. Invariants (enforce on every task)

1. **`cognee-rust` on `main` is read-only.** OSS edits go to `cognee-rust-oss`.
2. **Clean tree between tasks.** A task starts only when *both* repos have a clean
   `git status` (no staged/unstaged/untracked changes except the task ledger)
   and are on the expected branch. A task ends with a single commit per repo (or
   none, if the task touched that repo with no net change).
3. **One task at a time, in order.** No task starts until the previous one is
   committed and its validation gate is green.
4. **Tests run in debug** (no `--release`) unless the plan/README says otherwise.
5. **`.unwrap()` is forbidden in non-test code** — `expect("why-it-can't-fail")`
   or proper `?`/`map_err` propagation (see `cognee-rust/.claude/CLAUDE.md`).
6. **Reversibility.** Nothing is published to a public registry before T17. T17
   requires an explicit human "go" (and F8 — name reservations — must be done).
   T16's GitHub-push + tag + closed rev-pin are reversible by repo delete /
   tag delete / `flip-oss-source.sh dev`.

---

## 3. The per-task 5-step protocol

For each task in the ledger, run these five steps **strictly in order**. Each of
steps 1–4 is a **separate sub-agent** launched via the Agent tool; step 5 you (the
orchestrator) perform directly so a single actor owns the git state. Give every
sub-agent: the absolute paths of both repos, the path to the plan, the task id +
its plan references, and the relevant invariants above.

### Step 1 — Validate & prepare *(sub-agent)*
- Re-read the task's plan section(s); verify the task description and
  implementation steps are still accurate against the **current code** (check the
  `file:line` references — the code may have drifted since the plan was written).
- If the task spec is wrong or stale, **correct the ledger entry** (and, if the
  plan itself is wrong, note the needed plan fix) before proceeding.
- Confirm **both repos are clean** and on the expected branch
  (`cognee-rust-oss` → `oss-split`; `cognee-cloud-rust` → `main`). If not clean,
  **stop and report** — do not auto-discard changes.
- Output: a validated, code-checked task spec + which repo(s) this task touches +
  explicit confirmation of clean state. **Gate:** if state is dirty or the task
  is infeasible as written, halt the whole run and surface it to the human.

### Step 2 — Implement *(sub-agent)*
- Implement the validated task in the OSS tree and/or closed tree (per Step 1's
  determination — many early seam tasks touch only OSS).
- Follow all conventions (no `unwrap`, `thiserror` in libs / `anyhow` in bins,
  `Send + Sync` public traits, UUID5 determinism, `owner_id` in content hashes).
- Do **not** commit. Leave changes in the working tree for review.
- Output: a summary of what changed in each repo + any deviations from the plan
  and why.

### Step 3 — Review & fix *(sub-agent)*
- Review the working-tree diff in **both** repos. Judge: correct, clean,
  consistent with surrounding code, secure (no leaked secrets, no auth bypass, no
  closed concept names leaking into OSS files). Confirm the task is **actually
  implemented** by these changes (not partially / not stubbed).
- Fix what it finds. If it cannot make the change correct, **stop and report**.
- Output: review verdict + fixes applied. **Gate:** must end "task is correctly
  and completely implemented" or halt.

### Step 4 — Validate *(sub-agent)*
- Run the project check suite in **each** affected repo:
  `cargo fmt --check` → `cargo check --all-targets` → `cargo clippy -- -D warnings`
  → `scripts/check_all.sh` → the test suite (debug). For the OSS tree this is the
  existing `scripts/check_all.sh`; for the closed tree use its own check script
  (scaffolded in T0, kept in parity with the OSS one).
- Where the task changes the OSS-isolation surface, also run the OSS-only
  isolation check (per plan Phase-0 step 8).
- Fix fmt/clippy/test fallout. If a failure is fundamental, **stop and report**.
- Output: the actual command outputs (pass/fail), not a claim. **Gate:** all
  green, or halt.

### Step 5 — Mark done & commit *(orchestrator, you)*
- Tick the task in the ledger (`cognee-rust-oss/docs/roadmap/oss-split-tasks.md`)
  with a one-line result note, and apply any plan corrections Step 1 surfaced.
- Commit in each repo that changed, one commit per repo, message
  `split(<task-id>): <summary>`, ending with the trailer
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Verify both repos are clean again, then advance to the next task.

If any step halts: leave both trees as-is (do not revert), report the task id,
the failing step, and the exact blocker. Do not skip ahead.

---

## 4. Task ledger

Create `cognee-rust-oss/docs/roadmap/oss-split-tasks.md` in T0 as the live
checklist. Process tasks top-to-bottom. The references point into
[`oss-split-plan.md`](oss-split-plan.md); Step 1 of each task expands them.

| # | Task | Plan ref | Repos |
|---|------|----------|-------|
| **T0** | **Bootstrap.** Create the `oss-split` worktree off `main` HEAD; `git init -b main` the `cognee-cloud-rust` sibling with an empty initial commit; scaffold the closed workspace (root `Cargo.toml` with `[workspace]` + empty `[workspace.dependencies]`, `scripts/check_all.sh` mirroring OSS, `.gitignore`, README); create the task ledger. Verify the OSS worktree builds + `check_all.sh` passes (it is still the full mixed product at this point). | §8 prereq | both |
| T1 | S1 adapter-injection audit + builder hardening. | §4 S1 | OSS |
| T2 | S2 access-control extraction: `AclDb` trait stays OSS, impl→closed newtype; migration cleave (S2b: core baseline + new-named auth migration w/ `if_not_exists` + split `down()` + dialect-aware seeds); DB-free default user (S2c); remove `dyn AclDb` casts (S2d); move 13 auth entity files + `Role`/`Tenant` models, keep `User`+permission constants (S2e). **Highest risk — isolated, full test + cross-SDK parity.** | §4 S2, S2b–e | both |
| T3 | S3 `build_router` injection + OSS no-auth default path; move the 9 auth/cloud router files + `SyncRegistry` to closed; remove the `cognee-cloud` hard dep from OSS http-server; add `with_extra_validator` builder. | §4 S3 | both |
| T4 | S4 extract `cognee-vector-qdrant` + `cognee-llm-litert` to closed; confirm `cargo tree -e no-dev` shows zero git deps in OSS. | §4 S4 | both |
| T5 | Pure-Rust brute-force `VectorDB` impl + pgvector into OSS vector defaults. | §2 known gap, Phase 0.5 | OSS |
| T6 | S5 bindings reuse seam; isolate `ops/cloud.rs` so closed bindings depend on OSS `bindings-common` + add a cloud-ops module (no registry). | §4 S5, §6.1 | both |
| T7 | S6 isolate cloud re-exports; S7 remove `cloud` from the `default` feature set of lib, cli, bindings-common, python, neon. | §4 S6/S7 | both |
| T8 | Phase-0 exit: continuous OSS-isolation CI gate + partition manifest (`scripts/split/{oss,closed}-paths.txt`) covering 100% of tracked paths. | Phase 0 §8–9 | OSS |
| T9 | Phase-1 metadata: per-crate `description`/`repository`/`readme`/`keywords`/`categories`; dual `MIT OR Apache-2.0` + add `LICENSE-MIT`. | Phase 1.1 | OSS |
| T10 | Phase-1 readiness: `path`+`version` internal deps; docs.rs cfg for ONNX crates; `publish = false` on test-utils/bench/examples/telemetry-emit; pin the `cognee-litert-lm` rev; reserve names (crates.io/PyPI/npm); `cargo publish --dry-run` gate; release-plz. | Phase 1.2–8 | both |
| T11 | Phase-2 closed wiring: depend on OSS via `git`+`rev` for release with a `[patch]→path` dev override; concretize inherited `{ workspace = true }` deps in moved crates. | Phase 2.4–5 | closed |
| T12 | Phase-2 API-boundary contract tests in the closed repo (AclDb wrapper, build_router injection, adapter registration). | Phase 2.6 | closed |
| T13 | Phase-3 OSS CI workflows (lint/test/doc/publish-dry-run/bindings + tagged publish). | Phase 3 | OSS |
| T14 | Phase-3 closed CI workflows (lint/test/build-vs-pinned-rev/private-registry publish/scheduled rev bump). | Phase 3 | closed |
| T15 | Phase-4 bindings distribution config (OSS npm/PyPI/C; closed private registries reusing the OSS op wrappers). | Phase 4 | both |
| **T16** | **Clean-birth groundwork (reversible).** Refresh §8 prose for push-as-is + new repo names (`cognee-rs`, `cognee-cloud-rs`); push OSS `oss-split` → `cognee-rs`; push closed `main` → `cognee-cloud-rs`; stand up Phase-3 CI on both remotes (green); leak audit on the pushed OSS history; tag OSS `v0.1.0` + push tag (capture `OSS_REV`); flip closed manifests path → `git+rev = $OSS_REV`; closed-against-pinned-rev smoke; final `cargo publish --dry-run` topological sweep. Every step undoable via repo delete / tag delete / `flip-oss-source.sh dev` / `git revert`. Repos stay PRIVATE; history cleanup happens later before going public. | §8 (push-as-is rewrite), Steps 1–4, 5 (partial), 6–7 | both |
| **T17** | **Registry publish (irreversible — requires explicit human "go" + F8 done).** `cargo publish` 24 OSS crates to crates.io in topological order; `npm publish cognee-ts` + 7 prebuilt platform packages. Names burn forever; npm has 24h unpublish window only. PyPI + C-API GH-release tarballs explicitly removed — users build from sources. | §8 Step 5 (publish portion) | OSS |

Notes for Step 1 to resolve per task: the plan does seams "in one repo" then physically splits (Phase 2); in *this* execution, crates destined for closed (access-control in T2, qdrant/litert in T4, cloud routers in T3) are created **directly in the closed sibling** and wired via the dev `[patch]`, so the "physical split" is incremental and Phase 2 reduces to wiring + contract tests. If a task is cleaner done OSS-first-then-moved, Step 1 may say so — defer to the plan + code reality.

---

## 5. Guardrails & stop conditions

- **Stop and ask the human** when: a repo is unexpectedly dirty; the plan and code
  contradict in a way Step 1 can't safely resolve; a validation failure is
  structural (not a quick fix); or you reach **T17** (always pause for go/no-go —
  registry publish is irreversible).
- **Never** force-push, rewrite history of `main`, or run destructive git on the
  original checkout.
- **Cross-SDK parity** (`e2e-cross-sdk`) must be considered after T2/T3/T7 since
  those touch IDs/auth/defaults; flag if you cannot run it.
- Keep the closed repo's `scripts/check_all.sh` in parity with the OSS one as the
  OSS surface evolves.
- Prefer small, reviewable commits; one task = one commit per touched repo.

Begin with **T0**. Report after each task's Step 5 with: task id, what changed in
each repo, the validation result, and the next task.
