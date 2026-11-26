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
and edges, as an adapter returns them), one graph_<name>.json per extra graph
(the graphs of Python's code_retriever_test.py: "widget", "insights",
"architecture" and "bare_constructor"; and, for the text and flow
operations, those of code_retriever_navigation_test.py: "shop_no_flows" (no flows),
"shop_bare" (no flows, no docs), "shop_tests" (test code and score ties),
"checkout" and "checkout_documented"), .../goldens.json:

    [{"name": ..., "config": {...}, "query": "...", ["graph": "<name>",]
      "result": {...}, "context": "<json.dumps(sort_keys)>"}            # success
     or {"name": ..., "config": {...}, "query": "...", ["graph": "<name>",]
         "error": {"type": ..., "message": ..., "code": ..., "param": ...}}]

(no "graph" means the shop graph; a name ending in _empty_graph runs over no
graph at all; a result carries the "diagram" block get_retrieved_objects
attaches), and .../diagrams.json: render_mermaid and render_dot called
directly, for the label and title escapes no fixture name reaches.
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


def fixture_graph(*, with_flows=True, with_docs=True):
    """Python's navigation-test shop graph, plus the quirks the port must keep.

    ``with_flows`` and ``with_docs`` are ``_shop_graph``'s switches: without
    flows no CodeFlow node or flow edge is written, without docs no symbol
    carries a docstring.
    """

    def doc(text):
        return text if with_docs else None

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
                doc=doc(VALIDATE_DOC),
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
                doc=doc("Keeps user sessions in Redis."),
                prop_doc_only=True,
                symbol_kind="class",
            ),
        ),
        (
            "create",
            _symbol(
                "orders/api.create_order", "orders/api.py", 8, doc=doc(CREATE_DOC), enola_id=E_CREATE
            ),
        ),
        (
            "place",
            _symbol(
                "orders/service.place_order",
                "orders/service.py",
                20,
                doc=doc("Places an order and charges the card."),
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
                doc=doc("Charges a card through the payment provider."),
                enola_id=E_CHARGE,
            ),
        ),
        (
            "report",
            _symbol(
                "orders/report.render_report",
                "orders/report.py",
                2,
                doc=doc("Renders the monthly sales report."),
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
                doc=doc("Charges a card for the payments service."),
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
    if not with_flows:
        return nodes, edges
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


# --- The graphs of Python's code_retriever_test.py (SDK-1056 P1b) ---------------------
#
# Copied from cognee/tests/unit/modules/retrieval/code_retriever_test.py at the
# pin; the widget's UUID node id is written as its string.

WIDGET_ID = "00000000-0000-0000-0000-000000000001"


def widget_graph():
    """`_code_graph`: a class with a method and a constructor, two repos."""
    nodes = [
        ("repo-a", {"name": "repo-a", "type": "CodeRepository", "path": "/src/repo-a"}),
        (
            WIDGET_ID,
            {
                "name": "pkg.Widget",
                "type": "CodeSymbol",
                "properties": json.dumps(
                    {
                        "file_path": "pkg/widget.py",
                        "line": 10,
                        "repo": "repo-a",
                        "symbol_kind": "class",
                        "fact_properties": {"exported": True, "language": "python"},
                    }
                ),
            },
        ),
        (
            "method",
            {
                "name": "pkg.Widget.run",
                "type": "CodeSymbol",
                "file_path": "pkg/widget.py",
                "line": 20,
                "repo": "repo-a",
                "symbol_kind": "method",
                "fact_properties": {"exported": False},
            },
        ),
        (
            "constructor",
            {
                "name": "pkg.NewWidget",
                "type": "CodeSymbol",
                "file_path": "pkg/widget.py",
                "line": 5,
                "repo": "repo-a",
                "symbol_kind": "function",
            },
        ),
        (
            "entry",
            {
                "name": "pkg.entry",
                "type": "CodeSymbol",
                "file_path": "pkg/main.py",
                "line": 7,
                "repo": "repo-a",
                "symbol_kind": "function",
            },
        ),
        (
            "handler",
            {
                "name": "api.handler",
                "type": "CodeSymbol",
                "file_path": "api/handler.py",
                "line": 9,
                "repo": "repo-b",
                "symbol_kind": "function",
            },
        ),
        (
            "route",
            {
                "name": "GET /widgets",
                "type": "ApiEndpoint",
                "file_path": "api/routes.py",
                "line": 3,
                "repo": "repo-b",
                "fact_properties": {"method": "GET"},
            },
        ),
        (
            "database",
            {"name": "widget_db", "type": "StorageResource", "file_path": "pkg/storage.py", "repo": "repo-a"},
        ),
        (
            "helper-a",
            {"name": "shared.Helper", "type": "CodeSymbol", "file_path": "a/helper.py", "repo": "repo-a", "symbol_kind": "class"},
        ),
        (
            "helper-b",
            {"name": "shared.Helper", "type": "CodeSymbol", "file_path": "b/helper.py", "repo": "repo-b", "symbol_kind": "class"},
        ),
        ("noise", {"name": "ordinary text", "type": "DocumentChunk", "text": "not code"}),
    ]
    edges = [
        (WIDGET_ID, "method", "has_method", {}),
        ("entry", "method", "calls", {"weight": 1}),
        ("handler", "entry", "calls", {}),
        ("route", "handler", "handled_by", {}),
        ("method", "database", "depends_on", {}),
        ("noise", "entry", "mentions", {}),
    ]
    return nodes, edges


ENOLA_ID_DB = "a" * 32
ENOLA_ID_API = "b" * 32


def insight_graph():
    """`_insight_graph`: three explainer findings and the facts they cite."""
    nodes = [
        (
            "repo-a",
            {
                "name": "repo-a",
                "type": "CodeRepository",
                "path": "/src/repo-a",
                "last_snapshot_id": "sha256:abc",
                "last_receipt": {
                    "format_version": 1,
                    "enola_version": "0.4.12",
                    "quality": {"files_seen": 6, "files_parsed": 5, "parse_errors": 0},
                },
            },
        ),
        (
            "db",
            {
                "name": "app/db.Database",
                "type": "CodeSymbol",
                "file_path": "app/db.py",
                "line": 4,
                "end_line": 40,
                "repo": "repo-a",
                "symbol_kind": "class",
                "enola_id": ENOLA_ID_DB,
            },
        ),
        (
            "api",
            {
                "name": "app/api.handler",
                "type": "CodeSymbol",
                "file_path": "app/api.py",
                "repo": "repo-a",
                "symbol_kind": "function",
                "enola_id": ENOLA_ID_API,
            },
        ),
        (
            "hot",
            {
                "name": "Call-graph hotspot: app/db.Database",
                "type": "CodeInsight",
                "repo": "repo-a",
                "description": "A pinch point.",
                "fact_properties": {"source": "hotspots", "confidence": 0.7, "metrics": {"fan_in": 9}},
            },
        ),
        (
            "cyc",
            {
                "name": "Dependency cycle: app/api -> app/db -> app/api",
                "type": "CodeInsight",
                "repo": "repo-a",
                "fact_properties": {"source": "cycles", "confidence": 1.0},
            },
        ),
        (
            "info",
            {
                "name": "Domain findings do not apply to this repository",
                "type": "CodeInsight",
                "repo": "repo-a",
                "fact_properties": {"source": "domain", "confidence": 1.0, "informational": True},
            },
        ),
        # Additions over Python's fixture: an insight without a confidence
        # (sorted last) and one with a non-string source ("unknown" bucket).
        (
            "plain",
            {
                "name": "A finding with no score",
                "type": "CodeInsight",
                "repo": "repo-a",
                "fact_properties": {"source": "hotspots"},
            },
        ),
        (
            "odd",
            {
                "name": "An odd finding",
                "type": "CodeInsight",
                "repo": "repo-b",
                "fact_properties": {"source": 7, "confidence": 0, "informational": "yes"},
            },
        ),
        (
            "extraction",
            {
                "name": "python:calls",
                "type": "CodeExtractionAccount",
                "repo": "repo-a",
                "fact_properties": {"extractor": "python", "language": "python"},
            },
        ),
    ]
    edges = [
        ("hot", "db", "evidences", {}),
        ("cyc", "api", "evidences", {}),
        ("cyc", "db", "evidences", {}),
        ("api", "db", "calls", {}),
    ]
    return nodes, edges


def architecture_graph():
    """`_architecture_graph`: modules, the facts they declare, cross-module calls."""
    nodes = [
        ("repo", {"name": "shop", "type": "CodeRepository", "path": "/src/shop"}),
        ("m_root", {"name": ".", "type": "CodeModule", "file_path": ".", "repo": "shop"}),
        ("m_inv", {"name": "inventory", "type": "CodeModule", "file_path": "inventory", "repo": "shop"}),
        ("m_api", {"name": "api", "type": "CodeModule", "file_path": "api", "repo": "shop"}),
        ("s_main", {"name": "main.main", "type": "CodeSymbol", "file_path": "main.py", "repo": "shop", "symbol_kind": "function"}),
        (
            "s_total",
            {"name": "inventory/pricing.total", "type": "CodeSymbol", "file_path": "inventory/pricing.py", "repo": "shop", "symbol_kind": "function"},
        ),
        (
            "s_line",
            {"name": "inventory/pricing.line", "type": "CodeSymbol", "file_path": "inventory/pricing.py", "repo": "shop", "symbol_kind": "function"},
        ),
        (
            "s_handler",
            {"name": "api/routes.orders", "type": "CodeSymbol", "file_path": "api/routes.py", "repo": "shop", "symbol_kind": "function"},
        ),
        ("route", {"name": "GET /orders", "type": "ApiEndpoint", "file_path": "api/routes.py", "repo": "shop"}),
        ("db", {"name": "orders_table", "type": "StorageResource", "file_path": "inventory/store.py", "repo": "shop"}),
        ("dep", {"name": "main -> requests", "type": "ExternalDependency", "file_path": "main.py", "repo": "shop"}),
        # Additions over Python's fixture: a symbol with no declares edge,
        # placed by its file's directory (inventory/deep/x.py -> inventory),
        # and a second repository with its own module.
        (
            "s_deep",
            {"name": "inventory/deep/x.helper", "type": "CodeSymbol", "file_path": "inventory/deep/x.py", "repo": "shop", "symbol_kind": "function"},
        ),
        ("m_other", {"name": "lib", "type": "CodeModule", "file_path": "lib", "repo": "other"}),
        ("s_other", {"name": "lib/util.go", "type": "CodeSymbol", "file_path": "lib/util.py", "repo": "other", "symbol_kind": "function"}),
    ]
    edges = [
        ("s_main", "m_root", "declares", {}),
        ("s_total", "m_inv", "declares", {}),
        ("s_line", "m_inv", "declares", {}),
        ("s_handler", "m_api", "declares", {}),
        ("route", "m_api", "declares", {}),
        ("db", "m_inv", "declares", {}),
        ("s_main", "s_total", "calls", {}),
        ("s_main", "s_line", "calls", {}),
        ("s_handler", "s_total", "calls", {}),
        ("s_total", "s_line", "calls", {}),
        ("route", "s_handler", "handled_by", {}),
        ("s_total", "db", "uses", {}),
        ("dep", "m_root", "declares", {}),
        ("s_main", "repo", "part_of", {}),
        ("s_deep", "s_handler", "calls", {}),
        ("s_other", "s_main", "calls", {}),
    ]
    return nodes, edges


def bare_constructor_graph():
    """`test_impact_rollup_finds_bare_type_constructor_in_same_package`."""
    nodes = [
        ("bare-type", {"name": "Widget", "type": "CodeSymbol", "file_path": "pkg/widget.py", "repo": "repo-a", "symbol_kind": "class"}),
        ("bare-constructor", {"name": "NewWidget", "type": "CodeSymbol", "file_path": "pkg/factory.py", "repo": "repo-a", "symbol_kind": "function"}),
        ("wrong-package", {"name": "NewWidget", "type": "CodeSymbol", "file_path": "other/factory.py", "repo": "repo-a", "symbol_kind": "function"}),
    ]
    return nodes, []


# --- Variants of the shop graph for the text and flow operations (SDK-1056 P2) -----------


def shop_no_flows_graph():
    """`_shop_graph(with_flows=False)`: no flows.json was read."""
    return fixture_graph(with_flows=False)


def shop_bare_graph():
    """`_shop_graph(with_flows=False, with_docs=False)`: neither flows nor docstrings."""
    return fixture_graph(with_flows=False, with_docs=False)


def shop_tests_graph():
    """The shop graph plus what test_ask_and_hubs_leave_test_code_out_unless_asked and
    test_a_limited_text_match_is_the_head_of_the_full_ranking add: thirty test files
    calling the token validator, and ten symbols shaped alike that tie on every score."""
    nodes, edges = fixture_graph()
    for index in range(30):
        path = f"tests/test_tokens_{index:02d}.py"
        nodes.append(
            (
                f"test-ref-{index}",
                {"name": path, "type": "CodeTestReference", "file_path": path, "repo": REPO},
            )
        )
        edges.append((f"test-ref-{index}", "validate", "calls", {}))
    nodes += [
        (f"tie-{index}", _symbol(f"pkg{index}/tokens.check_token", f"pkg{index}/tokens.py", 1))
        for index in reversed(range(10))
    ]
    return nodes, edges


def _checkout_graph(*, route_doc):
    """test_ask_counts_docs_over_the_seeds_that_can_carry_one: a route and its handler."""
    route = {
        "name": "/checkout",
        "type": "ApiEndpoint",
        "file_path": "shop/api.py",
        "line": 14,
        "repo": REPO,
    }
    if route_doc:
        route["doc"] = route_doc
    handler = _symbol(
        "shop/api.process_checkout",
        "shop/api.py",
        15,
        doc="Charge the cart, then create the order.",
    )
    return [("route", route), ("handler", handler)], [("route", "handler", "handled_by", {})]


def checkout_graph():
    return _checkout_graph(route_doc=None)


def checkout_documented_graph():
    return _checkout_graph(route_doc="Checks the cart out.")


EXTRA_GRAPHS = {
    "widget": widget_graph,
    "insights": insight_graph,
    "architecture": architecture_graph,
    "bare_constructor": bare_constructor_graph,
    "shop_no_flows": shop_no_flows_graph,
    "shop_bare": shop_bare_graph,
    "shop_tests": shop_tests_graph,
    "checkout": checkout_graph,
    "checkout_documented": checkout_documented_graph,
}

# (name, config, query[, graph]): explore, traverse, find_path, impact_analysis,
# insights, architecture and the diagrams (SDK-1056 P1b).
P1B_CASES = [
    # explore
    ("explore_no_operation", {}, "orders/api.create_order"),
    ("explore_focus_alias", {"operation": "explore", "focus": "place_order"}, "ignored"),
    ("explore_depth_clamped", {"operation": "explore", "name": "orders/service.place_order", "max_depth": 99}, ""),
    ("explore_enola_id", {"operation": "explore", "id": E_VALIDATE}, ""),
    ("explore_node_id", {"operation": "explore", "node_id": "save", "direction": "Reverse"}, ""),
    (
        "explore_filters",
        {"operation": "explore", "name": "create_order", "max_depth": 2, "relation_kinds": "calls", "node_types": "symbol"},
        "",
    ),
    ("explore_walk_through_types", {"operation": "explore", "name": "save", "max_depth": 2, "node_types": ["storage", "module"]}, ""),
    ("explore_max_nodes", {"operation": "explore", "name": "create_order", "max_nodes": 2}, ""),
    ("explore_repo", {"operation": "explore", "repo": OTHER_REPO}, "billing.charge_card"),
    ("explore_ambiguous", {"operation": "explore"}, "billing.charge_card"),
    ("explore_missing", {"operation": "explore"}, "nowhere.at_all"),
    ("explore_empty", {"operation": "explore"}, "  "),
    ("explore_id_wrong_repo", {"operation": "explore", "id": "create", "repo": OTHER_REPO}, ""),
    ("explore_id_int", {"operation": "explore", "id": 5}, ""),
    ("explore_name_list", {"operation": "explore", "name": ["cli.main"]}, ""),
    ("explore_bad_direction", {"operation": "explore", "direction": "up"}, "cli.main"),
    ("explore_max_depth_string", {"operation": "explore", "max_depth": "2"}, "cli.main"),
    ("explore_bad_node_type", {"operation": "explore", "node_types": ["flow"]}, "cli.main"),
    ("explore_seed_error_first", {"operation": "explore", "direction": "up"}, "nowhere.at_all"),
    ("explore_diagram", {"operation": "explore", "diagram": True}, "orders/service.place_order"),
    ("explore_diagram_dot", {"operation": "explore", "diagram": "Dot"}, "orders/service.place_order"),
    ("explore_diagram_off", {"operation": "explore", "diagram": False}, "cli.main"),
    ("explore_widget", {"operation": "explore", "name": "pkg.Widget.run", "max_depth": 99, "diagram": True}, "", "widget"),
    ("explore_widget_ambiguous", {"operation": "explore"}, "shared.Helper", "widget"),
    ("explore_widget_repo", {"operation": "explore", "repo": "repo-b", "diagram": "dot"}, "shared.Helper", "widget"),
    # traverse
    ("traverse_query", {"operation": "traverse"}, "orders/api.create_order"),
    ("traverse_start_alias", {"operation": "traverse", "start": "place_order", "max_depth": 1}, ""),
    ("traverse_starts_and_ids", {"operation": "traverse", "starts": ["cli.main"], "start_ids": ["save"], "node_ids": E_ROUTE}, ""),
    ("traverse_names_and_id", {"operation": "traverse", "names": ["validate_token"], "id": "parse"}, "ignored"),
    ("traverse_reverse", {"operation": "traverse", "direction": "reverse", "relation_types": ["calls"]}, "orders/repository.save"),
    ("traverse_both", {"operation": "traverse", "direction": "both", "max_depth": 2}, "orders/repository.save"),
    ("traverse_max_depth_capped", {"operation": "traverse", "max_depth": 1000}, "cli.main"),
    ("traverse_max_nodes", {"operation": "traverse", "max_nodes": 3}, "orders/api.create_order"),
    ("traverse_seeds_over_max_nodes", {"operation": "traverse", "names": ["cli.main", "save", "validate_token"], "max_nodes": 2}, ""),
    ("traverse_node_types", {"operation": "traverse", "node_types": ["storage"]}, "orders/api.create_order"),
    ("traverse_no_seeds", {"operation": "traverse"}, " "),
    ("traverse_missing", {"operation": "traverse", "names": ["cli.main", "nowhere"]}, ""),
    ("traverse_missing_id_first", {"operation": "traverse", "names": ["tokens"], "node_ids": ["nope"]}, ""),
    ("traverse_ambiguous", {"operation": "traverse"}, "tokens"),
    ("traverse_names_not_list", {"operation": "traverse", "names": 3}, ""),
    ("traverse_diagram_dot", {"operation": "traverse", "diagram": "dot"}, "orders/api.create_order"),
    (
        "traverse_widget_reverse_rollup",
        {"operation": "traverse", "direction": "reverse", "relation_types": ["calls"], "max_depth": 2},
        "pkg.Widget",
        "widget",
    ),
    (
        "traverse_widget_no_rollup",
        {"operation": "traverse", "direction": "reverse", "type_rollup": False},
        "pkg.Widget",
        "widget",
    ),
    (
        "traverse_widget_rollup_null",
        {"operation": "traverse", "direction": "reverse", "type_rollup": None},
        "pkg.Widget",
        "widget",
    ),
    ("traverse_widget_start", {"operation": "traverse", "start": "api.handler", "max_depth": 1}, "", "widget"),
    (
        "traverse_widget_diagram",
        {"operation": "traverse", "direction": "both", "diagram": "mermaid"},
        "pkg.Widget.run",
        "widget",
    ),
    # find_path
    ("find_path_found", {"operation": "find_path", "target": "orders/repository.save"}, "orders/api.create_order"),
    ("find_path_aliases", {"operation": "find_path", "from": "POST /orders", "to": "orders_table"}, "ignored"),
    ("find_path_ids", {"operation": "find_path", "source_id": E_ROUTE, "target_id": "table"}, ""),
    ("find_path_from_id_to_id", {"operation": "find_path", "from_id": "cli", "to_id": "report"}, ""),
    ("find_path_same_node", {"operation": "find_path", "source": "cli.main", "target": "cli.main"}, ""),
    ("find_path_not_found", {"operation": "find_path", "source": "orders/repository.save", "target": "create_order"}, ""),
    ("find_path_depth_too_short", {"operation": "find_path", "max_depth": 1, "target": "save"}, "create_order"),
    ("find_path_relation_filter", {"operation": "find_path", "relation_types": "handled_by", "target": "save"}, "POST /orders"),
    ("find_path_no_target", {"operation": "find_path"}, "cli.main"),
    ("find_path_target_missing", {"operation": "find_path", "target": "nowhere.at_all"}, "cli.main"),
    ("find_path_source_missing", {"operation": "find_path", "target": "cli.main"}, "nowhere.at_all"),
    (
        "find_path_target_repo",
        {"operation": "find_path", "source_repo": OTHER_REPO, "target": "billing.charge_card", "target_repo": REPO},
        "billing.charge_card",
    ),
    ("find_path_target_ambiguous", {"operation": "find_path", "target": "billing.charge_card"}, "place_order"),
    ("find_path_bad_depth", {"operation": "find_path", "max_depth": True, "target": "save"}, "create_order"),
    ("find_path_diagram", {"operation": "find_path", "target": "orders_table", "diagram": True}, "POST /orders"),
    ("find_path_diagram_dot", {"operation": "find_path", "target": "orders_table", "diagram": "dot"}, "POST /orders"),
    ("find_path_widget", {"operation": "find_path", "target": "pkg.Widget.run", "relation_types": ["calls"]}, "api.handler", "widget"),
    ("find_path_widget_rollup", {"operation": "find_path", "target": "pkg.Widget", "relation_types": ["calls"]}, "api.handler", "widget"),
    (
        "find_path_widget_no_rollup",
        {"operation": "find_path", "target": "pkg.Widget", "target_rollup": False},
        "api.handler",
        "widget",
    ),
    (
        "find_path_widget_seed_is_member",
        {"operation": "find_path", "source": "pkg.Widget.run", "target": "pkg.Widget"},
        "",
        "widget",
    ),
    (
        "find_path_widget_diagram",
        {"operation": "find_path", "source": "api.handler", "target": "widget_db", "diagram": "mermaid"},
        "",
        "widget",
    ),
    # impact_analysis
    ("impact_query", {"operation": "impact_analysis"}, "auth/tokens.validate_token"),
    ("impact_no_dependents", {"operation": "impact_analysis"}, "cli.main"),
    ("impact_cross_repo", {"operation": "impact_analysis", "target": "billing.charge_card", "repo": REPO}, ""),
    ("impact_targets_and_ids", {"operation": "impact_analysis", "targets": ["save"], "target_ids": ["parse"]}, ""),
    ("impact_include_forward", {"operation": "impact_analysis", "include_forward": True, "max_depth": 1}, "place_order"),
    ("impact_include_dependencies", {"operation": "impact_analysis", "include_dependencies": "yes"}, "place_order"),
    ("impact_max_nodes", {"operation": "impact_analysis", "max_nodes": 2}, "orders/repository.save"),
    ("impact_relation_and_types", {"operation": "impact_analysis", "relation": "calls", "node_types": ["route"]}, "save"),
    ("impact_no_seeds", {"operation": "impact_analysis"}, ""),
    ("impact_missing", {"operation": "impact_analysis", "target": "nowhere"}, ""),
    ("impact_bad_max_depth", {"operation": "impact_analysis", "max_depth": "3"}, "save"),
    ("impact_seed_error_first", {"operation": "impact_analysis", "max_depth": "3"}, "nowhere"),
    ("impact_bad_relation_after_depth", {"operation": "impact_analysis", "max_nodes": 1.5, "relation": 3}, "save"),
    ("impact_diagram_dot", {"operation": "impact_analysis", "diagram": "dot"}, "orders/repository.save"),
    ("impact_widget", {"operation": "impact_analysis", "max_depth": 2, "max_nodes": 4}, "pkg.Widget", "widget"),
    ("impact_widget_forward", {"operation": "impact_analysis", "max_depth": 3, "include_forward": True}, "pkg.Widget", "widget"),
    ("impact_widget_target_alias", {"operation": "impact_analysis", "target": "pkg.Widget"}, "", "widget"),
    (
        "impact_widget_diagram_dot",
        {"operation": "impact_analysis", "name": "pkg.Widget.run", "max_depth": 5, "diagram": "dot"},
        "",
        "widget",
    ),
    ("impact_widget_diagram_mermaid", {"operation": "impact_analysis", "diagram": True}, "pkg.Widget", "widget"),
    ("impact_bare_constructor", {"operation": "impact_analysis"}, "Widget", "bare_constructor"),
    # insights
    ("insights_all", {"operation": "insights"}, "", "insights"),
    ("insights_confident", {"operation": "insights", "min_confidence": 0.9, "informational": False}, "", "insights"),
    ("insights_informational_text", {"operation": "insights", "informational": " TRUE "}, "", "insights"),
    ("insights_source", {"operation": "insights", "source": "hotspots"}, "", "insights"),
    ("insights_sources", {"operation": "insights", "sources": ["cycles", "domain"], "min_confidence": 1}, "", "insights"),
    ("insights_query_title", {"operation": "insights"}, "CYCLE", "insights"),
    ("insights_query_ignored", {"operation": "insights", "limit": 10}, "What did the explainers find?", "insights"),
    ("insights_name", {"operation": "insights", "name": "finding"}, "", "insights"),
    ("insights_repo", {"operation": "insights", "repo": "repo-b"}, "", "insights"),
    ("insights_page", {"operation": "insights", "offset": 1, "limit": 1}, "", "insights"),
    ("insights_min_confidence_zero", {"operation": "insights", "min_confidence": 0}, "", "insights"),
    ("insights_bad_confidence", {"operation": "insights", "min_confidence": 2}, "", "insights"),
    ("insights_confidence_string", {"operation": "insights", "min_confidence": "high"}, "", "insights"),
    ("insights_bad_informational", {"operation": "insights", "informational": "maybe"}, "", "insights"),
    ("insights_bad_sources", {"operation": "insights", "sources": 3}, "", "insights"),
    ("insights_bad_offset", {"operation": "insights", "offset": -1}, "", "insights"),
    ("insights_diagram", {"operation": "insights", "diagram": True}, "", "insights"),
    ("insights_diagram_dot", {"operation": "insights", "diagram": "dot", "source": "cycles"}, "", "insights"),
    ("insights_shop_none", {"operation": "insights", "diagram": True}, ""),
    ("query_facts_new_kind", {"operation": "query_facts", "kind": "extraction"}, "", "insights"),
    # architecture
    ("architecture_default", {"operation": "architecture"}, "", "architecture"),
    ("architecture_shop", {"operation": "architecture"}, "ignored"),
    ("architecture_shop_dot", {"operation": "architecture", "diagram": "dot"}, ""),
    (
        "architecture_modules_only",
        {"operation": "architecture", "node_types": ["module"], "relation_types": ["calls"], "max_nodes": 2, "diagram": False},
        "",
        "architecture",
    ),
    ("architecture_repo", {"operation": "architecture", "repo": "shop", "diagram": True}, "", "architecture"),
    ("architecture_other_repo", {"operation": "architecture", "repo": "elsewhere", "diagram": "dot"}, "", "architecture"),
    ("architecture_symbols", {"operation": "architecture", "node_types": ["symbol"]}, "", "architecture"),
    ("architecture_declares_only", {"operation": "architecture", "relation_types": ["declares"]}, "", "architecture"),
    ("architecture_max_nodes_one", {"operation": "architecture", "max_nodes": 1}, "", "architecture"),
    ("architecture_max_nodes_capped", {"operation": "architecture", "max_nodes": 1000}, "", "architecture"),
    ("architecture_bad_max_nodes", {"operation": "architecture", "max_nodes": "5"}, "", "architecture"),
    ("architecture_bad_node_types", {"operation": "architecture", "node_types": ["flow"]}, "", "architecture"),
    # Diagrams of the P1a operations, and of delta (which has no graph).
    ("query_facts_diagram", {"operation": "query_facts", "file_prefix": "orders/", "diagram": True}, ""),
    ("query_facts_diagram_dot", {"operation": "query_facts", "kind": "symbol", "diagram": "dot"}, ""),
    ("neighbors_diagram", {"operation": "neighbors", "diagram": "mermaid"}, "orders/service.place_order"),
    ("neighbors_diagram_dot", {"operation": "neighbors", "name": "validate_token", "diagram": "dot"}, ""),
    ("subgraph_diagram", {"operation": "subgraph", "seeds": ["orders/service.place_order"], "diagram": True}, ""),
    ("subgraph_diagram_dot", {"operation": "subgraph", "seeds": ["create_order", "cli.main"], "depth": 2, "diagram": "dot"}, ""),
    ("subgraph_diagram_two_repos", {"operation": "subgraph", "seed_ids": ["charge"], "diagram": True}, ""),
    ("delta_diagram", {"operation": "delta", "diagram": True}, ""),
    ("delta_diagram_dot", {"operation": "delta", "diagram": "dot"}, "", "widget"),
    # Review additions: Python truthiness of flags and fallbacks, lower and
    # upper bounds, and a configured key that turns the insights query off.
    ("explore_direction_upper", {"operation": "explore", "direction": "BOTH", "max_nodes": 0}, "place_order"),
    ("explore_max_depth_zero", {"operation": "explore", "max_depth": 0}, "place_order"),
    ("traverse_max_nodes_capped", {"operation": "traverse", "max_nodes": 10_000, "max_depth": -1}, "cli.main"),
    (
        "traverse_widget_rollup_text_false",
        {"operation": "traverse", "direction": "reverse", "type_rollup": "false"},
        "pkg.Widget",
        "widget",
    ),
    ("find_path_max_depth_capped", {"operation": "find_path", "max_depth": 99, "target": "orders_table"}, "POST /orders"),
    (
        "find_path_blank_source_repo",
        {"operation": "find_path", "source_repo": "", "repo": OTHER_REPO, "target": "billing.charge_card", "target_repo": REPO},
        "billing.charge_card",
    ),
    (
        "find_path_widget_rollup_zero",
        {"operation": "find_path", "target": "pkg.Widget", "target_rollup": 0},
        "api.handler",
        "widget",
    ),
    ("impact_max_depth_capped", {"operation": "impact_analysis", "max_depth": 50}, "auth/tokens.validate_token"),
    ("impact_include_forward_zero", {"operation": "impact_analysis", "include_forward": 0}, "place_order"),
    ("insights_limit_capped", {"operation": "insights", "limit": 1000}, "", "insights"),
    ("insights_limit_zero", {"operation": "insights", "limit": 0}, "", "insights"),
    ("insights_diagram_ignores_query", {"operation": "insights", "diagram": True}, "CYCLE", "insights"),
    ("insights_repo_null", {"operation": "insights", "repo": None}, "", "insights"),
    ("architecture_repo_null", {"operation": "architecture", "repo": None, "diagram": None}, "", "architecture"),
]

# (name, config, query[, graph]): search, flows, hubs and ask (SDK-1056 P2), after
# Python's code_retriever_navigation_test.py.
P2_CASES = [
    # search
    ("search_validate_token", {"operation": "search", "text": "validate token"}, ""),
    ("search_camel_query", {"operation": "search"}, "validateToken"),
    ("search_q_snake", {"operation": "search", "q": "validate_token"}, ""),
    ("search_query_field", {"operation": "search", "query": "validate token"}, "ignored"),
    ("search_blank_text_falls_back", {"operation": "search", "text": "  "}, "validate token"),
    ("search_flows_only", {"operation": "search", "text": "orders", "kinds": ["flow"]}, ""),
    ("search_codeflow_kind", {"operation": "search", "text": "main", "kinds": "CodeFlow"}, ""),
    ("search_include_tests", {"operation": "search", "text": "validate token", "include_tests": True}, ""),
    ("search_include_tests_text", {"operation": "search", "text": "validate token", "include_tests": "TRUE"}, ""),
    ("search_kinds_all", {"operation": "search", "text": "orders", "kinds": "all"}, ""),
    ("search_kinds_mixed_all", {"operation": "search", "text": "orders", "kinds": ["module", " All "]}, ""),
    ("search_kinds_types", {"operation": "search", "text": "orders", "kinds": ["CodeModule", "storage"]}, ""),
    ("search_page", {"operation": "search", "text": "orders", "limit": 2, "offset": 1}, ""),
    ("search_offset_past_end", {"operation": "search", "text": "orders", "offset": 100}, ""),
    ("search_limit_capped", {"operation": "search", "text": "orders", "limit": 1000, "kinds": "all"}, ""),
    ("search_limit_zero_is_default", {"operation": "search", "text": "order", "limit": 0, "kinds": "all"}, ""),
    ("search_repo_unknown", {"operation": "search", "text": "token", "repo": "other"}, ""),
    ("search_repo_payments", {"operation": "search", "text": "charge card", "repo": OTHER_REPO}, ""),
    ("search_repo_null", {"operation": "search", "text": "charge card", "repo": None}, ""),
    ("search_file_prefix", {"operation": "search", "text": "order", "file_prefix": "orders/"}, ""),
    ("search_exact_name", {"operation": "search", "text": "orders/api.create_order"}, ""),
    ("search_short_name", {"operation": "search", "text": "create_order"}, ""),
    ("search_qualified_suffix", {"operation": "search", "text": "api.create_order"}, ""),
    ("search_declaration_view", {"operation": "search", "text": "orders/models.Order", "kinds": "all"}, ""),
    ("search_storage_view_alone", {"operation": "search", "text": "orders/models.Order", "kinds": "storage"}, ""),
    ("search_casefold_name", {"operation": "search", "text": "GEO/STRASSE.LOOKUP", "kinds": "all"}, ""),
    ("search_flow_summary", {"operation": "search", "text": "hands off stages"}, ""),
    ("search_route_name", {"operation": "search", "text": "POST /orders", "kinds": ["route", "flow"]}, ""),
    ("search_stopwords_only", {"operation": "search", "text": "how does this work"}, ""),
    ("search_no_hits", {"operation": "search", "text": "kubernetes operator"}, ""),
    ("search_unicode_terms", {"operation": "search", "text": "Größe straße", "kinds": "all"}, ""),
    ("search_ties_limit", {"operation": "search", "text": "check token", "limit": 3}, "", "shop_tests"),
    ("search_ties_page", {"operation": "search", "text": "check token", "limit": 4, "offset": 2}, "", "shop_tests"),
    ("search_no_docs", {"operation": "search", "text": "validate token"}, "", "shop_bare"),
    ("search_blank", {"operation": "search"}, "   "),
    ("search_text_not_string", {"operation": "search", "text": 5}, "token"),
    ("search_q_list", {"operation": "search", "q": ["token"]}, ""),
    ("search_bad_kind", {"operation": "search", "text": "token", "kinds": ["widget"]}, ""),
    ("search_kinds_not_a_list", {"operation": "search", "text": "token", "kinds": 3}, ""),
    ("search_bad_include_tests", {"operation": "search", "text": "token", "include_tests": "maybe"}, ""),
    ("search_negative_limit", {"operation": "search", "text": "token", "limit": -1}, ""),
    ("search_limit_float", {"operation": "search", "text": "token", "limit": 1.5}, ""),
    ("search_bad_offset", {"operation": "search", "text": "token", "offset": "x"}, ""),
    # flows
    ("flows_list", {"operation": "flows"}, ""),
    ("flows_list_explicit_mode", {"operation": "flows", "mode": " LIST "}, "ignored"),
    ("flows_list_side_mechanism", {"operation": "flows", "include_side": True, "mechanism": "main"}, ""),
    ("flows_list_mechanisms", {"operation": "flows", "mechanism": ["route", "main"], "include_side": "true"}, ""),
    ("flows_list_page", {"operation": "flows", "limit": 1}, ""),
    ("flows_list_offset", {"operation": "flows", "offset": 1, "include_side": True}, ""),
    ("flows_list_limit_capped", {"operation": "flows", "limit": 500}, ""),
    ("flows_list_repo", {"operation": "flows", "repo": REPO}, ""),
    ("flows_list_repo_other", {"operation": "flows", "repo": OTHER_REPO}, ""),
    ("flows_show_name_suffix", {"operation": "flows", "name": "/orders"}, ""),
    ("flows_show_enola_id", {"operation": "flows", "id": F_ORDERS}, ""),
    ("flows_show_node_id", {"operation": "flows", "id": "flow-main"}, ""),
    ("flows_show_query", {"operation": "flows", "mode": "show"}, "POST /orders"),
    ("flows_show_flow_alias", {"operation": "flows", "flow": "cli.MAIN"}, ""),
    ("flows_show_substring", {"operation": "flows", "name": "seed"}, ""),
    ("flows_show_side_flow", {"operation": "flows", "name": "scripts/seed.main"}, ""),
    ("flows_show_ambiguous", {"operation": "flows", "name": "main"}, ""),
    ("flows_show_missing", {"operation": "flows", "flow": "DELETE /orders"}, ""),
    ("flows_show_missing_id", {"operation": "flows", "id": "nope"}, ""),
    ("flows_show_id_wrong_repo", {"operation": "flows", "id": F_ORDERS, "repo": OTHER_REPO}, ""),
    ("flows_show_name_wrong_repo", {"operation": "flows", "name": "POST /orders", "repo": OTHER_REPO}, ""),
    ("flows_show_blank", {"operation": "flows", "mode": "show"}, "  "),
    ("flows_through_symbol", {"operation": "flows", "symbol": "auth/tokens.parse_header"}, ""),
    ("flows_through_stage", {"operation": "flows", "through": "billing.charge_card", "repo": REPO}, ""),
    ("flows_through_ambiguous", {"operation": "flows", "through": "billing.charge_card"}, ""),
    ("flows_through_id", {"operation": "flows", "mode": "through", "id": "create"}, ""),
    ("flows_through_enola_id", {"operation": "flows", "mode": "through", "id": E_PLACE}, ""),
    ("flows_through_route", {"operation": "flows", "mode": "through"}, "POST /orders"),
    ("flows_through_outside_entry", {"operation": "flows", "symbol": "orders/report.render_report"}, ""),
    ("flows_through_none", {"operation": "flows", "symbol": "auth/session.SessionStore"}, ""),
    ("flows_through_other_repo", {"operation": "flows", "symbol": "billing.charge_card", "repo": OTHER_REPO}, ""),
    ("flows_through_missing", {"operation": "flows", "symbol": "nowhere.at_all"}, ""),
    ("flows_bad_mode", {"operation": "flows", "mode": "draw"}, ""),
    ("flows_mode_bool", {"operation": "flows", "mode": True}, ""),
    ("flows_no_flows", {"operation": "flows"}, "", "shop_no_flows"),
    ("flows_no_flows_show", {"operation": "flows", "name": "POST /orders"}, "", "shop_no_flows"),
    ("flows_bad_limit_no_flows", {"operation": "flows", "limit": -1}, "", "shop_no_flows"),
    ("flows_bad_offset_no_flows", {"operation": "flows", "offset": "x"}, "", "shop_no_flows"),
    ("flows_bad_mechanism_no_flows", {"operation": "flows", "mechanism": 5}, "", "shop_no_flows"),
    ("flows_bad_include_side_no_flows", {"operation": "flows", "include_side": "maybe"}, "", "shop_no_flows"),
    ("flows_bad_repo_no_flows", {"operation": "flows", "repo": 7}, "", "shop_no_flows"),
    ("flows_show_id_list_no_flows", {"operation": "flows", "mode": "show", "id": ["F1"]}, "", "shop_no_flows"),
    ("flows_show_name_int_no_flows", {"operation": "flows", "mode": "show", "name": 3}, "", "shop_no_flows"),
    ("flows_through_symbol_list_no_flows", {"operation": "flows", "mode": "through", "symbol": ["create"]}, "", "shop_no_flows"),
    ("flows_show_id_true", {"operation": "flows", "mode": "show", "id": True}, ""),
    ("flows_show_flow_list", {"operation": "flows", "mode": "show", "flow": ["POST /orders"]}, ""),
    ("flows_through_float", {"operation": "flows", "mode": "through", "through": 1.5}, ""),
    ("flows_through_id_list", {"operation": "flows", "mode": "through", "symbol": "create", "id": [1]}, ""),
    # hubs
    ("hubs_default", {"operation": "hubs"}, "ignored"),
    ("hubs_include_tests", {"operation": "hubs", "include_tests": True}, ""),
    ("hubs_reverse_limit", {"operation": "hubs", "direction": "reverse", "include_tests": True, "limit": 1}, ""),
    ("hubs_forward", {"operation": "hubs", "direction": "Forward"}, ""),
    ("hubs_modules_declares", {"operation": "hubs", "kinds": ["module"], "relation_types": ["declares"]}, ""),
    ("hubs_every_kind_calls", {"operation": "hubs", "kinds": ["symbol", "route", "storage"], "relation": "calls"}, ""),
    ("hubs_repo", {"operation": "hubs", "repo": OTHER_REPO}, ""),
    ("hubs_file_prefix", {"operation": "hubs", "file_prefix": "orders/"}, ""),
    ("hubs_limit_capped", {"operation": "hubs", "limit": 1000, "kinds": "all"}, ""),
    ("hubs_tests_graph", {"operation": "hubs"}, "", "shop_tests"),
    ("hubs_tests_graph_include_tests", {"operation": "hubs", "include_tests": True}, "", "shop_tests"),
    ("hubs_bad_include_tests", {"operation": "hubs", "include_tests": "maybe"}, ""),
    ("hubs_bad_direction", {"operation": "hubs", "direction": "up"}, ""),
    ("hubs_bad_kind", {"operation": "hubs", "kinds": ["flow"]}, ""),
    ("hubs_negative_limit", {"operation": "hubs", "limit": -3}, ""),
    ("hubs_bad_relation", {"operation": "hubs", "relation_types": [""]}, ""),
    # ask
    ("ask_question", {"operation": "ask", "question": "Where do we validate auth tokens for new orders?"}, ""),
    ("ask_query_text", {"operation": "ask"}, "how are orders placed and cards charged?"),
    ("ask_text_field", {"operation": "ask", "text": "render the sales report"}, ""),
    ("ask_no_flows_no_docs", {"operation": "ask"}, "how does order placement work?", "shop_bare"),
    ("ask_no_flows", {"operation": "ask", "question": "validate token"}, "", "shop_no_flows"),
    ("ask_nothing", {"operation": "ask", "text": "kubernetes operator reconcile"}, ""),
    ("ask_nothing_repo", {"operation": "ask", "text": "kubernetes", "repo": REPO}, ""),
    ("ask_nothing_no_flows", {"operation": "ask", "text": "kubernetes"}, "", "shop_no_flows"),
    ("ask_stopwords_only", {"operation": "ask"}, "how does this work?"),
    (
        "ask_cuts",
        {"operation": "ask", "question": "validate token order", "max_seeds": 2, "max_nodes": 3, "max_citations": 2},
        "",
    ),
    ("ask_max_nodes_2", {"operation": "ask", "question": "orders token card report", "max_seeds": 5, "max_nodes": 2}, ""),
    ("ask_limits_capped", {"operation": "ask", "question": "order token", "max_seeds": 99, "max_nodes": 999, "max_citations": 999, "kinds": "all"}, ""),
    ("ask_repo", {"operation": "ask", "question": "charge card", "repo": OTHER_REPO}, ""),
    ("ask_file_prefix", {"operation": "ask", "question": "order", "file_prefix": "orders/"}, ""),
    ("ask_flow_match", {"operation": "ask", "question": "cli main", "kinds": ["flow", "symbol"]}, ""),
    ("ask_flows_only", {"operation": "ask", "question": "orders", "kinds": ["flow"]}, ""),
    ("ask_kinds_all_tests", {"operation": "ask", "question": "validate token", "kinds": "all", "include_tests": True}, ""),
    ("ask_tests_graph", {"operation": "ask", "question": "validate token", "max_seeds": 1, "max_nodes": 10}, "", "shop_tests"),
    (
        "ask_tests_graph_include_tests",
        {"operation": "ask", "question": "validate token", "max_seeds": 1, "max_nodes": 10, "include_tests": True},
        "",
        "shop_tests",
    ),
    ("ask_ties", {"operation": "ask", "question": "check token", "max_seeds": 3}, "", "shop_tests"),
    ("ask_checkout", {"operation": "ask", "question": "How does checkout work?"}, "", "checkout"),
    ("ask_checkout_documented", {"operation": "ask", "question": "How does checkout work?"}, "", "checkout_documented"),
    ("ask_no_question", {"operation": "ask"}, ""),
    ("ask_question_not_string", {"operation": "ask", "question": 5}, "x"),
    ("ask_bad_max_seeds", {"operation": "ask", "question": "token", "max_seeds": -1}, ""),
    ("ask_bad_max_citations", {"operation": "ask", "question": "token", "max_citations": "ten"}, ""),
    ("ask_bad_kinds", {"operation": "ask", "question": "token", "kinds": ["widget"]}, ""),
    ("ask_bad_include_tests", {"operation": "ask", "question": "token", "include_tests": 1}, ""),
    # capabilities over the variants
    ("capabilities_no_flows", {"operation": "capabilities"}, "", "shop_no_flows"),
    ("capabilities_bare", {"operation": "capabilities"}, "", "shop_bare"),
]

# Direct renderer calls: (name, nodes, edges, highlight_ids, title).
DIAGRAM_CASES = [
    (
        "escapes",
        [
            {"id": "a", "kind": "route", "name": 'GET /items?q="x"<y>#1&2', "repo": "r"},
            {"id": "b", "kind": "module", "name": ".", "repo": "r"},
        ],
        [{"source_id": "a", "target_id": "b", "type": "declares", "count": 3}],
        ["b"],
        'a "quoted" # title: Vec<T>',
    ),
    (
        "every_shape_two_repos",
        [
            {"id": "m", "kind": "module", "name": "pkg", "repo": "z"},
            {"id": "s", "kind": "symbol", "name": "pkg.f\nsecond line", "repo": "z"},
            {"id": "r", "kind": "route", "name": "GET /", "repo": ""},
            {"id": "st", "kind": "storage", "name": "t", "repo": "a"},
            {"id": "d", "kind": "dependency", "name": "dep", "repo": "a"},
            {"id": "sv", "kind": "service", "name": "svc", "repo": "a"},
            {"id": "i", "kind": "insight", "name": "finding", "repo": "a"},
            {"id": "in", "kind": "intent", "name": "intent", "repo": "a"},
            {"id": "rp", "kind": "repository", "name": "repo", "repo": "a"},
            {"id": "x", "kind": "lint", "name": None, "repo": "a"},
            {"id": "y", "name": "", "repo": None},
        ],
        [
            {"source_id": "m", "target_id": "s", "type": "declares", "count": 1},
            {"source_id": "s", "target_id": "r", "type": "", "count": True},
            {"source_id": "r", "target_id": "missing", "type": "calls"},
            {"source_id": "d", "target_id": "sv", "type": "back\\slash \"q\"", "count": 2},
            {"source_id": "i", "target_id": "in"},
        ],
        ["s", "nope", "s", "x"],
        "back\\slash\nline",
    ),
    ("empty_untitled", [], [], [], None),
    ("empty_title", [], [], [], ""),
]


def _jsonable(value):
    """Python's results hold only JSON types; tuples become lists."""
    return json.loads(json.dumps(value))


async def run_case(code_retriever, snapshot, config, query):
    """CodeRetriever.get_retrieved_objects over a given snapshot."""
    retriever = code_retriever.CodeRetriever(config=config)
    handler = getattr(retriever, f"_{retriever.operation}")
    result = handler(snapshot, query or "")
    if retriever.diagram_format is not None:
        result["diagram"] = code_retriever.render_result_diagram(result, retriever.diagram_format)
    context = await retriever.get_context_from_objects(query, retrieved_objects=result)
    return result, context


def _write_graph(path, nodes, edges):
    graph = {
        "nodes": [[node_id, properties] for node_id, properties in nodes],
        "edges": [[source, target, relation, properties] for source, target, relation, properties in edges],
    }
    path.write_text(json.dumps(graph, indent=2, ensure_ascii=False) + "\n")


def main():
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} <python-cognee-checkout-at-the-pin>")
    code_retriever = load_retriever(Path(sys.argv[1]))
    diagram = sys.modules["cognee.modules.retrieval.code_graph_diagram"]
    nodes, edges = fixture_graph()

    OUT_DIR.mkdir(parents=True, exist_ok=True)
    _write_graph(OUT_DIR / "graph.json", nodes, edges)
    snapshots = {"shop": code_retriever._CodeGraphSnapshot(nodes, edges)}
    for graph_name, build in EXTRA_GRAPHS.items():
        extra_nodes, extra_edges = build()
        _write_graph(OUT_DIR / f"graph_{graph_name}.json", extra_nodes, extra_edges)
        snapshots[graph_name] = code_retriever._CodeGraphSnapshot(extra_nodes, extra_edges)

    empty = code_retriever._CodeGraphSnapshot([], [])
    cases = [(name, config, query, "shop", snapshots["shop"]) for name, config, query in CASES]
    cases.append(("capabilities_empty_graph", {"operation": "capabilities"}, "", "shop", empty))
    cases.append(("query_facts_empty_graph", {"operation": "query_facts"}, "x", "shop", empty))
    cases.append(("delta_empty_graph", {"operation": "delta"}, "", "shop", empty))
    for name, config, query, *graph_name in P1B_CASES:
        graph_name = graph_name[0] if graph_name else "shop"
        cases.append((name, config, query, graph_name, snapshots[graph_name]))
    for name, config, query, *graph_name in P2_CASES:
        graph_name = graph_name[0] if graph_name else "shop"
        cases.append((name, config, query, graph_name, snapshots[graph_name]))
    cases.append(("search_empty_graph", {"operation": "search"}, "token", "shop", empty))
    cases.append(("flows_empty_graph", {"operation": "flows"}, "", "shop", empty))
    cases.append(("flows_bad_mode_empty_graph", {"operation": "flows", "mode": "x"}, "", "shop", empty))
    cases.append(("hubs_empty_graph", {"operation": "hubs"}, "", "shop", empty))
    cases.append(("ask_empty_graph", {"operation": "ask"}, "checkout", "shop", empty))
    cases.append(("ask_no_question_empty_graph", {"operation": "ask"}, " ", "shop", empty))
    cases.append(("explore_empty_graph", {"operation": "explore"}, "x", "shop", empty))
    cases.append(("architecture_empty_graph", {"operation": "architecture"}, "", "shop", empty))
    cases.append(("insights_empty_graph", {"operation": "insights"}, "", "shop", empty))

    goldens = []
    names = set()
    for name, config, query, graph_name, graph_snapshot in cases:
        assert name not in names, f"duplicate case {name}"
        names.add(name)
        entry = {"name": name, "config": config, "query": query}
        if graph_name != "shop":
            entry["graph"] = graph_name
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

    diagrams = []
    for name, diagram_nodes, diagram_edges, highlight, title in DIAGRAM_CASES:
        diagrams.append(
            {
                "name": name,
                "nodes": diagram_nodes,
                "edges": diagram_edges,
                "highlight_ids": highlight,
                "title": title,
                "mermaid": diagram.render_mermaid(
                    diagram_nodes, diagram_edges, highlight_ids=highlight, title=title
                ),
                "dot": diagram.render_dot(
                    diagram_nodes, diagram_edges, highlight_ids=highlight, title=title
                ),
            }
        )
    (OUT_DIR / "diagrams.json").write_text(json.dumps(diagrams, indent=2, ensure_ascii=False) + "\n")
    print(f"wrote {len(goldens)} goldens and {len(diagrams)} diagrams to {OUT_DIR}")


if __name__ == "__main__":
    main()
