use super::*;
use crate::activity::ActivityStatus;
use lash::TurnInputApplication;
use lash::messages::MessageRole;
use lash::persistence::{CheckpointKind, TurnInputIngress};
use lash::plugins::{PluginMessage, PluginRuntimeEvent};
use lash::tools::{ToolCallOutput, ToolFailure, ToolFailureClass};
use lash::usage::TokenUsage;
use lash::{TurnActivity, TurnActivityId, TurnEvent};
use serde_json::json;

fn app() -> App {
    App::new("test-model".into(), "test".into(), "test-session-id".into())
}

fn activity(correlation_id: &str, event: TurnEvent) -> TurnActivity {
    TurnActivity::new(TurnActivityId::new(correlation_id), event)
}

fn activity_block(app: &App) -> &ActivityBlock {
    let activities = app
        .timeline
        .iter()
        .filter_map(|item| match item {
            UiTimelineItem::Activity(activity) => Some(activity.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(activities.len(), 1, "expected exactly one activity block");
    activities[0]
}

fn start_tool_call(app: &mut App, call_id: &str) {
    app.handle_turn_activity(activity(
        call_id,
        TurnEvent::ToolCallStarted {
            call_id: Some(call_id.to_string()),
            name: "lookup_widget".into(),
            args: json!({ "id": 7 }),
            graph_key: None,
            parent_call_id: None,
        },
    ));

    let block = activity_block(app);
    assert_eq!(block.call.call_id.as_deref(), Some(call_id));
    assert_eq!(block.result.status, ActivityStatus::Running);
}

fn complete_tool_call(app: &mut App, call_id: &str, output: ToolCallOutput) {
    app.handle_turn_activity(activity(
        call_id,
        TurnEvent::ToolCallCompleted {
            call_id: Some(call_id.to_string()),
            name: "lookup_widget".into(),
            args: json!({ "id": 7 }),
            output,
            duration_ms: 12,
            graph_key: None,
            parent_call_id: None,
        },
    ));
}

#[test]
fn reasoning_delta_lands_in_reasoning_lane() {
    let mut app = app();
    app.start_turn();

    app.handle_turn_activity(activity(
        "reasoning-1",
        TurnEvent::ReasoningDelta {
            text: "inspect the state".into(),
        },
    ));

    assert_eq!(
        app.live.reasoning.normalized_text().as_deref(),
        Some("inspect the state")
    );
    assert_eq!(app.live.assistant.normalized_text(), None);
}

#[test]
fn assistant_prose_delta_lands_in_assistant_lane() {
    let mut app = app();
    app.start_turn();

    app.handle_turn_activity(activity(
        "prose-1",
        TurnEvent::AssistantProseDelta {
            text: "Here is the answer.".into(),
        },
    ));

    assert_eq!(
        app.live.assistant.normalized_text().as_deref(),
        Some("Here is the answer.")
    );
    assert_eq!(app.live.reasoning.normalized_text(), None);
}

#[test]
fn model_attempt_reset_discards_matching_output_and_retains_other_chunks() {
    let mut app = app();
    app.start_turn();

    for (correlation_id, event) in [
        (
            "reasoning-discarded",
            TurnEvent::ReasoningDelta {
                text: "discarded reasoning. ".into(),
            },
        ),
        (
            "reasoning-retained",
            TurnEvent::ReasoningDelta {
                text: "retained reasoning.".into(),
            },
        ),
        (
            "prose-discarded",
            TurnEvent::AssistantProseDelta {
                text: "discarded prose. ".into(),
            },
        ),
        (
            "prose-retained",
            TurnEvent::AssistantProseDelta {
                text: "retained prose.".into(),
            },
        ),
    ] {
        app.handle_turn_activity(activity(correlation_id, event));
    }

    app.handle_turn_activity(TurnActivity::independent(TurnEvent::ModelAttemptReset {
        assistant_prose_correlation_ids: vec![TurnActivityId::new("prose-discarded")],
        reasoning_correlation_ids: vec![TurnActivityId::new("reasoning-discarded")],
    }));

    assert_eq!(
        app.timeline.items(),
        &[UiTimelineItem::AssistantReasoning(
            "retained reasoning.".into()
        )]
    );
    assert_eq!(
        app.live.assistant.normalized_text().as_deref(),
        Some("retained prose.")
    );
    assert_eq!(app.live.reasoning.normalized_text(), None);
    assert_eq!(app.usage.live_output_chars_estimate, 15);
}

#[test]
fn tool_call_lifecycle_replaces_running_block_with_success() {
    let mut app = app();
    app.start_turn();
    start_tool_call(&mut app, "call-success");

    complete_tool_call(
        &mut app,
        "call-success",
        ToolCallOutput::success(json!({ "answer": 42 })),
    );

    let block = activity_block(&app);
    assert_eq!(block.result.status, ActivityStatus::Completed);
    assert_eq!(block.result.raw, json!({ "answer": 42 }));
    assert_eq!(block.duration_ms, 12);
}

#[test]
fn tool_call_lifecycle_replaces_running_block_with_error() {
    let mut app = app();
    app.start_turn();
    start_tool_call(&mut app, "call-error");

    complete_tool_call(
        &mut app,
        "call-error",
        ToolCallOutput::failure(ToolFailure::tool(
            ToolFailureClass::Execution,
            "lookup_failed",
            "widget lookup failed",
        )),
    );

    let block = activity_block(&app);
    assert_eq!(block.result.status, ActivityStatus::Failed);
    assert_eq!(block.result.raw["code"], "lookup_failed");
    assert_eq!(block.result.raw["message"], "widget lookup failed");
    assert_eq!(block.duration_ms, 12);
}

#[test]
fn retry_status_moves_runtime_to_waiting_with_attempt_detail() {
    let mut app = app();
    app.start_turn();

    app.handle_turn_activity(TurnActivity::independent(TurnEvent::RetryStatus {
        wait_seconds: 3,
        attempt: 2,
        max_attempts: 5,
        reason: "rate limited".into(),
    }));

    assert_eq!(app.run_state, CliRunState::Waiting);
    let turn = app.live.turn.as_ref().expect("live turn status");
    assert_eq!(turn.run_state, CliRunState::Waiting);
    assert_eq!(
        turn.status_detail.as_deref(),
        Some("in 3s · attempt 2/5 · rate limited")
    );
    assert_eq!(
        app.pending_retry_status.as_deref(),
        Some("attempt 2/5 · rate limited")
    );
}

#[test]
fn error_surfaces_in_timeline_and_moves_runtime_to_transient_error() {
    let mut app = app();
    app.start_turn();

    app.handle_turn_activity(TurnActivity::independent(TurnEvent::Error {
        message: "runtime exploded".into(),
    }));

    assert_eq!(
        app.timeline.last(),
        Some(&UiTimelineItem::Error("runtime exploded".into()))
    );
    assert_eq!(app.run_state, CliRunState::Error);
    assert!(
        app.turn_active(),
        "non-cancellation errors leave the turn active"
    );
    let turn = app.live.turn.as_ref().expect("live error status");
    assert_eq!(turn.run_state, CliRunState::Error);
    assert_eq!(turn.status_detail.as_deref(), Some("runtime exploded"));
    assert!(turn.transient_until.is_some());
}

#[test]
fn usage_updates_response_and_cumulative_totals() {
    let mut app = app();
    app.start_turn();
    app.handle_turn_activity(TurnActivity::independent(TurnEvent::AssistantProseDelta {
        text: "estimated output".into(),
    }));
    assert!(app.usage.live_output_tokens_estimate > 0);
    let response = TokenUsage {
        input_tokens: 10,
        output_tokens: 4,
        cache_read_input_tokens: 2,
        cache_write_input_tokens: 1,
        reasoning_output_tokens: 3,
    };
    let cumulative = TokenUsage {
        input_tokens: 30,
        output_tokens: 12,
        cache_read_input_tokens: 5,
        cache_write_input_tokens: 2,
        reasoning_output_tokens: 7,
    };

    app.handle_turn_activity(TurnActivity::independent(TurnEvent::Usage {
        protocol_iteration: 1,
        usage: response.clone(),
        cumulative: cumulative.clone(),
    }));

    assert_eq!(app.usage.last_response_usage, response);
    assert_eq!(app.usage.token_usage, cumulative);
    assert_eq!(app.usage.live_output_chars_estimate, 0);
    assert_eq!(app.usage.live_output_tokens_estimate, 0);
}

#[test]
fn queued_input_accepted_adds_user_history_exactly_once() {
    let mut app = app();
    app.start_turn();
    let input_id = app.test_seed_queued_turn_snapshot(
        PreparedTurn::new("injected guidance".into(), Vec::new()),
        TurnInputIngress::next_turn(),
    );
    let event = || TurnEvent::QueuedInputAccepted {
        applications: vec![TurnInputApplication {
            input_id: input_id.clone(),
            source_key: None,
            turn_id: lash::persistence::TurnId::from("turn-1"),
            committed_message_id: "message-1".into(),
            checkpoint: Some(CheckpointKind::AfterWork),
        }],
    };

    app.handle_turn_activity(TurnActivity::independent(event()));
    app.handle_turn_activity(TurnActivity::independent(event()));

    assert!(app.pending_turn_input_snapshot().is_empty());
    assert_eq!(
        app.timeline
            .iter()
            .filter(|item| matches!(item, UiTimelineItem::UserInput(text) if text == "injected guidance"))
            .count(),
        1
    );
    assert_eq!(
        app.timeline
            .iter()
            .filter(|item| matches!(item, UiTimelineItem::TurnStart(turn) if turn.role == TurnRole::User))
            .count(),
        1
    );
}

#[test]
fn queued_messages_committed_adds_user_and_system_history() {
    let mut app = app();
    app.start_turn();

    app.handle_turn_activity(TurnActivity::independent(
        TurnEvent::QueuedMessagesCommitted {
            messages: vec![
                PluginMessage::text(MessageRole::User, "plugin guidance"),
                PluginMessage::text(MessageRole::System, "plugin checkpoint"),
            ],
            checkpoint: CheckpointKind::BeforeCompletion,
        },
    ));

    assert_eq!(
        app.timeline.items(),
        &[
            UiTimelineItem::TurnStart(Turn::user(false)),
            UiTimelineItem::UserInput("plugin guidance".into()),
            UiTimelineItem::SystemMessage("plugin checkpoint".into()),
        ]
    );
}

#[test]
fn plugin_runtime_maps_plan_process_status_event() {
    let mut app = app();
    app.start_turn();

    app.handle_turn_activity(TurnActivity::independent(TurnEvent::PluginRuntime {
        plugin_id: "plan_process".into(),
        event: PluginRuntimeEvent::Status {
            key: "plan-generation".into(),
            label: "thinking".into(),
            detail: Some("building execution plan".into()),
        },
    }));

    assert_eq!(app.run_state, CliRunState::Thinking);
    let turn = app.live.turn.as_ref().expect("live plugin status");
    assert_eq!(turn.run_state, CliRunState::Thinking);
    assert_eq!(
        turn.status_detail.as_deref(),
        Some("building execution plan")
    );
    assert!(turn.transient_until.is_some());
}
