"""Shared helpers for cross-SDK E2E tests.

Provides CLI runners for both the Rust and Python cognee CLIs,
SQLite query helpers, and file-comparison utilities.
"""

import json
import os
import re
import sqlite3
import subprocess
from pathlib import Path
from typing import Optional

RUST_CLI = "/usr/local/bin/cognee-cli-rust"
PYTHON_CLI = "/usr/local/bin/cognee-cli-python"
PYTHON_RUNNER = "/opt/python-venv/bin/python3"
TEST_DATA_DIR = Path("/test_data")

ONNX_MODEL_PATH = "/opt/models/BGE-Small-v1.5-model_quantized.onnx"
ONNX_TOKENIZER_PATH = "/opt/models/bge-small-tokenizer.json"

NLP_TEXT_FILE = TEST_DATA_DIR / "natural_language_processing.txt"
QC_TEXT_FILE = TEST_DATA_DIR / "quantum_computers.txt"

DATASET_NAME = "e2e_test"


# ── CLI runners ──────────────────────────────────────────────────────────────


def run_rust_cli(
    workdir: Path,
    args: list[str],
    *,
    env: Optional[dict] = None,
    check: bool = True,
) -> subprocess.CompletedProcess:
    """Run the Rust cognee-cli with config pointed at *workdir*.

    XDG_CONFIG_HOME is set so the Rust CLI writes/reads its config.json
    from ``workdir/config/cognee-rust/config.json``.
    """
    config_home = workdir / "config"
    config_home.mkdir(parents=True, exist_ok=True)

    run_env = {
        **os.environ,
        "XDG_CONFIG_HOME": str(config_home),
    }
    if env:
        run_env.update(env)

    return subprocess.run(
        [RUST_CLI, *args],
        cwd=str(workdir),
        env=run_env,
        capture_output=True,
        text=True,
        check=check,
        timeout=120,
    )


def run_python_cli(
    workdir: Path,
    args: list[str],
    *,
    env: Optional[dict] = None,
    check: bool = True,
) -> subprocess.CompletedProcess:
    """Run a Python cognee operation via the API (not the CLI wrapper).

    Uses a small Python script that calls ``cognee.add()`` / ``cognee.cognify()``
    directly, avoiding the CLI's generic exception handler that swallows errors.
    The first element of *args* must be the command (``"add"`` or ``"cognify"``).
    """
    py_system = workdir / ".cognee_system"
    py_storage = workdir / ".data_storage"
    py_system.mkdir(parents=True, exist_ok=True)
    py_storage.mkdir(parents=True, exist_ok=True)

    openai_key = os.environ.get("OPENAI_API_KEY") or os.environ.get("OPENAI_TOKEN", "")
    llm_model = os.environ.get("LLM_MODEL") or os.environ.get("OPENAI_MODEL", "gpt-4o-mini")

    run_env = {
        **os.environ,
        "DATA_ROOT_DIRECTORY": str(py_storage),
        "SYSTEM_ROOT_DIRECTORY": str(py_system),
        "DB_PROVIDER": "sqlite",
        "DB_NAME": "cognee_db",
        "GRAPH_DATABASE_PROVIDER": "kuzu",
        "VECTOR_DB_PROVIDER": "brute-force",
        "LLM_API_KEY": openai_key,
        "LLM_MODEL": llm_model,
        "LLM_PROVIDER": "openai",
        "EMBEDDING_PROVIDER": "openai",
        "EMBEDDING_MODEL": "openai/text-embedding-3-small",
        "EMBEDDING_DIMENSIONS": "1536",
        # Skip LLM connection test — add doesn't need an LLM, and we may
        # not have a valid API key for add-only tests.
        "COGNEE_SKIP_CONNECTION_TEST": "true",
    }
    if env:
        run_env.update(env)

    # Parse the CLI-like args into a Python API call
    command = args[0]  # "add" or "cognify"

    if command == "add":
        # Parse: add <data...> -d <dataset_name>
        dataset_name = "main_dataset"
        data_items = []
        i = 1
        while i < len(args):
            if args[i] in ("-d", "--dataset-name") and i + 1 < len(args):
                dataset_name = args[i + 1]
                i += 2
            else:
                data_items.append(args[i])
                i += 1

        # Generate inline Python script
        data_repr = repr(data_items if len(data_items) > 1 else data_items[0])
        script = (
            f"import asyncio, cognee\n"
            f"cognee.config.data_root_directory({str(py_storage)!r})\n"
            f"cognee.config.system_root_directory({str(py_system)!r})\n"
            f"async def main():\n"
            f"    await cognee.add(data={data_repr}, dataset_name={dataset_name!r})\n"
            f"    print('OK')\n"
            f"asyncio.run(main())\n"
        )
    elif command == "cognify":
        # Parse: cognify -d <dataset_name> [--temporal-cognify]
        dataset_name = None
        temporal_cognify = False
        i = 1
        while i < len(args):
            if args[i] in ("-d", "--datasets") and i + 1 < len(args):
                dataset_name = args[i + 1]
                i += 2
            elif args[i] == "--temporal-cognify":
                temporal_cognify = True
                i += 1
            else:
                i += 1

        ds_arg = f"[{dataset_name!r}]" if dataset_name else "None"
        temporal_arg = "True" if temporal_cognify else "False"
        script = (
            f"import asyncio, cognee\n"
            f"cognee.config.data_root_directory({str(py_storage)!r})\n"
            f"cognee.config.system_root_directory({str(py_system)!r})\n"
            f"async def main():\n"
            f"    await cognee.cognify(datasets={ds_arg}, temporal_cognify={temporal_arg})\n"
            f"    print('OK')\n"
            f"asyncio.run(main())\n"
        )
    elif command == "memify":
        # Parse: memify -d <dataset_name> [--node-name <name> ...]
        dataset_name = None
        node_names: list[str] = []
        i = 1
        while i < len(args):
            if args[i] in ("-d", "--datasets", "--dataset-name") and i + 1 < len(args):
                dataset_name = args[i + 1]
                i += 2
            elif args[i] in ("--node-name",) and i + 1 < len(args):
                node_names.append(args[i + 1])
                i += 2
            else:
                i += 1

        ds_arg = f"{dataset_name!r}" if dataset_name else "'main_dataset'"
        node_name_arg = f"{node_names!r}" if node_names else "None"
        # Invoke cognee.memify() with the existing graph as the enrichment
        # source. Default extraction/enrichment tasks are used.
        script = (
            f"import asyncio, cognee\n"
            f"cognee.config.data_root_directory({str(py_storage)!r})\n"
            f"cognee.config.system_root_directory({str(py_system)!r})\n"
            f"async def main():\n"
            f"    await cognee.memify(dataset={ds_arg}, node_name={node_name_arg})\n"
            f"    print('OK')\n"
            f"asyncio.run(main())\n"
        )
    elif command == "delete":
        # Parse: delete [--all] [-f] [-d <dataset_name>] [--data-id <id>]
        delete_all = False
        dataset_name = None
        data_id = None
        i = 1
        while i < len(args):
            if args[i] in ("--all",):
                delete_all = True
                i += 1
            elif args[i] in ("-f", "--force"):
                # Python SDK doesn't need --force; skip it
                i += 1
            elif args[i] in ("-d", "--dataset-name") and i + 1 < len(args):
                dataset_name = args[i + 1]
                i += 2
            elif args[i] in ("--data-id",) and i + 1 < len(args):
                data_id = args[i + 1]
                i += 2
            else:
                i += 1

        # Python SDK: cognee.prune.prune_data() removes all data,
        # cognee.prune.prune_system(metadata=True) removes system tables.
        # For --all we call both; for dataset-scoped we call prune_data
        # with the dataset filter.
        if delete_all:
            script = (
                f"import asyncio, cognee\n"
                f"cognee.config.data_root_directory({str(py_storage)!r})\n"
                f"cognee.config.system_root_directory({str(py_system)!r})\n"
                f"async def main():\n"
                f"    await cognee.prune.prune_data()\n"
                f"    await cognee.prune.prune_system(metadata=True)\n"
                f"    print('OK')\n"
                f"asyncio.run(main())\n"
            )
        elif dataset_name:
            script = (
                f"import asyncio, cognee\n"
                f"cognee.config.data_root_directory({str(py_storage)!r})\n"
                f"cognee.config.system_root_directory({str(py_system)!r})\n"
                f"async def main():\n"
                f"    await cognee.prune.prune_data(dataset_name={dataset_name!r})\n"
                f"    print('OK')\n"
                f"asyncio.run(main())\n"
            )
        else:
            script = (
                f"import asyncio, cognee\n"
                f"cognee.config.data_root_directory({str(py_storage)!r})\n"
                f"cognee.config.system_root_directory({str(py_system)!r})\n"
                f"async def main():\n"
                f"    await cognee.prune.prune_data()\n"
                f"    print('OK')\n"
                f"asyncio.run(main())\n"
            )
    elif command == "search":
        # Parse: search <query> -t <TYPE> -d <dataset> -k <top_k>
        query_text = args[1]
        query_type = "GRAPH_COMPLETION"
        dataset_name = None
        top_k = 10
        i = 2
        while i < len(args):
            if args[i] in ("-t", "--query-type") and i + 1 < len(args):
                query_type = args[i + 1]
                i += 2
            elif args[i] in ("-d", "--datasets") and i + 1 < len(args):
                dataset_name = args[i + 1]
                i += 2
            elif args[i] in ("-k", "--top-k") and i + 1 < len(args):
                top_k = int(args[i + 1])
                i += 2
            else:
                i += 1

        ds_arg = f"[{dataset_name!r}]" if dataset_name else "None"
        # The script prints a sentinel-wrapped JSON payload with the list of
        # string-coerced search_result values for each SearchResult.  The
        # harness extracts it with `parse_python_search_output`.
        # ``cognee.search`` returns one of several shapes depending on whether
        # backend access control is enabled, whether ``verbose=True``, and
        # (for single-dataset CHUNKS/SUMMARIES) whether a list-of-lists gets
        # unwrapped.  We flatten everything to a list of strings here so the
        # parity tests can do substring / len assertions uniformly.
        script = (
            f"import asyncio, json, cognee\n"
            f"cognee.config.data_root_directory({str(py_storage)!r})\n"
            f"cognee.config.system_root_directory({str(py_system)!r})\n"
            f"def _extract(item):\n"
            f"    if isinstance(item, dict):\n"
            f"        return item.get('search_result') or item.get('text_result') or item.get('context_result')\n"
            f"    return item\n"
            f"async def main():\n"
            f"    results = await cognee.search(\n"
            f"        query_text={query_text!r},\n"
            f"        query_type=cognee.SearchType.{query_type},\n"
            f"        datasets={ds_arg},\n"
            f"        top_k={top_k},\n"
            f"    )\n"
            f"    payload = []\n"
            f"    for r in results:\n"
            f"        v = _extract(r)\n"
            f"        if isinstance(v, list):\n"
            f"            payload.extend(str(x) for x in v if x is not None)\n"
            f"        elif v is not None:\n"
            f"            payload.append(str(v))\n"
            f"    print('>>>RESULTS<<<' + json.dumps(payload) + '>>>END<<<')\n"
            f"asyncio.run(main())\n"
        )
    else:
        raise ValueError(f"Unsupported Python command: {command}")

    return subprocess.run(
        [PYTHON_RUNNER, "-c", script],
        cwd=str(workdir),
        env=run_env,
        capture_output=True,
        text=True,
        check=check,
        timeout=120,
    )


# ── Rust config helpers ──────────────────────────────────────────────────────


def write_rust_config(
    workdir: Path,
    *,
    user_id: str = "00000000-0000-0000-0000-000000000001",
    extra: Optional[dict] = None,
) -> Path:
    """Write a Rust CLI config.json under *workdir*/config/cognee-rust/.

    Returns the path to the config file.
    """
    config_dir = workdir / "config" / "cognee-rust"
    config_dir.mkdir(parents=True, exist_ok=True)
    (workdir / ".data_storage").mkdir(parents=True, exist_ok=True)
    (workdir / ".cognee_system").mkdir(parents=True, exist_ok=True)
    config_path = config_dir / "config.json"

    openai_key = os.environ.get("OPENAI_API_KEY") or os.environ.get("OPENAI_TOKEN", "")
    llm_model = os.environ.get("LLM_MODEL") or os.environ.get("OPENAI_MODEL", "gpt-4o-mini")
    llm_endpoint = os.environ.get("OPENAI_URL", "https://api.openai.com/v1")

    settings = {
        "default_user_id": user_id,
        "relational_db_url": "sqlite:./cognee.db?mode=rwc",
        "data_root_directory": str(workdir / ".data_storage"),
        "system_root_directory": str(workdir / ".cognee_system"),
        "llm_provider": "openai",
        "llm_model": llm_model,
        "llm_api_key": openai_key,
        "llm_endpoint": llm_endpoint,
        "embedding_model_path": ONNX_MODEL_PATH,
        "embedding_tokenizer_path": ONNX_TOKENIZER_PATH,
    }
    if extra:
        settings.update(extra)

    config_path.write_text(json.dumps({"version": 1, "settings": settings}, indent=2))
    return config_path


# ── SQLite helpers ───────────────────────────────────────────────────────────


def open_db(path: Path) -> sqlite3.Connection:
    """Open a SQLite database at *path* with Row factory."""
    conn = sqlite3.connect(str(path))
    conn.row_factory = sqlite3.Row
    return conn


def _normalize_uuid(val):
    """Convert a UUID value to a lowercase hex string regardless of storage format.

    Python cognee stores UUIDs as hex text (no hyphens),
    Rust cognee stores them as hyphenated text or raw bytes.
    """
    if val is None:
        return None
    if isinstance(val, bytes):
        return val.hex()
    s = str(val).replace("-", "").lower()
    return s


def query_rows(conn: sqlite3.Connection, sql: str) -> list[dict]:
    """Execute *sql* and return results as a list of dicts.

    UUID columns (id, owner_id, tenant_id, dataset_id, data_id) are
    normalized to lowercase hex strings for cross-SDK comparison.
    """
    UUID_COLUMNS = {
        "id", "owner_id", "tenant_id", "dataset_id", "data_id",
        "user_id", "slug", "source_node_id", "destination_node_id",
        "query_id", "pipeline_run_id", "pipeline_id",
    }
    cursor = conn.execute(sql)
    rows = []
    for row in cursor.fetchall():
        d = dict(row)
        for col in UUID_COLUMNS:
            if col in d:
                d[col] = _normalize_uuid(d[col])
        rows.append(d)
    return rows


def query_data(conn: sqlite3.Connection) -> list[dict]:
    return query_rows(conn, "SELECT * FROM data ORDER BY name")


def query_datasets(conn: sqlite3.Connection) -> list[dict]:
    return query_rows(conn, "SELECT * FROM datasets ORDER BY name")


def query_dataset_data(conn: sqlite3.Connection) -> list[dict]:
    return query_rows(conn, "SELECT * FROM dataset_data")


def query_nodes(conn: sqlite3.Connection) -> list[dict]:
    # Rust uses "node_type", Python uses "type". Use * and sort in Python.
    rows = query_rows(conn, "SELECT * FROM nodes")
    # Normalize: expose "type" key regardless of actual column name
    for r in rows:
        if "node_type" in r and "type" not in r:
            r["type"] = r["node_type"]
        elif "type" in r and "node_type" not in r:
            r["node_type"] = r["type"]
    rows.sort(key=lambda r: (r.get("type", ""), r.get("label", "")))
    return rows


def query_nodes_by_type(conn: sqlite3.Connection, node_type: str) -> list[dict]:
    """Return all graph nodes of a given type from the relational SQLite store.

    Both the Rust and Python SDKs write graph nodes to a ``nodes`` table in
    the relational SQLite DB.  The column holding the node type is named
    ``type`` (matching Python's SQLAlchemy schema); the Rust SeaORM entity
    maps the Rust field ``node_type`` to this same ``type`` column.
    """
    rows = conn.execute(
        'SELECT * FROM nodes WHERE "type" = ?',
        (node_type,),
    ).fetchall()
    result = []
    for r in rows:
        d = dict(r)
        # Normalise: always expose both "type" and "node_type" keys
        if "node_type" in d and "type" not in d:
            d["type"] = d["node_type"]
        elif "type" in d and "node_type" not in d:
            d["node_type"] = d["type"]
        result.append(d)
    return result


def query_edges(conn: sqlite3.Connection) -> list[dict]:
    rows = query_rows(conn, "SELECT * FROM edges")
    rows.sort(key=lambda r: r.get("relationship_name", ""))
    return rows


# ── DB path resolvers ────────────────────────────────────────────────────────


def python_db_path(workspace: Path) -> Path:
    """Return the SQLite path that Python cognee uses.

    Python stores its DB at {SYSTEM_ROOT_DIRECTORY}/databases/cognee_db.
    """
    return workspace / ".cognee_system" / "databases" / "cognee_db"


def rust_db_path(workspace: Path) -> Path:
    """Return the SQLite path that Rust cognee uses.

    The Rust CLI config sets relational_db_url = "sqlite:{workdir}/cognee.db".
    """
    return workspace / "cognee.db"


# ── File helpers ─────────────────────────────────────────────────────────────


def resolve_stored_file(data_root: Path, raw_data_location: str) -> Path:
    """Resolve a raw_data_location value to an actual file path.

    Rust stores ``file://<absolute_path>``; Python stores an absolute path
    (sometimes with ``file://`` prefix, sometimes without).
    """
    loc = raw_data_location
    if loc.startswith("file://"):
        loc = loc[len("file://"):]
    return Path(loc)


def read_stored_file(data_root: Path, raw_data_location: str) -> bytes:
    """Read the bytes of a stored file given its raw_data_location."""
    path = resolve_stored_file(data_root, raw_data_location)
    return path.read_bytes()


# ── Search helpers ───────────────────────────────────────────────────────────

# The Rust CLI prints search results to stdout via `println!` (plain, no log
# prefix). Dependency-crate logging may still reach stdout through the tracing
# subscriber, whose default `fmt` formatter (a) wraps the timestamp and level in
# ANSI color escapes and (b) prefixes a line like:
#   ``\x1b[2m2026-04-13T12:34:56.789Z\x1b[0m \x1b[32m INFO\x1b[0m some log line``
# We strip the ANSI escapes first, then the timestamp+level prefix, so any
# stray log lines collapse to empty/noise and the plain result lines survive.
_ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-9;]*m")
_LOG_PREFIX_RE = re.compile(
    r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z?\s+(?:TRACE|DEBUG|INFO|WARN|ERROR)\s+"
)

# Rust CLI "pretty"/"simple" output noise lines we strip before joining.
_RUST_NOISE_LINES = (
    "No results found for your query.",
    "No rows returned.",
    "No rules returned.",
)

# To quieten the Rust CLI so we can isolate the actual search payload from the
# dependency-crate logging (sqlx migrations, pgvector setup, ort model
# loading, etc.) we restrict tracing to errors globally. Search results reach
# stdout via `println!` regardless of log level, so no info-level filter is
# needed to capture them.
RUST_SEARCH_LOG_FILTER = "error"


def _strip_log_prefix(line: str) -> str:
    """Remove ANSI escapes and the tracing subscriber prefix from one line."""
    without_ansi = _ANSI_ESCAPE_RE.sub("", line)
    return _LOG_PREFIX_RE.sub("", without_ansi, count=1)


def parse_python_search_output(stdout: str) -> list[str]:
    """Extract the sentinel-wrapped JSON payload printed by the Python search script."""
    marker_start = ">>>RESULTS<<<"
    marker_end = ">>>END<<<"
    start = stdout.find(marker_start)
    end = stdout.find(marker_end)
    if start == -1 or end == -1 or end < start:
        raise ValueError(
            f"Python search output did not contain sentinel markers.\n"
            f"--- stdout ---\n{stdout}"
        )
    payload = stdout[start + len(marker_start) : end]
    return json.loads(payload)


def parse_rust_search_output(stdout: str, *, query_type: str) -> list[str]:
    """Extract per-result strings from the Rust CLI's ``-f simple`` output.

    For GRAPH_COMPLETION / RAG_COMPLETION the entire response is a single text
    blob — we return it as a one-element list.  For CHUNKS / SUMMARIES each
    result is printed on its own line.
    """
    lines = []
    for raw in stdout.splitlines():
        stripped = _strip_log_prefix(raw).strip()
        if not stripped:
            continue
        if stripped in _RUST_NOISE_LINES:
            continue
        lines.append(stripped)

    if not lines:
        return []

    if query_type in ("GRAPH_COMPLETION", "RAG_COMPLETION"):
        # Single-blob text response — re-join all log-prefix-stripped lines
        # back together (the LLM may have emitted multi-line output).
        return ["\n".join(lines)]

    # CHUNKS / SUMMARIES / other list-shaped outputs: one item per line.
    return lines


def run_python_memify(
    workdir: Path,
    dataset: str,
    *,
    node_names: Optional[list[str]] = None,
    check: bool = True,
) -> subprocess.CompletedProcess:
    """Run ``cognee.memify()`` via the Python SDK on *dataset* in *workdir*.

    Mirrors the ``memify`` CLI subcommand on the Rust side:
    ``run_rust_cli(workdir, ["memify", "-d", dataset])``.
    """
    args = ["memify", "-d", dataset]
    if node_names:
        for n in node_names:
            args.extend(["--node-name", n])
    return run_python_cli(workdir, args, check=check)


def run_python_search(
    workdir: Path,
    query: str,
    *,
    query_type: str = "GRAPH_COMPLETION",
    dataset: Optional[str] = None,
    top_k: int = 10,
    check: bool = True,
) -> list[str]:
    """Run search via the Python SDK and return the list of result strings."""
    args = ["search", query, "-t", query_type, "-k", str(top_k)]
    if dataset:
        args.extend(["-d", dataset])
    result = run_python_cli(workdir, args, check=check)
    if result.returncode != 0:
        return []
    return parse_python_search_output(result.stdout)


def _ensure_rust_system_prompt(workdir: Path) -> Path:
    """Write a minimal system prompt file the Rust CLI can open.

    The Rust CLI defaults to the filename ``answer_simple_question.txt`` and
    tries to read it literally as a path — it is not bundled with prompt
    templates the way the Python SDK is.  Completion searches (GRAPH_COMPLETION,
    RAG_COMPLETION) therefore need a real file passed via ``--system-prompt-path``.
    """
    prompt_path = workdir / "answer_simple_question.txt"
    if not prompt_path.exists():
        prompt_path.write_text(
            "You are a helpful assistant. Answer the user's question using the "
            "provided context. If the context does not answer the question, say so.\n"
        )
    return prompt_path


def run_rust_search(
    workdir: Path,
    query: str,
    *,
    query_type: str = "GRAPH_COMPLETION",
    dataset: Optional[str] = None,
    top_k: int = 10,
    check: bool = True,
) -> list[str]:
    """Run search via the Rust CLI and return the list of result strings."""
    prompt_path = _ensure_rust_system_prompt(workdir)
    args = [
        "search",
        query,
        "-t",
        query_type,
        "-k",
        str(top_k),
        "-f",
        "simple",
        "--system-prompt-path",
        str(prompt_path),
    ]
    if dataset:
        args.extend(["-d", dataset])
    # Silence dependency-crate info logs so the search payload is the only
    # thing at INFO level on stdout.
    result = run_rust_cli(
        workdir, args, check=check, env={"RUST_LOG": RUST_SEARCH_LOG_FILTER}
    )
    if result.returncode != 0:
        return []
    return parse_rust_search_output(result.stdout, query_type=query_type)
