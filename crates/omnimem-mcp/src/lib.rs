//! OmniMem's MCP server.
//!
//! The tools keep 6.x's names, parameters, defaults and descriptions (agents
//! read the descriptions, so they are copied verbatim), and return the same
//! JSON. Transport is streamable HTTP at `/mcp`; 6.x's deprecated SSE
//! transport is not carried over.
//!
//! Phase 2 serves the core tools. The rest (experience, projects, briefing,
//! skills, knowledge) arrive with the engine phases that port them.

mod args;
mod descriptions;
mod handler;
mod http;

pub use handler::OmniMemServer;
pub use http::{ServerConfig, ServerError, router, serve};

/// Sent to every client on connect: the agent's operating instructions.
pub const INSTRUCTIONS: &str = include_str!("instructions.md");
