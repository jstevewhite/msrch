//! MCP front-end for msrch: index registry, tool handlers, and transport
//! startup. All search/index logic lives in msrch-core; this crate is a
//! thin protocol adapter, like the CLI.

pub mod registry;
pub mod server;

pub use server::{McpOptions, TransportKind, serve};
