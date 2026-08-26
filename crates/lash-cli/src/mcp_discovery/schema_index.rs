//! Schema indexing for MCP discovery.
#[cfg(feature = "semantic-tool-search")]
use super::catalog::CatalogTool;

#[cfg(feature = "semantic-tool-search")]
pub(crate) fn semantic_index_text(tool: &CatalogTool) -> String {
    let mut parts = vec![
        tool.contract.name.clone(),
        tool.contract.render_signature(),
        tool.contract.description.clone(),
    ];
    parts.extend(tool.contract.examples.clone());
    parts
        .into_iter()
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
