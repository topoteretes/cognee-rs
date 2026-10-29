# Extraction-request capture (SDK-1039 P18)

`capture_extraction.py` runs `add` + default `cognify` on `test_data/biography_temporal.txt`
5 times per SDK (gpt-4o-mini, no temperature) through a logging proxy, and records the
extraction request, the Timestamp-typed nodes in the raw model answer ("raw") and the
Timestamp nodes in the graph afterwards ("graph"). Request bodies are not committed.

Run it in the harness image (see the script's docstring). Results of 2026-10-29:

| run | Python raw / graph | Rust before (raw / graph) | Rust after (raw / graph) |
|---|---|---|---|
| 1 | before 2 / 2; after 9 / 8 | 6 / 6 | 7 / 7 |
| 2 | before 4 / 4; after 4 / 4 | 0 / 0 | 3 / 3 |
| 3 | before 4 / 4; after 10 / 10 | 0 / 0 | 5 / 5 |
| 4 | before 11 / 11; after 10 / 9 | 0 / 0 | 13 / 13 |
| 5 | before 2 / 2; after 8 / 6 | 0 / 0 | 4 / 4 |

## What the capture shows

* **The Rust drop is not the cause.** In every run "raw" equals "graph" for Rust: the post-processing
  keeps every Timestamp node the model returns. Rust's zero runs are runs where the model's own
  answer held no Timestamp-typed node (before: 0 in 4 of 5 runs).
* **The requests differed.** Before the change the system prompts differed (sha 8e0acbe93262 vs
  Python's 2aa9be2f13ba) and the schema carried Rust doc comments as descriptions. After it the system
  prompt is byte-identical to Python's (2aa9be2f13ba) and Rust's model-facing schema has Python's text.
  Rust then wrote 4 to 13 Timestamp nodes in all five runs.
* **One difference remains: the request transport.** Python sends `response_format: json_schema` with
  `strict: true` (litellm_native; strict is accepted for `KnowledgeGraph`, so Python is NOT demoted to
  non-strict). Rust's default `auto` mode sends a forced tool call (`tools`, non-strict); the strict
  `json_schema` request is the opt-in `LLM_STRUCTURED_OUTPUT_MODE=json_schema` (SDK-630).

## The proxy runs above were lucky; direct runs show the transport effect

The proxy hides nothing about the request, but five good Rust runs in a row were chance. Direct runs
(`--direct`, no proxy; Rust request bodies from `COGNEE_DEBUG_LLM_REQUEST`), after the prompt and schema
change, Rust graph Timestamp counts:

| Rust request | counts | runs with 0 |
|---|---|---|
| `auto` (forced tool call; the default) | 8, 0, 7, 0, 0, 12, 4 | 3 of 7 |
| `LLM_STRUCTURED_OUTPUT_MODE=json_schema` (strict; Python's request) | 4, 8, 10, 0, 8, 10, 3 | 1 of 7 |
| Python (default; 20 captured runs over the day) | 2, 4, 4, 11, 2, 9, 4, 10, 10, 8 and the lane's earlier values | 0 of 20 |

In each Rust zero run the model's own answer had no Timestamp-typed node (it typed the years as
`Event`/`Work` or dropped them), so the post-processing is not involved. With `json_schema` the request
body equals Python's except the schema name (`extract_structured_data` instead of `KnowledgeGraph`) and the
order of the `required` arrays; the system and user messages are identical.

Rust's default is a documented design choice (SDK-630: the operator opts in to `json_schema` because an
unknown endpoint cannot be probed for it). Changing it is outside P18; the harness pins the Rust side of
`test_temporal_search.py` to `json_schema` to compare like with like.
