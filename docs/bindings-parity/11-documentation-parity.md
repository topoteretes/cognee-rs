# DOC-2 — Documentation parity across bindings

- **Binding:** All (`python/`, `capi/`, `js/`)
- **Dimension:** Documentation
- **Priority:** P2
- **Status:** Not started

## Problem

Documentation quality is uneven and, in one case, the README does not show the
primary API:

| Binding | README | Reference docs | Gap |
|---|---|---|---|
| C API | Strong (187 lines: build/link, init pattern, ownership table, error handling) + two heavily-commented headers | Strong | Header drift (covered by [03-capi-header-cbindgen.md](03-capi-header-cbindgen.md)) |
| JS/TS | Strong (375 lines: every op, all 15 search types, migration guide) + TSDoc on every method | Strong | Distribution docs (covered by [07-js-distribution.md](07-js-distribution.md)) |
| Python | Good on logging/telemetry/env vars, **but the quick-start does not show `Cognee().add/cognify/search` at all** ([python/README.md:19-26](../../python/README.md#L19) shows only `Pipeline()` + a "configure tasks" placeholder) | Method docstrings strong; **no `.pyi`** (covered by [05-python-typing-stubs.md](05-python-typing-stubs.md)) | README unrepresentative of the primary API |

This task is the documentation cleanup that is **not** already owned by another
task: making the Python README represent the real SDK, and establishing a
consistent doc baseline across the three bindings.

## Goal / definition of done

Each binding's README opens with a runnable core-flow quick-start using the
primary API, links to its examples and reference docs, and documents
install/build and the env-var surface. A reader landing on any of the three gets
the same quality of orientation.

## Implementation plan

### Step 1 — Fix the Python README quick-start

Rewrite the quick-start in [python/README.md](../../python/README.md) to show the
actual primary flow:

```python
import asyncio
import json
from cognee_pipeline import Cognee, SearchType

async def main():
    cognee = Cognee()          # optionally pass json.dumps(settings) to override defaults
    await cognee.warm()        # build engines and resolve the default user
    await cognee.add(
        {"type": "text", "text": "Cognee turns data into a knowledge graph."},
        "main_dataset",        # dataset_name is required
    )
    await cognee.cognify("main_dataset")   # dataset_name is required
    result = await cognee.search("What does cognee do?", {"search_type": SearchType.GRAPH_COMPLETION})
    print(result)

asyncio.run(main())
```

Then, once [04-python-sdk-parity.md](04-python-sdk-parity.md) lands, add a second
snippet showing the upstream-compatible module-level form. Move the existing
`Pipeline()` engine content into an "Advanced: low-level pipeline" section, as
the JS README already does.

### Step 2 — Establish a common README skeleton

Align all three READMEs to the same section order so they are scannable in
parallel:

1. Install / build
2. Quick start (core add → cognify → search, primary API)
3. Configuration (programmatic + env vars table)
4. Operations reference (pipeline, retrieval, memory, data, datasets, sessions, config, visualization, cloud)
5. Examples (link to `examples/`)
6. Advanced / low-level engine
7. Error handling
8. Troubleshooting / platform notes

The JS README is closest to this; use it as the template and bring C API and
Python into the same shape.

### Step 3 — Cross-link the binding docs

- Add a top-level pointer (e.g. in the root `README.md` "Language bindings"
  section) to each binding README and to this `docs/bindings-parity/` plan.
- From each binding README, link to its reference docs
  ([docs/python-bindings/](../python-bindings/) for Python; the headers for C;
  TSDoc/`native.ts` for JS) and to the examples directory.

### Step 4 — Document the env-var surface uniformly

Ensure each README has the same env-var table (`OPENAI_URL`, `OPENAI_TOKEN`,
`EMBEDDING_*`, `MOCK_EMBEDDING`, model paths, logging/telemetry vars). The
canonical list is in the root README "Logging" + the embedding config docs;
reference rather than re-derive, and note any binding-specific vars.

### Step 5 — Refresh the parity matrix

Update [docs/.internal/python-bindings/STATUS.md](../.internal/python-bindings/STATUS.md) (or promote a
shared matrix into this folder) to reflect the post-parity state, and re-score
the baseline table in [README.md](README.md) when the P0/P1 tasks land.

## Verification

- Each binding README's quick-start is copy-paste runnable (matches an example in
  `examples/`).
- Manual review: the three READMEs follow the common skeleton and cross-link.
- No code change; this is a docs-only task. Run a link check if one is available
  (e.g. `markdown-link-check` over `docs/` and the binding READMEs).

## Risks / notes

- Keep doc snippets in sync with the real API — pull them from the example
  scripts in [09-examples-parity.md](09-examples-parity.md) so there is a single
  source of truth and the README cannot drift from runnable code.
- This task depends on the API-shape decisions in
  [04-python-sdk-parity.md](04-python-sdk-parity.md) and
  [05-python-typing-stubs.md](05-python-typing-stubs.md); sequence it after those so
  the documented surface is final.
