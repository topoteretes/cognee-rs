"""Dump the vector payloads Python cognee stores for each cognify collection.

Builds a few synthetic DataPoints the way cognify does (a TextDocument, a
DocumentChunk, an Entity with its EntityType, a TextSummary and an EdgeType),
indexes them through Python's own ``index_data_points`` task into a real
LanceDB store (and into pgvector when ``--pg-dsn`` is given), then reads the
stored rows back and writes one payload per collection as JSON.

No LLM is involved: the embedding engine is a stub that returns a constant
vector. The output is the golden fixture ``python_lancedb_payloads.json`` /
``python_pgvector_payloads.json`` next to this script; the Rust tests compare
``cognee_models::index_schema_payload`` against it (SDK-1039, part B).

Run it against a Python cognee checkout, with ``-I`` so neither the current
directory nor ``PYTHON*`` variables leak in::

    python -I generate_python_payloads.py --cognee-src /path/to/cognee \
        --out-dir . [--pg-dsn postgresql+asyncpg://user:pw@host:5432/db]

The interpreter needs cognee's dependencies (``lancedb``; for pgvector also
``asyncpg`` and ``pgvector``). See README.md for the exact invocation used.
"""

import argparse
import asyncio
import json
import os
import shutil
import sys
import tempfile
import uuid

VECTOR_SIZE = 8


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("--cognee-src", required=True, help="Python cognee checkout")
    parser.add_argument("--out-dir", required=True, help="where the JSON fixtures go")
    parser.add_argument("--pg-dsn", default=None, help="pgvector DSN (postgresql+asyncpg://…)")
    parser.add_argument(
        "--python-commit", default=None, help="commit of --cognee-src, when git cannot read it"
    )
    parser.add_argument(
        "--extra-sys-path",
        action="append",
        default=[],
        help="extra import directory (e.g. a `pip install --target` dir for asyncpg/pgvector)",
    )
    return parser.parse_args()


def setup_environment(root: str):
    # Keep every cognee default store inside a throwaway directory; the relational
    # engine is touched by PGVectorAdapter's constructor even when unused.
    os.environ.setdefault("SYSTEM_ROOT_DIRECTORY", os.path.join(root, "system"))
    os.environ.setdefault("DATA_ROOT_DIRECTORY", os.path.join(root, "data"))
    os.environ.setdefault("COGNEE_SKIP_CONNECTION_TEST", "true")
    os.environ.setdefault("TELEMETRY_DISABLED", "1")
    os.environ.setdefault("ENV", "test")


class StubEmbeddingEngine:
    """Constant vectors; reports zero tokens so EmbeddingGateway-style callers work."""

    model = "stub-embedding"

    async def embed_text(self, text):
        from cognee.infrastructure.databases.vector.embeddings.EmbeddingEngine import (
            EmbeddingResult,
        )

        return EmbeddingResult([[0.1] * VECTOR_SIZE for _ in text], tokens=0)

    def get_vector_size(self):
        return VECTOR_SIZE

    def get_batch_size(self):
        return 32


def build_data_points():
    """One DataPoint per collection cognify writes, shaped as cognify builds them."""
    from cognee.modules.chunking.models import DocumentChunk
    from cognee.modules.data.processing.document_types import TextDocument
    from cognee.modules.engine.models import Entity, EntityType
    from cognee.modules.graph.models.EdgeType import EdgeType
    from cognee.tasks.summarization.models import TextSummary

    provenance = {
        # Stamped on the source points by the pipeline; IndexSchema must not copy them.
        "source_pipeline": "cognify_pipeline",
        "source_task": "extract_graph_from_data",
        "source_user": "user@example.com",
        "source_content_hash": "md5:0123",
        "topological_rank": 3,
        "version": 2,
        "ontology_valid": True,
        "feedback_weight": 0.9,
    }

    document = TextDocument(
        id=uuid.UUID("00000000-0000-0000-0000-00000000d0c0"),
        name="alice.txt",
        raw_data_location="/data/alice.txt",
        external_metadata=json.dumps({"source": "fixture"}),
        mime_type="text/plain",
        importance_weight=0.7,
    )
    chunk = DocumentChunk(
        id=uuid.UUID("00000000-0000-0000-0000-0000000c0001"),
        text="Alice works at Acme.",
        chunk_size=4,
        chunk_index=0,
        cut_type="sentence_end",
        is_part_of=document,
        document_id=str(document.id),
        document_name=document.name,
        importance_weight=0.7,
        belongs_to_set=["fixture_set"],
        **provenance,
    )
    person = EntityType(name="person", description="person")
    entity = Entity(name="alice", is_a=person, description="Alice, an engineer", **provenance)
    summary = TextSummary(
        id=uuid.UUID("00000000-0000-0000-0000-00000000500a"),
        text="Alice is employed by Acme.",
        made_from=chunk,
        source_chunk_id=str(chunk.id),
        importance_weight=0.7,
        **provenance,
    )
    edge_type = EdgeType(relationship_name="works_at", number_of_edges=1)
    return [document, chunk, person, entity, summary, edge_type]


async def index_all(vector_engine, data_points):
    from cognee.tasks.storage.index_data_points import index_data_points

    await index_data_points(data_points, vector_engine=vector_engine)


def collection_names(data_points):
    names = []
    for dp in data_points:
        for field in dp.metadata["index_fields"]:
            names.append(f"{type(dp).__name__}_{field}")
    return names


def to_jsonable(value):
    if isinstance(value, dict):
        return {k: to_jsonable(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [to_jsonable(v) for v in value]
    if isinstance(value, uuid.UUID):
        return str(value)
    return value


async def dump_lancedb(root, data_points):
    import lancedb

    from cognee.infrastructure.databases.vector.lancedb.LanceDBAdapter import LanceDBAdapter

    url = os.path.join(root, "cognee.lancedb")
    adapter = LanceDBAdapter(url=url, api_key=None, embedding_engine=StubEmbeddingEngine())
    await index_all(adapter, data_points)
    await adapter.close()

    db = lancedb.connect(url)
    out = {}
    for name in collection_names(data_points):
        rows = db.open_table(name).to_arrow().to_pylist()
        assert len(rows) == 1, (name, len(rows))
        out[name] = to_jsonable(rows[0]["payload"])
    return out


async def dump_pgvector(dsn, data_points):
    from sqlalchemy import text
    from sqlalchemy.ext.asyncio import create_async_engine

    from cognee.infrastructure.databases.vector.pgvector.PGVectorAdapter import PGVectorAdapter

    engine = create_async_engine(dsn)
    async with engine.begin() as conn:
        await conn.execute(text("CREATE EXTENSION IF NOT EXISTS vector"))
        for name in collection_names(data_points):
            await conn.execute(text(f'DROP TABLE IF EXISTS "{name}"'))

    adapter = PGVectorAdapter(dsn, None, StubEmbeddingEngine())
    await index_all(adapter, data_points)

    out = {}
    async with engine.connect() as conn:
        for name in collection_names(data_points):
            rows = (await conn.execute(text(f'SELECT payload FROM "{name}"'))).all()
            assert len(rows) == 1, (name, len(rows))
            payload = rows[0][0]
            out[name] = to_jsonable(json.loads(payload) if isinstance(payload, str) else payload)
    await engine.dispose()
    return out


def write(path, payloads, python_commit):
    doc = {
        "_comment": (
            "Generated by generate_python_payloads.py from Python cognee "
            f"{python_commit}; one stored payload per collection. Do not edit by hand."
        ),
        "python_commit": python_commit,
        "collections": payloads,
    }
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(doc, fh, indent=2, sort_keys=True)
        fh.write("\n")


def python_commit(src):
    import subprocess

    try:
        return subprocess.check_output(
            ["git", "-C", src, "rev-parse", "HEAD"], text=True, stderr=subprocess.DEVNULL
        ).strip()
    except (OSError, subprocess.CalledProcessError):
        return "unknown"


def main():
    args = parse_args()
    src = os.path.abspath(args.cognee_src)
    sys.path.insert(0, src)
    sys.path.extend(os.path.abspath(p) for p in args.extra_sys_path)
    root = tempfile.mkdtemp(prefix="cognee-index-schema-")
    setup_environment(root)
    commit = args.python_commit or python_commit(src)
    try:
        lance = asyncio.run(dump_lancedb(root, build_data_points()))
        write(os.path.join(args.out_dir, "python_lancedb_payloads.json"), lance, commit)
        if args.pg_dsn:
            pg = asyncio.run(dump_pgvector(args.pg_dsn, build_data_points()))
            write(os.path.join(args.out_dir, "python_pgvector_payloads.json"), pg, commit)
    finally:
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    main()
