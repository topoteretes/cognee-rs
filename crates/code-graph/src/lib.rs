//! Deterministic code graph for cognee, from an [enola](https://github.com/enola-labs/enola)
//! snapshot.
//!
//! Port of Python cognee's `cognee/tasks/code_graph` (parity pin `a59433e6b`,
//! enola 0.5.1). It runs `enola --generate` on a repository
//! ([`run_enola_generate`]), reads the snapshot directory
//! ([`parse_enola_snapshot`]), maps its facts onto the 15 code node types
//! ([`map_facts_to_data_points`]), resolves their relations into typed edges
//! ([`build_code_graph_edges`]) and writes both to a graph
//! ([`add_code_graph_data_points`], [`add_code_graph_edges`]);
//! [`load_code_graph`] does all of it for one repository, and
//! [`extract_code_repo_graph`] for the repo item cognify's CODE_REPO route
//! hands it. Node ids,
//! `fact_hash` values, properties and edges match Python's byte for byte,
//! which the golden tests under `tests/` check against output Python
//! produced (`scripts/generate_code_graph_goldens.py`). Settings are the
//! `code_graph.*` section ([`CodeGraphConfig`]).

mod code_repo;
mod config;
mod edges;
mod error;
mod mapping;
mod models;
mod pipeline;
pub mod pyjson;
mod runner;
mod snapshot;
mod writer;

pub use code_repo::{CODE_REPO_MANIFEST_KIND, extract_code_repo_graph, manifest_repo_path};
pub use config::{CodeGraphConfig, DEFAULT_ENOLA_TIMEOUT_SECS};
pub use edges::{CodeGraphEdge, build_code_graph_edges};
pub use error::{CodeGraphError, CodeGraphResult};
pub use mapping::{CODE_GRAPH_MAPPING_VERSION, fact_node_id, map_facts_to_data_points};
pub use models::{
    CODE_INDEX_FIELDS, CodeGraphEntity, CodeNode, CodeNodeType, CodeRepository, FACT_KINDS,
};
pub use pipeline::{CodeGraphLoad, ExtractedCodeGraph, extract_code_graph, load_code_graph};
pub use runner::{INSTALL_HINT, SUBPROCESS_ENV_OVERRIDES, find_enola_binary, run_enola_generate};
pub use snapshot::{
    Fact, ParsedSnapshot, Receipt, SUPPORTED_FORMAT_VERSIONS, is_enola_id, is_enola_id_str,
    normalize_relation, parse_enola_snapshot, receipt_vouches_for_flows, relation_target_id,
    snapshot_identity, validate_receipt,
};
pub use writer::{
    CodeGraphLedger, CodeGraphLoadState, CodeGraphOwner, NodeDelta, RECEIPT_PROJECTION_KEYS,
    add_code_graph_data_points, add_code_graph_edges, receipt_projection,
};
