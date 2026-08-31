//! HTTP API routing and handlers.

pub mod extract;
pub mod handlers;
pub mod router;

pub use router::build_router;
