//! Silicon Commit backend library.
//!
//! Commit is a modular monolith: domain policy and application workflows are
//! independent from HTTP, PostgreSQL, and platform-service adapters. The API,
//! outbox worker, and migration binaries are deliberately thin composition
//! roots.

#![forbid(unsafe_code)]
#![deny(clippy::dbg_macro)]
#![deny(clippy::expect_used)]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::unwrap_used)]

pub mod api;
pub mod application;
pub mod config;
pub mod domain;
pub mod error;
pub mod infrastructure;
pub mod request_context;
pub mod shutdown;
pub mod telemetry;
pub mod worker;
