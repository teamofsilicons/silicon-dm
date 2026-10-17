//! HTTP API routing and handlers.

pub mod extract;
pub mod handlers;
pub mod router;

pub use router::build_router;

/// Signed Silicon Accounts webhook receiver.
pub mod webhook;

mod access;
mod contracts;
mod groups;
mod me;
mod receipts;
mod render;
mod speech;
mod sync;
