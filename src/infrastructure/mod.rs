//! PostgreSQL and external-service adapters.

pub mod giphy;
pub mod iam;
pub mod postgres;
pub mod ting;
pub mod ting_credentials;
pub mod ting_enrollment;
pub mod ting_proof;

/// Originator-authenticated durable Ting publisher.
pub mod ting_publisher;
