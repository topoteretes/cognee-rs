//! Database adapters for Evokoa's PostgreSQL extensions.
//!
//! The extensions are accelerators over ordinary PostgreSQL source tables. The
//! graph adapter therefore delegates authoritative CRUD to `PgGraphAdapter` and
//! registers those tables with pgGraph. The vector adapter owns ordinary source
//! tables and registers them with pgContext. [`EvokoaHybridAdapter`] gives both
//! adapters one SeaORM pool and exposes a single-statement graph+vector query.

mod graph;
mod hybrid;
mod vector;

pub use graph::EvokoaGraphAdapter;
pub use hybrid::{EvokoaHybridAdapter, HybridGraphVectorHit};
pub use vector::EvokoaVectorAdapter;
