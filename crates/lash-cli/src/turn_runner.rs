use lash::CancellationToken;
use lash::LashSession;
use lash::TurnExecutionMetrics;
use lash::TurnInput;
use lash::persistence::RuntimeSessionState;
use lash::turn::AssistantOutput;
use lash::turn::TurnIssue;
use lash::usage::TokenUsage;
use lash_core::facade_support::OutputState;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::app::PreparedTurn;
use crate::input_items::build_items_from_editor_input;

/// Returned by the spawned session task after the app-owned session has been updated in place.
pub(crate) struct RuntimeRunResult {
    pub(crate) stream_id: u64,
    pub(crate) result: lash::TurnReport,
}

pub(crate) fn make_turn_input(turn: &PreparedTurn) -> TurnInput {
    TurnInput::items(build_items_from_editor_input(
        &turn.effective_text,
        turn.images.clone(),
    ))
}

pub(crate) fn spawn_session_turn(
    session: LashSession,
    turn_input: TurnInput,
    stream_id: u64,
) -> (CancellationToken, oneshot::Receiver<RuntimeRunResult>) {
    let (return_tx, return_rx) = oneshot::channel();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task_session = session.clone();
    let return_session = session;

    let task = tokio::spawn(async move {
        tracing::debug!(stream_id, "runtime turn task spawned");
        let result = match async {
            let turn_id = format!("cli-turn:{stream_id}");
            task_session
                .turn(turn_input)
                .turn_id(turn_id)
                .cancel(task_cancel)
                .stream_to(&lash::runtime::NoopTurnActivitySink)
                .await
        }
        .await
        {
            Ok(turn) => turn,
            Err(err) => runtime_error_turn_result(&task_session, err.to_string()).await,
        };
        tracing::debug!(stream_id, outcome = ?result.outcome, "runtime turn task completed");
        result
    });
    tokio::spawn(return_turn_result(
        "runtime turn",
        stream_id,
        return_session,
        task,
        return_tx,
    ));

    (cancel, return_rx)
}

pub(crate) fn spawn_session_queued_turn(
    session: LashSession,
    stream_id: u64,
) -> (CancellationToken, oneshot::Receiver<RuntimeRunResult>) {
    let (return_tx, return_rx) = oneshot::channel();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task_session = session.clone();
    let return_session = session;

    let task = tokio::spawn(async move {
        tracing::debug!(stream_id, "queued runtime turn task spawned");
        let result = match async {
            let drain_id = format!("cli-queue-drain:{stream_id}");
            task_session
                .queued_turn()
                .drain_id(drain_id)
                .cancel(task_cancel)
                .stream_to(&lash::runtime::NoopTurnActivitySink)
                .await
        }
        .await
        {
            Ok(lash::QueuedTurnDrain::Ran(turn)) => turn,
            Ok(lash::QueuedTurnDrain::Empty(reason)) => {
                runtime_error_turn_result(
                    &task_session,
                    format!("no durable queued work was ready: {}", reason.as_str()),
                )
                .await
            }
            Err(err) => runtime_error_turn_result(&task_session, err.to_string()).await,
        };
        tracing::debug!(stream_id, outcome = ?result.outcome, "queued runtime turn task completed");
        result
    });
    tokio::spawn(return_turn_result(
        "queued runtime turn",
        stream_id,
        return_session,
        task,
        return_tx,
    ));

    (cancel, return_rx)
}

async fn return_turn_result(
    task_label: &'static str,
    stream_id: u64,
    session: LashSession,
    task: JoinHandle<lash::TurnReport>,
    return_tx: oneshot::Sender<RuntimeRunResult>,
) {
    let result = match task.await {
        Ok(result) => result,
        Err(err) => {
            let failure = if err.is_panic() {
                "panicked"
            } else if err.is_cancelled() {
                "was cancelled"
            } else {
                "failed"
            };
            tracing::error!(
                stream_id,
                task = task_label,
                error = %err,
                "runtime task join failed"
            );
            runtime_error_turn_result(&session, format!("{task_label} {failure}: {err}")).await
        }
    };
    let _ = return_tx.send(RuntimeRunResult { stream_id, result });
}

async fn runtime_error_turn_result(session: &LashSession, message: String) -> lash::TurnReport {
    let state = session
        .admin()
        .state()
        .persist_current()
        .await
        .unwrap_or_else(|_| RuntimeSessionState::new(session.policy_snapshot()));
    let state = state.to_snapshot();
    lash::TurnReport {
        execution: TurnExecutionMetrics::default(),
        state,
        outcome: lash::TurnOutcome::Stopped(lash::TurnStop::RuntimeError),
        assistant_output: AssistantOutput {
            safe_text: String::new(),
            raw_text: String::new(),
            state: OutputState::EmptyOutput,
        },
        usage: TokenUsage::default(),
        children_usage: Vec::new(),
        llm_calls: Vec::new(),
        tool_calls: Vec::new(),
        errors: vec![TurnIssue {
            kind: "runtime".to_string(),
            code: Some(message.clone()),
            terminal_reason: None,
            message,
            raw: None,
            retryable: None,
            provider_failure_kind: None,
        }],
        acceptance: None,
        cancel_input_outcome: Default::default(),
    }
}
