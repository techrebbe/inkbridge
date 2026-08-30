//! Google Drive is a device transport, never InkBridge's canonical store.
//!
//! This crate contains the deterministic, storage-independent part of the
//! gateway.  A later Cloud Run job supplies Drive, Cloud Storage, Secret
//! Manager, and checkpoint adapters around these plans.

mod model;
mod planner;

pub use model::*;
pub use planner::*;
