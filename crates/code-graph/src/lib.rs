//! Deterministic code graph for cognee, from an [enola](https://github.com/enola-labs/enola)
//! snapshot.
//!
//! Port of Python cognee's `cognee/tasks/code_graph` (parity pin `a59433e6b`,
//! enola 0.5.1). This crate is the pure half: it reads a snapshot directory
//! ([`parse_enola_snapshot`]), maps its facts onto the 15 code node types
//! ([`map_facts_to_data_points`]) and resolves their relations into typed
//! edges ([`build_code_graph_edges`]). It runs no subprocess and writes no
//! graph; node ids, `fact_hash` values, properties and edges match Python's
//! byte for byte, which the golden tests under `tests/` check against output
//! Python produced (`scripts/generate_code_graph_goldens.py`).

mod edges;
mod error;
mod mapping;
mod models;
pub mod pyjson;
mod snapshot;

pub use edges::{CodeGraphEdge, build_code_graph_edges};
pub use error::{CodeGraphError, CodeGraphResult};
pub use mapping::{CODE_GRAPH_MAPPING_VERSION, fact_node_id, map_facts_to_data_points};
pub use models::{
    CODE_INDEX_FIELDS, CodeGraphEntity, CodeNode, CodeNodeType, CodeRepository, FACT_KINDS,
};
pub use snapshot::{
    Fact, ParsedSnapshot, Receipt, SUPPORTED_FORMAT_VERSIONS, is_enola_id, is_enola_id_str,
    normalize_relation, parse_enola_snapshot, receipt_vouches_for_flows, relation_target_id,
    snapshot_identity, validate_receipt,
};
