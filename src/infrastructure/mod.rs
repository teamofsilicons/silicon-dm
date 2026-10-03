//! PostgreSQL and external-service adapters.

pub mod giphy;
pub mod iam;
pub mod postgres;
pub mod ting;
pub mod ting_authorization;
pub mod ting_credentials;
pub mod ting_enrollment;

/// Originator-authenticated durable Ting publisher.
pub mod ting_publisher;
