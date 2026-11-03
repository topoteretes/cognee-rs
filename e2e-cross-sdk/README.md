# Cross-SDK E2E Tests

Docker-based harness that verifies parity between the Python and Rust cognee
SDKs by driving both CLIs and comparing the databases they produce.

> **The HTTP lane has moved.** `cognee-http-server` no longer lives in this
> repository, and the 28 `test_http_*.py` files, their
> fixtures (`http_helpers.py`, `seed.py`, the `py_client`/`rs_client`/
> `authed_clients` fixtures), the dual-server entrypoint
> (`bin/start_servers.sh`, `harness/wait_for_health.sh`), the
> `openapi.python.json` snapshot and the `e2e-http-tests` compose service went
> with it. What remains here is the CLI/DB lane.

## Architecture

A multi-stage Dockerfile packages the Rust `cognee-cli` release binary (built
on the runner inside the Dockerfile's `rust-build-env` stage — the harness's
own base image — and copied in from `cognee-rust/dist/`), a Python cognee venv and
this pytest harness into one image. Each test shells out to both CLIs in
isolated tmpfs workspaces and compares the resulting SQLite state.

## Running locally

```bash
cd cognee-rust/e2e-cross-sdk
touch ../.env                         # stub required by docker-compose env_file

# Build the binaries the image expects, from the cognee-rust repo root —
# in the harness's own base image, exactly as CI does (needs docker and a
# stock rustup install; see the script header):
#   bash e2e-cross-sdk/build-rust-binaries.sh
# Not a plain host `cargo build --release`: that links against the host's
# glibc, and the binaries fail to start in the harness (Debian Trixie,
# glibc 2.41) on any newer host.

# Telemetry parity (no LLM). `-p` gives it fresh volumes: the test clears
# /captures/all.jsonl, which it mounts read-only, so a capture left in the
# shared `telemetry-captures` volume by an earlier run fails it with
# "Read-only file system" (CI always starts from an empty volume):
docker compose -p e2e-telemetry-$$ -f docker-compose.yml run --rm e2e-telemetry
docker compose -p e2e-telemetry-$$ -f docker-compose.yml down -v

# COGX import contract (no LLM):
docker compose -f docker-compose.yml run --rm \
  -e COGX_REQUIRE_PYTHON_SUPPORT=1 e2e-tests \
  pytest -vs /harness/test_cogx_import_contract.py --tb=short

# LLM-gated suites (requires an OpenAI key):
docker compose -f docker-compose.yml run --rm \
  -e OPENAI_TOKEN=sk-... -e LLM_PROVIDER=openai e2e-tests \
  pytest -vs /harness/test_provenance_parity.py --tb=short
```

## Python pin

The Python side is the `topoteretes/cognee` commit pinned in
`.github/workflows/http-parity.yml` (the `Checkout cognee` step), checked out
as the `cognee/` sibling of this repository. Locally, check out the same SHA
next to `cognee-rust/`. The Rust SDK follows Python's post-SDK-1115 state (no
triplet embedding, no `TRIPLET_COMPLETION`, no memify), so the pin must be at
or after that change.

Two things about the current Python that the harness accounts for:

- **Python's graph lives in the graph store only.** On Ladybug, Python keeps
  node provenance in the graph and does not write the relational `nodes` /
  `edges` rollback ledger, so those tables stay empty after cognify. Read
  Python's nodes and edges with `helpers.python_graph(workspace)`, never with
  `query_nodes` / `query_edges` on Python's SQLite. Rust's relational tables
  are still read directly.
- **Python's CHUNKS / SUMMARIES results are vector payloads** (`IndexSchema`
  dumps whose content is `text`), not `search_result` wrappers.

## What this harness does and does not prove

Read this before citing a green run as evidence of parity.

**Most of these test files are run by nothing.** MEASURED at the time this
section was written: of the 20 files in `harness/`, only **five** are selected
by any workflow step — `test_telemetry_parity`, `test_cogx_import_contract`,
`test_cogx_roundtrip`, `test_provenance_parity` and `test_logging_parity`.
`test_add_parity`, `test_cross_read`, `test_search_parity` and the other dozen
are collected by no CI lane at all. This is a pre-existing gap, not something
the HTTP move caused, but it is the single most important caveat on this
directory: "the cross-SDK parity lane is green" does not mean these files
passed, it means those five did.

**The CLI/DB comparison is the strong kind.** `test_*_parity`, `test_cross_*`
and `test_readd_*` shell out to both CLIs and compare SQLite state directly,
with exact equality on content hashes, dataset UUIDs and row counts — there is no
tolerance/ignore list anywhere in that path. (The HTTP lane that left compared
JSON responses through an ignore set that stripped every `id` field; that
weaker shape is gone from this repo along with it.)

**Known limits, in rough order of how much they weaken a green run:**

- **Rust's identity is seeded from Python's database.** `conftest.py` reads
  `owner_id`/`tenant_id` out of Python's SQLite and passes them to the Rust CLI,
  so UUID5 agreement proves the hash function agrees on identical inputs — not
  that both sides derive the same inputs.
- **Data ids are random in both SDKs.** A new `data` row gets a `uuid4` and
  dedup is a lookup by `(content_hash, dataset_id, owner_id, tenant_id)`
  (Python `ingest_data.py:364-389`), so `data.id` is never compared across SDKs;
  the tests compare content hashes, dataset membership and the uuid4 shape
  instead (SDK-1039 D7).
- **"Both absent" scores as agreement.** Tests that accept a missing result on
  both sides convert *neither side implements this* into a pass. Prefer
  `xfail(strict=True)` keyed to a tracked gap.
- **LLM determinism is unwired.** Rust has `MOCK_LLM`, `crates/llm/src/mock/`
  and cassette replay (`COGNEE_RECORD_LLM` / `COGNEE_TEST_REPLAY`); nothing
  under `e2e-cross-sdk/` uses any of it. Python has no general LLM mock, and a
  shared fake cannot key on prompt text because the two SDKs' prompts differ.

A fuller accounting, with the divergences these limits were hiding, is in
[`docs/roadmap/python-parity-audit.md`](../docs/roadmap/python-parity-audit.md).

## COGX golden archive

`harness/golden/cogx_archive/` is a COGX archive written by the Rust exporter
itself. `test_cogx_import_contract.py` runs Python cognee's real migration
loader over it, which gates the Rust→Python format contract on every PR — the
full roundtrip needs an LLM (you cannot export a graph you have not cognified)
and so only runs on key-gated lanes.

Regenerate it whenever the exporter's output changes; the same Rust test fails
if the committed copy drifts:

```bash
COGX_REGENERATE_GOLDEN=1 cargo test -p cognee-migration --test python_import_contract
```

## CI gate

The `Cross-SDK Parity` workflow (`.github/workflows/http-parity.yml` — the
filename and the job id `http-parity` are deliberately unchanged, because that
job id is the required-status-check context on `main`) runs on every push and
PR to `main`/`master`.

| Suite | Trigger | LLM | Release gate |
|---|---|---|---|
| Telemetry parity | push + PR | no | **required** |
| COGX import contract (Python reads a Rust archive) | push + PR | no | **required** |
| COGX roundtrip (Rust export → Python import) | push + PR when `OPENAI_KEY` set | yes | recommended |
| Provenance parity | push + PR when `OPENAI_KEY` set | yes | recommended |
| Logging parity | push + PR (when `OPENAI_KEY` present) | no | recommended |

Telemetry parity and the COGX import contract run unconditionally on every
push/PR (no OpenAI key required) and are the release gate. LLM-gated suites use
`secrets.OPENAI_KEY` (the same secret as `ci.yml`) and skip cleanly on forks
without the key. `workflow_dispatch` is still declared so the lane can be
re-run by hand; it no longer takes any input.
