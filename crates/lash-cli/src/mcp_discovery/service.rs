//! MCP discovery service.
use std::sync::{Arc, RwLock};

use lash::tools::{AttemptContext, ToolCall, ToolOutcome};
use lash_tool_support::{StaticToolExecute, StaticToolProvider, typed_args, typed_ok};
use serde_json::Value;

use super::common::{LLM_CANDIDATE_LIMIT, args_with_limit, catalog_key, limit_from_args};
use super::definitions::{SearchToolsArgs, search_tools_definition};
use super::ranking::ToolDiscoveryIndex;
use super::rerank::{
    llm_rerank_request, merge_llm_selection, parse_llm_tool_names, rerank_payment_action_intent,
};

#[derive(Clone, Default)]
struct IndexCache {
    index: Option<CachedIndex>,
}

#[derive(Clone)]
struct CachedIndex {
    catalog_addr: usize,
    catalog_len: usize,
    index: Arc<ToolDiscoveryIndex>,
}

impl CachedIndex {
    fn matches_catalog_arc(&self, catalog: &Arc<Vec<Value>>) -> bool {
        self.catalog_addr == Arc::as_ptr(catalog) as usize && self.catalog_len == catalog.len()
    }
}

#[derive(Clone)]
pub struct ToolDiscoveryToolsProvider {
    cache: Arc<RwLock<IndexCache>>,
    extra_catalog: Arc<Vec<Value>>,
}

impl Default for ToolDiscoveryToolsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDiscoveryToolsProvider {
    pub fn new() -> Self {
        Self::with_catalog(Vec::new())
    }

    pub fn with_catalog(extra_catalog: Vec<Value>) -> Self {
        Self {
            cache: Arc::default(),
            extra_catalog: Arc::new(extra_catalog),
        }
    }

    fn searchable_catalog(&self, resident_catalog: Arc<Vec<Value>>) -> Arc<Vec<Value>> {
        if self.extra_catalog.is_empty() {
            return resident_catalog;
        }
        let mut combined = Vec::with_capacity(resident_catalog.len() + self.extra_catalog.len());
        combined.extend(resident_catalog.iter().cloned());
        combined.extend(self.extra_catalog.iter().cloned());
        Arc::new(combined)
    }

    fn index_for_catalog(&self, catalog: Arc<Vec<Value>>) -> Arc<ToolDiscoveryIndex> {
        if let Some(index) = self
            .cache
            .read()
            .expect("tool discovery cache lock poisoned")
            .index
            .as_ref()
            .filter(|cached| cached.matches_catalog_arc(&catalog))
            .map(|cached| Arc::clone(&cached.index))
        {
            return index;
        }

        let key = catalog_key(catalog.as_ref());
        if let Some(index) = self
            .cache
            .read()
            .expect("tool discovery cache lock poisoned")
            .index
            .as_ref()
            .filter(|cached| cached.index.key == key)
            .map(|cached| Arc::clone(&cached.index))
        {
            return index;
        }

        let index = Arc::new(ToolDiscoveryIndex::build(key, catalog.as_ref()));
        let cached = CachedIndex {
            catalog_addr: Arc::as_ptr(&catalog) as usize,
            catalog_len: catalog.len(),
            index: Arc::clone(&index),
        };
        self.cache
            .write()
            .expect("tool discovery cache lock poisoned")
            .index = Some(cached);
        index
    }

    async fn search_tools(
        &self,
        args: &Value,
        resident_catalog: Arc<Vec<Value>>,
        context: &AttemptContext<'_>,
    ) -> ToolOutcome {
        let catalog = self.searchable_catalog(resident_catalog);
        let index = self.index_for_catalog(catalog);
        let limit = limit_from_args(args);
        let candidate_args = args_with_limit(args, LLM_CANDIDATE_LIMIT);
        let candidates = index.search(&candidate_args);
        if candidates.is_empty() {
            return typed_ok(Vec::<serde_json::Value>::new());
        }
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let candidates = rerank_payment_action_intent(query, candidates);

        let model = match context.sessions().model().await {
            Ok(model) => model,
            Err(err) => {
                return ToolOutcome::err_fmt(format_args!(
                    "search_tools could not resolve parent model: {err}"
                ));
            }
        };
        let request = llm_rerank_request(
            args,
            &candidates,
            limit,
            model.model,
            crate::model_selection::variant_from_reasoning_selection(model.model_variant),
        );
        let completion = match context
            .direct_completions()
            .complete(request, "search_tools")
            .await
        {
            Ok(completion) => completion,
            Err(err) => return ToolOutcome::err_fmt(format_args!("search_tools failed: {err}")),
        };

        let selected_names = match parse_llm_tool_names(&completion.text) {
            Ok(names) => names,
            Err(err) => {
                return ToolOutcome::err_fmt(format_args!(
                    "search_tools returned invalid JSON: {err}"
                ));
            }
        };

        typed_ok(merge_llm_selection(candidates, selected_names, limit))
    }
}

/// Build the `search_tools` provider with an additional non-resident catalog.
pub fn tool_discovery_provider_with_catalog(
    extra_catalog: Vec<Value>,
) -> StaticToolProvider<ToolDiscoveryToolsProvider> {
    StaticToolProvider::new(
        vec![search_tools_definition()],
        ToolDiscoveryToolsProvider::with_catalog(extra_catalog),
    )
}

#[async_trait::async_trait]
impl StaticToolExecute for ToolDiscoveryToolsProvider {
    async fn execute(&self, call: ToolCall<'_>) -> ToolOutcome {
        match call.name {
            "search_tools" => match typed_args::<SearchToolsArgs>(call.args) {
                Err(outcome) => outcome,
                Ok(_) => match call.context.sessions().shared_tool_catalog().await {
                    Ok(catalog) => self.search_tools(call.args, catalog, call.context).await,
                    Err(err) => ToolOutcome::err_fmt(err.to_string()),
                },
            },
            _ => ToolOutcome::err_fmt(format_args!("Unknown tool: {}", call.name)),
        }
    }
}
