# Minor Engine-Tier Gaps

## Status: ⚠️ Partially implemented

These are small gaps in the existing pipeline engine tier. The core functionality is complete;
these are missing convenience features present in both the C API and TS binding.

---

## Gap 1: `CancellationToken` and `cancellation_pair()`

### Status: ❌ Not implemented

### What is missing

The C API and TS binding expose a *pair* of objects: a `CancellationHandle` (owned by the
orchestrator, used to request cancellation) and a `CancellationToken` (shared with tasks, used to
observe cancellation). Python currently only exposes `CancellationHandle` via
`ctx.cancellation_handle`. There is no way to create an independent pair or share the token with
external code.

| Python name | C API | TS | Description |
|-------------|-------|----|-------------|
| `cancellation_pair()` | `cg_cancellation_pair()` | `createCancellationPair()` | Create a linked handle + token pair |
| `CancellationToken` | `CgCancellationToken*` | `CancellationToken` class | Observe-only side of the pair |
| `CancellationToken.is_cancelled` | `cg_cancellation_token_is_cancelled` | `token.isCancelled` | Check if cancelled |
| `CancellationToken.clone()` | `cg_cancellation_token_clone` | `token.clone()` | Clone token for sharing |

### Rationale

Without a `CancellationToken` object, external code that needs to *observe* cancellation (e.g.,
a long-running task that checks cancellation without having the handle) cannot do so cleanly.
The handle is the *authority* to cancel; the token is the *read-only view* of the cancellation
state.

### Implementation plan

**Step 1** — Create `PyCancellationToken` in `python/src/cancellation.rs`:

```rust
#[pyclass(name = "CancellationToken")]
pub struct PyCancellationToken {
    inner: CancellationToken,   // from cognee-core
}

#[pymethods]
impl PyCancellationToken {
    #[getter]
    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn clone_token(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}
```

**Step 2** — Add `cancellation_pair()` as a module-level function. In `cognee-core` this is a
free function, `cognee_core::cancellation_pair()` (see `crates/core/src/cancellation.rs:9`), not
an associated constructor:

```rust
#[pyfunction]
fn cancellation_pair(py: Python<'_>) -> PyResult<Bound<'_, PyTuple>> {
    let (handle, token) = cognee_core::cancellation_pair();
    let py_handle = Py::new(py, PyCancellationHandle { inner: handle })?;
    let py_token = Py::new(py, PyCancellationToken { inner: token })?;
    Ok(PyTuple::new(py, [py_handle.into_any(), py_token.into_any()])?)
}
```

**Step 3** — Expose in `__init__.py`:
```python
from cognee_pipeline._native import CancellationToken, cancellation_pair
```

**Step 4** — Tests:
```python
def test_cancellation_pair():
    handle, token = cancellation_pair()
    assert not token.is_cancelled
    handle.cancel()
    assert token.is_cancelled
    token2 = token.clone_token()
    assert token2.is_cancelled
```

---

## Gap 2: `ProgressToken.width` and `ProgressToken.subtoken()`

### Status: ❌ Not implemented (two methods)

### What is missing

| Python name | C API | TS | Description |
|-------------|-------|----|-------------|
| `token.width` | `cg_progress_token_width()` | `token.width` | The fraction of the root that this token occupies |
| `token.subtoken(frac_width)` | `cg_progress_token_subtoken()` | `token.subtoken()` | Create a sub-token occupying a given fraction of this token's width |

### Rationale

`width` and `subtoken()` are used for tree-structured progress tracking where a single token needs
to be subdivided by fractional width rather than equal-weight splitting. Without them, the Python
binding cannot fully express the same progress semantics as C and TS callers.

### Implementation plan

In `python/src/progress.rs`, add to `PyCogneeProgressToken`:

```rust
#[getter]
fn width(&self) -> f64 {
    self.inner.width()
}

fn subtoken(&self, frac_width: f64) -> PyResult<Self> {
    if !(0.0..=1.0).contains(&frac_width) {
        return Err(PyValueError::new_err("frac_width must be in [0.0, 1.0]"));
    }
    Ok(Self { inner: self.inner.subtoken(frac_width) })
}
```

**Tests**:
```python
def test_progress_width():
    root = ProgressToken()
    assert root.width == 1.0

def test_subtoken():
    root = ProgressToken()
    sub = root.subtoken(0.5)
    assert abs(sub.width - 0.5) < 1e-9
    sub.set(1.0)
    assert abs(root.root_fraction - 0.5) < 1e-9
```

---

## Gap 3: Typed `Watcher` class

### Status: ⚠️ Duck-typed bridge works; no explicit factory

### What is missing

The C API exposes `CgPipelineWatcher` built from an explicit vtable. TS exposes `createWatcher(events)`
and `createNoopWatcher()`. Python uses a duck-typed bridge: any object with the right method names
works as a watcher. This is Pythonic, but there is no explicit `Watcher` class or factory.

The gap: there is no `createWatcher(events={...})` or `Watcher.noop()` factory that Python users
can discover from type hints or IDE autocomplete. Users must know to pass any object with the right
method names.

### Rationale

This is a usability gap, not a functionality gap — the duck-typed bridge already works. However,
for a production Python SDK, discoverability matters. Adding a typed `Watcher` class with an
event-dict constructor makes the API self-documenting and matches the TS pattern.

### Implementation plan

**Option A (pure Python, preferred):** Add a `Watcher` class in `cognee_pipeline/__init__.py`:

```python
class Watcher:
    """A pipeline watcher that forwards events to Python callbacks."""

    def __init__(self, **callbacks):
        self._callbacks = callbacks

    @classmethod
    def noop(cls) -> "Watcher":
        return cls()

    def on_pipeline_run_started(self, run_id: str, pipeline_name: str) -> None:
        if cb := self._callbacks.get("on_pipeline_run_started"):
            cb(run_id, pipeline_name)

    def on_pipeline_run_completed(self, run_id: str, output_count: int) -> None:
        if cb := self._callbacks.get("on_pipeline_run_completed"):
            cb(run_id, output_count)

    def on_pipeline_run_errored(self, run_id: str, error: str) -> None:
        if cb := self._callbacks.get("on_pipeline_run_errored"):
            cb(run_id, error)

    def on_task_started(self, run_id: str, task_name: str, task_index: int) -> None:
        if cb := self._callbacks.get("on_task_started"):
            cb(run_id, task_name, task_index)

    def on_task_completed(self, run_id: str, task_name: str, output_count: int) -> None:
        if cb := self._callbacks.get("on_task_completed"):
            cb(run_id, task_name, output_count)

    def on_task_errored(self, run_id: str, task_name: str, error: str) -> None:
        if cb := self._callbacks.get("on_task_errored"):
            cb(run_id, task_name, error)
```

Usage:
```python
watcher = Watcher(
    on_task_started=lambda run_id, name, idx: print(f"Task {name} started"),
    on_pipeline_run_completed=lambda run_id, count: print(f"Done: {count} outputs"),
)
result = await pipeline.execute(inputs, ctx, watcher=watcher)
```

No Rust changes needed for this option.

**Option B:** Keep duck-typing only but add type stubs (`.pyi` file) documenting the expected
interface. Less work, less discoverable.

**Recommendation:** Option A — pure Python class, zero Rust cost, good IDE experience.

**Tests**:
```python
def test_watcher_factory():
    received = []
    w = Watcher(on_task_started=lambda *a: received.append(a))
    # pass w as watcher to a pipeline and verify callback fires
```
