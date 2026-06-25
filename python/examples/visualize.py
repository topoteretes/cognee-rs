"""visualize.py — Runnable example: render the knowledge graph to HTML.

Prerequisites
-------------
1. Build the native extension::

       cd python && maturin develop

2. Set the following environment variables::

       OPENAI_URL=https://api.openai.com/v1
       OPENAI_TOKEN=sk-...
       MOCK_EMBEDDING=true    # skip ONNX download

Running
-------
::

    cd python && python examples/visualize.py

What it does
------------
1. Adds text and runs the cognify pipeline so the graph has nodes and edges.
2. Calls ``visualize()`` to get the full self-contained d3.js HTML as a string.
3. Calls ``visualize_to_file()`` to write the HTML to a file and returns its path.

Requires the ``visualization`` Cargo feature to be compiled in. The example
exits with a clear message if the feature is absent.
"""

import asyncio
import json
import os
import sys
import tempfile

from cognee_py import Cognee, CogneeFeatureNotBuiltError


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
    use_mock = os.environ.get("MOCK_EMBEDDING", "").lower() in ("1", "true", "yes")

    settings: dict = {
        "llm_endpoint": llm_endpoint,
        "llm_api_key": llm_api_key,
        "llm_model": os.environ.get("OPENAI_MODEL", "gpt-4o-mini"),
    }
    if use_mock:
        settings["embedding_provider"] = "mock"

    cognee = Cognee(json.dumps(settings))

    print("Warming up cognee services...")
    await cognee.warm()

    dataset_name = "viz-demo"

    # ── Step 1: build a graph ──────────────────────────────────────────────────
    print(f'\nAdding data to dataset "{dataset_name}"...')
    await cognee.add(
        {
            "type": "text",
            "text": (
                "Albert Einstein developed the theory of relativity. "
                "He was awarded the Nobel Prize in Physics in 1921 for his discovery "
                "of the law of the photoelectric effect."
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

    # ── Step 2: visualize() — get HTML as a string ────────────────────────────
    print("\nRendering knowledge graph to HTML string...")
    try:
        html = await cognee.visualize()
    except CogneeFeatureNotBuiltError:
        print(
            "SKIP: The 'visualization' Cargo feature was not compiled in.\n"
            "Rebuild with: cargo build --features visualization"
        )
        return

    html_size_kb = len(html.encode()) // 1024
    print(f"HTML length: {html_size_kb} KB")
    print(f"Contains d3.js: {'d3' in html.lower()}")

    # ── Step 3: visualize_to_file() — write to disk ────────────────────────────
    with tempfile.NamedTemporaryFile(
        suffix=".html", prefix="cognee_graph_", delete=False
    ) as tmp:
        destination = tmp.name

    print(f"\nWriting graph HTML to {destination!r}...")
    written_path = await cognee.visualize_to_file({"destination_path": destination})
    print(f"Written to: {written_path}")
    assert os.path.isfile(written_path), f"File not found: {written_path}"
    print("File exists on disk: OK")
    print("\nVisualize example complete.")


if __name__ == "__main__":
    asyncio.run(main())
