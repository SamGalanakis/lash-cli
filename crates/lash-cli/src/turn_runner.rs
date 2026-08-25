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
use crate::event::{AppEvent, AppEventTx};
use crate::input_items::build_items_from_editor_input;

struct ActiveTurnTargetSink {
    stream_id: u64,
    app_tx: Option<AppEventTx>,
    last_turn_id: std::sync::Mutex<Option<String>>,
}

impl ActiveTurnTargetSink {
    fn new(stream_id: u64, app_tx: Option<AppEventTx>) -> Self {
        Self {
            stream_id,
            app_tx,
            last_turn_id: std::sync::Mutex::new(None),
        }
    }
}

#[async_trait::async_trait]
impl lash::TurnActivitySink for ActiveTurnTargetSink {
    fn is_noop(&self) -> bool {
        self.app_tx.is_none()
    }

    async fn emit(&self, _activity: lash::TurnActivity) {}

    async fn emit_for_turn(&self, turn_id: &str, _activity: lash::TurnActivity) {
        {
            let mut last_turn_id = self.last_turn_id.lock().expect("turn target lock");
            if last_turn_id.as_deref() == Some(turn_id) {
                return;
            }
            *last_turn_id = Some(turn_id.to_string());
        }
        if let Some(app_tx) = &self.app_tx {
            let _ = app_tx.send(AppEvent::ActiveTurnObserved {
                stream_id: self.stream_id,
                turn_id: turn_id.to_string(),
            });
        }
    }
}

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
    app_tx: Option<AppEventTx>,
) -> (CancellationToken, oneshot::Receiver<RuntimeRunResult>) {
    let (return_tx, return_rx) = oneshot::channel();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task_session = session.clone();
    let return_session = session;

    let task = tokio::spawn(async move {
        let target_sink = ActiveTurnTargetSink::new(stream_id, app_tx);
        tracing::debug!(stream_id, "runtime turn task spawned");
        let result = match async {
            let turn_id = format!("cli-turn:{stream_id}");
            task_session
                .turn(turn_input)
                .turn_id(turn_id)
                .cancel(task_cancel)
                .stream_to(&target_sink)
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
    app_tx: Option<AppEventTx>,
) -> (CancellationToken, oneshot::Receiver<RuntimeRunResult>) {
    let (return_tx, return_rx) = oneshot::channel();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task_session = session.clone();
    let return_session = session;

    let task = tokio::spawn(async move {
        let target_sink = ActiveTurnTargetSink::new(stream_id, app_tx);
        tracing::debug!(stream_id, "queued runtime turn task spawned");
        let result = match async {
            let drain_id = format!("cli-queue-drain:{stream_id}");
            task_session
                .queued_turn()
                .drain_id(drain_id)
                .cancel(task_cancel)
                .stream_to(&target_sink)
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use lash::TurnActivitySink as _;

    use super::*;
    use crate::app::App;
    use crate::event::{AppEvent, AppEventPump};
    use crate::interactive::restore_cancelled_input_texts;

    fn queued_cancel_test_core(provider: lash::provider::ProviderHandle) -> lash::LashCore {
        lash::LashCore::standard_builder(lash::TurnBudget::Unbounded)
            .commit_budget(lash::CommitBudget::bounded(1024 * 1024, 512))
            .queued_work_batching(lash::QueuedWorkBatchingConfig::new(1))
            .effect_host(Arc::new(
                lash::durability::InlineEffectHost::default()
                    .allow_process_lifetime_completion_keys(),
            ))
            .attachment_store(Arc::new(lash::persistence::InMemoryAttachmentStore::new()))
            .process_env_store(Arc::new(
                lash::persistence::InMemoryProcessExecutionEnvStore::new(),
            ))
            .provider(provider)
            .model(
                lash_core::ModelSpec::builder("queued-cancel-test")
                    .context_window_tokens(16_384)
                    .build()
                    .expect("test model"),
            )
            .store_factory(Arc::new(
                lash_core::facade_support::InMemorySessionStoreFactory::new(),
            ))
            .disable_queued_work_driver()
            .build(lash::persistence::LeaseOwnerIdentity::opaque(
                "lash-cli-test",
                "queued-cancel",
            ))
            .expect("test core")
    }

    #[tokio::test]
    async fn queued_drain_reports_exact_turn_target_and_restores_settled_drop() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let started_tx = Arc::new(Mutex::new(Some(started_tx)));
        let provider = lash::testing::TestProvider::builder()
            .kind("queued-cancel-test")
            .complete(move |_request| {
                let started_tx = Arc::clone(&started_tx);
                async move {
                    if let Some(tx) = started_tx.lock().expect("started lock").take() {
                        let _ = tx.send(());
                    }
                    std::future::pending::<()>().await;
                    unreachable!("provider future is cancelled with the turn")
                }
            })
            .build()
            .into_handle();
        let core = queued_cancel_test_core(provider);
        let session = core
            .session("queued-cancel-test")
            .open()
            .await
            .expect("open test session");
        session
            .enqueue(TurnInput::text("dropped queued input"))
            .id("queued-cancel-input")
            .send()
            .await
            .expect("enqueue test input");

        let mut event_pump = AppEventPump::new();
        let (process_cancel, return_rx) =
            spawn_session_queued_turn(session.clone(), 7, Some(event_pump.sender()));
        tokio::time::timeout(Duration::from_secs(2), started_rx)
            .await
            .expect("queued drain reaches provider")
            .expect("provider-start signal");
        let observed = tokio::time::timeout(Duration::from_secs(2), event_pump.recv())
            .await
            .expect("turn target event")
            .expect("event pump remains open");
        let AppEvent::ActiveTurnObserved { stream_id, turn_id } = observed.event else {
            panic!("expected active-turn target event");
        };
        assert_eq!(stream_id, 7);
        let dropped_steer = session
            .enqueue(TurnInput::text("dropped queued steer"))
            .id("queued-cancel-steer")
            .ingress(lash::persistence::TurnInputIngress::active_turn(
                &turn_id,
                lash::persistence::TurnInputCheckpointBoundary::AfterWork,
            ))
            .send()
            .await
            .expect("enqueue active steer against observed queued turn");

        let request = lash::TurnCancelRequest::new(
            lash::TurnAddress::new(session.session_id(), &turn_id),
            "queued-cancel-request",
            Some("test-operator".to_string()),
        )
        .undelivered(lash::TurnCancelDisposition::Drop);
        let receipt = core
            .turn_work_driver()
            .request_cancel(request)
            .await
            .expect("submit exact durable cancel");
        assert!(matches!(
            receipt.outcome,
            lash::TurnCancelOutcome::Requested(_)
        ));
        process_cancel.cancel();

        let done = tokio::time::timeout(Duration::from_secs(2), return_rx)
            .await
            .expect("queued drain settles")
            .expect("queued drain returns report");
        assert!(matches!(
            done.result.outcome,
            lash::TurnOutcome::Stopped(lash::TurnStop::Cancelled { .. })
        ));
        assert!(
            done.result
                .cancel_input_outcome
                .affected_inputs
                .iter()
                .any(|input| input.input_id == dropped_steer.input_id
                    && matches!(input.disposition, lash::TurnCancelDisposition::Drop))
        );

        let mut app = App::new(
            "test-model".to_string(),
            "test-session".to_string(),
            session.session_id().to_string(),
        );
        app.set_input("current draft".to_string());
        restore_cancelled_input_texts(&mut app, &done.result.cancel_input_outcome);
        assert_eq!(app.input(), "dropped queued steer\n\ncurrent draft");
    }

    #[tokio::test]
    async fn active_turn_target_sink_tracks_each_distinct_physical_turn_id() {
        let mut event_pump = AppEventPump::new();
        let sink = ActiveTurnTargetSink::new(11, Some(event_pump.sender()));
        sink.emit_for_turn(
            "physical-turn-a",
            lash::TurnActivity::independent(lash::TurnEvent::ModelRequestStarted {
                protocol_iteration: 0,
            }),
        )
        .await;
        sink.emit_for_turn(
            "physical-turn-b",
            lash::TurnActivity::independent(lash::TurnEvent::ModelRequestStarted {
                protocol_iteration: 1,
            }),
        )
        .await;

        let first = event_pump.recv().await.expect("first target event");
        assert!(matches!(
            first.event,
            AppEvent::ActiveTurnObserved { stream_id: 11, ref turn_id }
                if turn_id == "physical-turn-a"
        ));
        let second = event_pump.recv().await.expect("second target event");
        assert!(matches!(
            second.event,
            AppEvent::ActiveTurnObserved { stream_id: 11, ref turn_id }
                if turn_id == "physical-turn-b"
        ));
        assert_eq!(event_pump.lane_depths(), (0, 0, 0));
    }
}
