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
//! hands it; [`extract_code_file_graph`] loads one stored code file of the
//! per-file CODE route, staged in a temporary directory. A dataset's
//! repositories are extracted together as one enola cluster
//! ([`snapshot_dataset_code_repos`], [`load_cluster_member`]), which gives
//! the edges between them. Node ids,
//! `fact_hash` values, properties and edges match Python's byte for byte,
//! which the golden tests under `tests/` check against output Python
//! produced (`scripts/generate_code_graph_goldens.py`). Settings are the
//! `code_graph.*` section ([`CodeGraphConfig`]).
//!
//! Loads are graph-only by default: no embedding call, no vector index.
//! `index_vectors` opts a load in ([`CodeGraphVectors`]): the nodes it writes
//! are then embedded too ([`index_code_graph_vectors`]).

mod cluster;
mod code_files;
mod code_repo;
mod config;
mod edges;
mod error;
mod mapping;
mod models;
mod pipeline;
pub mod pyjson;
pub mod query;
mod runner;
mod snapshot;
mod vectors;
mod writer;

pub use cluster::{
    CLUSTER_ENOLA_TIMEOUT_CAP_SECS, CLUSTER_INLINE_REBUILD_LIMIT, ClusterMember,
    DatasetCodeSnapshot, cluster_enola_timeout, cluster_output_dir, resolve_member_labels,
    snapshot_dataset_code_repos,
};
pub use code_files::{
    CODE_FILE_STAGING_PREFIX, detection_markers, extract_code_file_graph, is_code_sourced,
    original_file_name, stage_code_file, staged_repo_name,
};
pub use code_repo::{CODE_REPO_MANIFEST_KIND, extract_code_repo_graph, manifest_repo_path};
pub use config::{CodeGraphConfig, DEFAULT_ENOLA_TIMEOUT_SECS};
pub use edges::{CodeGraphEdge, build_code_graph_edges};
pub use error::{CodeGraphError, CodeGraphResult};
pub use mapping::{CODE_GRAPH_MAPPING_VERSION, fact_node_id, map_facts_to_data_points};
pub use models::{
    CODE_INDEX_FIELDS, CodeGraphEntity, CodeNode, CodeNodeType, CodeRepository, FACT_KINDS,
};
pub use pipeline::{
    CodeGraphLoad, ExtractedCodeGraph, extract_code_graph, load_cluster_member, load_code_graph,
};
pub use runner::{
    INSTALL_HINT, SUBPROCESS_ENV_OVERRIDES, find_enola_binary, render_cluster_config,
    run_enola_cluster, run_enola_generate,
};
pub use snapshot::{
    Fact, ParsedSnapshot, Receipt, SUPPORTED_FORMAT_VERSIONS, is_enola_id, is_enola_id_str,
    normalize_relation, parse_enola_snapshot, receipt_vouches_for_flows, relation_target_id,
    snapshot_identity, validate_receipt,
};
pub use vectors::{CodeGraphVectors, IndexedCollections, index_code_graph_vectors};
pub use writer::{
    CodeGraphLedger, CodeGraphLoadState, CodeGraphOwner, NodeDelta, RECEIPT_PROJECTION_KEYS,
    StoredRepositoryState, add_code_graph_data_points, add_code_graph_edges, receipt_projection,
    stored_repository_state,
};
