"""Shared helpers for cross-SDK E2E tests.

Provides CLI runners for both the Rust and Python cognee CLIs,
SQLite query helpers, and file-comparison utilities.
"""

import json
import os
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
        "VECTOR_DB_PROVIDER": "lancedb",
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
        # Parse: cognify -d <dataset_name>
        dataset_name = None
        i = 1
        while i < len(args):
            if args[i] in ("-d", "--datasets") and i + 1 < len(args):
                dataset_name = args[i + 1]
                i += 2
            else:
                i += 1

        ds_arg = f"[{dataset_name!r}]" if dataset_name else "None"
        script = (
            f"import asyncio, cognee\n"
            f"cognee.config.data_root_directory({str(py_storage)!r})\n"
            f"cognee.config.system_root_directory({str(py_system)!r})\n"
            f"async def main():\n"
            f"    await cognee.cognify(datasets={ds_arg})\n"
            f"    print('OK')\n"
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
