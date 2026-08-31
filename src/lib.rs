//! Silicon DM's reusable domain, application, and infrastructure library.

#![forbid(unsafe_code)]

pub mod api;
pub mod application;
pub mod bootstrap;
pub mod config;
pub mod domain;
pub mod error;
pub mod infrastructure;
pub mod realtime;
pub mod shutdown;
pub mod telemetry;
pub mod worker;

pub use config::Settings;
pub use error::{AppError, AppResult};
