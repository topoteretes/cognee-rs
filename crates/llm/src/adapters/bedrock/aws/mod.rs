//! Shared AWS plumbing for the Bedrock LLM adapter and the Bedrock embedding
//! engine: one credential/region/endpoint resolver, one signer, one transport
//! seam.
//!
//! # Why this lives in `cognee-llm` and not in `cognee-utils`
//!
//! `docs/roadmap/bedrock-provider-plan.md` §2.2 left the home of this module
//! open ("decide at R1"). It is decided here, and the answer is `cognee-llm`:
//!
//! * `crates/utils` is deliberately **wasm32-buildable** — it carries a
//!   `[target.'cfg(target_arch = "wasm32")'.dependencies] getrandom` shim, no
//!   `reqwest`, and no library-level `tokio` (see its `Cargo.toml` comments and
//!   `docs/spike-wasm-config1.md`). Pulling `aws-config` / `aws-sigv4` /
//!   `reqwest` in there would destroy that property for every consumer of the
//!   crate, feature-gated or not (the dep graph is what breaks, not the code).
//! * `crates/embedding` does not depend on `cognee-llm` today, but plan §2.3
//!   already specifies `bedrock = ["cognee-llm/bedrock"]` for it — so the
//!   `cognee-embedding -> cognee-llm` edge is intended, and is added by R4
//!   rather than avoided by relocating this module.
//!
//! R4 should not re-open the question.
//!
//! # Parity
//!
//! Every rule in here is a port of litellm's
//! `litellm/llms/bedrock/base_aws_llm.py`, which is what Python cognee reaches
//! through `litellm.completion`. The plan's §1.2 (auth ladder) and §1.3
//! (region / endpoint chains) are the audited spec; individual functions cite
//! the Python symbol they mirror.

pub mod credentials;
pub mod endpoint;
pub mod env;
pub mod region;
pub mod signer;
pub(crate) mod transport;
