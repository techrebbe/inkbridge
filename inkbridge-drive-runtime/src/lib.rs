//! Runtime adapters for the storage-independent InkBridge Drive gateway.
//!
//! The job deliberately keeps Google Drive as a device transport. Immutable
//! evidence is written to Cloud Storage before the broker sees an event, and
//! the Firestore checkpoint advances only after every downstream effect is
//! durable.

mod google;
mod runtime;

pub use google::*;
pub use runtime::*;
