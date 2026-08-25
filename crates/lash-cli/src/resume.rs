use lash::LashSession;
use lash::messages::Message;
use lash::provider::ProviderHandle;
use lash::tools::ToolState;

use crate::app::{App, UiTimelineItem};
use crate::execution_settings::ExecutionMode;
use crate::model_catalog::CachedModelCatalog;
use crate::session_log;

#[allow(clippy::too_many_arguments)]
pub async fn load_resumed_session(
    _identifier: &str,
    app: &mut App,
    _logger: &mut session_log::SessionLogger,
    history: &mut Vec<Message>,
    runtime: &mut Option<LashSession>,
    turn_counter: &mut usize,
    execution_mode: &mut ExecutionMode,
    provider: &ProviderHandle,
    current_model_variant: &mut Option<String>,
    active_tool_state: &mut ToolState,
    _model_catalog: &CachedModelCatalog,
) -> Result<(), String> {
    let session = runtime
        .as_ref()
        .ok_or_else(|| "opened session was not installed".to_string())?;
    let loaded = session_log::load_opened_session(session)
        .map_err(|error| format!("Could not load session: {error}"))?;
    *history = loaded.messages;
    app.timeline = loaded.blocks;
    app.session_id = loaded.session_id.clone();
    app.session_name = loaded.session_name;
    app.usage.last_response_usage = loaded.last_token_usage;
    app.usage.last_prompt_usage = None;
    app.usage.token_usage = session.usage_report().usage.usage;
    app.plugin_mode_indicators = loaded.plugin_mode_indicators;
    app.replace_ui_activity_journal(loaded.ui_activity_journal);
    match session_log::take_cancel_recovery(&loaded.session_id) {
        Ok(texts) if !texts.is_empty() => {
            app.timeline.push(UiTimelineItem::SystemMessage(format!(
                "Inputs dropped by the last cancelled turn:\n{}",
                texts.join("\n\n")
            )));
        }
        Ok(_) => {}
        Err(error) => {
            app.timeline.push(UiTimelineItem::SystemMessage(format!(
                "Could not read cancelled-input recovery: {error}"
            )));
        }
    }
    app.timeline.push(UiTimelineItem::SystemMessage(format!(
        "Resumed: {}",
        loaded.filename
    )));
    let policy = session.policy_snapshot();
    app.model = policy.model.id.clone();
    app.usage.context_window = Some(policy.model.context_window_tokens() as u64);
    *current_model_variant =
        crate::model_selection::variant_from_reasoning_selection(policy.model.variant);
    app.set_model_variant(current_model_variant.clone());
    *turn_counter = session.read_view().turn_index();
    *active_tool_state = session
        .admin()
        .tools()
        .state()
        .await
        .map_err(|error| error.to_string())?;
    let _ = provider;
    app.stop_turn();
    app.live.tool_output = loaded.live_tool_output;
    app.set_execution_mode_label(execution_mode);
    app.invalidate_height_cache();
    app.resume_follow_output();
    Ok(())
}
