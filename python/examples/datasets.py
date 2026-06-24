"""datasets.py — Runnable example: dataset listing, status, and deletion.

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

    cd python && python examples/datasets.py

What it does
------------
1. Adds text to two named datasets.
2. Lists all datasets and prints their IDs.
3. Checks whether each dataset has content (``has``).
4. Queries pipeline-run statuses for a batch of dataset IDs.
5. Lists data items inside one dataset.
6. Empties one dataset and confirms it is gone.
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

    # ── Step 1: populate two datasets ─────────────────────────────────────────
    print("\nAdding data to 'dataset-alpha' and 'dataset-beta'...")

    await cognee.add(
        {"type": "text", "text": "Alpha dataset: content about AI memory systems."},
        "dataset-alpha",
    )
    await cognee.add(
        {"type": "text", "text": "Beta dataset: content about knowledge graphs."},
        "dataset-beta",
    )
    print("Add complete.")

    # ── Step 2: list all datasets ──────────────────────────────────────────────
    print("\nListing all datasets...")
    datasets = await cognee.datasets.list()
    print(f"Found {len(datasets)} dataset(s):")
    for ds in datasets:
        print(f"  id={ds['id']}  name={ds['name']}")

    if not datasets:
        print("No datasets found — exiting early.")
        return

    # ── Step 3: has() — check content ─────────────────────────────────────────
    first_id = datasets[0]["id"]
    has_content = await cognee.datasets.has(first_id)
    print(f"\ndatasets.has({first_id!r}) = {has_content}")

    # ── Step 4: status() — pipeline-run statuses ──────────────────────────────
    all_ids = [ds["id"] for ds in datasets]
    print(f"\nQuerying pipeline-run status for {len(all_ids)} dataset(s)...")
    statuses = await cognee.datasets.status(all_ids)
    for ds_id, status in statuses.items():
        print(f"  {ds_id}: {status}")

    # ── Step 5: list_data() — data items in the first dataset ─────────────────
    print(f"\nListing data items in dataset {first_id!r}...")
    items = await cognee.datasets.list_data(first_id)
    print(f"Found {len(items)} item(s):")
    for item in items:
        print(f"  id={item.get('id')}  name={item.get('name')}")

    # ── Step 6: empty() — delete a dataset ────────────────────────────────────
    print(f"\nEmptying dataset {first_id!r}...")
    delete_result = await cognee.datasets.empty(first_id)
    print("Delete result:", json.dumps(delete_result, indent=2, default=str))

    # Confirm it's gone.
    still_has = await cognee.datasets.has(first_id)
    print(f"has({first_id!r}) after empty: {still_has}")


if __name__ == "__main__":
    asyncio.run(main())
