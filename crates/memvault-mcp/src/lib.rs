//! MemVault MCP + REST server library.
//!
//! Splitting the server logic into a library crate allows the tool handlers and
//! REST API endpoints to be exercised by unit tests, while [`main.rs`] (the binary)
//! stays a thin bootstrap layer that wires up storage, the memory router and the
//! selected transport.

pub mod metrics_setup;
pub mod rest_api;
pub mod server;
pub mod shutdown;
pub mod sse_server;
