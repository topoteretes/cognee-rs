# EX-1 — Example parity across bindings

- **Binding:** Python (`python/`), JS/TS (`js/`)
- **Dimension:** Examples
- **Priority:** P2
- **Status:** Not started

## Problem

Example coverage is uneven:

| Binding | Examples | Verdict |
|---|---|---|
| C API | 20 `.c` files under [capi/examples/](../../capi/examples/), compiled+run in `check.sh`, exemplary memory management | **Strong — the reference standard** |
| JS/TS | 1 file ([js/examples/add-cognify-search.ts](../../js/examples/add-cognify-search.ts)); not an npm script; covers only add→cognify→search | **Adequate** |
| Python | **0 example scripts** anywhere; only the test suite + docstrings | **Missing** |

The C API examples are the bar. The goal is for Python and JS to reach
comparable coverage: at minimum the core `add → cognify → search` flow, plus the
main ops (memify/recall, datasets, sessions, config), each runnable with a single
command and env-gated so they skip cleanly without credentials.

## Goal / definition of done

- Python has a `python/examples/` directory with runnable scripts covering the
  core flow and the main ops, referenced from the README.
- JS has more than one example and they are discoverable via `npm run`.
- All examples follow the C API examples' pattern: env-var validation up front,
  clear step comments, graceful skip without credentials, proper cleanup.

## Implementation plan

### Step 1 — Define the canonical example set

Mirror the C API set so the three bindings teach the same things. Minimum set per
binding:

1. `add_cognify_search` — the core pipeline end to end (the one JS already has).
2. `memify_recall` — enrichment + recall.
3. `datasets` — list / status / delete data.
4. `sessions` — QA history / feedback.
5. `config` — programmatic configuration (LLM/embedding/vector/graph).
6. `visualize` — render the graph to HTML.

### Step 2 — Python examples (`python/examples/`)

Create `python/examples/` with one script per item, using the **handle API** and,
once [04-python-sdk-parity.md](04-python-sdk-parity.md) lands, a second
`add_cognify_search_compat.py` using the module-level compat API to demonstrate
upstream-style usage. Each script:

- reads config from env (`OPENAI_URL`, `OPENAI_TOKEN`, `MOCK_EMBEDDING`, model
  paths) and prints a clear "skipping: set X" message if unset, exiting 0;
- uses `asyncio.run(main())`;
- has a module docstring with the run command.

Model the structure on [js/examples/add-cognify-search.ts](../../js/examples/add-cognify-search.ts).
Add an `examples` extra in `pyproject.toml` if any script needs extra deps (see
[06-python-packaging-tests.md](06-python-packaging-tests.md)).

### Step 3 — JS examples (`js/examples/`)

Add the remaining scripts (`memify-recall.ts`, `datasets.ts`, `sessions.ts`,
`config.ts`, `visualize.ts`) following the existing example's structure, and add
an example for the low-level `Pipeline` engine (currently undocumented by
example despite being public).

### Step 4 — Make examples discoverable

- **JS:** add npm scripts to [js/package.json](../../js/package.json):
  ```json
  "scripts": {
    "example": "ts-node examples/add-cognify-search.ts",
    "example:memify": "ts-node examples/memify-recall.ts",
    "...": "..."
  }
  ```
  and document `npm run example` in the README.
- **Python:** document `python -m examples.add_cognify_search` (or
  `python examples/add_cognify_search.py`) in the README quick-start.
- **C API:** already discoverable via the CMake build in `check.sh`; no change.

### Step 5 — Optionally smoke them in CI

Add a credential-gated CI step (or extend each binding's `check.sh`) that runs
the `add_cognify_search` example in `MOCK_EMBEDDING` mode, so the examples cannot
silently rot. Keep it gated so it skips without secrets, matching the C API
examples' SKIP-guard approach.

## Verification

```bash
# Python
cd python && maturin develop && python examples/add_cognify_search.py   # runs or skips cleanly
# JS
cd js && npm run build && npm run example
# C API (reference)
cd capi && bash scripts/check.sh
```

Each binding's core example runs end-to-end with credentials, and prints a clear
skip message without them.

## Risks / notes

- Examples need a working LLM + embedding model; the `MOCK_EMBEDDING` path keeps
  them runnable in CI without external services for the deterministic parts.
- Keep examples thin and focused — they are teaching artifacts, not test
  harnesses; the real coverage lives in each binding's test suite.
- The Python compat example depends on [04-python-sdk-parity.md](04-python-sdk-parity.md);
  ship the handle-API example first and add the compat one when that lands.
