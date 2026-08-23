//! MCP discovery plugin.
use std::sync::Arc;

use lash_core::ToolProvider;
use lash_core::plugin::{
    PluginError, PluginFactory, PluginSessionContext, PluginSpec, SessionPlugin,
    StaticPluginFactory,
};
use lash_lashlang_runtime::catalogue_preview_contribution;

use super::service::tool_discovery_provider_with_catalog;

/// Plugin factory for the `search_tools` deferred-discovery catalog.
///
/// Declares its provider through a [`PluginSpec`] driven by
/// [`StaticPluginFactory`], so it does not hand-roll the `SessionPlugin` +
/// `register` ceremony. Alongside the `search_tools` tool, it advertises the
/// deferred (non-resident) MCP tools to the model through a catalogue-preview
/// prompt contribution. `with_catalog(...)` is the CLI/RLM path for a large MCP
/// tail: `search_tools` indexes resident tools plus that explicit tail, while
/// the prompt preview lists only the deferred catalog so resident members keep
/// their full RLM docs.
pub struct ToolDiscoveryPluginFactory {
    inner: StaticPluginFactory,
}

impl ToolDiscoveryPluginFactory {
    pub fn new() -> Self {
        Self::with_catalog(Vec::new())
    }

    pub fn with_catalog(extra_catalog: Vec<serde_json::Value>) -> Self {
        if extra_catalog.is_empty() {
            return Self {
                inner: StaticPluginFactory::new("tool_discovery", PluginSpec::new()),
            };
        }
        let extra_catalog = Arc::new(extra_catalog);
        let discovery_catalog = Arc::clone(&extra_catalog);
        let spec = PluginSpec::new()
            .with_tool_provider(Arc::new(tool_discovery_provider_with_catalog(
                extra_catalog.as_ref().clone(),
            )) as Arc<dyn ToolProvider>)
            .with_prompt_contributor(Arc::new(
                move |_ctx: lash_core::plugin::PromptHookContext| {
                    let discovery_catalog = Arc::clone(&discovery_catalog);
                    Box::pin(async move {
                        Ok(catalogue_preview_contribution(discovery_catalog.as_ref())
                            .into_iter()
                            .collect())
                    })
                },
            ));
        Self {
            inner: StaticPluginFactory::new("tool_discovery", spec),
        }
    }
}

impl Default for ToolDiscoveryPluginFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginFactory for ToolDiscoveryPluginFactory {
    fn id(&self) -> &'static str {
        self.inner.id()
    }

    fn build(&self, ctx: &PluginSessionContext) -> Result<Arc<dyn SessionPlugin>, PluginError> {
        self.inner.build(ctx)
    }
}
