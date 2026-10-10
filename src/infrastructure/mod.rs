//! PostgreSQL and external-service adapters.

pub mod accounts;
pub mod giphy;
pub mod postgres;
pub mod ting;
/// Ting recipient enrolment with User verification proofs (only when Ting is configured).
pub mod ting_enrollment;

/// DM's App-verification-proof Ting publisher.
pub mod ting_publisher;
