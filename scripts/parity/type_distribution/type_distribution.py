"""Entity-type distribution of one or more graph exports (SDK-1694 test 11).

Usage: type_distribution.py <cmp_dir> <engine>...  — reads
``<cmp_dir>/<engine>/db/graph_{node,edge}.csv.gz`` (a Postgres ``\\copy`` dump,
or ``export_to_csv.py`` output) and prints, per engine: entities with several
types, entities with more types than the documents that mention them
("violations", 0 under Python's rule without an ontology), and the extra types
per entity by the number of documents mentioning it. An entity's documents
come from chunk ``contains`` → entity plus chunk ``is_part_of`` → document;
its types from ``is_a`` edges into EntityType nodes.

This is the script behind the SDK-1694 findings (the 81- and 320-document
comparison), kept as it was run.
"""

import csv, gzip, sys, collections, re, json
csv.field_size_limit(10**9)
B = sys.argv[1]
def load(e):
    nodes = {}
    with gzip.open(f"{B}/{e}/db/graph_node.csv.gz", "rt") as f:
        for r in csv.DictReader(f):
            nodes[r["id"]] = (r["type"], r["name"])
    edges = []
    with gzip.open(f"{B}/{e}/db/graph_edge.csv.gz", "rt") as f:
        for r in csv.DictReader(f):
            edges.append((r["source_id"], r["target_id"], r["relationship_name"]))
    return nodes, edges
for e in sys.argv[2:]:
    nodes, edges = load(e)
    T = lambda i: nodes.get(i, ("?",))[0]
    chunk_doc = {s: t for s, t, r in edges if r == "is_part_of" and T(s) == "DocumentChunk"}
    ent_docs = collections.defaultdict(set); ent_chunks = collections.defaultdict(set)
    for s, t, r in edges:
        if r == "contains" and T(s) == "DocumentChunk" and T(t) == "Entity":
            ent_docs[t].add(chunk_doc.get(s)); ent_chunks[t].add(s)
    types = collections.defaultdict(set)
    for s, t, r in edges:
        if r == "is_a" and T(s) == "Entity" and T(t) == "EntityType":
            types[s].add(t)
    ents = [i for i in nodes if T(i) == "Entity"]
    ndocs = len({i for i in nodes if T(i).endswith("Document")})
    multi = [i for i in ents if len(types[i]) > 1]
    print(f"== {e}: docs {ndocs} chunks {len(chunk_doc)} entities {len(ents)} EntityType nodes {sum(1 for i in nodes if T(i)=='EntityType')}")
    print(f"   entities with 0 types {sum(1 for i in ents if not types[i])}, >1 types {len(multi)}, max {max((len(types[i]) for i in ents), default=0)}")
    print(f"   violations (#types > #docs mentioning): {sum(1 for i in ents if len(types[i]) > max(1,len(ent_docs[i])))}")
    print(f"   single-doc entities with >1 type: {sum(1 for i in multi if len(ent_docs[i])==1)}; no-doc entities: {sum(1 for i in ents if not ent_docs[i])}")
    by = collections.defaultdict(lambda: [0, 0, 0])
    for i in ents:
        k = min(len(ent_docs[i]), 8); b = by[k]; b[0] += 1; b[1] += max(0, len(types[i]) - 1); b[2] += len(types[i]) > 1
    print("   docs: n entities / extra types per entity / share multi-typed")
    for k in sorted(by): print(f"     {k}{'+' if k==8 else ''}: {by[k][0]} / {by[k][1]/by[k][0]:.3f} / {by[k][2]/by[k][0]:.3f}")
    # equality rate: types == docs for multi-doc
    md = [i for i in ents if len(ent_docs[i]) >= 2]
    print(f"   multi-doc entities {len(md)}; of them 1 type: {sum(1 for i in md if len(types[i])==1)}")
    # type nodes unreferenced
    used = set().union(*types.values()) if types else set()
    print(f"   EntityType nodes with no is_a from an Entity: {sum(1 for i in nodes if T(i)=='EntityType' and i not in used)}")
    STRUCT = {"contains", "is_a", "made_from", "is_part_of"}
    sem = [r for s, t, r in edges if r not in STRUCT]
    bad = [r for r in sem if re.search(r"[A-Z ']", r)]
    print(f"   semantic edges {len(sem)}, un-normalized names {len(bad)} sample {sorted(set(bad))[:6]}")
    # case-only duplicate edges
    seen = collections.defaultdict(set)
    for s, t, r in edges:
        if r not in STRUCT: seen[(s, t, r.lower().replace(' ', '_').replace("'", ''))].add(r)
    print(f"   (src,tgt) triples collapsing under generate_edge_name: {sum(1 for v in seen.values() if len(v)>1)}")
