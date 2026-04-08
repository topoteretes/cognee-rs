"""Diagnostic script: run cognify on both SDKs with verbose logging.

Shows intermediate edge/node counts at each pipeline stage to identify
where Rust and Python diverge in edge production.

Usage (inside Docker):
    python3 /harness/diag_edge_count.py
"""

import json
import os
import sqlite3
import subprocess
import tempfile
import shutil
from pathlib import Path

RUST_CLI = "/usr/local/bin/cognee-cli-rust"
PYTHON_RUNNER = "/opt/python-venv/bin/python3"
ONNX_MODEL_PATH = "/opt/models/BGE-Small-v1.5-model_quantized.onnx"
ONNX_TOKENIZER_PATH = "/opt/models/bge-small-tokenizer.json"
NLP_TEXT_FILE = Path("/test_data/natural_language_processing.txt")
DATASET_NAME = "diag_dataset"


def run(cmd, env=None, workdir=None, timeout=300):
    merged = {**os.environ, **(env or {})}
    result = subprocess.run(
        cmd, env=merged, cwd=workdir,
        capture_output=False,  # stream directly to stdout
        text=True, timeout=timeout
    )
    return result.returncode


def section(title):
    print(f"\n{'='*70}")
    print(f"  {title}")
    print('='*70)


def query(db_path, sql):
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    cur = conn.execute(sql)
    rows = [dict(r) for r in cur.fetchall()]
    conn.close()
    return rows


def main():
    openai_key = os.environ.get("OPENAI_API_KEY") or os.environ.get("OPENAI_TOKEN", "")
    llm_model = os.environ.get("LLM_MODEL", "gpt-4o-mini")
    llm_endpoint = os.environ.get("OPENAI_URL", "https://api.openai.com/v1")

    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)

        # ── RUST ─────────────────────────────────────────────────────────────
        section("RUST: add + cognify (RUST_LOG=cognee_cognify=info,cognee_cli=info)")
        rust_ws = tmp / "rust"
        rust_ws.mkdir()
        config_dir = rust_ws / "config" / "cognee-rust"
        config_dir.mkdir(parents=True)
        (rust_ws / ".data_storage").mkdir()
        (rust_ws / ".cognee_system").mkdir()

        input_file = rust_ws / "input.txt"
        input_file.write_text(NLP_TEXT_FILE.read_text())

        config = {
            "version": 1,
            "settings": {
                "relational_db_url": "sqlite:./cognee.db?mode=rwc",
                "data_root_directory": str(rust_ws / ".data_storage"),
                "system_root_directory": str(rust_ws / ".cognee_system"),
                "llm_provider": "openai",
                "llm_model": llm_model,
                "llm_api_key": openai_key,
                "llm_endpoint": llm_endpoint,
                "embedding_model_path": ONNX_MODEL_PATH,
                "embedding_tokenizer_path": ONNX_TOKENIZER_PATH,
            }
        }
        (config_dir / "config.json").write_text(json.dumps(config, indent=2))

        rust_env = {"XDG_CONFIG_HOME": str(rust_ws / "config"),
                    "RUST_LOG": "cognee_cognify=info,cognee_cli=info,cognee_database=info"}

        print("\n--- Rust add ---")
        run([RUST_CLI, "add", str(input_file), "-d", DATASET_NAME],
            env=rust_env, workdir=str(rust_ws))

        # Extract user_id from DB for display
        rust_db = rust_ws / "cognee.db"
        ds = query(rust_db, "SELECT * FROM datasets")
        print(f"\nRust datasets: {ds}")

        print("\n--- Rust cognify ---")
        run([RUST_CLI, "cognify", "-d", DATASET_NAME],
            env=rust_env, workdir=str(rust_ws))

        rust_nodes = query(rust_db, "SELECT \"type\", COUNT(*) as cnt FROM nodes GROUP BY \"type\"")
        rust_edges = query(rust_db, "SELECT relationship_name, COUNT(*) as cnt FROM edges GROUP BY relationship_name")
        rust_edge_total = query(rust_db, "SELECT COUNT(*) as cnt FROM edges")[0]["cnt"]
        rust_node_total = query(rust_db, "SELECT COUNT(*) as cnt FROM nodes")[0]["cnt"]

        # ── PYTHON ───────────────────────────────────────────────────────────
        section("PYTHON: add + cognify (LOG_LEVEL=INFO)")
        py_ws = tmp / "python"
        py_ws.mkdir()
        py_system = py_ws / ".cognee_system"
        py_storage = py_ws / ".data_storage"
        py_system.mkdir()
        py_storage.mkdir()

        py_input = py_ws / "input.txt"
        py_input.write_text(NLP_TEXT_FILE.read_text())

        py_env = {
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
            "COGNEE_SKIP_CONNECTION_TEST": "true",
            "LOG_LEVEL": "INFO",
        }

        script_add = (
            f"import asyncio, logging, cognee\n"
            f"logging.basicConfig(level=logging.INFO, format='%(name)s %(levelname)s %(message)s')\n"
            f"cognee.config.data_root_directory({str(py_storage)!r})\n"
            f"cognee.config.system_root_directory({str(py_system)!r})\n"
            f"async def main():\n"
            f"    await cognee.add(data={str(py_input)!r}, dataset_name={DATASET_NAME!r})\n"
            f"    print('ADD OK')\n"
            f"asyncio.run(main())\n"
        )

        script_cognify = (
            f"import asyncio, logging, cognee\n"
            f"logging.basicConfig(level=logging.INFO, format='%(name)s %(levelname)s %(message)s')\n"
            f"cognee.config.data_root_directory({str(py_storage)!r})\n"
            f"cognee.config.system_root_directory({str(py_system)!r})\n"
            f"async def main():\n"
            f"    await cognee.cognify(datasets=[{DATASET_NAME!r}])\n"
            f"    print('COGNIFY OK')\n"
            f"asyncio.run(main())\n"
        )

        print("\n--- Python add ---")
        run([PYTHON_RUNNER, "-c", script_add], env=py_env, workdir=str(py_ws))

        print("\n--- Python cognify ---")
        run([PYTHON_RUNNER, "-c", script_cognify], env=py_env, workdir=str(py_ws))

        py_db = py_system / "databases" / "cognee_db"
        py_nodes = query(py_db, "SELECT type, COUNT(*) as cnt FROM nodes GROUP BY type")
        py_edges = query(py_db, "SELECT relationship_name, COUNT(*) as cnt FROM edges GROUP BY relationship_name")
        py_edge_total = query(py_db, "SELECT COUNT(*) as cnt FROM edges")[0]["cnt"]
        py_node_total = query(py_db, "SELECT COUNT(*) as cnt FROM nodes")[0]["cnt"]

        # ── COMPARISON ───────────────────────────────────────────────────────
        section("COMPARISON SUMMARY")
        print(f"\nNodes  — Python: {py_node_total}  Rust: {rust_node_total}")
        print(f"Edges  — Python: {py_edge_total}  Rust: {rust_edge_total}")

        print("\n--- Python node types ---")
        for r in sorted(py_nodes, key=lambda x: x["type"]):
            print(f"  {r['type']:40s} {r['cnt']}")

        print("\n--- Rust node types ---")
        for r in sorted(rust_nodes, key=lambda x: x.get("type", "")):
            print(f"  {str(r.get('type', '?')):40s} {r['cnt']}")

        print("\n--- Python edge relationship_names ---")
        for r in sorted(py_edges, key=lambda x: x["relationship_name"]):
            print(f"  {r['relationship_name']:40s} {r['cnt']}")

        print("\n--- Rust edge relationship_names ---")
        for r in sorted(rust_edges, key=lambda x: x["relationship_name"]):
            print(f"  {r['relationship_name']:40s} {r['cnt']}")


if __name__ == "__main__":
    main()
