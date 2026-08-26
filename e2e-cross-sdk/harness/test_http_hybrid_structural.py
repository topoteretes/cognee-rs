"""Structural parity for the HYBRID_COMPLETION retriever over HTTP.

Phase-2, LLM-gated (cognify invokes the LLM for entity/fact extraction). The
hybrid search itself runs with ``onlyContext=true`` so the completion LLM call
is skipped and the comparison stays deterministic — what is being compared is
the chunk/entity/fact retrieval lanes, not a generated answer.

**only_context output-shape divergence (accepted cross-SDK gap).** With
``onlyContext=true`` the two SDKs shape ``searchResult`` differently:

* **Python** returns the ``format_hybrid_context`` **markdown string** with
  ``## Relevant passages`` / ``## Relevant entities`` / ``## Related facts``
  sections (the markdown lands in ``searchResult`` because completion is
  skipped).
* **Rust** returns a **JSON array** of ``{id, score, payload}`` ``SearchItem``
  dicts, keyed on ``payload["kind"]`` in ``{chunk, entity, fact}`` — the
  markdown is only built on the completion path, which ``only_context`` never
  hits.

So each side is parsed in its NATIVE shape (asymmetric parsers below), and the
recovered structural quantities (entity-name set, passage count, fact count)
are what get compared across SDKs — with cognify-style tolerances, since
LLM extraction is non-deterministic.
"""

import os
import json
import re

import pytest

from helpers import NLP_TEXT_FILE
from seed import seed_cognify, seed_dataset_with_text
from conftest import requires_openai


QUERY = "What is natural language processing about?"

# Mirrors test_search_parity.py:34 — the NLP fixture is about natural language
# processing, so any reasonable retrieval should surface one of these.
NLP_KEYWORDS = ("language", "nlp", "computer", "processing")


# ── Shape-specific parsers ───────────────────────────────────────────────────


def _split_markdown_sections(md: str) -> dict[str, str]:
    """Split a hybrid-context markdown string into ``{header: body}``.

    Splits on ``## `` (level-2) headers only; ``### `` entity blocks stay
    inside their section body.
    """
    sections: dict[str, str] = {}
    current: str | None = None
    buf: list[str] = []
    for line in md.splitlines():
        m = re.match(r"^## (.+)$", line)
        if m:
            if current is not None:
                sections[current] = "\n".join(buf)
            current = m.group(1).strip()
            buf = []
        else:
            buf.append(line)
    if current is not None:
        sections[current] = "\n".join(buf)
    return sections


def _extract_from_python_markdown(md: str) -> tuple[set[str], int, int]:
    """Parse Python's markdown ``searchResult`` into structural quantities.

    Returns ``(entity_names, passage_count, fact_count)``.
    """
    sections = _split_markdown_sections(md)

    # Entity names: "### {name}" or "### {name} ({type})" blocks under the
    # "Relevant entities" section; strip a trailing " (Type)" suffix.
    entities_body = sections.get("Relevant entities", "")
    entity_names: set[str] = set()
    for line in entities_body.splitlines():
        m = re.match(r"^### (.+)$", line)
        if not m:
            continue
        name = re.sub(r"\s*\([^)]*\)\s*$", "", m.group(1)).strip()
        if name:
            entity_names.add(name)

    # Passages: body split on the literal "\n---\n" separator; count non-empty
    # parts.
    passages_body = sections.get("Relevant passages", "")
    if passages_body.strip():
        passage_count = sum(1 for part in passages_body.split("\n---\n") if part.strip())
    else:
        passage_count = 0

    # Facts: "- {text}" bullets under "Related facts".
    facts_body = sections.get("Related facts", "")
    fact_count = sum(1 for line in facts_body.splitlines() if line.startswith("- "))

    return entity_names, passage_count, fact_count


def _extract_from_rust_items(items: list) -> tuple[set[str], int, int]:
    """Parse Rust's JSON item-array ``searchResult`` into structural quantities.

    Keyed on ``payload["kind"]`` in ``{chunk, entity, fact}``.
    Returns ``(entity_names, passage_count, fact_count)``.
    """
    entity_names: set[str] = set()
    passage_count = 0
    fact_count = 0
    for item in items:
        if not isinstance(item, dict):
            continue
        payload = item.get("payload") or {}
        kind = payload.get("kind")
        if kind == "entity":
            name = payload.get("name")
            if name:
                entity_names.add(str(name).strip())
        elif kind == "chunk":
            passage_count += 1
        elif kind == "fact":
            fact_count += 1
    return entity_names, passage_count, fact_count


# ── Fixture ──────────────────────────────────────────────────────────────────


@pytest.fixture
def hybrid_seeded_dataset(authed_clients, unique_dataset_name):
    """add + cognify the NLP fixture text into both servers over HTTP.

    Mirrors ``seeded_dataset`` in test_http_search.py but adds the cognify call
    (which that fixture deliberately omits) so entity/fact extraction has real
    content behind the hybrid search. Returns ``{"py": id, "rs": id}``.
    """
    text = NLP_TEXT_FILE.read_text()
    dataset_ids: dict[str, str] = {}
    for side, client in authed_clients.items():
        resp = seed_dataset_with_text(client, name=unique_dataset_name, text=text)
        dataset_id = resp.get("dataset_id") or resp.get("id")
        assert dataset_id, f"{side}: seed_dataset_with_text returned no dataset id: {resp}"
        seed_cognify(client, dataset_id=dataset_id)
        dataset_ids[side] = dataset_id
    return dataset_ids


# ── Test ─────────────────────────────────────────────────────────────────────


@requires_openai
def test_hybrid_context_structural_parity(authed_clients, hybrid_seeded_dataset):
    """HYBRID_COMPLETION only-context output is structurally comparable.

    Uses defaults only (no ``retrieverSpecificConfig`` — absent from both HTTP
    wires). Parses each side in its native shape, then compares the recovered
    structural quantities with cognify-style tolerances.
    """
    payload = {
        "query": QUERY,
        "searchType": "HYBRID_COMPLETION",
        "onlyContext": True,
        "topK": 5,
    }
    py = authed_clients["py"].post("/api/v1/search", json=payload)
    rs = authed_clients["rs"].post("/api/v1/search", json=payload)

    # Not-yet-wired tolerance: only a 404 skips (mirrors test_search_type_parity).
    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("POST /api/v1/search HYBRID_COMPLETION not yet implemented")

    assert py.status_code == 200, f"Python search failed: {py.status_code} {py.text[:500]}"
    assert rs.status_code == 200, f"Rust search failed: {rs.status_code} {rs.text[:500]}"

    py_body = py.json()
    rs_body = rs.json()
    assert isinstance(py_body, list) and py_body, f"Python body not a non-empty list: {py_body}"
    assert isinstance(rs_body, list) and rs_body, f"Rust body not a non-empty list: {rs_body}"

    py_result = py_body[0]["searchResult"]
    rs_result = rs_body[0]["searchResult"]

    # Shape-aware non-emptiness (accepted divergence): Python markdown string,
    # Rust JSON item array. An empty result is a HARD failure, not a skip.
    assert isinstance(py_result, str) and py_result.strip(), (
        f"Python searchResult must be a non-empty markdown string: {py_result!r}"
    )
    assert isinstance(rs_result, list) and len(rs_result) > 0, (
        f"Rust searchResult must be a non-empty item array: {rs_result!r}"
    )

    # Topic-keyword presence — works for both shapes via json.dumps.
    py_blob = json.dumps(py_result).lower()
    rs_blob = json.dumps(rs_result).lower()
    assert any(kw in py_blob for kw in NLP_KEYWORDS), (
        f"Python searchResult mentions no NLP keyword {NLP_KEYWORDS}: {py_result!r}"
    )
    assert any(kw in rs_blob for kw in NLP_KEYWORDS), (
        f"Rust searchResult mentions no NLP keyword {NLP_KEYWORDS}: {rs_result!r}"
    )

    # ── Smoke guard against silent 0-parsing (formatter/shape drift) ─────────
    assert any(
        h in py_result
        for h in ("## Relevant passages", "## Relevant entities", "## Related facts")
    ), (
        "Python markdown has none of the expected section headers — formatter "
        f"drift would make the counts silently 0. Got: {py_result!r}"
    )
    assert any(
        isinstance(it, dict)
        and (it.get("payload") or {}).get("kind") in ("chunk", "entity", "fact")
        for it in rs_result
    ), (
        "Rust item array has no item with a recognized kind (chunk/entity/fact) "
        f"— shape drift would make the counts silently 0. Got: {rs_result!r}"
    )

    # ── Extract structural quantities in each native shape ───────────────────
    py_entities, py_passages, py_facts = _extract_from_python_markdown(py_result)
    rs_entities, rs_passages, rs_facts = _extract_from_rust_items(rs_result)

    # Entity-name overlap: Jaccard >= 0.3 (skip if both empty).
    if not py_entities and not rs_entities:
        pytest.skip("Both SDKs produced zero entity names")
    intersection = py_entities & rs_entities
    union = py_entities | rs_entities
    jaccard = len(intersection) / len(union) if union else 0
    assert jaccard >= 0.3, (
        f"Entity-name Jaccard similarity too low ({jaccard:.2f}):\n"
        f"  Python entities: {sorted(py_entities)}\n"
        f"  Rust entities:   {sorted(rs_entities)}\n"
        f"  Overlap:         {sorted(intersection)}"
    )

    # Passage-count tolerance: both > 0, and within COUNT_TOLERANCE.
    assert py_passages > 0, "Python produced zero passages"
    assert rs_passages > 0, "Rust produced zero passages"
    _assert_within_tolerance("Passage", py_passages, rs_passages)

    # Fact-count tolerance: both > 0, and within COUNT_TOLERANCE.
    assert py_facts > 0, "Python produced zero facts"
    assert rs_facts > 0, "Rust produced zero facts"
    _assert_within_tolerance("Fact", py_facts, rs_facts)


# Maximum accepted relative divergence, as |py - rust| / mean. Overridable so a
# genuinely noisy signal can be widened deliberately rather than silently.
COUNT_TOLERANCE = float(os.environ.get("COGNEE_PARITY_COUNT_TOLERANCE", "0.5"))


def _assert_within_tolerance(label: str, py_count: int, rust_count: int) -> None:
    """Fail if the two counts diverge by more than COUNT_TOLERANCE.

    This previously warned instead of asserting, which made the surrounding
    checks unfailable except on a literal zero. If the bound proves too tight
    under real-LLM runs, raise COGNEE_PARITY_COUNT_TOLERANCE or mock the LLM;
    do not return to warning.
    """
    avg = (py_count + rust_count) / 2
    ratio = abs(py_count - rust_count) / avg if avg > 0 else 0
    assert ratio <= COUNT_TOLERANCE, (
        f"{label} count divergence {ratio:.0%} exceeds the "
        f"{COUNT_TOLERANCE:.0%} tolerance: Python={py_count}, Rust={rust_count}"
    )
