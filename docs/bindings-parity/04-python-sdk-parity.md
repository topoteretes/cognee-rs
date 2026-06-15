# ID-1 — Python: drop-in `cognee` SDK API parity

- **Binding:** Python (`python/`)
- **Dimension:** Idiomaticity / Functionality
- **Priority:** P1
- **Status:** Not started

## Problem

A stated project goal (root CLAUDE.md) is for the Rust port to be a *"drop-in
replacement of the Python `cognee` SDK"*. The Python binding does **not** meet
this at the API-shape level:

| Aspect | Upstream `cognee` (Python) | This binding |
|---|---|---|
| Import name | `import cognee` | `import cognee_pipeline` |
| Pip name | `cognee` | `cognee-pipeline` ([python/pyproject.toml](../../python/pyproject.toml) `name = "cognee-pipeline"`) |
| Call shape | module-level: `await cognee.add("text")`, `await cognee.cognify()`, `await cognee.search(...)` | handle/method: `Cognee().add(...)` |
| `add` input | raw `str` / `list` / `Path` | typed descriptor dict `{"type": "text", "text": "..."}` ([python/src/sdk_ops.rs:99](../../python/src/sdk_ops.rs#L99)) |
| `search` args | kwargs: `search(query_text, query_type=..., top_k=...)` | `search(query, opts_dict)` |
| `prune` | `await cognee.prune.prune_data()` | `Cognee().prune_data()` |

So upstream code (`import cognee; await cognee.add("hello")`) does not run against
this binding without rewrites. The underlying functionality is fully present —
this is purely a surface-shape gap.

### `SearchType` nuance (don't over-promise)

[python/cognee_pipeline/__init__.py:25-39](../../python/cognee_pipeline/__init__.py#L25)
exposes all **15** search types the Rust core supports. Upstream `cognee` has
**16–17**, including `AGENTIC_COMPLETION` and `GRAPH_COMPLETION_DECOMPOSITION`,
which the **Rust core does not implement**. Those two are therefore *blocked on
core work*, not a Python-binding omission. This task covers exposing what the
core supports in an upstream-compatible shape; the two extra types are tracked
separately as a core dependency.

## Goal / definition of done

A Python developer can take a basic upstream `cognee` script — module-level
`add` / `cognify` / `search` / `prune` with raw-string/path inputs and `SearchType`
kwargs — and run it against this package with no code changes beyond (optionally)
the import line. The handle-based API remains available for advanced use.

## Design decision: compatibility module vs. shadowing `cognee`

Publishing under the import name `cognee` would collide with the real upstream
package if both are installed. Recommended approach:

- Add a **functional compatibility layer** as a pure-Python module that wraps a
  process-default `Cognee` handle and exposes module-level functions matching the
  upstream signatures.
- Expose it both as `cognee_pipeline.compat` **and**, behind an opt-in, as a
  top-level `cognee` module (so `import cognee` works when the real package is
  absent). Make the top-level alias a separate installable/extra to avoid
  silently shadowing upstream.

This keeps the existing `Cognee` handle API (the JS/C-aligned surface) intact and
layers compatibility on top, rather than rewriting the native layer.

## Implementation plan

### Step 1 — Inventory the upstream surface

Clone the reference and extract the exact module-level signatures to match:

```bash
git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python
# inspect cognee/__init__.py, cognee/api/v1/{add,cognify,search}/*, prune
```

Record signatures for: `add`, `cognify`, `add_and_cognify` (if present),
`search`, `prune.prune_data`, `prune.prune_system`, `memify`, plus the
`SearchType` enum shape and default values (`top_k`, default `query_type`).

### Step 2 — Build a default-handle accessor

In a new `python/cognee_pipeline/compat.py`, create a lazily-initialized
process-global `Cognee` handle (mirroring upstream's implicit global state):

```python
from . import Cognee
_default = None
def _handle() -> "Cognee":
    global _default
    if _default is None:
        _default = Cognee()
    return _default
```

### Step 3 — Implement module-level functions with upstream signatures

Map raw inputs to the binding's typed descriptors and kwargs to the `opts` dict.
Example for `add`:

```python
async def add(data, dataset_name: str | None = None, **kwargs):
    inputs = _coerce_data_inputs(data)   # str -> {"type":"text",...}; Path -> {"type":"file",...}; list -> many
    return await _handle().add(inputs, _opts(dataset_name=dataset_name, **kwargs))
```

`_coerce_data_inputs` handles `str` (text), `pathlib.Path`/path-like (file),
`list`/`tuple` (fan-out), and URL strings (`{"type":"url"}`). For `search`:

```python
async def search(query_text=None, query_type=SearchType.GRAPH_COMPLETION,
                 top_k=10, *, datasets=None, **kwargs):
    return await _handle().search(query_text, _opts(search_type=str(query_type),
                                                    top_k=top_k, datasets=datasets, **kwargs))
```

Provide a `prune` object with `prune_data()` / `prune_system()` to match
`cognee.prune.prune_data()`.

### Step 4 — Make `SearchType` upstream-compatible

Convert `SearchType` from a plain constants class to `class SearchType(str, Enum)`
(matching upstream so `query_type=SearchType.CHUNKS` and string comparison both
work). Add `AGENTIC_COMPLETION` and `GRAPH_COMPLETION_DECOMPOSITION` **only after**
the Rust core supports them — until then, document them as unsupported and raise
a clear `CogneeValidationError` if passed (see the core dependency note above and
[docs/python-bindings/minor-engine-gaps.md](../python-bindings/minor-engine-gaps.md)).

### Step 5 — Optional top-level `cognee` alias

Add an opt-in mechanism so `import cognee` resolves to the compat module when the
real package is not installed. Options: a thin `cognee/__init__.py` shipped under
a separate extra (`pip install cognee-pipeline[drop-in]`), or a documented
`import cognee_pipeline.compat as cognee`. Recommend the explicit extra so the
default install never shadows upstream silently.

### Step 6 — Tests

Add `python/tests/test_compat_api.py` that runs an upstream-style script
(`await cognee.add("text"); await cognee.cognify(); await cognee.search("q",
query_type=SearchType.GRAPH_COMPLETION)`) against the compat module, gated on the
same `MOCK_EMBEDDING` / LLM env vars as the existing tests so it skips gracefully.

### Step 7 — Document the parity matrix

Update [docs/.internal/python-bindings/STATUS.md](../.internal/python-bindings/STATUS.md) with an
"upstream `cognee` SDK parity" section: which module-level functions are
supported, the input-coercion rules, and the two `SearchType` values blocked on
core.

## Verification

```bash
cd python && maturin develop && pytest tests/test_compat_api.py -v
# from repo root
scripts/check_all.sh
```

Manual: run a 5-line upstream `cognee` quickstart against the compat module.

## Risks / notes

- This intentionally introduces a *second* API shape (compat + handle). Keep the
  compat layer a thin wrapper so there is no logic duplication — it only coerces
  inputs and forwards to the handle.
- Decide the `cognee` import-name policy with maintainers before Step 5; shadowing
  a widely-installed package by default is a footgun. The conservative default
  (explicit extra / explicit compat import) is recommended.
- Full kwarg parity with upstream `search`/`cognify` may reveal options the Rust
  core doesn't support; surface those as documented no-ops or validation errors,
  not silent drops.
