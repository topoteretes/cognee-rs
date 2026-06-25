"""add_cognify_search.py — Runnable example: full add → cognify → search pipeline.

Prerequisites
-------------
1. Build the native extension::

       cd python && maturin develop

2. Set the following environment variables (or export them before running)::

       OPENAI_URL=https://api.openai.com/v1    # or any OpenAI-compatible endpoint
       OPENAI_TOKEN=sk-...                     # API key
       OPENAI_MODEL=gpt-4o-mini                # model name (optional)
       MOCK_EMBEDDING=true                     # skip ONNX download in CI / quick tests
       COGNEE_BINDING_SUPPRESS_LOGS=1          # suppress Rust tracing on stderr (optional)

Running
-------
::

    cd python && python examples/add_cognify_search.py

What it does
------------
1. Creates a Cognee instance configured from the environment.
2. Warms up the services (builds engines and resolves the default user).
3. Adds two short text snippets to a dataset named "demo".
4. Runs the cognify pipeline to extract a knowledge graph.
5. Searches the graph with a natural-language query and prints the result.
"""

import asyncio
import json
import os
import sys

from cognee_py import Cognee


def _check_env() -> tuple[str, str]:
    """Validate required env vars; print a skip message and exit 0 if absent."""
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

    # ── Step 1: construct a Cognee instance ────────────────────────────────────
    #
    # Pass a JSON settings string to override env-derived defaults.
    # The overlay order is: compiled-in defaults < env vars < settings object.
    settings: dict = {
        "llm_endpoint": llm_endpoint,
        "llm_api_key": llm_api_key,
        "llm_model": llm_model,
    }
    if use_mock:
        settings["embedding_provider"] = "mock"
    else:
        settings.update({
            "embedding_provider": os.environ.get("EMBEDDING_PROVIDER", "openai"),
            "embedding_endpoint": llm_endpoint,
            "embedding_api_key": llm_api_key,
            "embedding_model": os.environ.get("EMBEDDING_MODEL", "text-embedding-3-small"),
            "embedding_dimensions": int(os.environ.get("EMBEDDING_DIMENSIONS", "1536")),
        })

    cognee = Cognee(json.dumps(settings))

    # ── Step 2: warm up ────────────────────────────────────────────────────────
    #
    # Builds all engines (vector DB, graph DB, embedding engine) and resolves
    # the default user. Safe to call multiple times (idempotent).
    print("Warming up cognee services...")
    await cognee.warm()
    owner_id = await cognee.owner_id()
    print(f"Owner ID: {owner_id}")

    # ── Step 3: add data ───────────────────────────────────────────────────────
    #
    # Text inputs are ingested as UTF-8 blobs.  You can also pass a file-path
    # descriptor ({"type": "file", "path": "/path/to/doc.txt"}) or a list.
    dataset_name = "demo"
    print(f'\nAdding text snippets to dataset "{dataset_name}"...')

    add_result = await cognee.add(
        [
            {
                "type": "text",
                "text": (
                    "The Eiffel Tower was built between 1887 and 1889 as a centerpiece "
                    "for the 1889 World's Fair in Paris. It was designed by Gustave "
                    "Eiffel's engineering company and stands 330 metres tall."
                ),
            },
            {
                "type": "text",
                "text": (
                    "The Statue of Liberty was a gift from France to the United States, "
                    "dedicated in 1886. It was designed by Frédéric Auguste Bartholdi "
                    "with its metal framework built by Gustave Eiffel."
                ),
            },
        ],
        dataset_name,
    )

    print(
        f"Added {add_result['addedCount']} item(s), "
        f"{add_result['deduplicatedCount']} duplicate(s)."
    )

    # ── Step 4: cognify ────────────────────────────────────────────────────────
    #
    # Extracts entities, relationships, and summaries from the ingested text via
    # the LLM, then indexes them in the graph and vector databases.
    # This step requires a live LLM endpoint.
    print("\nRunning cognify pipeline (this calls the LLM)...")
    cognify_result = await cognee.cognify(dataset_name)

    print(
        f"Cognify complete: {cognify_result['chunks']} chunk(s), "
        f"{cognify_result['entities']} entit(ies), "
        f"{cognify_result['edges']} edge(s)."
    )

    # ── Step 5: search ─────────────────────────────────────────────────────────
    #
    # Queries the knowledge graph. The default search type is GRAPH_COMPLETION,
    # which uses the LLM to synthesize an answer from the retrieved graph context.
    query = "Who designed the Eiffel Tower?"
    print(f'\nSearching: "{query}"')
    search_result = await cognee.search(query)

    print("\nSearch result:")
    print(json.dumps(search_result, indent=2, default=str))


if __name__ == "__main__":
    asyncio.run(main())
