//! MemVault MCP proxy library.
//!
//! The proxy logic lives here so the tool handlers, injection engine and merge
//! helpers can be exercised by unit tests. [`main.rs`] (the binary) stays a thin
//! bootstrap that wires config, storage, the memory router and the upstream
//! connections together.

pub mod config;
pub mod context;
pub mod extraction;
pub mod handler;
pub mod injection;
pub mod merge;
pub mod shutdown;
pub mod upstream;
