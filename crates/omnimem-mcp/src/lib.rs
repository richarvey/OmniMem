//! OmniMem's MCP server.
//!
//! The tools keep 6.x's names, parameters, defaults and descriptions (agents
//! read the descriptions, so they are copied verbatim), and return the same
//! JSON. Transport is streamable HTTP at `/mcp`; 6.x's deprecated SSE
//! transport is not carried over. `/mcp` is protected by a shared bearer
//! token, OAuth 2.1 (the authorisation server lives in `oauth`), or both.

mod args;
mod descriptions;
mod handler;
mod http;
mod oauth;

pub use handler::OmniMemServer;
pub use http::{ServerConfig, ServerError, router, serve};
pub use oauth::{OAuthConfig, OAuthSetup};

/// Sent to every client on connect: the agent's operating instructions.
pub const INSTRUCTIONS: &str = include_str!("instructions.md");

/// What OmniMem puts in an agent's context before any tool is called, in
/// characters: the instructions, every tool's name, description and
/// parameter schema as `tools/list` sends them, and the deferred
/// `mcp__omnimem__<tool>` names a client lists. 6.x hardcoded these counts;
/// here they are measured from what the server actually sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextOverhead {
    pub instructions_chars: usize,
    pub tool_count: usize,
    pub tool_schemas_chars: usize,
    pub deferred_names_chars: usize,
}

pub fn context_overhead() -> ContextOverhead {
    let tools = handler::tools();
    ContextOverhead {
        instructions_chars: INSTRUCTIONS.chars().count(),
        tool_count: tools.len(),
        tool_schemas_chars: serde_json::to_string(&tools).map_or(0, |s| s.chars().count()),
        deferred_names_chars: tools
            .iter()
            .map(|tool| format!("mcp__omnimem__{}\n", tool.name).chars().count())
            .sum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_context_overhead_is_measured() {
        let overhead = context_overhead();
        assert_eq!(overhead.tool_count, handler::tools().len());
        assert!(overhead.tool_count > 40, "{overhead:?}");
        assert!(overhead.instructions_chars > 1000);
        assert!(overhead.tool_schemas_chars > overhead.tool_count * 100);
        assert!(overhead.deferred_names_chars > overhead.tool_count * 15);
    }
}
