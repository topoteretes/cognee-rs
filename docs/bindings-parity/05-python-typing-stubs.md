# ID-2 / DOC-1 — Python: type stubs, typed inputs & options, result-key convention

- **Binding:** Python (`python/`)
- **Dimension:** Idiomaticity (ID-2) + Documentation (DOC-1)
- **Priority:** P1
- **Status:** Not started

This covers two tightly-coupled gaps: the binding ships a typing marker with no
types, and it leans on untyped dicts for inputs/options/results.

## Problem

### Misleading `py.typed`, no stubs (DOC-1)

[python/cognee_pipeline/py.typed](../../python/cognee_pipeline/py.typed) exists but is
**0 bytes**, and there are **no `.pyi` stub files** anywhere (`find python -name
'*.pyi'` is empty). The native module `cognee_pipeline._native` exposes zero
signatures. Shipping `py.typed` tells mypy/pyright "this package is typed" while
giving them nothing — so type checkers report missing attributes or fall back to
`Any`, which is worse than an honest untyped package.

### Untyped dict inputs/options/results (ID-2)

- Inputs use typed descriptor dicts (`{"type": "text", ...}`) with no
  `TypedDict`/dataclass.
- Options are `opts: Option<Bound<PyAny>>` everywhere — dozens of documented keys
  (`search_type`, `top_k`, `chunk_size`, …) passed as untyped string-keyed dicts
  with no kwargs.
- Results are untyped dicts with **camelCase** keys (`result["addedCount"]`,
  `result["deletedDataId"]`) — un-Pythonic and divergent from upstream's
  snake_case object returns. This is a deliberate cross-binding-uniformity choice
  (documented in [python/tests/test_data_ops.py](../../python/tests/test_data_ops.py));
  this task revisits whether Python should present a snake_case view.

## Goal / definition of done

1. `py.typed` is backed by real type information: a type checker run against a
   sample script using `Cognee().add/cognify/search` reports accurate types, not
   `Any` and not "missing attribute".
2. Public option keys and result fields are discoverable via types (TypedDict or
   typed return objects), not only docstrings.
3. The result-key casing decision is made explicitly and documented.

## Implementation plan

### Step 1 — Decide the typing-source strategy

Because the API is a compiled PyO3 module, choose one:

- **Option A (recommended): hand-written `.pyi` stubs** in
  `python/cognee_pipeline/` (`__init__.pyi`, `_native.pyi`). Full control,
  matches the docstrings already on the Rust `#[pymethods]`. This is the standard
  approach for native extensions.
- **Option B: a typed pure-Python facade** that re-exports the native symbols
  with annotations, so types live in `.py` not `.pyi`. Only viable if the facade
  becomes the import surface (overlaps with the compat layer in
  [04-python-sdk-parity.md](04-python-sdk-parity.md)).

Recommend **A** (stubs) for the native surface, optionally combined with B for
the compat layer.

### Step 2 — Author the stubs

Create `python/cognee_pipeline/__init__.pyi` covering every public class and
method: `Cognee` (+ `config`/`datasets`/`sessions`/`notebooks` sub-objects),
`Pipeline`, `TaskContext`, `CancellationHandle`/`Token`, `ProgressToken`,
`Watcher`, `SearchType`, the exception hierarchy (`CogneeError` + subclasses),
and module-level `serve`/`disconnect`/`setup_logging`/`setup_telemetry`.

- Async methods must be typed `async def … -> <ReturnType>` (or
  `Coroutine[Any, Any, <ReturnType>]`) so `await` type-checks.
- Pull parameter/return descriptions from the existing Rust docstrings (e.g.
  [python/src/sdk_retrieval.rs:75](../../python/src/sdk_retrieval.rs#L75)) so the stubs
  carry the same documentation.

### Step 3 — Define `TypedDict`s for inputs, options, and results

In a typed module (`python/cognee_pipeline/types.py`, importable at runtime and
referenced from the stubs), define:

- **Inputs:** `TextInput`, `FileInput`, `UrlInput`, `BinaryInput` as `TypedDict`s
  with a literal `type` discriminator; `DataInput = Union[...]`. Mirrors the JS
  discriminated unions in [js/src/types.ts](../../js/src/types.ts) (`CogneeDataInput`).
- **Options:** `AddOpts`, `CognifyOpts`, `SearchOpts`, `ForgetOpts`, … as
  `TypedDict(total=False)` with every documented key typed (`search_type:
  SearchType`, `top_k: int`, `datasets: list[str]`, …).
- **Results:** `AddResult`, `SearchResponse`, `RecallResult`, `ForgetResult`, …
  matching the serde wire shape.

### Step 4 — Add keyword-argument overloads (idiomatic call style)

For the highest-traffic methods (`search`, `add`, `cognify`), accept idiomatic
kwargs in addition to the `opts` dict. Implement in the Python layer (or compat
layer) so it forwards to the existing native `opts` path — no native change
required:

```python
async def search(self, query, *, search_type=SearchType.GRAPH_COMPLETION,
                 top_k=10, datasets=None, **opts): ...
```

Keep the dict form working for cross-binding parity; kwargs become the
documented, type-checked default.

### Step 5 — Decide result-key casing

Two acceptable outcomes, pick one and document it in `STATUS.md`:

- **Keep camelCase**, but ship `Result` `TypedDict`s with camelCase keys so at
  least the keys are typed and discoverable. Lowest churn.
- **Add a snake_case view** (recommended for idiomaticity): the Python layer
  re-keys results to snake_case (`added_count`, `deleted_data_id`) and types them
  as snake_case `TypedDict`s, while a `raw=True` escape hatch returns the
  cross-binding camelCase dict. Document the mapping.

Recommendation: snake_case view via the Python wrapper layer — it does not touch
the native serde contract (which stays camelCase for C/JS uniformity) and gives
Python users idiomatic keys.

### Step 6 — Verify the stubs against the runtime

Add a typing test to the suite:

```bash
pip install mypy
mypy --strict python/cognee_pipeline/types.py
# stub-vs-runtime consistency:
python -c "import cognee_pipeline; print('ok')"
```

Optionally add `stubtest` (`python -m mypy.stubtest cognee_pipeline`) to catch
stub/impl divergence, gated so CI can run it once stubs exist.

## Verification

```bash
cd python && maturin develop
mypy --strict <sample_script_using_Cognee.py>   # must type-check with real types
python -m mypy.stubtest cognee_pipeline          # stubs match the module
# from repo root
scripts/check_all.sh
```

## Risks / notes

- Stubs drift from the implementation over time; `stubtest` in CI is the
  mitigation — add it (see also [06-python-packaging-tests.md](06-python-packaging-tests.md)
  for declaring the dev dep).
- The result-casing decision interacts with the compat layer
  ([04-python-sdk-parity.md](04-python-sdk-parity.md)) and with cross-SDK tests
  that may assert camelCase — coordinate so the wire contract for C/JS is
  untouched and only the Python-facing view changes.
- Until stubs land, the *honest* interim is to **remove** `py.typed` so checkers
  treat the package as untyped rather than typed-with-nothing. Do this only if
  stubs slip; the goal is stubs, not removal.
