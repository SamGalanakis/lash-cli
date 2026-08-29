use lash::CancellationToken;
use lash::LashSession;
use lash::TurnExecutionMetrics;
use lash::TurnInput;
use lash::persistence::RuntimeSessionState;
use lash::runtime::OutputState;
use lash::turn::AssistantOutput;
use lash::turn::TurnIssue;
use lash::usage::TokenUsage;
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

    async fn emit_for_turn(&self, emitted_turn_id: &str, activity: lash::TurnActivity) {
        let (turn_id, is_turn_started) = match activity.event {
            lash::TurnEvent::TurnStarted { turn_id } => {
                debug_assert_eq!(emitted_turn_id, turn_id);
                (turn_id, true)
            }
            _ => (emitted_turn_id.to_string(), false),
        };
        {
            let mut last_turn_id = self.last_turn_id.lock().expect("turn target lock");
            if !is_turn_started {
                if let Some(announced_turn_id) = last_turn_id.as_deref() {
                    debug_assert_eq!(
                        announced_turn_id, emitted_turn_id,
                        "physical-turn activity must match the announced turn"
                    );
                } else {
                    tracing::warn!(
                        turn_id = emitted_turn_id,
                        "physical-turn activity arrived before TurnStarted; using emitted turn id"
                    );
                }
            }
            if last_turn_id.as_deref() == Some(turn_id.as_str()) {
                return;
            }
            *last_turn_id = Some(turn_id.clone());
        }
        if let Some(app_tx) = &self.app_tx {
            let _ = app_tx.send(AppEvent::ActiveTurnObserved {
                stream_id: self.stream_id,
                turn_id,
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
    use lash::persistence::QueuedWorkStore as _;

    use super::*;
    use crate::app::App;
    use crate::event::{AppEvent, AppEventPump};
    use crate::interactive::restore_cancelled_input_texts;

    fn queued_cancel_test_core(
        provider: lash::provider::ProviderHandle,
    ) -> (
        lash::LashCore,
        Arc<lash::persistence::InMemorySessionStoreFactory>,
    ) {
        let store_factory = Arc::new(lash::persistence::InMemorySessionStoreFactory::new());
        let core = lash::LashCore::standard_builder(lash::TurnBudget::Unbounded)
            .commit_budget(crate::host_policy::commit_budget())
            .queued_work_batching(crate::host_policy::queued_work_batching())
            .effect_host(Arc::new(
                lash::durability::NativeEffectHost::default()
                    .allow_process_lifetime_completion_keys(),
            ))
            .attachment_store(Arc::new(lash::persistence::InMemoryAttachmentStore::new()))
            .process_env_store(Arc::new(
                lash::persistence::InMemoryProcessExecutionEnvStore::new(),
            ))
            .provider(provider)
            .model(
                lash::ModelSpec::builder("queued-cancel-test")
                    .context_window_tokens(1_000_000)
                    .build()
                    .expect("test model"),
            )
            .store_factory(store_factory.clone())
            .without_queued_work()
            .build(lash::persistence::LeaseOwnerIdentity::opaque(
                "lash-cli-test",
                "queued-cancel",
            ))
            .expect("test core");
        (core, store_factory)
    }

    async fn observed_turn_target(event_pump: &mut AppEventPump, stream_id: u64) -> String {
        let observed = tokio::time::timeout(Duration::from_secs(2), event_pump.recv())
            .await
            .expect("turn target event")
            .expect("event pump remains open");
        let AppEvent::ActiveTurnObserved {
            stream_id: observed_stream_id,
            turn_id,
        } = observed.event
        else {
            panic!("expected active-turn target event");
        };
        assert_eq!(observed_stream_id, stream_id);
        turn_id
    }

    async fn enqueue_selected_task(
        store_factory: &lash::persistence::InMemorySessionStoreFactory,
        session_id: &str,
        source_key: &str,
        task: String,
    ) -> lash::persistence::QueuedWorkBatch {
        let store = store_factory
            .raw_store_for_testing(session_id)
            .expect("opened session retains its in-memory store");
        store
            .enqueue_queued_work(
                lash::persistence::QueuedWorkBatchDraft::new(
                    session_id,
                    lash::persistence::DeliveryPolicy::EarliestSafeBoundary,
                    vec![lash::persistence::QueuedWorkPayload::agent_frame_task(
                        lash::testing::frame_node_id(session_id, source_key),
                        task,
                        None,
                    )],
                )
                .with_source_key(source_key),
            )
            .await
            .expect("enqueue selected queued work")
    }

    #[tokio::test]
    async fn direct_turn_started_immediately_targets_exact_cancellation() {
        let provider = lash::testing::TestProvider::builder()
            .kind("direct-turn-started-cancel")
            .complete(|_request| async {
                std::future::pending::<()>().await;
                unreachable!("provider future is cancelled with the turn")
            })
            .build()
            .into_handle();
        let (core, _) = queued_cancel_test_core(provider);
        let session = core
            .session("direct-turn-started-cancel")
            .open()
            .await
            .expect("open test session");
        let mut event_pump = AppEventPump::new();
        let (_process_cancel, return_rx) = spawn_session_turn(
            session.clone(),
            TurnInput::text("wait for cancellation"),
            5,
            Some(event_pump.sender()),
        );

        let turn_id = observed_turn_target(&mut event_pump, 5).await;
        assert_eq!(turn_id, "cli-turn:5");
        let receipt = session
            .request_turn_cancel(
                &turn_id,
                "direct-turn-started-cancel-request",
                Some("lash-cli-test".to_string()),
                Some("cancel from TurnStarted target".to_string()),
            )
            .await
            .expect("request exact direct-turn cancellation");
        assert!(matches!(
            receipt.outcome,
            lash::TurnCancelOutcome::Requested(_)
        ));

        let done = tokio::time::timeout(Duration::from_secs(2), return_rx)
            .await
            .expect("direct turn settles")
            .expect("direct turn returns report");
        assert!(matches!(
            done.result.outcome,
            lash::TurnOutcome::Stopped(lash::TurnStop::Cancelled { .. })
        ));
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
        let (core, _) = queued_cancel_test_core(provider);
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
        let turn_id = observed_turn_target(&mut event_pump, 7).await;
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
        drop(process_cancel);

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
    async fn selected_batch_turn_started_immediately_targets_exact_cancellation() {
        let provider = lash::testing::TestProvider::builder()
            .kind("selected-turn-started-cancel")
            .complete(|_request| async {
                std::future::pending::<()>().await;
                unreachable!("provider future is cancelled with the turn")
            })
            .build()
            .into_handle();
        let (core, store_factory) = queued_cancel_test_core(provider);
        let session_id = "selected-turn-started-cancel";
        let session = core
            .session(session_id)
            .open()
            .await
            .expect("open test session");
        let selected = enqueue_selected_task(
            &store_factory,
            session_id,
            "selected-cancel-source",
            "selected queued input".to_string(),
        )
        .await;
        let mut event_pump = AppEventPump::new();
        let target_sink = ActiveTurnTargetSink::new(13, Some(event_pump.sender()));
        let run_session = session.clone();
        let run = tokio::spawn(async move {
            run_session
                .queued_turn()
                .batch_ids([selected.batch_id])
                .turn_id("selected-turn-started-cancel-id")
                .stream_to(&target_sink)
                .await
        });

        let turn_id = observed_turn_target(&mut event_pump, 13).await;
        assert_eq!(turn_id, "selected-turn-started-cancel-id");
        let receipt = session
            .request_turn_cancel(
                &turn_id,
                "selected-turn-started-cancel-request",
                Some("lash-cli-test".to_string()),
                Some("cancel selected drain from TurnStarted target".to_string()),
            )
            .await
            .expect("request exact selected-turn cancellation");
        assert!(matches!(
            receipt.outcome,
            lash::TurnCancelOutcome::Requested(_)
        ));

        let outcome = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .expect("selected turn settles")
            .expect("selected turn task joins")
            .expect("selected drain succeeds");
        let report = outcome.turn.expect("selected drain ran a turn");
        assert!(matches!(
            report.outcome,
            lash::TurnOutcome::Stopped(lash::TurnStop::Cancelled { .. })
        ));
    }

    #[tokio::test]
    async fn large_selected_queued_input_commits_with_cli_budget() {
        let provider = lash::testing::TestProvider::builder()
            .kind("selected-commit-budget")
            .complete(|_request| async {
                Ok(lash::provider::LlmResponse {
                    parts: vec![lash::direct::LlmOutputPart::Text {
                        text: "large selected input committed".to_string(),
                        response_meta: None,
                    }],
                    response_metadata: Default::default(),
                    ..Default::default()
                })
            })
            .build()
            .into_handle();
        let (core, store_factory) = queued_cancel_test_core(provider);
        let session_id = "selected-commit-budget";
        let session = core
            .session(session_id)
            .open()
            .await
            .expect("open test session");
        let selected = enqueue_selected_task(
            &store_factory,
            session_id,
            "large-selected-source",
            "x".repeat(96 * 1024),
        )
        .await;

        let outcome = session
            .queued_turn()
            .batch_ids([selected.batch_id])
            .turn_id("large-selected-turn")
            .run()
            .await
            .expect("large selected drain remains within CLI commit budget");
        let turn = outcome.turn.expect("large selected drain ran a turn");
        assert!(matches!(
            turn.result.outcome,
            lash::TurnOutcome::Finished(_)
        ));
        assert_eq!(
            turn.result.assistant_output.safe_text,
            "large selected input committed"
        );
    }

    #[tokio::test]
    async fn active_turn_target_sink_prefers_turn_started_and_tracks_successive_turns() {
        let mut event_pump = AppEventPump::new();
        let sink = ActiveTurnTargetSink::new(11, Some(event_pump.sender()));
        sink.emit_for_turn(
            "physical-turn-a",
            lash::TurnActivity::independent(lash::TurnEvent::TurnStarted {
                turn_id: "physical-turn-a".to_string(),
            }),
        )
        .await;
        assert_eq!(event_pump.lane_depths(), (1, 0, 0));
        sink.emit_for_turn(
            "physical-turn-a",
            lash::TurnActivity::independent(lash::TurnEvent::ModelRequestStarted {
                protocol_iteration: 0,
            }),
        )
        .await;
        assert_eq!(event_pump.lane_depths(), (1, 0, 0));
        sink.emit_for_turn(
            "physical-turn-b",
            lash::TurnActivity::independent(lash::TurnEvent::TurnStarted {
                turn_id: "physical-turn-b".to_string(),
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

    #[tokio::test]
    async fn active_turn_target_sink_falls_back_to_first_activity_turn_id() {
        let mut event_pump = AppEventPump::new();
        let sink = ActiveTurnTargetSink::new(12, Some(event_pump.sender()));

        sink.emit_for_turn(
            "physical-turn-fallback",
            lash::TurnActivity::independent(lash::TurnEvent::ModelRequestStarted {
                protocol_iteration: 0,
            }),
        )
        .await;

        let observed = event_pump.recv().await.expect("fallback target event");
        assert!(matches!(
            observed.event,
            AppEvent::ActiveTurnObserved { stream_id: 12, ref turn_id }
                if turn_id == "physical-turn-fallback"
        ));
        assert_eq!(event_pump.lane_depths(), (0, 0, 0));
    }
}
