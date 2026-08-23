//! Deferred MCP tool definitions.
use lash_core::{ToolActivation, ToolDefinition};
use lash_tool_support::{ToolBinding, ToolDefinitionBindingExt};
use serde_json::{Value, json};

pub(crate) fn search_tools_definition() -> ToolDefinition {
    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct SearchToolsArgs {
        #[schemars(
            description = "Concise tool search query. Prefer keywords and short intent phrases with the app/domain, action, object, qualifiers, and important fields; for multi-constraint tasks include every constraint, such as \"spotify liked songs library\"."
        )]
        query: String,
        #[cfg(feature = "lashlang")]
        #[schemars(description = "Optional module filter, such as \"appworld\" or \"web\".")]
        module: Option<ModuleFilter>,
        #[schemars(range(min = 1, max = 100))]
        #[schemars(description = "Maximum number of results to return. Defaults to 10.")]
        limit: Option<usize>,
        #[schemars(description = "Exact tool name or names to exclude from results.")]
        exclude: Option<NameFilter>,
    }

    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    #[cfg(feature = "lashlang")]
    #[serde(untagged)]
    enum ModuleFilter {
        One(String),
        Many(Vec<String>),
    }

    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    #[serde(untagged)]
    enum NameFilter {
        One(String),
        Many(Vec<String>),
    }

    let description = "Search catalogued capabilities, aliases, descriptions, signatures, return fields, and examples. Use this when the capability you need is only listed in the catalogued-capabilities preview or is too sparse to call confidently. Query with concise keywords and short intent phrases: include the app/domain, action, object, qualifiers, and important fields or constraints.";

    ToolDefinition::raw(
        "tool:search_tools",
        "search_tools",
        description,
        schema_for::<SearchToolsArgs>(),
        search_tools_output_schema(),
    )
    .with_activation(ToolActivation::Always)
    .with_tool_binding(ToolBinding::new(["tools"], "search").with_aliases(["tool_search"]))
}

fn schema_for<T>() -> Value
where
    T: schemars::JsonSchema,
{
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or_else(|_| json!({}))
}

fn search_tools_output_schema() -> Value {
    let schema = json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["id", "name", "signature"],
            "properties": {
                "id": { "type": "string" },
                "name": { "type": "string" },
                "signature": {
                    "type": "string",
                    "description": "Callable signature with successful return type plus compact parameter and return-field details."
                },
                "description": { "type": "string" },
                "examples": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            }
        }
    });
    #[cfg(feature = "lashlang")]
    {
        let mut schema = schema;
        let item = schema
            .get_mut("items")
            .and_then(Value::as_object_mut)
            .expect("search tools item schema");
        item.get_mut("required")
            .and_then(Value::as_array_mut)
            .expect("required")
            .push(json!("call"));
        let properties = item
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .expect("properties");
        properties.insert(
            "module_path".to_string(),
            json!({
                "type": "array",
                "items": { "type": "string" }
            }),
        );
        properties.insert("operation".to_string(), json!({ "type": "string" }));
        properties.insert(
            "call".to_string(),
            json!({
                "type": "string",
                "description": "Exact callable module path for the active RLM dialect."
            }),
        );
        schema
    }
    #[cfg(not(feature = "lashlang"))]
    {
        schema
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn search_tools_has_typed_result_schema() {
        let definition = search_tools_definition();

        assert_eq!(
            definition.contract.output_schema.canonical["type"],
            json!("array")
        );
        let item = &definition.contract.output_schema.canonical["items"];
        assert_eq!(item["type"], json!("object"));
        let required = item["required"].as_array().expect("required");
        assert!(required.contains(&json!("id")));
        assert!(required.contains(&json!("name")));
        #[cfg(feature = "lashlang")]
        assert!(required.contains(&json!("call")));
        #[cfg(not(feature = "lashlang"))]
        assert!(!required.contains(&json!("call")));
        assert!(required.contains(&json!("signature")));
        let rendered_signature = definition.compact_contract().render_signature();
        assert!(
            rendered_signature.starts_with("search_tools({ query: str"),
            "{rendered_signature}"
        );
        #[cfg(feature = "lashlang")]
        assert!(
            rendered_signature.contains("module?: list[str] | str | null"),
            "{rendered_signature}"
        );
        #[cfg(not(feature = "lashlang"))]
        assert!(
            !rendered_signature.contains("module?:"),
            "{rendered_signature}"
        );
        assert!(
            rendered_signature.contains("exclude?: list[str] | str | null"),
            "{rendered_signature}"
        );
        assert!(
            rendered_signature.contains("-> list[record{"),
            "{rendered_signature}"
        );
        assert!(
            rendered_signature.contains("id: str"),
            "{rendered_signature}"
        );
        assert!(
            rendered_signature.contains("name: str"),
            "{rendered_signature}"
        );
        #[cfg(feature = "lashlang")]
        assert!(
            rendered_signature.contains("call: str"),
            "{rendered_signature}"
        );
        #[cfg(not(feature = "lashlang"))]
        assert!(
            !rendered_signature.contains("call: str"),
            "{rendered_signature}"
        );
        assert!(
            rendered_signature.contains("signature: str"),
            "{rendered_signature}"
        );
    }
}
