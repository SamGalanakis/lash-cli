//! Render persisted lash sessions into human-viewable formats.
//!
//! This crate is independent of `lash-cli`. It reads a session's Sqlite
//! store, projects the `SessionGraph` into committed messages and protocol
//! events, and writes a self-contained HTML (or JSON) document.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use lash::TurnBudget;
use lash::persistence::{
    ChronologicalEntry, RuntimePersistence, SessionMeta, SessionRelation,
    SessionStoreCreateRequest, SessionStoreFactory,
};
use lash::preflight::{PreflightOptions, PreflightOutcome, probe_store};
use lash::runtime::{SessionPolicy, SessionSnapshot};
use lash_sqlite_store::{SqliteSessionStoreFactory, SqliteStorePreflight};

pub mod html;
pub mod json;
pub mod markdown;
pub mod trace;
pub mod transcript;
pub mod tree;

pub use trace::LlmPromptSnapshot;
pub use tree::{
    LoadedSessionNode, LoadedSessionTree, NodeRelation, SubagentEdge, load_tree_from_paths,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Html,
    Json,
}

impl ExportFormat {
    pub fn parse(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "html" => Ok(Self::Html),
            "json" => Ok(Self::Json),
            other => Err(anyhow!(
                "unknown export format `{other}` (expected html|json)"
            )),
        }
    }
}

/// A loaded session ready to be rendered.
pub struct LoadedSession {
    pub meta: Option<SessionMeta>,
    pub chronological: Vec<ChronologicalEntry>,
    pub trace_path: PathBuf,
    /// Derived from the current persisted session-head policy.
    pub model_id: Option<String>,
    pub context_window_tokens: Option<u64>,
    /// One snapshot per `llm_call_started` event found in the required
    /// provider trace, in trace order.
    pub llm_prompts: Vec<LlmPromptSnapshot>,
}

/// One SQLite schema stamp that refuses the current Lash build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaStampMismatch {
    pub database: String,
    pub found: i64,
    pub expected: i64,
}

/// A typed session-load failure.
#[derive(Debug)]
pub enum LoadSessionError {
    IncompatibleStore {
        store_root: PathBuf,
        stamps: Vec<SchemaStampMismatch>,
        message: String,
    },
    PreflightUndecided {
        store_root: PathBuf,
        message: String,
    },
    Preflight(lash::persistence::StoreError),
    OpenStore(String),
    SessionNotFound(String),
    Store(lash::persistence::StoreError),
    Trace(anyhow::Error),
}

impl fmt::Display for LoadSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompatibleStore { message, .. } => f.write_str(message),
            Self::PreflightUndecided { message, .. } => f.write_str(message),
            Self::Preflight(error) => write!(f, "preflighting session store: {error}"),
            Self::OpenStore(error) => write!(f, "opening existing session store: {error}"),
            Self::SessionNotFound(session_id) => {
                write!(f, "session `{session_id}` was not found in the store")
            }
            Self::Store(error) => write!(f, "reading session store: {error}"),
            Self::Trace(error) => write!(f, "reading provider trace: {error:#}"),
        }
    }
}

impl Error for LoadSessionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Preflight(error) | Self::Store(error) => Some(error),
            Self::Trace(error) => Some(error.as_ref()),
            Self::IncompatibleStore { .. }
            | Self::PreflightUndecided { .. }
            | Self::OpenStore(_)
            | Self::SessionNotFound(_) => None,
        }
    }
}

/// Load a session from a Lash main SQLite store root and full provider trace.
pub async fn load_session_from_paths(
    store_root: &Path,
    session_id: &str,
    trace_path: &Path,
) -> std::result::Result<LoadedSession, LoadSessionError> {
    preflight_store(store_root).await?;
    let store = open_session(store_root, session_id).await?;
    let mut loaded = load_session(&*store, session_id).await?;
    loaded.trace_path = trace_path.to_path_buf();
    loaded.llm_prompts =
        trace::load_prompts_from_trace(trace_path).map_err(LoadSessionError::Trace)?;
    Ok(loaded)
}

pub(crate) async fn preflight_store(
    store_root: &Path,
) -> std::result::Result<(), LoadSessionError> {
    let preflight = SqliteStorePreflight::for_session_store_root(store_root);
    let report = probe_store(&preflight, PreflightOptions::summary())
        .await
        .map_err(LoadSessionError::Preflight)?;
    match report.outcome {
        PreflightOutcome::Ready => return Ok(()),
        PreflightOutcome::Undecided => {
            return Err(LoadSessionError::PreflightUndecided {
                store_root: store_root.to_path_buf(),
                message: report.to_string(),
            });
        }
        PreflightOutcome::Refused => {}
        _ => {
            return Err(LoadSessionError::PreflightUndecided {
                store_root: store_root.to_path_buf(),
                message: report.to_string(),
            });
        }
    }
    let stamps = report
        .schema
        .databases
        .iter()
        .filter(|database| database.verdict == "mismatch")
        .filter_map(|database| {
            database.found.map(|found| SchemaStampMismatch {
                database: database.name.to_string(),
                found,
                expected: database.expected,
            })
        })
        .collect();
    let message = report
        .refusal_message()
        .unwrap_or_else(|| report.to_string());
    Err(LoadSessionError::IncompatibleStore {
        store_root: store_root.to_path_buf(),
        stamps,
        message,
    })
}

pub(crate) async fn open_session(
    store_root: &Path,
    session_id: &str,
) -> std::result::Result<std::sync::Arc<dyn RuntimePersistence>, LoadSessionError> {
    let factory = SqliteSessionStoreFactory::new(store_root);
    let request = SessionStoreCreateRequest {
        session_id: session_id.to_string(),
        relation: SessionRelation::Root,
        policy: SessionPolicy::new(TurnBudget::Unbounded),
    };
    factory
        .open_existing_store(&request)
        .await
        .map_err(LoadSessionError::OpenStore)?
        .ok_or_else(|| LoadSessionError::SessionNotFound(session_id.to_string()))
}

pub(crate) async fn load_session(
    store: &dyn RuntimePersistence,
    session_id: &str,
) -> std::result::Result<LoadedSession, LoadSessionError> {
    let meta = store
        .load_session_meta()
        .await
        .map_err(LoadSessionError::Store)?;
    let persisted = store
        .load_session()
        .await
        .map_err(LoadSessionError::Store)?
        .ok_or_else(|| LoadSessionError::SessionNotFound(session_id.to_string()))?;
    let (model_id, context_window_tokens) = {
        let model = &persisted.config.model;
        (
            Some(model.id.clone()),
            Some(model.context_window_tokens() as u64),
        )
    };
    let mut state = SessionSnapshot::new(SessionPolicy::new(TurnBudget::Unbounded));
    state.session_id = session_id.to_string();
    state.current_frame_node_id = persisted.current_frame_node_id;
    state.session_graph = persisted.graph;
    let chronological = state.read_view().chronological_projection().into_entries();
    Ok(LoadedSession {
        meta,
        chronological,
        trace_path: PathBuf::new(),
        model_id,
        context_window_tokens,
        llm_prompts: Vec::new(),
    })
}

/// Render a loaded session to a string in the requested format.
pub fn render(session: &LoadedSession, format: ExportFormat) -> String {
    match format {
        ExportFormat::Html => html::render(session),
        ExportFormat::Json => json::render(session),
    }
}

/// Render a multi-session tree. Currently html-only; json falls back to
/// rendering the root session alone.
pub fn render_tree(tree: &LoadedSessionTree, format: ExportFormat) -> String {
    match format {
        ExportFormat::Html => html::render_tree(tree),
        ExportFormat::Json => json::render(&loaded_tree_root(tree)),
    }
}

fn loaded_tree_root(tree: &LoadedSessionTree) -> LoadedSession {
    LoadedSession {
        meta: Some(tree.root().meta.clone()),
        chronological: tree.root().chronological.clone(),
        trace_path: tree.trace_path.clone(),
        model_id: tree.root().model_id.clone(),
        context_window_tokens: tree.root().context_window_tokens,
        llm_prompts: tree.root().llm_prompts.clone(),
    }
}

/// End-to-end: load a session from the store root plus its full provider trace and write the
/// rendered output to disk. If `out` is `None`, returns the rendered string
/// instead of writing it.
///
/// For tree HTML, `session_ids` is the host-owned session roster to consider.
/// Descendants reachable from `root_session_id` render as a tree of views with
/// breadcrumb navigation; a root-only roster renders as a single session.
pub async fn export(
    store_root: &Path,
    root_session_id: &str,
    session_ids: &[String],
    trace_path: &Path,
    format: ExportFormat,
    out: Option<&Path>,
) -> Result<String> {
    let rendered = match format {
        ExportFormat::Html => {
            let tree =
                load_tree_from_paths(store_root, root_session_id, session_ids, trace_path).await?;
            if tree.nodes.len() > 1 {
                render_tree(&tree, format)
            } else {
                render(&loaded_tree_root(&tree), format)
            }
        }
        ExportFormat::Json => {
            let session = load_session_from_paths(store_root, root_session_id, trace_path).await?;
            render(&session, format)
        }
    };
    if let Some(path) = out {
        fs::write(path, &rendered)
            .with_context(|| format!("writing export to {}", path.display()))?;
    }
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lash::direct::LlmOutputPart;
    use lash::provider::LlmResponse;
    use lash::{LashCore, ModelSpec, TurnInput};

    use super::*;

    fn runtime_builder(store_root: &Path, trace_path: &Path) -> lash::LashCoreBuilder {
        let provider = lash::testing::TestProvider::builder()
            .kind("lash-export-fixture")
            .complete(|_request| async move {
                let text = "runtime fixture answer".to_string();
                Ok(LlmResponse {
                    full_text: text.clone(),
                    parts: vec![LlmOutputPart::Text {
                        text,
                        response_meta: None,
                    }],
                    response_metadata: Default::default(),
                    ..Default::default()
                })
            })
            .build()
            .into_handle();
        LashCore::standard_builder(TurnBudget::Unbounded)
            .provider(provider)
            .model(
                ModelSpec::builder("fixture-model")
                    .context_window_tokens(4_096)
                    .build()
                    .expect("valid fixture model"),
            )
            .store_factory(Arc::new(SqliteSessionStoreFactory::new(store_root)))
            .effect_host(Arc::new(lash::durability::InlineEffectHost::default()))
            .attachment_store(Arc::new(lash::persistence::InMemoryAttachmentStore::new()))
            .process_env_store(Arc::new(
                lash::persistence::InMemoryProcessExecutionEnvStore::new(),
            ))
            .commit_budget(lash::CommitBudget::bounded(1024 * 1024, 512))
            .queued_work_batching(lash::QueuedWorkBatchingConfig::new(1))
            .trace_jsonl_path(trace_path)
    }

    #[tokio::test]
    async fn exports_current_runtime_sessions_in_all_formats() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store_root = temp.path().join("sessions");
        let trace_path = temp.path().join("trace.jsonl");
        let core = runtime_builder(&store_root, &trace_path)
            .build(lash::persistence::LeaseOwnerIdentity::opaque(
                "lash-export-test",
                "lash-export-test:boot",
            ))
            .expect("build current Lash runtime");

        let root = core
            .session("root-session")
            .open()
            .await
            .expect("open root");
        root.turn(TurnInput::text("root prompt"))
            .run()
            .await
            .expect("run root turn");
        let child = core
            .session("child-session")
            .parent("root-session")
            .open()
            .await
            .expect("open child");
        child
            .turn(TurnInput::text("child prompt"))
            .run()
            .await
            .expect("run child turn");
        core.flush_trace_sink().expect("flush trace");

        let json = export(
            &store_root,
            "root-session",
            &[],
            &trace_path,
            ExportFormat::Json,
            None,
        )
        .await
        .expect("export current JSON");
        assert!(json.contains("root prompt"));
        assert!(json.contains("runtime fixture answer"));
        assert!(json.contains("fixture-model"));

        let html = export(
            &store_root,
            "root-session",
            &[],
            &trace_path,
            ExportFormat::Html,
            None,
        )
        .await
        .expect("export current single-session HTML");
        assert!(html.contains("root prompt"));
        assert!(html.contains("runtime fixture answer"));
        assert!(html.contains("fixture-model"));

        let tree = export(
            &store_root,
            "root-session",
            &["child-session".to_string()],
            &trace_path,
            ExportFormat::Html,
            None,
        )
        .await
        .expect("export current tree HTML");
        assert!(tree.contains("root prompt"));
        assert!(tree.contains("child prompt"));
        assert!(tree.contains("runtime fixture answer"));
    }

    #[tokio::test]
    async fn old_schema_stamp_returns_typed_refusal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store_root = temp.path().join("sessions");
        std::fs::create_dir_all(&store_root).expect("create store root");
        let database_path = store_root.join("durable-core.db");
        let connection = rusqlite::Connection::open(&database_path).expect("open empty database");
        connection
            .pragma_update(None, "user_version", 11_i64)
            .expect("stamp old schema");
        drop(connection);

        let error = match load_session_from_paths(
            &store_root,
            "old-session",
            &temp.path().join("unused.trace.jsonl"),
        )
        .await
        {
            Ok(_) => panic!("old schema must be refused before session or trace loading"),
            Err(error) => error,
        };
        match error {
            LoadSessionError::IncompatibleStore {
                store_root: refused_root,
                stamps,
                message,
            } => {
                assert_eq!(refused_root, store_root);
                assert_eq!(stamps.len(), 1);
                assert_eq!(stamps[0].database, "durable core");
                assert_eq!(stamps[0].found, 11);
                assert_eq!(stamps[0].expected, 38);
                assert!(message.contains("11"));
                assert!(message.contains("38"));
            }
            other => panic!("expected typed schema refusal, got {other}"),
        }
    }
}
