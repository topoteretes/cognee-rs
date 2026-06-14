# PKG-1 — Python: declare test/dev dependencies and example deps

- **Binding:** Python (`python/`)
- **Dimension:** Cleanliness
- **Priority:** P2
- **Status:** Not started

## Problem

[python/scripts/check.sh](../../python/scripts/check.sh) runs `maturin develop` then
`pytest tests/ -v`, but [python/pyproject.toml](../../python/pyproject.toml) declares
**no test/dev dependencies**. The test suite uses `@pytest.mark.asyncio`
heavily (e.g. [python/tests/test_async.py](../../python/tests/test_async.py),
[python/tests/test_data_ops.py](../../python/tests/test_data_ops.py)), which requires
`pytest-asyncio`, and there is no `asyncio_mode` configuration. The check
therefore only passes on a machine where `pytest`, `pytest-asyncio`, and
`maturin` happen to be pre-installed; on a clean environment it errors or
silently skips the async tests (which is most of the suite).

## Goal / definition of done

A clean checkout can run `python/scripts/check.sh` and get a deterministic,
correctly-configured test run with all async tests executing, using only deps
declared in `pyproject.toml`.

## Implementation plan

### Step 1 — Add an optional dev/test dependency group

In [python/pyproject.toml](../../python/pyproject.toml), add:

```toml
[project.optional-dependencies]
test = [
    "pytest>=8",
    "pytest-asyncio>=0.24",
]
dev = [
    "maturin>=1.5",
    "mypy>=1.10",        # for the stub checks in 05-python-typing-stubs.md
]
```

(Use `[dependency-groups]` per PEP 735 instead if the toolchain targets it; the
`optional-dependencies` form is the broadly-compatible choice.)

### Step 2 — Configure pytest-asyncio explicitly

Add a `[tool.pytest.ini_options]` section so async mode is not left to ambient
defaults:

```toml
[tool.pytest.ini_options]
asyncio_mode = "auto"
testpaths = ["tests"]
```

The existing tests all use `@pytest.mark.asyncio` explicitly (confirmed in
`test_async.py`, `test_data_ops.py`, and all other async test files). Use
`asyncio_mode = "strict"` — it matches the existing decorator style and does
not silently collect bare `async def` functions as tests.

### Step 3 — Install deps in `check.sh`

Update [python/scripts/check.sh](../../python/scripts/check.sh) to install the declared
extras before running pytest, so the script is self-contained:

```bash
maturin develop
pip install -e ".[test]"
pytest tests/ -v
```

Confirm ordering: `maturin develop` builds and installs the package first;
installing `.[test]` then pulls the test extra without rebuilding.

Also update the `python-check` job in [.github/workflows/ci.yml](../../.github/workflows/ci.yml)
(there is no separate `python-check.yml` — the job lives in `ci.yml` under
`jobs.python-check`, currently at the step "Create venv and install
dependencies" which manually does `pip install maturin pytest pytest-asyncio`).
Replace that hard-coded list with `pip install maturin && pip install -e
".[test]"` (run from `python/`) so the workflow derives its deps from
`pyproject.toml` rather than duplicating them.

### Step 4 — (Coordinated) add example deps

If [09-examples-parity.md](09-examples-parity.md) adds Python example scripts that
need extra packages (e.g. `python-dotenv`), add an `examples` extra so they are
declared, not assumed.

## Verification

```bash
# in a fresh venv
python -m venv /tmp/v && . /tmp/v/bin/activate
pip install maturin && cd python && maturin develop && pip install -e ".[test]"
pytest tests/ -v          # all async tests run (none silently skipped)
bash scripts/check.sh
```

Confirm that without `pytest-asyncio` installed the async tests would have been
skipped/errored, and that after this change they execute.

## Risks / notes

- `pytest-asyncio` mode (`auto` vs `strict`) changes how tests are collected;
  pick the one matching the current decorators to avoid mass test churn.
- Keep the Rust test harness disabled for this crate (`test = false`,
  `doctest = false` in [python/Cargo.toml](../../python/Cargo.toml)) — that is correct
  for a PyO3 `extension-module` cdylib and is out of scope here.
