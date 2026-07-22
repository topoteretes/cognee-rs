"""Cross-SDK parity of deterministic Entity node ids (issue #57).

Both SDKs derive an Entity node id as ``uuid5(NAMESPACE_OID, "Entity:" +
normalize(node_id))`` — a content-addressed id that does not depend on the
storage backend. So for any entity both SDKs extract from the same text with
the same model, the stored node id must be *identical* across SDKs.

Before the Rust fix, Rust assigned random ``uuid4`` ids to entities, so the
Entity-node id sets shared **nothing** with Python's deterministic ids. This
test asserts the id sets overlap — the one graph-level gate that a
random-vs-deterministic id scheme cannot pass.

LLM extraction is non-deterministic, so the two SDKs won't extract identical
entity sets; we require a non-empty intersection (impossible under the old
random scheme) and warn if the overlap ratio is low.
"""

import warnings

import pytest

from helpers import (
    open_db,
    query_nodes_by_type,
    _normalize_uuid,
    python_db_path,
    rust_db_path,
)
from conftest import requires_openai


def _entity_ids(conn) -> set[str]:
    """Normalized-hex id set of all Entity-class nodes in a relational store."""
    return {
        _normalize_uuid(n["id"])
        for n in query_nodes_by_type(conn, "Entity")
        if n.get("id") is not None
    }


@requires_openai
def test_entity_node_ids_match_across_sdks(both_cognified):
    py_ws, rust_ws = both_cognified

    py_ids = _entity_ids(open_db(python_db_path(py_ws)))
    rust_ids = _entity_ids(open_db(rust_db_path(rust_ws)))

    if not py_ids or not rust_ids:
        pytest.skip("One or both SDKs produced zero Entity-class nodes")

    shared = py_ids & rust_ids
    assert shared, (
        "Python and Rust share no Entity node ids — entity ids are not "
        "computed with the same deterministic scheme "
        '(uuid5(OID, "Entity:" + normalize(node_id))).\n'
        f"  Python ids: {sorted(py_ids)}\n"
        f"  Rust ids:   {sorted(rust_ids)}"
    )

    union = py_ids | rust_ids
    jaccard = len(shared) / len(union)
    if jaccard < 0.3:
        warnings.warn(
            f"Entity id overlap is low ({jaccard:.0%}); likely LLM extraction "
            f"divergence rather than an id-scheme mismatch. "
            f"shared={len(shared)}, python={len(py_ids)}, rust={len(rust_ids)}"
        )
