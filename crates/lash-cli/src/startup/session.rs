//! Build and open CLI sessions through the public Lash facade.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use lash::durability::EffectHost;
use lash::persistence::{AttachmentStore, LeaseOwnerIdentity, ProcessExecutionEnvStore};
use lash::prompt::PromptLayer;
use lash::rlm::{RLM_PROTOCOL_PLUGIN_ID, RlmCreateExtras, RlmSessionExt};
use lash::runtime::SessionPolicy;
use lash::{LashCore, LashSession, PluginStack, PromptLayerSink, SessionSpec};

use crate::execution_settings::{
    ExecutionMode, RlmDialect, RlmTerminationMode, default_rlm_termination_for_mode,
};
use crate::session_log::{self, SessionLogger};

pub(crate) enum SessionBootstrapSource {
    Fresh,
    Resume(String),
}

impl SessionBootstrapSource {
    pub(crate) async fn from_resume_arg(resume: Option<String>) -> Self {
        match resume {
            Some(identifier) => Self::Resume(
                session_log::roster_session_id_for_display_name(&identifier).unwrap_or(identifier),
            ),
            None => Self::Fresh,
        }
    }
}

pub(crate) struct SessionBootstrap {
    session_id: String,
    session_name: String,
    host_config: Option<CliSessionHostConfig>,
    resumed: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CliSessionHostConfig {
    pub(crate) execution_mode: ExecutionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) rlm_dialect: Option<RlmDialect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) rlm_termination: Option<RlmTerminationMode>,
}

impl CliSessionHostConfig {
    pub(crate) fn new(
        execution_mode: ExecutionMode,
        rlm_dialect: Option<RlmDialect>,
        rlm_termination: Option<RlmTerminationMode>,
    ) -> Self {
        Self {
            execution_mode,
            rlm_dialect: execution_mode
                .is_rlm()
                .then_some(rlm_dialect.unwrap_or_default()),
            rlm_termination: if execution_mode.is_rlm() {
                rlm_termination.or_else(|| default_rlm_termination_for_mode(execution_mode))
            } else {
                None
            },
        }
    }
}

pub(crate) struct OpenedCliLashSession {
    pub(crate) bootstrap: SessionBootstrap,
    pub(crate) logger: SessionLogger,
    pub(crate) session: LashSession,
}

#[derive(Clone)]
pub(crate) struct CliSessionOpener {
    plugin_stack: PluginStack,
    prompt_layer: PromptLayer,
    attachment_store: Arc<dyn AttachmentStore>,
    provider: lash::provider::ProviderHandle,
    deferred_tool_resolver: Option<lash::tools::SharedDeferredToolResolver>,
    trace_jsonl_path: Option<PathBuf>,
    trace_level: lash::tracing::TraceLevel,
    owns_visible_queued_turns: bool,
    opened_cores: Arc<tokio::sync::Mutex<Vec<LashCore>>>,
}

impl SessionBootstrap {
    pub(crate) async fn open(source: SessionBootstrapSource) -> Result<Self> {
        std::fs::create_dir_all(session_log::sessions_dir())?;
        let (session_id, resumed) = match source {
            SessionBootstrapSource::Fresh => (uuid::Uuid::new_v4().to_string(), false),
            SessionBootstrapSource::Resume(session_id) => (session_id, true),
        };
        let roster = session_log::load_roster_entry(&session_id).ok();
        if resumed && roster.is_none() {
            let old_path = session_log::sessions_dir().join(&session_id);
            if old_path
                .extension()
                .is_some_and(|extension| extension == "db")
            {
                anyhow::bail!(session_log::incompatible_session_message(&old_path).await);
            }
        }
        let session_name = roster
            .as_ref()
            .map(|entry| entry.session_name.clone())
            .unwrap_or(crate::generate_session_name(&session_log::sessions_dir()).await);
        let host_config = session_log::load_host_config(&session_id).ok();
        Ok(Self {
            session_id,
            session_name,
            host_config,
            resumed,
        })
    }

    pub(crate) fn filename(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn run_session_id(&self) -> Option<String> {
        Some(self.session_id.clone())
    }

    pub(crate) fn persisted_host_config(&self) -> Option<CliSessionHostConfig> {
        self.host_config.clone()
    }

    pub(crate) fn save_host_config(&self, config: &CliSessionHostConfig) -> Result<()> {
        session_log::save_host_config(&self.session_id, config)
    }

    pub(crate) fn session_name(&self) -> String {
        self.session_name.clone()
    }

    pub(crate) async fn logger(
        &self,
        model: &str,
        _session_id: Option<String>,
    ) -> Result<SessionLogger> {
        let logger = SessionLogger::new(
            self.session_id.clone(),
            self.session_name.clone(),
            model,
            None,
        )?;
        Ok(logger)
    }
}

impl CliSessionOpener {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        plugin_stack: PluginStack,
        prompt_layer: PromptLayer,
        attachment_store: Arc<dyn AttachmentStore>,
        provider: lash::provider::ProviderHandle,
        deferred_tool_resolver: Option<lash::tools::SharedDeferredToolResolver>,
        trace_jsonl_path: Option<PathBuf>,
        trace_level: lash::tracing::TraceLevel,
        owns_visible_queued_turns: bool,
    ) -> Self {
        Self {
            plugin_stack,
            prompt_layer,
            attachment_store,
            provider,
            deferred_tool_resolver,
            trace_jsonl_path,
            trace_level,
            owns_visible_queued_turns,
            opened_cores: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        }
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        let cores = self.opened_cores.lock().await.clone();
        for core in cores.into_iter().rev() {
            core.flush_trace_sink()?;
            core.shutdown().await?;
        }
        Ok(())
    }

    async fn active_core(&self) -> Result<LashCore> {
        self.opened_cores
            .lock()
            .await
            .last()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no Lash core is open"))
    }

    pub(crate) async fn list_recent_sessions(
        &self,
        limit: usize,
    ) -> Result<Vec<session_log::SessionInfo>> {
        let core = self.active_core().await?;
        session_log::list_recent_sessions(&core, limit).await
    }

    pub(crate) async fn cancel_active_turn(
        &self,
        session_id: &str,
        turn_id: &str,
        process_cancel: Option<lash::CancellationToken>,
    ) -> Result<()> {
        let driver = self.active_core().await?.turn_work_driver();
        let address = lash::TurnAddress::new(session_id, turn_id);
        let request = lash::TurnCancelRequest::new(
            address,
            format!("cli-cancel:{}", uuid::Uuid::new_v4()),
            Some("interactive-user".to_string()),
        )
        .with_reason("operator requested turn cancellation")
        .undelivered(lash::TurnCancelDisposition::Drop);
        let requested = driver.request_cancel(request).await.map(|_| ());
        if let Some(token) = process_cancel {
            token.cancel();
        }
        Ok(requested?)
    }

    pub(crate) async fn fork_at(&self, node_id: &str, child_session_id: &str) -> Result<()> {
        let core = self.active_core().await?;
        core.pin(node_id).await?;
        core.fork_at(node_id, child_session_id).await?;
        Ok(())
    }

    pub(crate) async fn open_prepared(
        &self,
        mut bootstrap: SessionBootstrap,
        fallback_policy: SessionPolicy,
        host_config: CliSessionHostConfig,
    ) -> Result<OpenedCliLashSession> {
        std::fs::create_dir_all(crate::paths::store_dir())?;
        let process_registry = Arc::new(
            lash_sqlite_store::SqliteProcessRegistry::open(
                &crate::paths::processes_db(),
                crate::paths::store_dir(),
            )
            .await?,
        );
        let store_factory: Arc<dyn lash::persistence::SessionStoreFactory> = Arc::new(
            lash_sqlite_store::SqliteSessionStoreFactory::new_with_process_registry(
                crate::paths::store_dir(),
                crate::paths::processes_db(),
            ),
        );
        let effect_host: Arc<dyn EffectHost> =
            Arc::new(lash_sqlite_store::SqliteEffectHost::open(&crate::paths::effects_db()).await?);
        let trigger_store = Arc::new(
            lash_sqlite_store::SqliteTriggerStore::open(&crate::paths::triggers_db()).await?,
        );
        let process_env_store: Arc<dyn ProcessExecutionEnvStore> =
            Arc::new(lash_sqlite_store::Store::open(&crate::paths::process_env_db()).await?);
        let artifact_store =
            Arc::new(lash_sqlite_store::Store::open(&crate::paths::artifacts_db()).await?);

        let builder = match host_config.execution_mode {
            ExecutionMode::Standard => {
                LashCore::standard_builder(crate::host_policy::turn_budget())
            }
            ExecutionMode::Rlm => {
                let mut factory = lash::rlm::RlmProtocolPluginFactory::new(
                    crate::host_policy::rlm_protocol_config(),
                    artifact_store,
                );
                if let Some(resolver) = self.deferred_tool_resolver.clone() {
                    factory = factory.with_deferred_tool_resolver(resolver);
                }
                LashCore::rlm_builder(crate::host_policy::turn_budget(), factory)
            }
        };
        let mut builder = builder
            .provider(self.provider.clone())
            .model(fallback_policy.model.clone())
            .no_progress_budget(crate::host_policy::no_progress_budget())
            .store_factory(store_factory)
            .plugins(self.plugin_stack.clone())
            .prompt_layer(self.prompt_layer.clone())
            .effect_host(Arc::clone(&effect_host))
            .attachment_store(Arc::clone(&self.attachment_store))
            .process_env_store(process_env_store)
            .commit_budget(crate::host_policy::commit_budget())
            .queued_work_batching(crate::host_policy::queued_work_batching())
            .process_registry(process_registry)
            .trigger_store(trigger_store)
            .trace_level(self.trace_level);
        if self.owns_visible_queued_turns {
            builder = builder.disable_queued_work_driver();
        }
        if let Some(trace_path) = self.trace_jsonl_path.clone() {
            builder = builder.trace_jsonl_path(trace_path);
        }
        let core = builder
            .build(LeaseOwnerIdentity::opaque(
                crate::paths::host_id()?,
                uuid::Uuid::new_v4().to_string(),
            ))
            .context("build Lash core")?;
        if bootstrap.resumed {
            let requested = bootstrap.session_id.clone();
            bootstrap.session_id = session_log::resolve_session_identifier(&core, &requested)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Could not resolve session `{requested}`"))?;
            let roster = session_log::load_roster_entry(&bootstrap.session_id).ok();
            bootstrap.session_name = roster
                .as_ref()
                .map(|entry| entry.session_name.clone())
                .unwrap_or_else(|| session_log::fallback_session_label(&bootstrap.session_id));
            bootstrap.host_config = session_log::load_host_config(&bootstrap.session_id).ok();
        }

        let session_spec = SessionSpec::new()
            .provider_id(fallback_policy.provider_id.clone())
            .model(fallback_policy.model.clone())
            .turn_budget(crate::host_policy::turn_budget())
            .no_progress_budget(crate::host_policy::no_progress_budget())
            .prompt_layer(self.prompt_layer.clone());
        let mut session_builder = core
            .session(bootstrap.session_id.clone())
            .session_spec(session_spec);
        if host_config.execution_mode.is_rlm() {
            session_builder = session_builder.plugin_option(
                RLM_PROTOCOL_PLUGIN_ID,
                RlmCreateExtras {
                    dialect: host_config.rlm_dialect,
                    termination: host_config
                        .rlm_termination
                        .map(RlmTerminationMode::as_rlm_termination),
                    final_answer_format: None,
                },
            )?;
        }
        let session = session_builder.open().await.map_err(|error| {
            let requested = host_config
                .rlm_dialect
                .map(RlmDialect::language_id)
                .unwrap_or("none");
            anyhow::anyhow!("could not open session with RLM dialect `{requested}`: {error}")
        })?;
        if let Some(requested) = host_config.rlm_dialect {
            let recorded = session.rlm_config().dialect.unwrap_or_default();
            if recorded != requested {
                anyhow::bail!(
                    "RLM dialect conflict: session records `{}`, requested `{}`",
                    recorded.language_id(),
                    requested.language_id()
                );
            }
        }
        bootstrap.save_host_config(&host_config)?;
        let logger = bootstrap
            .logger(
                &fallback_policy.model.id,
                Some(bootstrap.session_id.clone()),
            )
            .await?;
        self.opened_cores.lock().await.push(core.clone());
        Ok(OpenedCliLashSession {
            bootstrap,
            logger,
            session,
        })
    }

    pub(crate) async fn fresh(
        &self,
        fallback_policy: SessionPolicy,
        execution_mode: ExecutionMode,
        rlm_dialect: Option<RlmDialect>,
    ) -> Result<OpenedCliLashSession> {
        let bootstrap = SessionBootstrap::open(SessionBootstrapSource::Fresh).await?;
        self.open_prepared(
            bootstrap,
            fallback_policy,
            CliSessionHostConfig::new(
                execution_mode,
                rlm_dialect,
                default_rlm_termination_for_mode(execution_mode),
            ),
        )
        .await
    }

    pub(crate) async fn resume(
        &self,
        identifier: &str,
        fallback_policy: SessionPolicy,
        execution_mode: ExecutionMode,
        rlm_dialect: Option<RlmDialect>,
    ) -> Result<OpenedCliLashSession> {
        let bootstrap = SessionBootstrap::open(
            SessionBootstrapSource::from_resume_arg(Some(identifier.to_string())).await,
        )
        .await?;
        let persisted = bootstrap.persisted_host_config();
        let resolved_execution_mode = persisted
            .as_ref()
            .map(|config| config.execution_mode)
            .unwrap_or(execution_mode);
        let host_config = CliSessionHostConfig::new(
            resolved_execution_mode,
            persisted
                .as_ref()
                .and_then(|config| config.rlm_dialect)
                .or(rlm_dialect),
            persisted
                .as_ref()
                .and_then(|config| config.rlm_termination)
                .or_else(|| default_rlm_termination_for_mode(resolved_execution_mode)),
        );
        self.open_prepared(bootstrap, fallback_policy, host_config)
            .await
    }
}

pub(crate) async fn refresh_tool_catalog_and_wait(
    session: &LashSession,
    reason: &str,
    idempotency_key: &str,
) -> Result<()> {
    let receipt = session
        .admin()
        .commands()
        .refresh_tool_catalog(reason, idempotency_key)
        .await?;
    let outcome = session
        .queued_turn()
        .batch_ids([receipt.batch_id.clone()])
        .drain_id(receipt.batch_id)
        .run()
        .await?;
    anyhow::ensure!(
        !outcome.executed_selected_turn(),
        "tool catalog refresh unexpectedly executed a model turn"
    );
    Ok(())
}
