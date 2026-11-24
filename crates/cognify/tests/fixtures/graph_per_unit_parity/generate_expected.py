"""Generate expected.json for the SDK-1694 graph-per-unit parity test.

Run with a Python environment that has cognee's dependencies installed and
the cognee checkout to compare against first on the path:

    PYTHONPATH=/path/to/cognee python generate_expected.py

It feeds input.json to Python's ``construct_data_points_and_edges`` the way
the pipeline does: one call per data item (every data item here is smaller
than ``chunks_per_batch``, so each is one batch), each with fresh maps. It
writes what the Rust test compares, as sorted lists.

Only EntityTypes some entity points at count: construction also creates one
for a type that lost to an earlier declaration in the same batch, but nothing
references it, so ``add_data_points`` (which walks from the chunks) never
stores it.
"""

import json
import sys
from pathlib import Path
from uuid import UUID

from cognee.modules.chunking.models import DocumentChunk
from cognee.modules.data.processing.document_types import TextDocument
from cognee.modules.engine.models import Entity, Timestamp
from cognee.modules.graph.utils.expand_with_nodes_and_edges import (
    construct_data_points_and_edges,
)
from cognee.shared.data_models import KnowledgeGraph

HERE = Path(__file__).parent


def main() -> None:
    fixture = json.loads((HERE / "input.json").read_text())
    by_data_item: dict[str, list[dict]] = {}
    for chunk in fixture["chunks"]:
        by_data_item.setdefault(chunk["data_id"], []).append(chunk)

    entity_ids: set[str] = set()
    entity_types: set[tuple[str, str]] = set()
    type_names: dict[str, str] = {}
    timestamps: set[str] = set()
    edges: set[tuple[str, str, str]] = set()

    for data_id, chunks in by_data_item.items():
        document = TextDocument(
            id=UUID(data_id),
            name=data_id,
            raw_data_location=data_id,
            external_metadata=None,
            mime_type="text/plain",
        )
        data_chunks = [
            DocumentChunk(
                id=UUID(chunk["chunk_id"]),
                text=chunk["chunk_id"],
                chunk_size=1,
                chunk_index=index,
                cut_type="sentence_end",
                is_part_of=document,
            )
            for index, chunk in enumerate(chunks)
        ]
        graphs = [KnowledgeGraph.model_validate(chunk["graph"]) for chunk in chunks]

        data_points, edges_by_identity = construct_data_points_and_edges(data_chunks, graphs)

        for data_point in data_points.values():
            if isinstance(data_point, Entity):
                entity_ids.add(str(data_point.id))
                if data_point.is_a is not None:
                    entity_types.add((str(data_point.id), str(data_point.is_a.id)))
                    type_names[str(data_point.is_a.id)] = data_point.is_a.name
            elif isinstance(data_point, Timestamp):
                timestamps.add(str(data_point.id))
        for identity in edges_by_identity:
            edges.add((identity.source_id, identity.target_id, identity.relationship_name))

    expected = {
        "generated_by": "generate_expected.py",
        "entity_ids": sorted(entity_ids),
        "entity_types": sorted(list(pair) for pair in entity_types),
        "type_names": dict(sorted(type_names.items())),
        "timestamp_ids": sorted(timestamps),
        "semantic_edges": sorted(list(edge) for edge in edges),
    }
    (HERE / "expected.json").write_text(json.dumps(expected, indent=2) + "\n")
    print(json.dumps(expected, indent=2), file=sys.stderr)


if __name__ == "__main__":
    main()
