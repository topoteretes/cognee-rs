//! AWS Bedrock provider (feature `bedrock`).
//!
//! Scope of this module today is the shared AWS plumbing in [`aws`]: env
//! resolution, the region and endpoint chains, the credential ladder, SigV4
//! signing and the transport seam. The adapter itself (`BedrockAdapter`,
//! model-id normalisation, route selection, the capability table and the
//! Converse transforms) lands in a later step — see
//! `docs/roadmap/bedrock-provider-plan.md` §4 R3.
//!
//! Nothing here is re-exported from `lib.rs` yet, deliberately: the public
//! surface starts existing when the adapter does.

pub mod aws;
