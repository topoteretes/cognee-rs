"""Optional drop-in ``cognee`` namespace shim.

This shim lets upstream scripts that ``import cognee`` keep working unchanged
by re-exporting the public surface of :mod:`cognee_pipeline.compat`.

**It is deliberately NOT packaged into the wheel.** ``pip install
cognee-pipeline`` ships only the ``cognee_pipeline`` package, so it never
shadows the real upstream ``cognee`` package (the wheel excludes this
directory — see ``python/pyproject.toml``). The shim is provided in the repo
for users who explicitly want a drop-in replacement and are *not* using the
real ``cognee`` package; to opt in, vendor this directory or add the repo's
``python/`` directory to ``PYTHONPATH``.

Do **not** make this importable alongside the real ``cognee`` package — the
two collide on the ``cognee`` import name.

Example usage once this shim is on the import path::

    import cognee                           # resolves to this shim
    await cognee.add("Hello world")
    await cognee.cognify()
    results = await cognee.search("query", query_type=cognee.SearchType.CHUNKS)
    await cognee.prune.prune_data()
"""

# Re-export the entire public surface of the compat module so that
# ``from cognee import add, cognify, search, prune, SearchType`` works.
from cognee_pipeline.compat import (  # noqa: F401
    add,
    cognify,
    add_and_cognify,
    search,
    prune,
    SearchType,
    reset_default_handle,
)
from cognee_pipeline.compat import _handle  # noqa: F401  (advanced use)

__all__ = [
    "add",
    "cognify",
    "add_and_cognify",
    "search",
    "prune",
    "SearchType",
    "reset_default_handle",
]
