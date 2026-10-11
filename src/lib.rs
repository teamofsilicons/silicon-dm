//! Silicon DM's reusable domain, application, and infrastructure library.

#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod bootstrap;
pub mod config;
pub mod domain;
pub mod error;
pub mod infrastructure;
pub mod logging;
/// `dm-migrate` operator commands (identity re-key at cutover).
pub mod operator;
/// Durable bug reporting and Postmark delivery.
pub mod reporting;
pub mod shutdown;
pub mod worker;

pub use config::Settings;
pub use error::{AppError, AppResult};
