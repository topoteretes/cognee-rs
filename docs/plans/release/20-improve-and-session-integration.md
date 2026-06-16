# 20 — `improve()` stages + session→search integration

> Wave 4 · Priority P2 (nice-to-have) · Track A · Release-blocking: no · Effort: 1.5d ·
> Depends on: — · Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) B4.2, B5.1 · [index](00-INDEX.md)

## Goal

Close two related parity gaps in the memory subsystem:

1. **`improve()` missing stages** — add (a) a persist-agent-trace-steps stage, (b) a
   `build_global_context_index` stage, and (c) a single-session improve lock so concurrent
   `improve()` calls on the same session don't duplicate work. The feedback-weight math
   already matches Python and **must be preserved**.
2. **Session→search integration** — make the search orchestrator (a) prepend the session's
   stored graph-context snapshot to the conversation history, (b) persist
   conversationally-detected feedback to the **prior** QA entry via `add_feedback`, and
   (c) populate `used_graph_element_ids` when saving a QA entry.

End state: Rust's `improve()`/search session loop produce the same provenance and context
behavior Python relies on for downstream memify/improve.

## Background & why

These are the "memory provenance" parity items (audit B4.2 + B5.1). Without them:

- **Concurrent `improve()` duplicates work** — Python takes a per-session lock; Rust runs
  unconditionally.
- **Agent traces are never persisted by `improve()`** — Python's persist-trace stage is the
  bulk of the agent use case.
- **No global context index** is built — Python optionally builds root/bucket summaries.
- **Search never reuses the stored graph snapshot** — Python prepends it to history so
  follow-up questions see prior graph knowledge.
- **Feedback in conversation is dropped** — Python detects "that was wrong" style feedback
  and writes it to the previous QA entry; Rust never does, starving the feedback-weights
  pipeline.
- **`used_graph_element_ids` is always `None`** in Rust's saved QA entries — so memify can't
  trace which graph elements produced an answer.

Good news: the **data structures already match** (`SessionQAEntry` has `context`,
`feedback_text`, `feedback_score`, `used_graph_element_ids`, `memify_metadata`; the
`add_feedback`/`get_graph_context`/`set_graph_context` methods exist on Rust's
`SessionManager`). The gaps are in the **wiring**, not the schema.

### Verified match (preserve, do not touch)

| Item | Python | Rust | Status |
|---|---|---|---|
| feedback normalize `(score-1)/4` | `apply_feedback_weights.py:43–50` | `feedback_weights.rs:69–77` | MATCH |
| EMA `w' = w + α(r-w)` clip+round 4dp | `apply_feedback_weights.py:53–59` | `feedback_weights.rs:83–95` | MATCH |
| default `feedback_alpha = 0.1` | `improve.py:130` | passed param | MATCH |
| `SessionQAEntry` fields | `cache/models.py:19–86` | `session/src/types.rs:19–41` | MATCH |
| `add_feedback` resets `feedback_weights_applied=false` | `session_manager.py:707–734` | `session_manager.rs:198–229` | MATCH |
| `sync_graph_to_session` (improve stage 4) | — | `memify/sync_graph_session.rs:143–245` | present |

## Prerequisites

```bash
git checkout -b task/20-improve-and-session-integration
```

Read first:

- Python:
  - `/tmp/cognee-python/cognee/api/v1/improve/improve.py` (lines ~130–213: lock,
    persist-traces, global-index stages).
  - `/tmp/cognee-python/cognee/infrastructure/locks/session_lock.py` (lines 76–100:
    `try_acquire_improve_lock` / `release_improve_lock`).
  - `/tmp/cognee-python/cognee/infrastructure/session/session_manager.py` (lines 435–450
    graph-context prepend; 492–525 add_feedback + used_graph_element_ids; 707–734
    `add_feedback`).
- Rust:
  - `crates/lib/src/api/improve.rs` (the 4-stage flow, ~127–424).
  - `crates/search/src/orchestration/search_orchestrator.rs` (~345–362 history load,
    ~400–413 save_qa).
  - `crates/session/src/session_manager.rs` (`save_qa` ~106–139, `add_feedback`
    ~198–229, `get_graph_context`/`set_graph_context` ~252–272).
  - `crates/session/src/types.rs` (`SessionQAEntry`, `UsedGraphElementIds` ~12–17, `SessionContext` ~46–50; ~19–41).

Re-grep:

```bash
grep -n "stages_run\|feedback_alpha\|try_acquire\|persist\|global_context\|MemifyConfig" crates/lib/src/api/improve.rs
grep -n "load_history_both\|save_qa\|graph_context\|used_graph_element_ids\|SessionContext" crates/search/src/orchestration/search_orchestrator.rs
grep -n "fn save_qa\|fn add_feedback\|fn get_graph_context\|fn set_graph_context" crates/session/src/session_manager.rs
grep -n "struct SessionContext\|graph_context" crates/session/src/types.rs
```

## Python reference

### improve.py (`/tmp/cognee-python/cognee/api/v1/improve/improve.py`)

- **Single-session lock (lines 136–150):** if exactly one `session_id`, call
  `try_acquire_improve_lock(sole_session)`; if it returns `False`, log and `return {}`
  (no-op). Store `acquired_lock_for` and release in a `finally`.
- **`try_acquire_improve_lock` (`locks/session_lock.py:76–100`):** non-blocking; module-level
  `set[str]` `_improving_sessions` guarded by an `asyncio.Lock`. Returns `True` iff claimed.
  Empty `session_id` → `True` (no-op). Caller must `release_improve_lock` in `finally`.
- **Persist-trace stage (lines 166–176):** `await _persist_session_traces(dataset,
  session_ids, user, run_in_background)`; appends `"persist_trace_steps"` to `stages_run`.
- **Global context index (lines 201–213):** when `build_global_context_index` is set and not
  background, `await _build_global_context_index(dataset, user)`; appends
  `"global_context_index"` to `stages_run`.

### session_manager.py (`/tmp/cognee-python/cognee/infrastructure/session/session_manager.py`)

- **Prepend graph snapshot (lines 435–450):** `graph_context = await
  self.get_graph_context(...)`; if present, cap to `max_context_chars` (param > config >
  unlimited) and prepend:

  ```
  "Background knowledge from the knowledge graph:\n" + graph_context + "\n\n" + conversation_history
  ```

- **Auto-feedback + provenance on save (lines 492–525):** if feedback was detected for the
  current turn, call `add_feedback(qa_id=last_qa_id, ...)` against the **previous** entry;
  then `add_qa(..., context=context_to_store, used_graph_element_ids=used_graph_element_ids)`.
- **`add_feedback` (lines 707–734):** delegates to `update_qa` and resets
  `memify_metadata[feedback_weights_applied] = False`.

## Implementation steps

### Part A — `improve()` stages

1. **Add a single-session improve lock.** Add a process-global registry in the session
   crate (parity with Python's module-level set). New file
   `crates/session/src/improve_lock.rs`:

   ```rust
   use std::collections::HashSet;
   use std::sync::Mutex;
   use std::sync::OnceLock;

   static IMPROVING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

   fn registry() -> &'static Mutex<HashSet<String>> {
       IMPROVING.get_or_init(|| Mutex::new(HashSet::new()))
   }

   /// Claim the improve-lock for `session_id`. Returns true iff acquired.
   /// Empty session id is a no-op (returns true). Caller MUST release in a guard.
   pub fn try_acquire_improve_lock(session_id: &str) -> bool {
       if session_id.is_empty() { return true; }
       // lock poison is unrecoverable
       let mut set = registry().lock().unwrap();
       set.insert(session_id.to_string())
   }

   pub fn release_improve_lock(session_id: &str) {
       if session_id.is_empty() { return; }
       // lock poison is unrecoverable
       registry().lock().unwrap().remove(session_id);
   }

   /// RAII guard: releases on drop so an early-return/panic can't leak the lock.
   pub struct ImproveLockGuard(Option<String>);
   impl ImproveLockGuard {
       pub fn acquire(session_id: &str) -> Option<Self> {
           if try_acquire_improve_lock(session_id) {
               Some(Self(Some(session_id.to_string())))
           } else { None }
       }
   }
   impl Drop for ImproveLockGuard {
       fn drop(&mut self) {
           if let Some(s) = self.0.take() { release_improve_lock(&s); }
       }
   }
   ```

   Export it from `crates/session/src/lib.rs`.

   > A sync `Mutex` is fine here: the critical section is a single `HashSet` op, never held
   > across `.await`. Do **not** use a tokio mutex held across awaits.

2. **Guard `improve()` on a single session.** In `crates/lib/src/api/improve.rs`, near the
   top of the flow, when `session_ids` has exactly one element:

   ```rust
   let _improve_guard = if let Some(sids) = session_ids.as_ref() {
       if sids.len() == 1 {
           match cognee_session::ImproveLockGuard::acquire(&sids[0]) {
               Some(g) => Some(g),
               None => {
                   info!(session_id = %sids[0], "improve: session already being improved, skipping");
                   return Ok(ImproveResult::default()); // empty result, parity with Python `return {}`
               }
           }
       } else { None }
   } else { None };
   ```

   The guard drops at function end (or any early return), mirroring Python's `finally`.

3. **Add the persist-agent-trace-steps stage.** Determine whether Rust already persists
   traces anywhere (`grep -rn "trace" crates/lib/src/api crates/session crates/cognify`).
   Python's `_persist_session_traces` reads `Trace`-type memory entries from the session(s)
   and writes them into the knowledge graph (similar to how QA entries get persisted in
   stage 2). Implement a stage that:
   - reads trace entries for the session(s) (Rust `MemoryEntry::Trace` already exists per
     the audit);
   - persists them via the same path stage 2 uses for QA persistence
     (`persist_sessions_in_knowledge_graph` / the add pipeline);
   - pushes `"persist_trace_steps"` onto `result.stages_run`.

   If the trace-persist primitive does not yet exist, scope it to: collect trace text,
   run it through the existing add→cognify persistence used by stage 2, gated behind the
   same `session_store`/`add_pipeline`/`db` availability check. Keep it warning-only on
   failure (matches the existing stage error handling).

4. **Add the `build_global_context_index` stage.** Add a `build_global_context_index: bool`
   parameter (default `false`, parity with Python's opt-in). When `true` and not running in
   background:
   - read the current knowledge graph;
   - build the global/root context summary (Python's `_build_global_context_index`
     summarizes the graph into a root + bucket index used by later completions);
   - store it where the search side can read it (the session graph-context store is the
     natural home — reuse `set_graph_context`, or a dedicated global key).
   - push `"global_context_index"` onto `result.stages_run`.

   If a full global-index implementation is out of scope for 0.1.0, implement the **stage
   plumbing + flag + stages_run entry** and have it summarize-and-store the graph context
   (a minimal but functional version), with a `// TODO(parity): bucket index` note. Document
   the partial in `docs/not-implemented.md`.

### Part B — session→search integration

5. **Add a `graph_context` field to `SessionContext`** in
   `crates/session/src/types.rs` (lines ~46–50; note: there is no
   `crates/search/src/orchestration/types.rs` — `SessionContext` is defined and re-exported
   from the session crate):

   ```rust
   pub struct SessionContext {
       pub session_id: Option<String>,
       pub history: Vec<Message>,
       pub formatted_history: String,
       /// Stored knowledge-graph snapshot to prepend to history (from improve()).
       pub graph_context: Option<String>,
   }
   ```

6. **Load + prepend the graph snapshot.** In
   `crates/search/src/orchestration/search_orchestrator.rs` where `session_context` is
   built (~345–362), after loading history, fetch and prepend the snapshot:

   ```rust
   let graph_context = sm
       .get_graph_context(Some(session_id), user_id_str.as_deref())
       .await
       .ok()
       .flatten();
   let formatted_history = if let Some(gc) = graph_context.as_deref().filter(|s| !s.is_empty()) {
       let gc = apply_context_char_limit(gc); // explicit param > config > unlimited
       format!("Background knowledge from the knowledge graph:\n{gc}\n\n{formatted_history}")
   } else {
       formatted_history
   };
   ```

   Match Python's exact prefix string `"Background knowledge from the knowledge graph:\n"`
   and the `"\n\n"` separator (a parity test can assert this). Implement
   `apply_context_char_limit` reading the same precedence Python uses (explicit param →
   config `max_session_context_chars` → unlimited).

7. **Populate `used_graph_element_ids` on save.** The retrievers that traverse the graph
   (graph-completion family) already know which node/edge IDs they used. Thread those IDs
   out of the retriever into the orchestrator and pass them to `save_qa`. This requires
   extending `SessionManager::save_qa` to accept the ids:

   - Change `crates/session/src/session_manager.rs` `save_qa` signature to take
     `used_graph_element_ids: Option<UsedGraphElementIds>` and forward it to
     `store.create_qa_entry(...)` (extend the store trait + impls accordingly).
   - In `search_orchestrator.rs` (~400–413), build `UsedGraphElementIds { node_ids,
     edge_ids }` from the retriever's used elements and pass it in.

   If a retriever doesn't traverse the graph (e.g. pure RAG), pass `None`.

8. **Persist conversationally-detected feedback to the prior QA entry.** Rust already has a
   feedback-detection path (`FEEDBACK` search type / `utils/feedback_detection.rs`). When a
   turn is detected as feedback on the previous answer:
   - look up the previous entry's `qa_id` for the session (the store can return the latest
     entry id; add a `latest_qa_id(session_id, user_id)` if absent);
   - call `sm.add_feedback(session_id, user_id, last_qa_id, feedback_text, feedback_score)`
     — this method already exists and resets `feedback_weights_applied=false`;
   - keep it warning-only on failure (Python wraps it in try/except, lines 502–516).

   Place this **before** saving the current turn's QA entry, mirroring Python's order
   (lines 492–525): feedback to the previous entry first, then `save_qa` for the new turn.

9. **(Stored-context payload)** — Note: whether the saved `context` should be `""`/summary
   vs full context is tracked separately as **B5.2 in [task 21](21-parity-backlog-misc.md)**.
   Do **not** change the stored-context behavior here; this task only adds
   `used_graph_element_ids` and the feedback wiring.

## Verification

```bash
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test -p cognee-session improve_lock
bash scripts/run_tests_with_openai.sh improve_session_integration
```

### Tests to add

1. **`improve_lock_excludes_concurrent`** (session crate, no LLM):
   - `try_acquire_improve_lock("s1")` → true; a second `try_acquire_improve_lock("s1")`
     → false; after dropping the guard, acquire again → true.
   - Empty session id always returns true.

2. **`improve_skips_when_locked`** (lib): hold the lock for `s1`, call `improve()` with
   `session_ids=[s1]`, assert it returns an empty/default result and runs no stages.

3. **`search_prepends_graph_context`** (search): set a graph snapshot via
   `set_graph_context`, run a session search, assert the formatted history fed to the LLM
   begins with `"Background knowledge from the knowledge graph:\n"` followed by the
   snapshot. (Can assert on the prompt the mock LLM receives.)

4. **`save_qa_populates_used_graph_element_ids`** (search + session): run a
   graph-completion search in a session, load the saved entry, assert
   `used_graph_element_ids` is `Some` with non-empty `node_ids`.

5. **`conversational_feedback_persists_to_prior_entry`** (search + session): save a QA
   entry, then issue a feedback-style turn, assert the **prior** entry now has
   `feedback_text`/`feedback_score` set and `memify_metadata.feedback_weights_applied ==
   false`.

6. **Preserve-math regression** — keep the existing feedback-weights tests green
   (`cargo test -p cognee-cognify feedback`); assert no change to normalize/EMA.

## Acceptance criteria

- [ ] `try_acquire_improve_lock`/`release_improve_lock` + RAII guard exist and are exported.
- [ ] `improve()` no-ops (empty result) when the single target session is already being
      improved; the lock releases on any return path.
- [ ] `improve()` runs a persist-agent-trace-steps stage and records
      `"persist_trace_steps"` in `stages_run`.
- [ ] `improve()` supports `build_global_context_index` (flag + stage +
      `"global_context_index"` in `stages_run`).
- [ ] Search prepends the stored graph-context snapshot to history with the exact Python
      prefix/separator and char-limit precedence.
- [ ] Saved QA entries carry `used_graph_element_ids` for graph-traversal searches.
- [ ] Conversationally-detected feedback is written to the prior QA entry via
      `add_feedback` (resetting `feedback_weights_applied`).
- [ ] Feedback-weight math unchanged; all existing + new tests pass; clippy clean.

## Gotchas / do-not

- **Do NOT alter the feedback-weight math.** `(score-1)/4`, `w' = w + α(r-w)`, clamp [0,1],
  round 4dp, default α=0.1 — all verified to match Python. Touching them breaks parity.
- **Lock must not be held across `.await`.** Use a sync `Mutex` for the
  registry set only; never hold the `MutexGuard` across an await point (Send/deadlock
  hazard). The `ImproveLockGuard` stores a `String`, not a guard, so it's await-safe.
- **Release the lock on every path.** Use the RAII guard, not manual release, so panics and
  `?` early-returns can't leak it (Python uses try/finally for the same reason).
- **Exact prefix string parity.** The graph-context prepend uses Python's literal
  `"Background knowledge from the knowledge graph:\n"` + `"\n\n"` separator. A parity test
  should pin this string.
- **Feedback goes to the PRIOR entry, before saving the new turn** — order matters and is
  what Python does (lines 492–525). Don't attach feedback to the current turn.
- **`used_graph_element_ids` shape is `{node_ids, edge_ids}`** (lists of strings) — matches
  Python's `Dict[str, List[str]]`. Don't invent a different shape; cross-SDK readers expect
  exactly these two keys.
- **Stored-context payload (B5.2) is out of scope here** — handle it in task 21 to avoid
  conflating provenance wiring with the context-storage policy change.
- **Global-context-index may be partial for 0.1.0** — if so, ship the stage plumbing and
  record the partial in `docs/not-implemented.md` rather than silently omitting it.

## Rollback

Parts A and B are independent and each step is additive. Revert by removing
`improve_lock.rs` + its export, the lock guard in `improve.rs`, the two new stages, the
`graph_context` field + prepend, the `used_graph_element_ids` plumbing, and the feedback
wiring. `git checkout main -- crates/lib/src/api/improve.rs
crates/search/src/orchestration/ crates/session/src/` restores prior behavior. No schema,
ID, collection-format, or content-hash changes are made.
