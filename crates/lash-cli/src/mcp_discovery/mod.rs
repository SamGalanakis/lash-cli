//! MCP tool-discovery support owned by the CLI host.
//!
//! This is host policy, not a lash primitive: lash ships no tool discovery. The
//! example shows the recommended way to make a large MCP tool set discoverable
//! under the flat Tool Catalog + RLM deferred-resolution model:
//!
//! 1. Enumerate MCP tools and build a ranking index ([`ranking`]).
//! 2. Advertise them through a catalogue-preview prompt contribution
//!    ([`lash_lashlang_runtime::catalogue_preview_contribution`]).
//! 3. Expose a `search_tools` host tool over the index ([`definitions`],
//!    [`service`]).
//! 4. Register a [`DeferredToolResolver`](lash_lashlang_runtime::DeferredToolResolver)
//!    that resolves chosen MCP call-paths into a Tool Grant + Tool Execution
//!    Binding ([`resolver`]).
//!
//! The index ranking (BM25 / optional semantic / RRF) lives here as a reference
//! example hosts can copy or adapt. The catalogue-preview formatter is a public
//! helper in `lash-lashlang-runtime`.

mod catalog;
mod common;
mod definitions;
mod plugin;
mod ranking;
mod rerank;
mod resolver;
mod schema_index;
mod service;

pub use plugin::ToolDiscoveryPluginFactory;
pub use resolver::{McpCatalogedTool, McpDeferredToolResolver};

use std::sync::Arc;

use lash_core::ToolProvider;
use serde_json::json;

/// Enumerate provider tools into the shared MCP discovery/deferred record.
pub fn mcp_cataloged_tools(server: &str, provider: &dyn ToolProvider) -> Vec<McpCatalogedTool> {
    provider
        .tool_manifests()
        .into_iter()
        .filter_map(|manifest| {
            let contract = provider.resolve_contract(&manifest.name)?;
            Some(McpCatalogedTool {
                server: server.to_string(),
                definition: lash_core::ToolDefinition::from_parts(
                    manifest,
                    Arc::unwrap_or_clone(contract),
                ),
            })
        })
        .collect()
}

/// Project enumerated MCP tools to the JSON catalog consumed by `search_tools`
/// and the catalogue-preview prompt contribution.
pub fn mcp_catalog_records(tools: &[McpCatalogedTool]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| {
            let manifest = tool.definition.manifest();
            json!({
                "id": manifest.id,
                "name": manifest.name,
                "description": manifest.description,
                "bindings": manifest.bindings,
                "activation": manifest.activation,
                "contract": manifest.compact_contract,
            })
        })
        .collect()
}
