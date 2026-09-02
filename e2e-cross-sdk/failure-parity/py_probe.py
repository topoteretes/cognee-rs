"""Python half of the cognify-failure differential harness.

Runs ONE (scenario, config) cell of the matrix in a fresh process against a
fresh workspace and prints one JSON observation on a line prefixed with
``@@OBS@@``. Everything but the AI calls is real: real ingestion, real
chunking, real ladybug graph writes, real LanceDB vector writes, real
relational records, real ``cognify()`` entry point — so what is observed is
what the SDK actually does, not a model of it.

Usage:  python py_probe.py <scenario> <config>
  scenario: unreadable_file | extraction_failure | summarization_failure
            | second_run_after_success | clean
  config:   raise_true | raise_false
"""

import asyncio
import json
import os
import pathlib
import sys
import tempfile
import traceback

SCENARIO = sys.argv[1]
CONFIG = sys.argv[2]

ROOT = pathlib.Path(tempfile.mkdtemp(prefix=f"fp-{SCENARIO}-{CONFIG}-"))

os.environ["COGNEE_SKIP_CONNECTION_TEST"] = "true"
os.environ["LLM_API_KEY"] = "sk-mocked"
os.environ["MOCK_EMBEDDING"] = "true"
os.environ["EMBEDDING_PROVIDER"] = "openai"
os.environ["EMBEDDING_MODEL"] = "openai/text-embedding-3-large"
os.environ["EMBEDDING_DIMENSIONS"] = "8"
os.environ["TELEMETRY_DISABLED"] = "true"

# ── Axis 1, the only configuration Python exposes for this ──────────────────
# run_tasks_data_item.py: the incremental item wrapper re-raises the item's
# error unless this is falsy. Default "true".
if CONFIG == "raise_false":
    os.environ["RAISE_INCREMENTAL_LOADING_ERRORS"] = "false"
elif CONFIG == "raise_true":
    os.environ["RAISE_INCREMENTAL_LOADING_ERRORS"] = "true"
else:
    raise SystemExit(f"unknown config {CONFIG}")

import cognee  # noqa: E402
from cognee.modules.engine.operations.setup import setup as engine_setup  # noqa: E402
from cognee.shared.data_models import KnowledgeGraph, SummarizedContent  # noqa: E402
from cognee.infrastructure.llm.LLMGateway import LLMGateway  # noqa: E402

FAIL_MARKER = "FAILMARKER"
ROLES = ["good_a", "poison", "good_b"]

# Every file yields the SAME two entities and the same edge, so an artifact is
# genuinely shared between the failing file and the surviving ones — the case a
# scoped sweep has to get right. Mirrors the Rust harness's canned_graph_response.
CANNED_KG = {
    "nodes": [
        {"id": "alice", "name": "Alice", "type": "PERSON", "description": "A person."},
        {"id": "acme", "name": "Acme", "type": "ORGANIZATION", "description": "A company."},
    ],
    "edges": [
        {
            "source_node_id": "alice",
            "target_node_id": "acme",
            "relationship_name": "works_at",
            "description": "Alice works at Acme.",
        }
    ],
}


def install_llm_mock(fail_stage: str):
    """Replay the canned graph/summary, raising for the poisoned chunk in the
    stage under test. `fail_stage` is "graph", "summary" or "" (never fails)."""

    @staticmethod
    async def _mock(text_input, system_prompt, response_model, **kwargs):
        poisoned = FAIL_MARKER in (text_input or "")
        if isinstance(response_model, type) and issubclass(response_model, KnowledgeGraph):
            if poisoned and fail_stage == "graph":
                raise RuntimeError("simulated LLM failure during graph extraction")
            return KnowledgeGraph(**CANNED_KG)
        if isinstance(response_model, type) and issubclass(response_model, SummarizedContent):
            if poisoned and fail_stage == "summary":
                raise RuntimeError("simulated LLM failure during summarization")
            return SummarizedContent(summary="Alice and Acme.", description="A description.")
        return response_model()

    LLMGateway.acreate_structured_output = _mock


# ── Observation helpers ─────────────────────────────────────────────────────


async def graph_state():
    from cognee.infrastructure.databases.graph import get_graph_engine

    from cognee.infrastructure.databases.provenance.markers import (
        stores_provenance_in_graph,
    )

    engine = await get_graph_engine()
    nodes, edges = await engine.get_graph_data()
    node_ids = {str(n[0]) for n in nodes}
    return {
        "node_count": len(nodes),
        "edge_count": len(edges),
        "node_ids": node_ids,
        # Whether this graph carries its own provenance. When it does there is
        # no relational Node/Edge ledger to compare against Rust's — recorded so
        # the comparison can exclude that axis on evidence, not on belief.
        "stores_provenance_in_graph": await stores_provenance_in_graph(engine),
    }


def vector_state():
    """Row counts across every LanceDB table under the system root."""
    import lancedb

    total = 0
    tables = {}
    for lance_dir in ROOT.glob("system/**/*.lance.db"):
        try:
            db = lancedb.connect(str(lance_dir))
            for name in db.table_names():
                n = db.open_table(name).count_rows()
                tables[f"{lance_dir.name}:{name}"] = n
                total += n
        except Exception as exc:  # pragma: no cover - reported, not swallowed
            tables[f"{lance_dir.name}:<error>"] = repr(exc)
    return {"point_count": total, "tables": tables}


async def relational_state(dataset_id, data_ids_by_role):
    from sqlalchemy import select
    from cognee.infrastructure.databases.relational import get_relational_engine
    from cognee.modules.data.models import Data
    from cognee.modules.graph.models import Edge, Node
    from cognee.modules.pipelines.models import PipelineRun

    engine = get_relational_engine()
    out = {}
    async with engine.get_async_session() as session:
        nodes = (await session.execute(select(Node))).scalars().all()
        edges = (await session.execute(select(Edge))).scalars().all()
        out["ownership"] = {
            "node_rows": len(nodes),
            "edge_rows": len(edges),
            "node_rows_by_role": {
                role: sum(1 for n in nodes if str(n.data_id) == str(did))
                for role, did in data_ids_by_role.items()
            },
        }

        markers = {}
        marker_keys = {}
        for role, did in data_ids_by_role.items():
            row = (
                await session.execute(select(Data).where(Data.id == did))
            ).scalar_one_or_none()
            status = (row.pipeline_status if row else None) or {}
            cognify = status.get("cognify_pipeline") or {}
            markers[role] = {
                str(k): str(v) for k, v in cognify.items()
            }
            marker_keys[role] = sorted(cognify.keys())
        out["markers"] = markers
        out["marker_dataset_key_matches_dashed_uuid"] = all(
            keys == [] or keys == [str(dataset_id)] for keys in marker_keys.values()
        )

        runs = (await session.execute(select(PipelineRun))).scalars().all()
        cognify_runs = [r for r in runs if r.pipeline_name == "cognify_pipeline"]
        cognify_runs.sort(key=lambda r: (r.created_at, str(r.status)))
        out["pipeline_runs"] = [
            {
                "status": r.status.value if hasattr(r.status, "value") else str(r.status),
                "run_info_keys": sorted((r.run_info or {}).keys()),
                "run_info_error_present": bool((r.run_info or {}).get("error")),
                "outcome": r.outcome,
                "error_class": r.error_class,
            }
            for r in cognify_runs
        ]
        out["distinct_run_ids"] = len({str(r.pipeline_run_id) for r in cognify_runs})
    return out


async def main():
    cognee.config.set_graph_db_config({"graph_database_provider": "ladybug"})
    cognee.config.set_vector_db_config({"vector_db_provider": "lancedb"})
    cognee.config.set_relational_db_config({"db_provider": "sqlite"})
    cognee.config.system_root_directory(str(ROOT / "system"))
    cognee.config.data_root_directory(str(ROOT / "data"))
    await cognee.prune.prune_data()
    await cognee.prune.prune_system(metadata=True)
    await engine_setup()

    # The poisoned file is the MIDDLE one so neither "first" nor "last"
    # dispatch order can hide a divergence.
    install_llm_mock(
        {
            "extraction_failure": "graph",
            "summarization_failure": "summary",
            "second_run_after_success": "graph",
        }.get(SCENARIO, "")
    )

    def write_file(role):
        path = ROOT / f"{role}.txt"
        body = f"Document {role}. Alice works at Acme corporation. " * 8
        if role == "poison" and SCENARIO in (
            "extraction_failure",
            "summarization_failure",
            "second_run_after_success",
        ):
            body = f"{FAIL_MARKER} " + body
        path.write_text(body)
        return str(path)

    if SCENARIO == "second_run_after_success":
        # Run A: the two good files alone, and it succeeds — so both carry a
        # completion marker and their artifacts are in the stores. Run B then
        # fails on a newly added third file. What run B does to run A's
        # markers and run A's artifacts is the question.
        await cognee.add([write_file(r) for r in ("good_a", "good_b")], dataset_name="fp")
        await cognee.cognify(datasets=["fp"])
        await cognee.add([write_file("poison")], dataset_name="fp")
    else:
        await cognee.add([write_file(role) for role in ROLES], dataset_name="fp")

    from cognee.modules.data.methods import get_authorized_existing_datasets
    from cognee.modules.data.methods.get_dataset_data import get_dataset_data
    from cognee.modules.users.methods import get_default_user

    user = await get_default_user()
    datasets = await get_authorized_existing_datasets(["fp"], "write", user)
    dataset = datasets[0]
    items = await get_dataset_data(dataset_id=dataset.id)
    by_name = {i.name: i for i in items}
    data_ids_by_role = {role: by_name[role].id for role in ROLES}

    if SCENARIO == "unreadable_file":
        # A file that fails to read: the ingested copy is removed after add, so
        # the document's own read() raises on the way into the chunker.
        target = by_name["poison"].raw_data_location
        # raw_data_location is stored as a "file:" URL on this version.
        if target.startswith("file:"):
            target = target[len("file:") :]
        pathlib.Path(target).unlink()

    caller = {}
    try:
        result = await cognee.cognify(datasets=["fp"])
        statuses = sorted(
            {getattr(v, "status", str(v)) for v in result.values()}
            if isinstance(result, dict)
            else {str(result)}
        )
        caller = {"kind": "value", "run_info_statuses": statuses}
    except BaseException as exc:  # noqa: BLE001 - the observation IS the type
        caller = {
            "kind": "exception",
            "type": type(exc).__name__,
            "message": str(exc)[:200],
        }
        traceback.print_exc(file=sys.stderr)

    graph = await graph_state()
    doc_node_present = {
        role: str(did) in graph["node_ids"] for role, did in data_ids_by_role.items()
    }
    graph.pop("node_ids")
    provenance_in_graph = graph.pop("stores_provenance_in_graph")

    rel = await relational_state(dataset.id, data_ids_by_role)

    obs = {
        "sdk": "python",
        "dataset_id": str(dataset.id),
        "scenario": SCENARIO,
        "config": CONFIG,
        "caller": caller,
        "graph": graph,
        "stores_provenance_in_graph": provenance_in_graph,
        "graph_document_node_present": doc_node_present,
        "vector": vector_state(),
        **rel,
    }
    print("@@OBS@@" + json.dumps(obs, sort_keys=True, default=str))


asyncio.run(main())
