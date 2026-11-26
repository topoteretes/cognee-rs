#!/usr/bin/env python3
"""Regenerate the CODE search goldens from Python cognee's own retriever.

Python cognee is the source of truth for what a deterministic CODE operation
returns. This script loads Python's ``code_retriever.py`` (with
``code_search_index.py`` and ``code_graph_diagram.py``) straight from a Python
cognee checkout at the parity pin, builds ``_CodeGraphSnapshot`` over the
fixture graph below (the shop repository of Python's
``code_retriever_navigation_test.py``, plus the adapter quirks the Rust port
must reproduce), runs every case through ``CodeRetriever`` and writes what it
returned, so the Rust port can be compared with it.

It needs nothing but the standard library: the few cognee modules the
retriever imports at load time are stubbed, since no case reaches a graph
engine, a dataset context or the global cache.

Run it from this repo's root with the Python checkout at the pin (the commit
named in crates/code-graph/README.md):

    git -C <python-cognee> worktree add /tmp/cognee-pin a59433e6b   # or a checkout
    python3 scripts/generate_code_search_goldens.py /tmp/cognee-pin

It writes crates/code-graph/tests/fixtures/code_search/graph.json (the raw nodes
and edges, as an adapter returns them) and .../goldens.json:

    [{"name": ..., "config": {...}, "query": "...",
      "result": {...}, "context": "<json.dumps(sort_keys)>"}            # success
     or {"name": ..., "config": {...}, "query": "...",
         "error": {"type": ..., "message": ..., "code": ..., "param": ...}}]
"""

import asyncio
import importlib.util
import json
import sys
import types
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
OUT_DIR = REPO_ROOT / "crates" / "code-graph" / "tests" / "fixtures" / "code_search"


def _stub_module(name, **attributes):
    module = types.ModuleType(name)
    module.__dict__.update(attributes)
    sys.modules[name] = module
    return module


def load_retriever(python_root: Path):
    """Import Python's code_retriever with its cognee imports stubbed."""

    class CogneeValidationError(Exception):
        def __init__(self, message="", name="CogneeValidationError", log=True, status_code=422):
            super().__init__(message)
            self.message = message
            self.name = name

    class BaseRetriever:
        pass

    async def _no_engine():
        raise RuntimeError("the goldens never reach a graph engine")

    for package in (
        "cognee",
        "cognee.infrastructure",
        "cognee.infrastructure.databases",
        "cognee.modules",
        "cognee.modules.retrieval",
    ):
        _stub_module(package, __path__=[])
    import contextvars

    _stub_module(
        "cognee.context_global_variables",
        current_dataset_id=contextvars.ContextVar("current_dataset_id", default=None),
    )
    _stub_module("cognee.exceptions", CogneeValidationError=CogneeValidationError)
    _stub_module("cognee.infrastructure.databases.graph", get_graph_engine=_no_engine, __path__=[])
    _stub_module(
        "cognee.infrastructure.databases.graph.config", get_graph_context_config=lambda: {}
    )
    _stub_module("cognee.modules.retrieval.base_retriever", BaseRetriever=BaseRetriever)

    retrieval = python_root / "cognee" / "modules" / "retrieval"
    loaded = {}
    for name in ("code_search_index", "code_graph_diagram", "code_retriever"):
        qualified = f"cognee.modules.retrieval.{name}"
        spec = importlib.util.spec_from_file_location(qualified, retrieval / f"{name}.py")
        module = importlib.util.module_from_spec(spec)
        sys.modules[qualified] = module
        spec.loader.exec_module(module)
        loaded[name] = module
    return loaded["code_retriever"]


# --- The fixture graph ----------------------------------------------------------------

REPO = "shop"
OTHER_REPO = "payments"
E_CREATE = "1" * 32
E_VALIDATE = "2" * 32
E_PLACE = "3" * 32
E_CHARGE = "4" * 32
E_CHARGE_ALIAS = "5" * 32
E_ROUTE = "6" * 32
E_MAIN = "7" * 32
E_VENDOR = "8" * 32
F_ORDERS = "a" * 32
F_MAIN = "b" * 32
F_SEED = "c" * 32

VALIDATE_DOC = "Validates a bearer token and returns its claims."
CREATE_DOC = "Creates an order after checking the caller's token."


def _symbol(
    name, file_path, line, *, doc=None, prop_doc_only=False, enola_id=None, repo=REPO, **extra
):
    properties = {"symbol_kind": extra.pop("symbol_kind", "function"), "language": "python"}
    node = {
        "name": name,
        "type": "CodeSymbol",
        "kind": "symbol",
        "file_path": file_path,
        "line": line,
        "repo": repo,
        "symbol_kind": properties["symbol_kind"],
        "fact_properties": properties,
        **extra,
    }
    if doc is not None:
        properties["doc"] = doc
        node["description"] = doc
        if not prop_doc_only:
            node["doc"] = doc
    if enola_id is not None:
        node["enola_id"] = enola_id
    return node


def _ref(name, enola_id, file_path, line, **extra):
    return {"name": name, "id": enola_id, "file": file_path, "line": line, **extra}


def _flow(name, enola_id, *, rank, mechanism, entry, stages=(), routes=(), side=False, **metrics):
    summary = f"The program starts at {name}, which hands off to its stages."
    properties = {
        "mechanism": mechanism,
        "rank": rank,
        "summary": summary,
        "metrics": {"reach": 5, "modules": 3, "max_depth": 2, "unresolved_calls": 0, **metrics},
        "modules": [{"name": "orders", "symbols": 3}, {"name": "auth", "symbols": 2}],
        "entry": entry,
    }
    if stages:
        properties["stages"] = list(stages)
    if routes:
        properties["routes"] = list(routes)
    if side:
        properties["side"] = True
    return {
        "name": name,
        "type": "CodeFlow",
        "kind": "flow",
        "file_path": entry["file"],
        "line": entry["line"],
        "repo": REPO,
        "enola_id": enola_id,
        "description": summary,
        # Some adapters hand nested properties back as JSON text.
        "fact_properties": json.dumps(properties),
    }


def fixture_graph():
    """Python's navigation-test shop graph, plus the quirks the port must keep."""
    nodes = [
        (
            "repo",
            {
                "name": REPO,
                "type": "CodeRepository",
                "path": "/src/shop",
                "last_snapshot_id": "snap-shop-2",
                "last_delta": {
                    "nodes_added": 3,
                    "nodes_updated": 1,
                    "nodes_unchanged": 12,
                    "edges_added": 4,
                    "edges_removed": 0,
                    "nodes_removed": 0,
                    "samples_added": ["orders/api.create_order"],
                    "snapshot_id": "snap-shop-2",
                },
                # Ladybug returns a stored dict as JSON text.
                "last_receipt": json.dumps({"enola_version": "0.5.1", "format_version": 1}),
            },
        ),
        (
            "repo-payments",
            {"name": OTHER_REPO, "type": "CodeRepository", "path": OTHER_REPO},
        ),
        ("m-auth", {"name": "auth", "type": "CodeModule", "file_path": "auth", "repo": REPO}),
        ("m-orders", {"name": "orders", "type": "CodeModule", "file_path": "orders", "repo": REPO}),
        (
            "validate",
            _symbol(
                "auth/tokens.validate_token",
                "auth/tokens.py",
                12,
                doc=VALIDATE_DOC,
                enola_id=E_VALIDATE,
                end_line=30,
            ),
        ),
        ("parse", _symbol("auth/tokens.parse_header", "auth/tokens.py", 40)),
        (
            "session",
            _symbol(
                "auth/session.SessionStore",
                "auth/session.py",
                5,
                doc="Keeps user sessions in Redis.",
                prop_doc_only=True,
                symbol_kind="class",
            ),
        ),
        (
            "create",
            _symbol(
                "orders/api.create_order", "orders/api.py", 8, doc=CREATE_DOC, enola_id=E_CREATE
            ),
        ),
        (
            "place",
            _symbol(
                "orders/service.place_order",
                "orders/service.py",
                20,
                doc="Places an order and charges the card.",
                enola_id=E_PLACE,
            ),
        ),
        ("save", _symbol("orders/repository.save", "orders/repository.py", 3)),
        (
            "charge",
            _symbol(
                "billing.charge_card",
                "billing/stripe.py",
                7,
                doc="Charges a card through the payment provider.",
                enola_id=E_CHARGE,
            ),
        ),
        (
            "report",
            _symbol(
                "orders/report.render_report",
                "orders/report.py",
                2,
                doc="Renders the monthly sales report.",
            ),
        ),
        ("camel", _symbol("web/auth.validateToken", "web/auth.ts", 4)),
        ("test", _symbol("tests/test_tokens.test_validate_token", "tests/test_tokens.py", 3)),
        ("cli", _symbol("cli.main", "cli.py", 1, enola_id=E_MAIN)),
        (
            "route",
            {
                "name": "POST /orders",
                "type": "ApiEndpoint",
                "file_path": "orders/api.py",
                "line": 7,
                "repo": REPO,
                "enola_id": E_ROUTE,
                "fact_properties": {"method": "POST", "auth": {"required": True, "scheme": "bearer"}},
            },
        ),
        (
            "table",
            {
                "name": "orders_table",
                "type": "StorageResource",
                "file_path": "orders/repository.py",
                "repo": REPO,
            },
        ),
        ("noise", {"name": "ordinary text", "type": "DocumentChunk", "text": "token"}),
        # --- Additions over Python's fixture ---
        # A class and the storage view a framework extractor names after it.
        (
            "order-model",
            _symbol("orders/models.Order", "orders/models.py", 4, symbol_kind="class"),
        ),
        (
            "order-storage",
            {
                "name": "orders/models.Order",
                "type": "StorageResource",
                "file_path": "orders/models.py",
                "line": 4,
                "repo": REPO,
                "fact_properties": {"table": "orders"},
            },
        ),
        # The same qualified name in a second repository.
        (
            "charge-payments",
            _symbol(
                "billing.charge_card",
                "billing/card.py",
                11,
                repo=OTHER_REPO,
                doc="Charges a card for the payments service.",
            ),
        ),
        # Ladybug-style node: every user property inside a JSON-text
        # ``properties`` column, with a null doc.
        (
            "list",
            {
                "id": "list",
                "name": "orders/api.list_orders",
                "type": "CodeSymbol",
                "properties": json.dumps(
                    {
                        "kind": "symbol",
                        "file_path": "orders/api.py",
                        "line": 30,
                        "end_line": 41,
                        "repo": REPO,
                        "symbol_kind": "function",
                        "doc": None,
                        "fact_properties": {
                            "symbol_kind": "function",
                            "language": "python",
                            "decorators": ["get"],
                        },
                    }
                ),
            },
        ),
        # A blank doc is no doc; casefold, not lower: ß folds to ss.
        (
            "strasse",
            _symbol("geo/Straße.lookup", "geo/straße.py", 1, doc="   ", description="Größe"),
        ),
        ("dep", {"name": "requests", "type": "ExternalDependency", "repo": REPO}),
        ("nodeset", {"name": "shop", "type": "NodeSet"}),
    ]
    edges = [
        ("route", "create", "handled_by", {}),
        ("create", "validate", "calls", {}),
        ("create", "place", "calls", {}),
        ("place", "save", "calls", {}),
        # The same edge twice: the one whose properties sort first is kept,
        # and only its public properties are shown.
        (
            "place",
            "save",
            "calls",
            {
                "relationship_name": "calls",
                "source_node_id": "place",
                "target_node_id": "save",
                "edge_text": "place calls save",
                "weight": 2,
            },
        ),
        ("place", "charge", "calls", {}),
        ("validate", "parse", "calls", {}),
        ("test", "validate", "calls", {}),
        ("cli", "report", "calls", {}),
        # Edge properties nested as JSON text, as some adapters return them.
        ("save", "table", "depends_on", {"properties": json.dumps({"access": "write"})}),
        ("validate", "m-auth", "declares", {}),
        ("parse", "m-auth", "declares", {}),
        ("create", "m-orders", "declares", {}),
        ("place", "m-orders", "declares", {}),
        ("list", "m-orders", "declares", {}),
        ("m-orders", "dep", "imports", {}),
        ("create", "repo", "part_of", {}),
        ("create", "nodeset", "belongs_to_set", {}),
        ("charge-payments", "charge", "calls", {}),
    ]
    orders_entry = _ref("orders/api.create_order", E_CREATE, "orders/api.py", 8, doc=CREATE_DOC)
    nodes += [
        (
            "flow-orders",
            _flow(
                "POST /orders",
                F_ORDERS,
                rank=1,
                mechanism="route",
                entry=orders_entry,
                stages=[
                    {
                        **_ref("orders/service.place_order", E_PLACE, "orders/service.py", 20),
                        "order": 1,
                        "depth": 1,
                        "reach": 3,
                    },
                    {
                        **_ref("billing.charge_card", E_CHARGE_ALIAS, "billing/stripe.py", 7),
                        "order": 2,
                        "depth": 2,
                        "parent": E_PLACE,
                        "reach": 1,
                    },
                    {
                        **_ref("auth/tokens.validate_token", E_VALIDATE, "auth/tokens.py", 12),
                        "order": 3,
                        "depth": 1,
                        "reach": 2,
                    },
                    {
                        **_ref("vendor.retry", E_VENDOR, "vendor/retry.py", 9),
                        "order": 4,
                        "depth": 2,
                        "parent": E_PLACE,
                        "reach": 1,
                    },
                ],
                routes=[_ref("POST /orders", E_ROUTE, "orders/api.py", 7, method="POST")],
                unresolved_calls=4,
            ),
        ),
        (
            "flow-main",
            _flow(
                "cli.main",
                F_MAIN,
                rank=2,
                mechanism="main",
                entry=_ref("cli.main", E_MAIN, "cli.py", 1),
            ),
        ),
        (
            "flow-seed",
            _flow(
                "scripts/seed.main",
                F_SEED,
                rank=3,
                mechanism="main",
                side=True,
                entry=_ref("scripts/seed.main", "9" * 32, "scripts/seed.py", 1),
            ),
        ),
    ]
    edges += [
        ("flow-orders", "create", "flow_entry", {}),
        ("flow-orders", "place", "flow_stage", {}),
        ("flow-orders", "charge", "flow_stage", {}),
        ("flow-orders", "validate", "flow_stage", {}),
        ("flow-orders", "route", "flow_route", {}),
        ("flow-main", "cli", "flow_entry", {}),
    ]
    return nodes, edges


# --- The cases ------------------------------------------------------------------------

_MISSING_SEEDS = ["orders/api.create_order"] + [f"missing_{index}" for index in range(1, 21)]

CASES = [
    # Operation and diagram validation (CodeRetriever.__init__).
    ("unknown_operation", {"operation": "bogus"}, ""),
    ("default_operation_is_explore_capitalised", {"operation": "CAPABILITIES"}, ""),
    ("diagram_unsupported_value", {"operation": "neighbors", "diagram": "svg"}, ""),
    ("diagram_number", {"operation": "neighbors", "diagram": 1}, ""),
    ("diagram_capabilities", {"operation": "capabilities", "diagram": True}, ""),
    ("diagram_search", {"operation": "search", "diagram": "mermaid"}, ""),
    ("diagram_flows", {"operation": "flows", "diagram": "dot"}, ""),
    ("diagram_hubs", {"operation": "hubs", "diagram": " DOT "}, ""),
    ("diagram_ask", {"operation": "ask", "diagram": True}, ""),
    ("diagram_false_is_none", {"operation": "capabilities", "diagram": False}, ""),
    # capabilities
    ("capabilities", {"operation": "capabilities"}, "ignored"),
    # delta
    ("delta_all", {"operation": "delta"}, ""),
    ("delta_repo_filter_casefold", {"operation": "delta", "repo": "PAY"}, ""),
    ("delta_query_filter", {"operation": "delta"}, "shop"),
    ("delta_name_filter_no_match", {"operation": "delta", "name": "nothing"}, ""),
    # query_facts
    ("query_facts_query_text", {"operation": "query_facts"}, "token"),
    ("query_facts_query_text_ignored_with_filters", {"operation": "query_facts", "kind": "route"}, "token"),
    ("query_facts_kinds", {"operation": "query_facts", "kinds": ["route", "Storage"]}, ""),
    (
        "query_facts_kind_prefix_page",
        {"operation": "query_facts", "kind": "symbol", "file_prefix": "orders/", "limit": 2, "offset": 1},
        "",
    ),
    ("query_facts_names", {"operation": "query_facts", "names": ["cli.main", "POST /orders"]}, ""),
    (
        "query_facts_files_or_prefix",
        {"operation": "query_facts", "files": ["auth/tokens.py"], "file_prefix": "billing/"},
        "",
    ),
    ("query_facts_file", {"operation": "query_facts", "file": "orders/api.py"}, ""),
    ("query_facts_repo", {"operation": "query_facts", "repo": OTHER_REPO}, ""),
    (
        "query_facts_property_value",
        {"operation": "query_facts", "property": "method", "property_value": "POST"},
        "",
    ),
    (
        "query_facts_nested_prop_bool",
        {"operation": "query_facts", "prop": "auth.required", "prop_value": True},
        "",
    ),
    ("query_facts_property_presence", {"operation": "query_facts", "property": "decorators"}, ""),
    (
        "query_facts_property_list_value",
        {"operation": "query_facts", "property": "decorators", "property_value": ["get"]},
        "",
    ),
    (
        "query_facts_property_int_value",
        {"operation": "query_facts", "property": "line", "property_value": 12},
        "",
    ),
    (
        "query_facts_relation_and_name",
        {"operation": "query_facts", "relation_types": "calls", "name": "ORDER"},
        "",
    ),
    ("query_facts_casefold", {"operation": "query_facts", "name": "STRASSE"}, ""),
    ("query_facts_limit_zero_is_default", {"operation": "query_facts", "limit": 0, "kind": "module"}, ""),
    ("query_facts_limit_capped", {"operation": "query_facts", "limit": 1000, "kind": "module"}, ""),
    ("query_facts_offset_past_end", {"operation": "query_facts", "kind": "module", "offset": 10}, ""),
    ("query_facts_limit_string", {"operation": "query_facts", "limit": "5"}, ""),
    ("query_facts_limit_bool", {"operation": "query_facts", "limit": True}, ""),
    ("query_facts_negative_offset", {"operation": "query_facts", "offset": -1}, ""),
    ("query_facts_unknown_kind", {"operation": "query_facts", "kinds": ["bogus"]}, ""),
    ("query_facts_empty_name_in_list", {"operation": "query_facts", "names": ["a", ""]}, ""),
    ("query_facts_names_not_a_list", {"operation": "query_facts", "names": 5}, ""),
    ("query_facts_flow_kind_rejected", {"operation": "query_facts", "kind": "flow"}, ""),
    # neighbors
    ("neighbors_query", {"operation": "neighbors"}, "orders/api.create_order"),
    ("neighbors_short_name_forward", {"operation": "neighbors", "name": "place_order", "direction": "FORWARD"}, ""),
    (
        "neighbors_enola_id_reverse_calls",
        {"operation": "neighbors", "id": E_VALIDATE, "direction": "reverse", "relation": "calls"},
        "",
    ),
    ("neighbors_kinds", {"operation": "neighbors", "node_id": "create", "kinds": ["module", "route"]}, ""),
    ("neighbors_page", {"operation": "neighbors", "node_id": "create", "limit": 1, "offset": 1}, ""),
    ("neighbors_no_flows", {"operation": "neighbors"}, "orders/report.render_report"),
    ("neighbors_stage_flow", {"operation": "neighbors", "name": "billing.charge_card", "repo": REPO}, ""),
    ("neighbors_declaration_view", {"operation": "neighbors", "name": "orders/models.Order"}, ""),
    ("neighbors_ladybug_node", {"operation": "neighbors", "name": "list_orders"}, ""),
    ("neighbors_edge_properties", {"operation": "neighbors", "name": "orders/repository.save"}, ""),
    ("neighbors_missing", {"operation": "neighbors", "name": "nowhere.at_all"}, ""),
    ("neighbors_missing_id", {"operation": "neighbors", "id": "f" * 32}, ""),
    ("neighbors_id_wrong_repo", {"operation": "neighbors", "id": "create", "repo": OTHER_REPO}, ""),
    ("neighbors_ambiguous", {"operation": "neighbors", "name": "tokens"}, ""),
    ("neighbors_ambiguous_across_repos", {"operation": "neighbors", "name": "billing.charge_card"}, ""),
    ("neighbors_empty_anchor", {"operation": "neighbors"}, "   "),
    ("neighbors_name_list", {"operation": "neighbors", "name": ["a"]}, ""),
    ("neighbors_id_int", {"operation": "neighbors", "id": 5}, ""),
    ("neighbors_bad_direction", {"operation": "neighbors", "name": "cli.main", "direction": "sideways"}, ""),
    ("neighbors_negative_limit", {"operation": "neighbors", "name": "cli.main", "limit": -1}, ""),
    ("neighbors_bad_kind", {"operation": "neighbors", "name": "cli.main", "kinds": "flow"}, ""),
    # subgraph
    ("subgraph_one_seed", {"operation": "subgraph", "seeds": ["orders/api.create_order"]}, ""),
    ("subgraph_query_seed", {"operation": "subgraph"}, "place_order"),
    (
        "subgraph_kinds_depth",
        {"operation": "subgraph", "seeds": ["create_order", "validate_token"], "depth": 2, "kinds": ["symbol"]},
        "",
    ),
    (
        "subgraph_every_truncation",
        {
            "operation": "subgraph",
            "seeds": _MISSING_SEEDS,
            "depth": 7,
            "max_nodes": 3,
            "max_edges": 1,
            "relation_types": ["calls", "handled_by"],
        },
        "",
    ),
    ("subgraph_max_depth_alias", {"operation": "subgraph", "seed_ids": ["create"], "max_depth": 4}, ""),
    ("subgraph_max_nodes", {"operation": "subgraph", "names": ["orders/api.create_order"], "max_nodes": 2}, ""),
    ("subgraph_max_edges", {"operation": "subgraph", "ids": [E_CREATE], "max_edges": 2}, ""),
    (
        "subgraph_reverse_calls",
        {"operation": "subgraph", "seeds": "save", "direction": "reverse", "relation_types": ["calls"], "depth": 3},
        "",
    ),
    (
        "subgraph_missing_seeds",
        {
            "operation": "subgraph",
            "seeds": ["orders/api.create_order", "tokens", "validat_tokens", "billing.charge_card"],
            "seed_ids": ["nope-id"],
        },
        "",
    ),
    ("subgraph_none_resolve", {"operation": "subgraph", "seeds": ["nowhere", "nothing.here"]}, ""),
    ("subgraph_none_resolve_ambiguous", {"operation": "subgraph", "seeds": ["tokens"]}, ""),
    ("subgraph_none_resolve_mixed", {"operation": "subgraph", "seeds": ["tokens", "nowhere"]}, ""),
    ("subgraph_no_seeds", {"operation": "subgraph"}, "  "),
    ("subgraph_seeds_repo", {"operation": "subgraph", "seeds": ["billing.charge_card"], "repo": OTHER_REPO}, ""),
    ("subgraph_negative_depth", {"operation": "subgraph", "seeds": ["cli.main"], "depth": -2}, ""),
    ("subgraph_node_types_alias", {"operation": "subgraph", "seeds": ["cli.main"], "node_types": ["symbol"]}, ""),
]


def _jsonable(value):
    """Python's results hold only JSON types; tuples become lists."""
    return json.loads(json.dumps(value))


async def run_case(code_retriever, snapshot, config, query):
    retriever = code_retriever.CodeRetriever(config=config)
    handler = getattr(retriever, f"_{retriever.operation}")
    result = handler(snapshot, query or "")
    context = await retriever.get_context_from_objects(query, retrieved_objects=result)
    return result, context


def main():
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <python-cognee-checkout-at-the-pin>")
    code_retriever = load_retriever(Path(sys.argv[1]))
    nodes, edges = fixture_graph()

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    graph = {
        "nodes": [[node_id, properties] for node_id, properties in nodes],
        "edges": [[source, target, relation, properties] for source, target, relation, properties in edges],
    }
    (OUT_DIR / "graph.json").write_text(json.dumps(graph, indent=2, ensure_ascii=False) + "\n")

    snapshot = code_retriever._CodeGraphSnapshot(nodes, edges)
    empty = code_retriever._CodeGraphSnapshot([], [])
    cases = [(name, config, query, snapshot) for name, config, query in CASES]
    cases.append(("capabilities_empty_graph", {"operation": "capabilities"}, "", empty))
    cases.append(("query_facts_empty_graph", {"operation": "query_facts"}, "x", empty))
    cases.append(("delta_empty_graph", {"operation": "delta"}, "", empty))

    goldens = []
    for name, config, query, graph_snapshot in cases:
        entry = {"name": name, "config": config, "query": query}
        try:
            result, context = asyncio.run(run_case(code_retriever, graph_snapshot, config, query))
        except code_retriever.CodeSearchValidationError as error:
            entry["error"] = {
                "type": type(error).__name__,
                "message": error.message,
                "code": error.error_code,
                "param": error.error_param,
            }
        else:
            entry["result"] = _jsonable(result)
            entry["context"] = context
        goldens.append(entry)
    (OUT_DIR / "goldens.json").write_text(json.dumps(goldens, indent=2, ensure_ascii=False) + "\n")
    print(f"wrote {len(goldens)} goldens to {OUT_DIR}")


if __name__ == "__main__":
    main()
