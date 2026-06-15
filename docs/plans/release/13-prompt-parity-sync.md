# 13 — Sync LLM Prompts to Python + Drift Guard

> Wave 3 · Priority P1 · Track A · Release-blocking: no · Effort: 0.5d ·
> Depends on: — · Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) B2.3, B2.4, B3.3, B3.5; [release-readiness-plan.md](../release-readiness-plan.md) Phase 7 T8.4. See [index](00-INDEX.md).

## Goal

Three Rust prompts have diverged from their Python `.txt` sources, changing pipeline
output (node casing, edge content, summary structure, feedback classification). Bring
each into byte-parity with Python by **vendoring the Python `.txt` into the crate and
loading it via `include_str!`** (matching the existing precedent in
`crates/llm/src/prompts/`), then add a **drift guard** so future upstream prompt edits
are caught.

The three prompts:

| # | Rust const | Rust file | Python source (`/tmp/cognee-python/cognee/infrastructure/llm/prompts/…`) | Audit |
|---|---|---|---|---|
| 1 | `DEFAULT_GRAPH_PROMPT` | `crates/cognify/src/fact_extraction/extractor.rs:18` | `generate_graph_prompt.txt` | B2.3 |
| 2 | `DEFAULT_SUMMARY_PROMPT` | `crates/cognify/src/summarization/extractor.rs:28` | `summarize_content.txt` | B2.4 |
| 3 | `FEEDBACK_DETECTION_SYSTEM_PROMPT` | `crates/search/src/utils/feedback_detection.rs:5` | `feedback_detection_system.txt` | B3.3 |

## Background & why

These prompts are the LLM's instructions; divergence directly changes the model's
output and breaks answer/structure parity with Python cognee:

- **Graph (B2.3):** Rust **forces UPPERCASE** type labels (`"PERSON"`, `"DATE"`) and
  **drops the edge-description paragraph + good/bad examples**; Python uses **Title-case**
  (`"Person"`, `"Date"`) and instructs the model to write edge descriptions (consumed
  downstream as `edge_text` for triplet/edge-type embeddings). The Rust prompt was
  rewritten with a bespoke `id`/`name`/`type`/`description` field schema that Python does
  not have. Result: different node `type` casing in the graph and missing edge fact text.
- **Summary (B2.4):** Rust is a generic 3-line "rewrite the text" paraphrase. Python is a
  structured 29-line "categories + ordered facts, ≤200 tokens" retrieval prompt. The
  "Based on Python's prompts/summarize_content.txt" doc comment at
  `summarization/extractor.rs:26` is **stale**.
- **Feedback (B3.3):** Rust is ~11 rendered lines; Python is 31 content lines with
  explicit true/false detection criteria, examples, per-field generation rules, and the
  exact JSON field list. The same LLM classifies feedback differently.

**No `to_uppercase()` exists in code** — the uppercase forcing is purely instructional in
the Rust graph prompt text, so swapping the text fully fixes it.

### Existing precedent (use this pattern)

The codebase has two existing patterns to follow:
1. `crates/llm/src/prompts/mod.rs:15-19` — `pub const … = include_str!("…​.txt")` for
   4 files, with a module doc comment: *"Filenames are kept identical for cross-SDK diffing."*
2. `crates/cognify/src/temporal_extraction/` — the **closer precedent**: private consts
   using `include_str!("prompts/temporal_*.txt")` with crate-local `prompts/` subdirs
   (exactly the same structure as the three changes here).
Follow pattern 2 (temporal_extraction) for these three, since all three consts are private.

### Drift guard (B3.5)

Rust hardcodes prompts as `const &str` / `include_str!` while Python loads `.txt` at
runtime (`read_query_prompt`, base dir `./infrastructure/llm/prompts`). There is **no**
existing test comparing a Rust const to a Python `.txt`, and `/tmp/cognee-python` is a
developer-local clone **not present in CI** — so the guard cannot read Python files
directly. The viable guard: vendor the `.txt` into the crate and add a unit test
asserting the live const equals the vendored `include_str!` content (turning drift into a
checked-in, reviewable diff), plus a documented manual re-sync step.

## Prerequisites

```bash
git checkout -b task/13-prompt-parity-sync
# Ensure the Python reference is present (the .txt content is also embedded below):
[ -d /tmp/cognee-python ] || git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python
```

Read first:
- `crates/cognify/src/fact_extraction/extractor.rs` (const + how it's passed to the LLM)
- `crates/cognify/src/summarization/extractor.rs` (const + the `test_default_prompt_not_empty` test at lines 196-200 — it asserts `.contains("summarization")`, which WILL break; see step)
- `crates/search/src/utils/feedback_detection.rs` (const + `detect_feedback` usage)
- `crates/llm/src/prompts/mod.rs` (the `include_str!` precedent to mirror)
- Python sources (verbatim text embedded below, but re-verify against
  `/tmp/cognee-python/cognee/infrastructure/llm/prompts/{generate_graph_prompt,summarize_content,feedback_detection_system}.txt`)

## Files to change

| Path | Change |
|---|---|
| `crates/cognify/src/fact_extraction/prompts/generate_graph_prompt.txt` | **New.** Verbatim copy of Python's `generate_graph_prompt.txt`. |
| `crates/cognify/src/fact_extraction/extractor.rs` | Replace `DEFAULT_GRAPH_PROMPT` literal with `include_str!("prompts/generate_graph_prompt.txt")`; fix doc comment. |
| `crates/cognify/src/summarization/prompts/summarize_content.txt` | **New.** Verbatim copy of Python's `summarize_content.txt`. |
| `crates/cognify/src/summarization/extractor.rs` | Replace `DEFAULT_SUMMARY_PROMPT` with `include_str!(...)`; fix stale doc comment; update the `test_default_prompt_not_empty` assertion. |
| `crates/search/src/utils/prompts/feedback_detection_system.txt` | **New.** Verbatim copy of Python's `feedback_detection_system.txt`. |
| `crates/search/src/utils/feedback_detection.rs` | Replace `FEEDBACK_DETECTION_SYSTEM_PROMPT` with `include_str!(...)`. |
| (inline `#[cfg(test)]` blocks in the three source files above) | **Modified.** Drift-guard tests added inline (private consts require this; no new external test file). |

> Use a crate-local `prompts/` subdir next to each consuming source file (mirrors
> `temporal_extraction/prompts/`). `include_str!` paths are **relative to the source
> file**, so `extractor.rs` in `fact_extraction/` references `prompts/…txt`.

## Python reference (exact, verbatim — embed these as the new `.txt` files)

### 1. `generate_graph_prompt.txt`
`/tmp/cognee-python/cognee/infrastructure/llm/prompts/generate_graph_prompt.txt` (34 lines, trailing newline):

```text
You are a top-tier algorithm designed for extracting information in structured formats to build a knowledge graph.
**Nodes** represent entities and concepts. They're akin to Wikipedia nodes.
**Edges** represent relationships between concepts. They're akin to Wikipedia links.
Every edge should include a description when the text supports relevant
information about the endpoints. The description must use the endpoint names,
stay dry and efficient, and may include useful qualifiers from the source text.
Do not add outside knowledge.
  - Good: Alice works at Acme as a platform engineer on the search team.
  - Bad: This edge describes an employment relationship.

The aim is to achieve simplicity and clarity in the knowledge graph.
# 1. Labeling Nodes
**Consistency**: Ensure you use basic or elementary types for node labels.
  - For example, when you identify an entity representing a person, always label it as **"Person"**.
  - Avoid using more specific terms like "Mathematician" or "Scientist", keep those as "profession" property.
  - Don't use too generic terms like "Entity".
**Node IDs**: Never utilize integers as node IDs.
  - Node IDs should be names or human-readable identifiers found in the text.
**Node Names**: Every node MUST include a "name" field.
  - Use the most complete human-readable name for the entity (e.g., "Albert Einstein", "Python").
# 2. Handling Numerical Data and Dates
  - For example, when you identify an entity representing a date, make sure it has type **"Date"**.
  - Extract the date in the format "YYYY-MM-DD"
  - If not possible to extract the whole date, extract month or year, or both if available.
  - **Property Format**: Properties must be in a key-value format.
  - **Quotation Marks**: Never use escaped single or double quotes within property values.
  - **Naming Convention**: Use snake_case for relationship names, e.g., `acted_in`.
# 3. Coreference Resolution
  - **Maintain Entity Consistency**: When extracting entities, it's vital to ensure consistency.
  If an entity, is mentioned multiple times in the text but is referred to by different names or pronouns,
  always use the most complete identifier for that entity throughout the knowledge graph.
Remember, the knowledge graph should be coherent and easily understandable, so maintaining consistency in entity references is crucial.
# 4. Strict Compliance
Adhere to the rules strictly. Non-compliance will result in termination
```

### 2. `summarize_content.txt`
`/tmp/cognee-python/cognee/infrastructure/llm/prompts/summarize_content.txt` (29 lines, trailing newline):

```text
Summarize the chunk for retrieval.

Output two sections only.

First section:
This chunk is about:
- <Category>: <names or topics>
- <Category>: <names or topics>

First-section rules:
1. List entity/topic categories. Do not list facts here.
2. Use only clear, useful categories.
3. Good categories include People, Companies, Organizations, Places, Roles, Projects, Products, Systems, Concepts, Events, and Topics.
4. Keep category lines short.

Second section:
Facts:
- <self-contained fact>
- <self-contained fact>

Second-section rules:
1. Write complete sentences with clear subjects from the first section.
2. Each fact must stand alone without the chunk or the other facts.
3. Order facts by: time first, category second, entity/topic third.
4. Do not group all facts about one entity if that makes the facts jump backward or forward in time.
5. Make sure the facts cover the full content of the chunk.
6. Do not invent.

Max 200 tokens.
```

### 3. `feedback_detection_system.txt`
`/tmp/cognee-python/cognee/infrastructure/llm/prompts/feedback_detection_system.txt` (31 content lines):

```text
You analyze user messages in a Q&A session. Only treat a message as feedback when the user is explicitly evaluating the correctness, accuracy, or quality of the previous answer (the last response they received)—i.e. they are validating or commenting on whether the answer was right, wrong, helpful, or accurate.

Set feedback_detected to true ONLY when the user is clearly commenting on the previous answer itself, for example:
- Saying the answer was wrong or right (e.g. "that was wrong", "correct", "not quite", "yes that's right")
- Correcting the answer (e.g. "the date was 2020, not 2021", "it's actually X")
- Rating or scoring the answer (e.g. "5/5", "3 stars")
- Thanks or praise directed at the answer (e.g. "thanks, that was helpful", "perfect answer")
- Short confirmation or rejection of the answer (e.g. "nope", "yes", "wrong")

Set feedback_detected to false when:
- The message is a new question or request for information (e.g. "What is the capital of France?", "How does X work?")
- The message is a follow-up question (e.g. "can you elaborate?", "what about X?")
- The message is only a reaction to the topic or subject matter, not to the correctness of the answer (e.g. "that place is nice", "oooh interesting", "I've been there")—these do not validate whether the answer was correct or wrong.

The message may contain BOTH feedback AND a follow-up question (e.g. "that was wrong, but what about X?", "thanks! Can you also explain Y?"). When feedback_detected is true, set contains_followup_question to true only if the user is asking a distinct new or follow-up question in the same message that should be answered. Otherwise set contains_followup_question to false.

When feedback_detected is true you MUST always provide feedback_text, feedback_score, response_to_user, and contains_followup_question. feedback_text must NEVER be empty.

response_to_user: Generate a brief, friendly message (one sentence) to show the user in reply—e.g. thanking them for their feedback. Simply acknowledge and accept the feedback; be kind. Do NOT ask follow-up questions (e.g. do not ask "Could you provide more details?", "What was incorrect?", "Can you elaborate?"). Just thank them or acknowledge; do not invite the user to elaborate. You may adapt tone or language to match the user's message (formal/informal, language). Examples: "Thanks for your feedback!", "We appreciate you letting us know.", "Thank you for the feedback—it helps us improve.", "I'm sorry that wasn't right—thanks for letting us know."

feedback_text: Write a short description (one or two sentences) that includes what the user said and why it counts as feedback. Incorporate the user's words either by quoting them or by summarizing. Examples:
- "User said 'that was wrong — the date was 2020': correction of the previous answer."
- "User expressed thanks: 'that was helpful, exactly what I needed.' Positive feedback."
- "User gave a rating (5/5): positive feedback on the previous response."
- "User said 'nope' or 'correct': short reactive feedback on the previous answer."
So the reader understands both what the user sent and why it was detected as feedback. Keep it concise (e.g. under 300 characters).

feedback_score: Map to a scale of 1-5 (1 = negative, 5 = positive). E.g. "5/5" -> 5, "that was wrong" -> 1, "thanks" -> 5. Use 3 (neutral) if no clear score can be inferred.

Respond with the exact JSON structure: feedback_detected (boolean), feedback_text (string, required when feedback_detected is true), feedback_score (number 1-5, required when feedback_detected is true), response_to_user (string, required when feedback_detected is true—message to show the user), contains_followup_question (boolean, required when feedback_detected is true—true if the message also asks a new or follow-up question to answer).
```

> **Re-verify before committing:** these texts were captured 2026-06-14. Run
> `diff` against the live Python files to confirm they have not changed:
> ```bash
> diff /tmp/cognee-python/cognee/infrastructure/llm/prompts/generate_graph_prompt.txt \
>      crates/cognify/src/fact_extraction/prompts/generate_graph_prompt.txt
> ```

## Implementation steps

### Prompt 1 — graph extraction (B2.3)

1. Create `crates/cognify/src/fact_extraction/prompts/generate_graph_prompt.txt` with the
   **exact** content of Python's `generate_graph_prompt.txt` above (preserve the trailing
   newline; copy with `cp` from the clone to avoid transcription drift):
   ```bash
   mkdir -p crates/cognify/src/fact_extraction/prompts
   cp /tmp/cognee-python/cognee/infrastructure/llm/prompts/generate_graph_prompt.txt \
      crates/cognify/src/fact_extraction/prompts/generate_graph_prompt.txt
   ```
2. In `crates/cognify/src/fact_extraction/extractor.rs`, replace the const. Before
   (lines 18-56, the `r#"…"#` literal):
   ```rust
   pub const DEFAULT_GRAPH_PROMPT: &str = r#"You are a top-tier algorithm ... Extract nodes and edges from the provided text."#;
   ```
   After:
   ```rust
   /// Default system prompt for knowledge graph extraction.
   ///
   /// Vendored byte-for-byte from Python's
   /// `cognee/infrastructure/llm/prompts/generate_graph_prompt.txt` (kept in sync via
   /// the prompt-parity drift guard in the inline `#[cfg(test)]` block below).
   const DEFAULT_GRAPH_PROMPT: &str =
       include_str!("prompts/generate_graph_prompt.txt");
   ```
   (The const is `private` — keep it that way. The `pub fn default_graph_prompt()`
   method already exposes it to callers and integration tests.)

### Prompt 2 — summarization (B2.4)

3. Create `crates/cognify/src/summarization/prompts/summarize_content.txt`:
   ```bash
   mkdir -p crates/cognify/src/summarization/prompts
   cp /tmp/cognee-python/cognee/infrastructure/llm/prompts/summarize_content.txt \
      crates/cognify/src/summarization/prompts/summarize_content.txt
   ```
4. In `crates/cognify/src/summarization/extractor.rs`, replace the const and fix the
   stale doc comment. Before (lines 24-30):
   ```rust
   /// Default system prompt for text summarization.
   ///
   /// Based on Python's prompts/summarize_content.txt.
   /// Instructs the LLM to create brief, concise summaries while preserving key information.
   const DEFAULT_SUMMARY_PROMPT: &str = r#"You are a top-tier summarization engine. ...keep the meaning."#;
   ```
   After:
   ```rust
   /// Default system prompt for text summarization.
   ///
   /// Vendored byte-for-byte from Python's
   /// `cognee/infrastructure/llm/prompts/summarize_content.txt` (structured
   /// categories + ordered facts, ≤200 tokens). Kept in sync via the prompt-parity
   /// drift guard.
   const DEFAULT_SUMMARY_PROMPT: &str = include_str!("prompts/summarize_content.txt");
   ```
5. **Fix the breaking unit test** at `summarization/extractor.rs:196-200`. The Python text
   does not contain the substring `"summarization"`, so the existing assertion fails.
   Before:
   ```rust
   #[test]
   fn test_default_prompt_not_empty() {
       assert!(!DEFAULT_SUMMARY_PROMPT.is_empty());
       assert!(DEFAULT_SUMMARY_PROMPT.contains("summarization"));
   }
   ```
   After (assert against a token that exists in the new Python text, e.g. `"Summarize"`
   or `"Facts:"`):
   ```rust
   #[test]
   fn test_default_prompt_not_empty() {
       assert!(!DEFAULT_SUMMARY_PROMPT.is_empty());
       assert!(DEFAULT_SUMMARY_PROMPT.contains("Summarize the chunk for retrieval"));
   }
   ```

### Prompt 3 — feedback detection (B3.3)

6. Create `crates/search/src/utils/prompts/feedback_detection_system.txt`:
   ```bash
   mkdir -p crates/search/src/utils/prompts
   cp /tmp/cognee-python/cognee/infrastructure/llm/prompts/feedback_detection_system.txt \
      crates/search/src/utils/prompts/feedback_detection_system.txt
   ```
7. In `crates/search/src/utils/feedback_detection.rs`, replace the const (lines 5-17):
   ```rust
   /// System prompt for conversational feedback detection.
   ///
   /// Vendored byte-for-byte from Python's
   /// `cognee/infrastructure/llm/prompts/feedback_detection_system.txt`. Kept in sync
   /// via the prompt-parity drift guard.
   const FEEDBACK_DETECTION_SYSTEM_PROMPT: &str =
       include_str!("prompts/feedback_detection_system.txt");
   ```
   Confirm the consuming `detect_feedback` code still compiles (the const type is
   unchanged: `&'static str`).

### Drift guard (B3.5)

8. All three consts are private, so **all drift assertions must live in inline
   `#[cfg(test)] mod tests` blocks** inside the owning source files (not in an external
   `tests/` file). The guard asserts: (a) the const equals the vendored file via
   `include_str!` (so editing just the `.txt` breaks the test), and (b) Python-specific
   markers that the old divergent Rust text lacked are present.

   In `crates/cognify/src/fact_extraction/extractor.rs`, add to the existing
   `#[cfg(test)] mod tests` block:
   ```rust
   #[test]
   fn graph_prompt_matches_vendored_txt() {
       // Drift guard: const must equal the vendored .txt byte-for-byte.
       // Manual re-sync: cp /tmp/cognee-python/cognee/infrastructure/llm/prompts/generate_graph_prompt.txt \
       //   crates/cognify/src/fact_extraction/prompts/generate_graph_prompt.txt
       let vendored = include_str!("prompts/generate_graph_prompt.txt");
       assert_eq!(DEFAULT_GRAPH_PROMPT, vendored, "const drifted from vendored .txt");
       // Python markers the old Rust prompt did NOT have:
       assert!(vendored.contains("Every edge should include a description"),
           "edge-description paragraph missing — not the Python prompt");
       assert!(vendored.contains(r#"label it as **"Person"**"#),
           "Title-case 'Person' missing — UPPERCASE Rust prompt regressed");
       assert!(!vendored.contains("the entity type label in uppercase"),
           "old UPPERCASE-forcing line still present");
   }
   ```

   In `crates/cognify/src/summarization/extractor.rs`, add to the existing
   `#[cfg(test)] mod tests` block:
   ```rust
   #[test]
   fn summary_prompt_matches_vendored_txt() {
       let vendored = include_str!("prompts/summarize_content.txt");
       assert_eq!(DEFAULT_SUMMARY_PROMPT, vendored, "const drifted from vendored .txt");
       assert!(vendored.contains("Output two sections only"),
           "Python two-section structure marker missing");
       assert!(vendored.contains("Max 200 tokens"),
           "token-limit marker missing");
   }
   ```

   In `crates/search/src/utils/feedback_detection.rs`, add to the existing
   `#[cfg(test)] mod tests` block:
   ```rust
   #[test]
   fn feedback_prompt_matches_vendored_txt() {
       let vendored = include_str!("prompts/feedback_detection_system.txt");
       assert_eq!(FEEDBACK_DETECTION_SYSTEM_PROMPT, vendored,
           "const drifted from vendored .txt");
       assert!(vendored.contains("Set feedback_detected to true ONLY"),
           "Python specificity marker missing");
       assert!(vendored.contains("response_to_user:"),
           "response_to_user field marker missing");
   }
   ```

   Note: there is no need for an external `crates/cognify/tests/prompt_parity.rs`
   file — the inline `#[cfg(test)]` approach is both simpler and works with private consts.

## Verification

```bash
# Compiles (include_str! paths resolve, tests build).
cargo check -p cognee-cognify -p cognee-search --all-targets

# Drift-guard + the fixed summary test pass.
cargo test -p cognee-cognify graph_prompt_matches_vendored_txt
cargo test -p cognee-cognify summary_prompt_matches_vendored_txt
cargo test -p cognee-cognify test_default_prompt_not_empty
cargo test -p cognee-search feedback_prompt_matches_vendored_txt

# Byte-parity sanity vs the live Python clone (developer machine; not a CI step).
for f in \
  "fact_extraction/prompts/generate_graph_prompt.txt:generate_graph_prompt.txt" \
  "summarization/prompts/summarize_content.txt:summarize_content.txt" ; do
  r="crates/cognify/src/${f%%:*}"; p="/tmp/cognee-python/cognee/infrastructure/llm/prompts/${f##*:}"
  diff "$p" "$r" && echo "OK $r" || echo "DRIFT $r"
done
diff /tmp/cognee-python/cognee/infrastructure/llm/prompts/feedback_detection_system.txt \
     crates/search/src/utils/prompts/feedback_detection_system.txt && echo OK

# Full gate.
scripts/check_all.sh
```

Expected: all three `diff`s report no differences; drift-guard tests pass;
`test_default_prompt_not_empty` passes against the new marker.

## Acceptance criteria

- [ ] Three `.txt` files vendored under crate-local `prompts/` dirs, byte-identical to Python.
- [ ] All three consts use `include_str!` (no inline prompt literals remain).
- [ ] Graph prompt no longer forces UPPERCASE and includes the edge-description paragraph + good/bad examples.
- [ ] Summary prompt is the structured two-section Python version; stale doc comment fixed; `test_default_prompt_not_empty` updated and passing.
- [ ] Feedback prompt is the full 31-line Python version.
- [ ] Drift-guard tests (inline `#[cfg(test)]`) assert const == vendored `.txt` and that Python-specific markers are present (and old Rust markers absent).
- [ ] `scripts/check_all.sh` passes.

## Gotchas / do-not

- **Byte-for-byte matters.** Copy with `cp` from the clone; do not retype. Preserve
  trailing newlines and the exact em-dashes (`—`) and curly apostrophes in the feedback
  prompt — they are UTF-8 in the Python source. `include_str!` preserves bytes exactly.
- **`infer_schema_system.txt` also diverges (out of scope here).** The Rust copy in
  `crates/llm/src/prompts/infer_schema_system.txt` is missing 4 lines compared to the
  Python source (the sample-text note, the `description` field line, a more detailed
  rule 6, and rule 9 about primitive properties). This is a separate drift that does NOT
  affect the add→cognify→search core pipeline — it only affects schema inference — and
  is explicitly out of scope for this task. It should be tracked separately.
- **Node-type casing change is a behavioral/parity change, and intended.** Switching
  from forced UPPERCASE to Python Title-case will change node `type` values emitted by
  the LLM (e.g. `"PERSON"` → `"Person"`). This is the parity goal, but it changes graph
  output for any text re-cognified after this lands — call it out in the PR. (It does not
  change deterministic uuid5 *chunk* IDs, which are content/index-based, not type-based.)
- **Edge descriptions need a sink.** The graph prompt now instructs the model to produce
  edge descriptions, but consuming them as `edge_text` requires `Edge.description` on the
  Rust model — that is **out of scope here** (audit B2.5, task 16). This task only syncs
  the *prompt text*; the field wiring is a separate task. Adding the prompt early is safe
  (the model may emit a description the current schema ignores).
- **Do not read `/tmp/cognee-python` from a test/build.** It is not present in CI. The
  drift guard must rely on the vendored `.txt` + markers, plus the documented manual
  re-sync command.
- **Check the const visibility.** All three consts are **private** (`const`, no `pub`):
  `DEFAULT_GRAPH_PROMPT` (exposed to callers only via the `pub fn default_graph_prompt()`
  method), `DEFAULT_SUMMARY_PROMPT`, and `FEEDBACK_DETECTION_SYSTEM_PROMPT`. Keep them
  private. Drift assertions go in inline `#[cfg(test)] mod tests` blocks in each owning
  file — an external `tests/` file cannot access private consts.
- **`include_str!` path is source-relative.** From `fact_extraction/extractor.rs`, the
  path is `prompts/generate_graph_prompt.txt` (not crate-root-relative).

## Rollback

```bash
git checkout main -- \
  crates/cognify/src/fact_extraction/extractor.rs \
  crates/cognify/src/summarization/extractor.rs \
  crates/search/src/utils/feedback_detection.rs
rm -rf crates/cognify/src/fact_extraction/prompts \
       crates/cognify/src/summarization/prompts \
       crates/search/src/utils/prompts
```
Reverts to the divergent inline prompts. No data/schema impact.
