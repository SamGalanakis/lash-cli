use std::sync::Arc;

use lash::plugins::{
    PluginDirective, PluginError, PluginFactory, PluginRegistrar, PluginRuntimeEvent,
    PluginSessionContext, SessionPlugin,
};
use lash::tools::{
    ToolCall, ToolContract, ToolDefinition, ToolManifest, ToolOutcome, ToolProvider,
};
use lash_lashlang_runtime::{ToolBinding, ToolDefinitionBindingExt};
use serde::{Deserialize, Serialize};

pub(crate) const PLUGIN_ID: &str = "update_plan";
pub(crate) const PLAN_EVENT: &str = "plan_mode.state";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanItem {
    pub(crate) step: String,
    pub(crate) status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PlanSnapshot {
    #[serde(default)]
    pub(crate) explanation: Option<String>,
    #[serde(default)]
    pub(crate) generation: u64,
    pub(crate) plan: Vec<PlanItem>,
}

pub(crate) struct UpdatePlanPluginFactory;

impl PluginFactory for UpdatePlanPluginFactory {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn build(&self, context: &PluginSessionContext) -> Result<Arc<dyn SessionPlugin>, PluginError> {
        Ok(Arc::new(UpdatePlanPlugin {
            active: context.is_root_session(),
        }))
    }
}

struct UpdatePlanPlugin {
    active: bool,
}

impl SessionPlugin for UpdatePlanPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, registrar: &mut PluginRegistrar) -> Result<(), PluginError> {
        if !self.active {
            return Ok(());
        }
        registrar
            .tools()
            .provider(Arc::new(UpdatePlanTool) as Arc<dyn ToolProvider>)?;
        registrar.tool_calls().after(Arc::new(|context| {
            Box::pin(async move {
                if context.tool_name != "update_plan" {
                    return Ok(Vec::new());
                }
                let snapshot = serde_json::from_value::<PlanSnapshot>(context.args)
                    .map_err(|error| PluginError::Invoke(error.to_string()))?;
                Ok(vec![PluginDirective::emit_runtime_events(vec![
                    PluginRuntimeEvent::Custom {
                        name: PLAN_EVENT.to_string(),
                        payload: serde_json::to_value(snapshot)
                            .map_err(|error| PluginError::Invoke(error.to_string()))?,
                    },
                ])])
            })
        }));
        Ok(())
    }
}

struct UpdatePlanTool;

#[async_trait::async_trait]
impl ToolProvider for UpdatePlanTool {
    fn tool_manifests(&self) -> Vec<ToolManifest> {
        vec![update_plan_definition().manifest()]
    }

    fn resolve_contract(&self, name: &str) -> Option<Arc<ToolContract>> {
        (name == "update_plan").then(|| Arc::new(update_plan_definition().contract()))
    }

    async fn execute(&self, call: ToolCall<'_>) -> ToolOutcome {
        match serde_json::from_value::<PlanSnapshot>(call.args.clone()) {
            Ok(snapshot) => ToolOutcome::ok(serde_json::json!({
                "generation": snapshot.generation,
            })),
            Err(error) => ToolOutcome::err_fmt(format_args!("invalid plan: {error}")),
        }
    }
}

fn update_plan_definition() -> ToolDefinition {
    ToolDefinition::raw(
        "tool:update_plan",
        "update_plan",
        "Publish or replace the current task plan.",
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["plan"],
            "properties": {
                "explanation": {"type": ["string", "null"]},
                "generation": {"type": "integer", "minimum": 0},
                "plan": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["step", "status"],
                        "properties": {
                            "step": {"type": "string"},
                            "status": {"enum": ["pending", "in_progress", "completed"]}
                        }
                    }
                }
            }
        }),
        serde_json::json!({
            "type": "object",
            "required": ["generation"],
            "properties": {"generation": {"type": "integer"}}
        }),
    )
    .with_tool_binding(ToolBinding::new(["plan"], "update"))
}
