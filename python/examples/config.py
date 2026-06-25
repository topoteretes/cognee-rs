"""config.py — Runnable example: programmatic configuration of LLM, embedding,
vector-DB, and graph-DB backends.

Prerequisites
-------------
1. Build the native extension::

       cd python && maturin develop

2. Set the following environment variables::

       OPENAI_URL=https://api.openai.com/v1
       OPENAI_TOKEN=sk-...

Running
-------
::

    cd python && python examples/config.py

What it does
------------
1. Shows three ways to set config on a Cognee handle:
   a. Pass a JSON settings string at construction time.
   b. Use ``config.set_str`` / ``config.set`` for single key-value pairs.
   c. Use bulk setters: ``config.set_llm_config``, ``config.set_embedding_config``,
      ``config.set_vector_db_config``, ``config.set_graph_db_config``.
2. Reads the config back (secret fields are redacted) and prints it.
3. Demonstrates that the config is live: a post-construction change is
   immediately visible in the read-back dict.

No LLM or embedding calls are made — this example is purely about config and
exits without warming the handle.
"""

import json
import os
import sys

from cognee_py import Cognee, CogneeUnknownConfigKeyError


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


def main() -> None:
    llm_endpoint, llm_api_key = _check_env()

    # ── Method A: JSON settings at construction ────────────────────────────────
    #
    # Pass a JSON object (as a string) to override env-derived defaults.
    # The keys are snake_case Settings field names.
    print("=== Method A: JSON settings at construction ===")
    initial_settings = json.dumps({
        "llm_endpoint": llm_endpoint,
        "llm_api_key": llm_api_key,
        "llm_model": "gpt-4o-mini",
        "embedding_provider": "mock",
    })
    cognee = Cognee(initial_settings)

    cfg = cognee.config.get()
    print(f"  llm_model     = {cfg.get('llm_model')}")
    print(f"  llm_api_key   = {cfg.get('llm_api_key')}  (redacted)")
    print(f"  emb_provider  = {cfg.get('embedding_provider')}")

    # ── Method B: single key-value setters ────────────────────────────────────
    #
    # set_str() is a convenience wrapper that validates the key and type.
    # set() accepts str, int, float, bool, list, or dict.
    print("\n=== Method B: single key-value setters ===")
    cognee.config.set_str("llm_model", "gpt-4o")
    cognee.config.set("llm_temperature", 0.1)

    cfg = cognee.config.get()
    print(f"  llm_model after set_str: {cfg.get('llm_model')}")
    print(f"  llm_temperature after set: {cfg.get('llm_temperature')}")

    # set() raises CogneeUnknownConfigKeyError for unknown keys.
    try:
        cognee.config.set("definitely_not_a_real_key", 42)
    except CogneeUnknownConfigKeyError as exc:
        print(f"  Expected error for unknown key: {exc}")

    # ── Method C: bulk setters ─────────────────────────────────────────────────
    #
    # Each bulk setter updates a group of related settings atomically.
    print("\n=== Method C: bulk setters ===")

    cognee.config.set_llm_config({
        "llm_model": "gpt-4o-mini",
        "llm_temperature": 0.0,
        "llm_max_retries": 3,
    })

    cognee.config.set_embedding_config({
        "embedding_provider": "mock",
        "embedding_dimensions": 128,
    })

    cognee.config.set_vector_db_config({
        "vector_db_provider": "brute-force",
    })

    cognee.config.set_graph_db_config({
        "graph_database_provider": "ladybug",
    })

    # ── Read-back ──────────────────────────────────────────────────────────────
    print("\n=== Final config read-back (secrets redacted) ===")
    final_cfg = cognee.config.get()
    # Print only the keys we explicitly set to keep output manageable.
    keys_to_show = [
        "llm_model",
        "llm_api_key",
        "llm_temperature",
        "llm_max_retries",
        "embedding_provider",
        "embedding_dimensions",
        "vector_db_provider",
        "graph_database_provider",
    ]
    for key in keys_to_show:
        if key in final_cfg:
            print(f"  {key} = {final_cfg[key]!r}")

    print("\nConfig example complete.")


if __name__ == "__main__":
    main()
