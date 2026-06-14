"""sessions.py — Runnable example: QA-history sessions and feedback.

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

    cd python && python examples/sessions.py

What it does
------------
1. Adds text and runs cognify so the graph has content.
2. Searches with ``save_interaction=True`` to persist a QA entry.
3. Retrieves the stored session history.
4. Adds and then removes feedback on a QA entry.
5. Stores and reads back a graph-context snapshot on the session.
"""

import asyncio
import json
import os
import sys

from cognee_pipeline import Cognee


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

    dataset_name = "session-demo"

    # ── Step 1: populate the graph ─────────────────────────────────────────────
    print(f'\nAdding data to dataset "{dataset_name}"...')
    await cognee.add(
        {
            "type": "text",
            "text": (
                "Isaac Newton formulated the laws of motion and universal gravitation. "
                "He also made foundational contributions to optics and invented calculus "
                "independently of Leibniz."
            ),
        },
        dataset_name,
    )
    print("Running cognify pipeline...")
    await cognee.cognify(dataset_name)
    print("Cognify complete.")

    # ── Step 2: search and save the interaction ────────────────────────────────
    #
    # Passing save_interaction=True (or session_id) persists a QA entry so it
    # can be retrieved later from session history.
    session_id = "example-session-001"
    query = "What did Isaac Newton discover?"
    print(f'\nSearching with session_id={session_id!r}: "{query}"')
    await cognee.search(
        query,
        {"save_interaction": True, "session_id": session_id},
    )
    print("Search saved to session history.")

    # ── Step 3: retrieve session history ──────────────────────────────────────
    print(f"\nRetrieving session history for {session_id!r}...")
    entries = await cognee.sessions.get(session_id)
    print(f"Found {len(entries)} QA entry(ies):")
    for entry in entries:
        print(f"  id={entry.get('id')}  question={entry.get('question', '')!r}")

    if not entries:
        print("No session entries found — feedback demo skipped.")
    else:
        qa_id = entries[0]["id"]

        # ── Step 4: add feedback ───────────────────────────────────────────────
        print(f"\nAdding feedback to QA entry {qa_id!r}...")
        added = await cognee.sessions.add_feedback(
            session_id,
            qa_id,
            {"feedback_text": "Very helpful!", "feedback_score": 5},
        )
        print(f"add_feedback returned: {added}")

        # ── Step 5: remove feedback ────────────────────────────────────────────
        print(f"\nRemoving feedback from QA entry {qa_id!r}...")
        removed = await cognee.sessions.delete_feedback(session_id, qa_id)
        print(f"delete_feedback returned: {removed}")

    # ── Step 6: graph-context snapshot ────────────────────────────────────────
    context_before = await cognee.sessions.get_graph_context(session_id)
    print(f"\nGraph context before set: {context_before!r}")

    await cognee.sessions.set_graph_context(session_id, '{"nodes": ["newton"]}')
    context_after = await cognee.sessions.get_graph_context(session_id)
    print(f"Graph context after set: {context_after!r}")


if __name__ == "__main__":
    asyncio.run(main())
