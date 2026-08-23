use std::sync::Arc;

use chrono::Utc;

use lash::plugins::{
    ContextError, PluginDirective, PluginError, PluginFactory, PluginRegistrar,
    PluginSessionContext, PreparedContext, SessionPlugin, TurnContextTransform,
    TurnTransformContext,
};
use lash_core::{Message, MessageRole, Part, PluginMessage, PromptContribution};

use crate::execution_settings::ExecutionMode;

/// Host-provided source for project instructions.
///
/// This is plugin-owned prompt policy, not a core runtime concept. The
/// `lash-cli` host wires an [`InstructionSource`] implementation when
/// installing the prompt-context plugin.
pub trait InstructionSource: Send + Sync {
    fn system_instructions(&self) -> String;
    fn context_instructions_for_reads(&self, read_paths: &[String]) -> String;
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PromptContextPluginConfig {
    pub include_environment: bool,
    pub include_project_instructions: bool,
}

impl Default for PromptContextPluginConfig {
    fn default() -> Self {
        Self {
            include_environment: true,
            include_project_instructions: true,
        }
    }
}

pub struct PromptContextPluginFactory {
    instruction_source: Arc<dyn InstructionSource>,
    config: PromptContextPluginConfig,
    execution_mode: ExecutionMode,
}

impl PromptContextPluginFactory {
    pub fn new(
        instruction_source: Arc<dyn InstructionSource>,
        config: PromptContextPluginConfig,
        execution_mode: ExecutionMode,
    ) -> Self {
        Self {
            instruction_source,
            config,
            execution_mode,
        }
    }
}

impl PluginFactory for PromptContextPluginFactory {
    fn id(&self) -> &'static str {
        "prompt_context"
    }

    fn build(&self, _ctx: &PluginSessionContext) -> Result<Arc<dyn SessionPlugin>, PluginError> {
        Ok(Arc::new(PromptContextPlugin {
            instruction_source: Arc::clone(&self.instruction_source),
            config: self.config.clone(),
            execution_mode: self.execution_mode,
        }))
    }
}

struct PromptContextPlugin {
    instruction_source: Arc<dyn InstructionSource>,
    config: PromptContextPluginConfig,
    execution_mode: ExecutionMode,
}

impl SessionPlugin for PromptContextPlugin {
    fn id(&self) -> &'static str {
        "prompt_context"
    }

    fn register(&self, reg: &mut PluginRegistrar) -> Result<(), PluginError> {
        let instruction_source = Arc::clone(&self.instruction_source);
        let include_project_instructions = self.config.include_project_instructions;
        reg.prompt().contribute(Arc::new(move |_ctx| {
            let instruction_source = Arc::clone(&instruction_source);
            Box::pin(async move {
                let mut contributions = Vec::new();
                if include_project_instructions {
                    let project_instructions = instruction_source.system_instructions();
                    if !project_instructions.trim().is_empty() {
                        contributions.push(PromptContribution::project_instructions(
                            project_instructions,
                        ));
                    }
                }
                Ok(contributions)
            })
        }));
        let instruction_source = Arc::clone(&self.instruction_source);
        reg.tool_calls().after(Arc::new(move |ctx| {
            let instruction_source = Arc::clone(&instruction_source);
            Box::pin(async move {
                if !ctx.result.is_success() || ctx.tool_name != "read_file" {
                    return Ok(Vec::new());
                }
                let Some(path) = ctx.args.get("path").and_then(|value| value.as_str()) else {
                    return Ok(Vec::new());
                };
                if path.is_empty() {
                    return Ok(Vec::new());
                }
                let instructions =
                    instruction_source.context_instructions_for_reads(&[path.to_string()]);
                if instructions.trim().is_empty() {
                    return Ok(Vec::new());
                }
                Ok(vec![PluginDirective::EnqueueMessages {
                    messages: vec![PluginMessage::text(MessageRole::System, instructions)],
                }])
            })
        }));

        if self.config.include_environment && self.execution_mode.is_standard() {
            reg.context()
                .prepare_turn(50, Arc::new(EnvironmentTailTransform));
        }
        Ok(())
    }
}

struct EnvironmentTailTransform;

#[async_trait::async_trait]
impl TurnContextTransform for EnvironmentTailTransform {
    fn id(&self) -> &'static str {
        "prompt_context.environment_tail"
    }

    async fn transform(
        &self,
        _ctx: &TurnTransformContext<'_>,
        input: PreparedContext,
    ) -> Result<PreparedContext, ContextError> {
        let context = build_prompt_environment_context();
        if context.trim().is_empty() {
            return Ok(input);
        }

        let mut output = input;
        let mut messages: Vec<Message> = output.messages.iter().cloned().collect();
        messages.push(environment_tail_message(context));
        output.messages = messages.into();
        Ok(output)
    }
}

fn environment_tail_message(content: String) -> Message {
    let id = "prompt-context-env";
    Message {
        id: id.to_string(),
        role: MessageRole::User,
        parts: vec![Part::prose(
            format!("{id}.p0"),
            format!("<system-reminder>\n{content}\n</system-reminder>"),
            None,
        )]
        .into(),
        origin: Some(lash_core::MessageOrigin::Plugin {
            plugin_id: "prompt_context".to_string(),
            transient: true,
        }),
    }
}

fn build_prompt_environment_context() -> String {
    let mut parts = Vec::new();
    let now = Utc::now();
    parts.push(format!("Current date (UTC): {}", now.format("%Y-%m-%d")));

    if let Ok(cwd) = std::env::current_dir() {
        parts.push(format!("Working directory: {}", cwd.display()));

        if cwd.join(".git").exists() {
            parts.push("Git repository: yes".to_string());
        }
    }

    parts.join("\n")
}
