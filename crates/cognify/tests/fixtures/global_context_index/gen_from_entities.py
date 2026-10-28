"""Run Python's from_entities pure functions (origin/dev) on a synthetic graph.

Usage: python3 gen_from_entities.py <path to a cognee checkout> > from_entities.json
"""
import json, re, subprocess, sys, types, random
from uuid import uuid5, NAMESPACE_OID

if len(sys.argv) != 2:
    sys.exit("usage: gen_from_entities.py <path to a cognee checkout>")
PY = sys.argv[1]
def src(path):
    return subprocess.check_output(["git", "-C", PY, "show", f"origin/dev:{path}"], text=True)

def strip_imports(code):
    out = []
    skip = False
    for line in code.splitlines():
        if skip:
            if line.strip().endswith(")"):
                skip = False
            continue
        if re.match(r"^(from \.|from cognee|import cognee)", line):
            if line.rstrip().endswith("("):
                skip = True
            continue
        out.append(line)
    return "\n".join(out)

mod = types.ModuleType("gen"); sys.modules["gen"] = mod; ns = mod.__dict__
exec(strip_imports(src("cognee/tasks/memify/global_context_index/constants.py")), ns)
exec(strip_imports(src("cognee/modules/graph/utils/entity_scoring.py")), ns)
models = strip_imports(src("cognee/tasks/memify/global_context_index/models.py"))
models = models.replace("from pydantic import BaseModel", "BaseModel = object")
exec(models, ns)
ids = strip_imports(src("cognee/tasks/memify/global_context_index/ids.py"))
exec(ids, ns)

class GlobalContextSummary:
    def __init__(self, id, text, dataset_id, level, is_root=False, document_id=None):
        self.id, self.text, self.dataset_id, self.level, self.is_root, self.document_id = (
            id, text, dataset_id, level, is_root, document_id)
ns["GlobalContextSummary"] = GlobalContextSummary
ns["is_internal_node"] = lambda p: bool(p.get("is_internal"))
ns["task_summary"] = lambda t: (lambda f: f)
ns["Node"] = ns["EdgeData"] = object
ns["PipelineContext"] = object
exec(strip_imports(src("cognee/tasks/memify/global_context_index/load_dataset_content.py")), ns)
exec(strip_imports(src("cognee/tasks/memify/global_context_index/build_document_index.py")), ns)

rng = random.Random(1044)
ds = "6f1c1a40-0000-5000-8000-000000000044"
docs = {f"d{i}": f"{n}" for i, n in enumerate(["alpha.md", "beta notes.txt", "gamma.pdf", "alpha.md", "  "])}
doc_ids = {k: str(uuid5(NAMESPACE_OID, k)) for k in docs}
names = ["Alice", "Bob", "Cara", "Dan", "Eve", "Acme Corp", "Globex", "Paris", "Berlin",
         "Project  Apollo", "Quantum\tSystems", "Zed", "Ünïcode Name", "x" * 100, "Mallory", "Trent"]
types_ = ["Person", "Organization", "Place", "Project", "Concept"]
nodes, edges = [], []
chunks = []
for k, did in doc_ids.items():
    for c in range(rng.randint(1, 4)):
        cid = str(uuid5(NAMESPACE_OID, f"{k}-c{c}"))
        chunks.append(cid)
        nodes.append([cid, {"type": "DocumentChunk", "document_id": did, "text": "..."}])
# a chunk of a document outside the dataset, and an internal chunk
nodes.append(["c-foreign", {"type": "DocumentChunk", "document_id": "not-in-dataset"}])
nodes.append(["c-internal", {"type": "DocumentChunk", "document_id": doc_ids["d0"], "is_internal": True}])
type_ids = {}
for t in types_:
    tid = str(uuid5(NAMESPACE_OID, "type:" + t))
    type_ids[t] = tid
    nodes.append([tid, {"type": "EntityType", "name": t}])
ent_ids = []
for n in names:
    eid = str(uuid5(NAMESPACE_OID, "ent:" + n))
    ent_ids.append(eid)
    nodes.append([eid, {"type": "Entity", "name": n}])
    for t in rng.sample(types_, rng.choice([0, 1, 1, 2])):
        edges.append([eid, type_ids[t], "is_a", {"relationship_name": "is_a"}])
    for cid in rng.sample(chunks + ["c-foreign"], rng.randint(0, 4)):
        edges.append([cid, eid, "contains", {"relationship_name": "contains"}])
for _ in range(40):
    s, t = rng.sample(ent_ids, 2)
    rel = rng.choice(["knows", "works_at", "located_in"])
    text = rng.choice([f"{rel} relation", f"rel {rng.randint(0, 9)}", "  spaced   text  ", ""])
    props = {"relationship_name": rel}
    if text:
        props["edge_text"] = text
    edges.append([s, t, rel, props])
nodes.append(["9e5b970d-fb7f-5280-bbdf-3f12ed0869ee", {"type": "GlobalContextSummary", "dataset_id": ds, "text": "stale"}])
nodes.append(["other-ds-node", {"type": "GlobalContextSummary", "dataset_id": "other", "text": "x"}])

document_names = {did: " ".join(docs[k].split()) or None for k, did in doc_ids.items()}
# Python reads names from chunks; build the chunk property the same way for its run.
py_nodes = []
for node_id, props in nodes:
    if props.get("type") == "DocumentChunk" and document_names.get(props.get("document_id")):
        props = dict(props, document_name=document_names[props["document_id"]])
    py_nodes.append((node_id, props))
content = ns["_dataset_content_from_rows"](py_nodes, [tuple(e) for e in edges], ds, set(doc_ids.values()))
summaries = ns["build_context_summaries"](content)
fixture = {
    "dataset_id": ds,
    "documents": {did: docs[k] for k, did in doc_ids.items()},
    "nodes": nodes,
    "edges": edges,
    "expected": [
        {"id": str(s.id), "text": s.text, "level": s.level, "is_root": s.is_root, "document_id": s.document_id}
        for s in summaries
    ],
}
json.dump(fixture, sys.stdout, indent=1, ensure_ascii=False)
