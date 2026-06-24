"""memify_recall.py — Runnable example: graph enrichment (memify) + session recall.

Prerequisites
-------------
1. Build the native extension::

       cd python && maturin develop

2. Set the following environment variables::

       OPENAI_URL=https://api.openai.com/v1
       OPENAI_TOKEN=sk-...
       OPENAI_MODEL=gpt-4o-mini               # optional
       MOCK_EMBEDDING=true                    # skip ONNX download in CI / quick tests

Running
-------
::

    cd python && python examples/memify_recall.py

What it does
------------
1. Adds text to a dataset and runs the cognify pipeline.
2. Runs ``memify`` to build triplet embeddings from the knowledge graph, enabling
   ``TripletCompletion`` search.
3. Demonstrates ``recall``, which combines graph search with session-history routing.
4. Shows ``remember``, the single-call add+cognify shortcut.
"""

import asyncio
import json
import os
import sys

from cognee_py import Cognee


def _check_env() -> tuple[str, str]:
    llm_endpoint = os.environ.get("OPENAI_URL", "")
    llm_api_key = os.environ.get("OPENAI_TOKEN", "")
    if not llm_endpoint or not llm_api_key:
        print(
            "SKIP: OPENAI_URL and OPENAI_TOKEN must be set.\n"
            "Example:\n"
            "  export OPENAI_URL=https://api.openai.com/v1\n"
            "  export OPENAI_TOKEN=sk-..."
        )
        sys.exit(0)
    return llm_endpoint, llm_api_key


async def main() -> None:
    llm_endpoint, llm_api_key = _check_env()
    llm_model = os.environ.get("OPENAI_MODEL", "gpt-4o-mini")
    use_mock = os.environ.get("MOCK_EMBEDDING", "").lower() in ("1", "true", "yes")

    settings: dict = {
        "llm_endpoint": llm_endpoint,
        "llm_api_key": llm_api_key,
        "llm_model": llm_model,
    }
    if use_mock:
        settings["embedding_provider"] = "mock"

    cognee = Cognee(json.dumps(settings))

    print("Warming up cognee services...")
    await cognee.warm()

    dataset_name = "memify-demo"

    # ── Step 1: add + cognify ──────────────────────────────────────────────────
    print(f'\nAdding text to dataset "{dataset_name}"...')
    await cognee.add(
        {
            "type": "text",
            "text": (
                "Marie Curie was a physicist and chemist who conducted pioneering research "
                "on radioactivity. She was the first woman to win a Nobel Prize, and the "
                "only person to win the Nobel Prize in two different sciences."
            ),
        },
        dataset_name,
    )

    print("Running cognify pipeline...")
    cognify_result = await cognee.cognify(dataset_name)
    print(
        f"Cognify complete: {cognify_result['chunks']} chunk(s), "
        f"{cognify_result['entities']} entit(ies)."
    )

    # ── Step 2: memify ─────────────────────────────────────────────────────────
    #
    # Builds triplet embeddings from all edges in the knowledge graph.
    # After memify, you can use SearchType.TRIPLET_COMPLETION in search.
    # Idempotent — safe to call multiple times.
    print("\nRunning memify (builds triplet embeddings)...")
    memify_result = await cognee.memify()
    print(
        f"Memify complete: {memify_result['tripletCount']} triplet(s), "
        f"{memify_result['indexedCount']} indexed."
    )

    # ── Step 3: recall ─────────────────────────────────────────────────────────
    #
    # recall() routes the query through session history first, then falls back
    # to graph search.  scope controls which sources are checked.
    print("\nRecalling with TRIPLET_COMPLETION search type...")
    recall_result = await cognee.recall(
        "What fields did Marie Curie work in?",
        {"search_type": "TRIPLET_COMPLETION", "top_k": 5},
    )
    print(
        f"Recall: searchTypeUsed={recall_result['searchTypeUsed']}, "
        f"autoRouted={recall_result['autoRouted']}"
    )
    print("Items:")
    print(json.dumps(recall_result["items"], indent=2, default=str))

    # ── Step 4: remember ───────────────────────────────────────────────────────
    #
    # remember() is a single-call add+cognify shortcut.  Pass selfImprovement=True
    # to also run a memify pass.
    print("\nDemonstrating remember() (add + cognify in one call)...")
    remember_result = await cognee.remember(
        {"type": "text", "text": "Curie's husband Pierre Curie was also a physicist."},
        dataset_name,
    )
    print("Remember result keys:", list(remember_result.keys()))


if __name__ == "__main__":
    asyncio.run(main())
