use std::cmp::Reverse;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use lash::messages::Message;
use lash::usage::TokenUsage;

use crate::app::{
    LiveToolOutput, PreparedTurn, UiActivityJournal, UiActivityRecord, UiTimeline, UiTimelineItem,
    timeline_from_read_view,
};

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub filename: String,
    pub session_id: String,
    pub message_count: usize,
    pub first_message: String,
    pub modified: SystemTime,
    pub cwd: Option<PathBuf>,
}

pub struct LoadedSession {
    pub session_id: String,
    pub session_name: String,
    pub filename: String,
    pub messages: Vec<Message>,
    pub blocks: UiTimeline,
    pub last_token_usage: TokenUsage,
    pub plugin_mode_indicators: std::collections::BTreeMap<String, String>,
    pub live_tool_output: LiveToolOutput,
    pub(crate) ui_activity_journal: UiActivityJournal,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct RosterEntry {
    pub(crate) session_id: String,
    pub(crate) session_name: String,
    pub(crate) model: String,
    pub(crate) created_at: String,
    pub(crate) cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent_session_id: Option<String>,
    #[serde(default)]
    pub(crate) message_count: usize,
    #[serde(default)]
    pub(crate) first_message: String,
    #[serde(default)]
    pub(crate) inputs: Vec<HostInputRecord>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct HostInputRecord {
    display_text: String,
    effective_text: String,
}

pub struct SessionLogger {
    pub session_id: String,
    filename: String,
}

impl SessionLogger {
    pub fn new(
        session_id: String,
        session_name: String,
        model: &str,
        parent_session_id: Option<String>,
    ) -> Result<Self> {
        std::fs::create_dir_all(sessions_dir())?;
        let entry = load_roster_entry(&session_id).unwrap_or_else(|_| RosterEntry {
            session_id: session_id.clone(),
            session_name,
            model: model.to_string(),
            created_at: chrono::Local::now().to_rfc3339(),
            cwd: std::env::current_dir().ok(),
            parent_session_id,
            message_count: 0,
            first_message: String::new(),
            inputs: Vec::new(),
        });
        save_roster_entry(&entry)?;
        Ok(Self {
            session_id: session_id.clone(),
            filename: session_id,
        })
    }

    pub fn db_path(&self) -> PathBuf {
        crate::paths::durable_core_db()
    }

    fn ui_sidecar_path_for(session_id: &str) -> PathBuf {
        sessions_dir().join(format!("{session_id}.ui.json"))
    }

    fn ui_activity_log_path_for(session_id: &str) -> PathBuf {
        sessions_dir().join(format!("{session_id}.ui-activity.jsonl"))
    }

    pub fn record_host_input(&self, turn: &PreparedTurn) -> Result<()> {
        let mut roster = load_roster_entry(&self.session_id)?;
        roster.message_count = roster.message_count.saturating_add(1);
        if roster.first_message.is_empty() {
            roster.first_message = turn.display_text.trim().to_string();
        }
        if !turn.display_text.trim().is_empty() && turn.display_text != turn.effective_text {
            roster.inputs.push(HostInputRecord {
                display_text: turn.display_text.clone(),
                effective_text: turn.effective_text.clone(),
            });
        }
        save_roster_entry(&roster)
    }

    pub(crate) fn append_ui_activity_records(&self, records: &[UiActivityRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(Self::ui_activity_log_path_for(&self.filename))?;
        for record in records {
            serde_json::to_writer(&mut file, record)?;
            file.write_all(b"\n")?;
        }
        Ok(())
    }
}

pub(crate) fn save_roster_entry(entry: &RosterEntry) -> Result<()> {
    std::fs::create_dir_all(sessions_dir())?;
    std::fs::write(
        SessionLogger::ui_sidecar_path_for(&entry.session_id),
        serde_json::to_vec_pretty(entry)?,
    )?;
    Ok(())
}

pub(crate) fn load_roster_entry(session_id: &str) -> Result<RosterEntry> {
    let path = SessionLogger::ui_sidecar_path_for(session_id);
    let bytes = std::fs::read(&path)
        .with_context(|| format!("could not read session roster entry {}", path.display()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) fn save_host_config(
    session_id: &str,
    config: &crate::startup::session::CliSessionHostConfig,
) -> Result<()> {
    std::fs::create_dir_all(sessions_dir())?;
    std::fs::write(
        sessions_dir().join(format!("{session_id}.host.json")),
        serde_json::to_vec_pretty(config)?,
    )?;
    Ok(())
}

pub(crate) fn load_host_config(
    session_id: &str,
) -> Result<crate::startup::session::CliSessionHostConfig> {
    let bytes = std::fs::read(sessions_dir().join(format!("{session_id}.host.json")))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn load_ui_activity_journal(session_id: &str) -> Result<UiActivityJournal> {
    let path = SessionLogger::ui_activity_log_path_for(session_id);
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UiActivityJournal::default());
        }
        Err(error) => return Err(error.into()),
    };
    let mut journal = UiActivityJournal::default();
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if !line.trim().is_empty() {
            journal.apply_record(serde_json::from_str(&line)?);
        }
    }
    Ok(journal)
}

fn apply_host_inputs(timeline: &mut UiTimeline, roster: &RosterEntry) {
    for item in timeline.iter_mut() {
        let UiTimelineItem::UserInput(text) = item else {
            continue;
        };
        if let Some(record) = roster
            .inputs
            .iter()
            .find(|record| record.effective_text.trim() == text.trim())
        {
            *text = record.display_text.trim().to_string();
        }
    }
}

pub(crate) fn load_opened_session(session: &lash::LashSession) -> Result<LoadedSession> {
    let session_id = session.session_id();
    let roster = load_roster_entry(&session_id).ok();
    let read_view = session.read_view();
    let activity_journal = load_ui_activity_journal(&session_id)?;
    let ui_state = crate::app::UiProjectionState {
        activity_journal,
        ..crate::app::UiProjectionState::default()
    };
    let mut blocks = timeline_from_read_view(&read_view, &ui_state);
    if let Some(roster) = roster.as_ref() {
        apply_host_inputs(&mut blocks, roster);
    }
    Ok(LoadedSession {
        session_id: session_id.clone(),
        session_name: roster
            .map(|entry| entry.session_name)
            .unwrap_or_else(|| fallback_session_label(&session_id)),
        filename: session_id,
        messages: read_view.messages().to_vec(),
        blocks,
        last_token_usage: TokenUsage::default(),
        plugin_mode_indicators: ui_state.plugin_mode_indicators.clone(),
        live_tool_output: ui_state.live_tool_output.clone(),
        ui_activity_journal: ui_state.activity_journal,
    })
}

impl SessionInfo {
    pub fn relative_time(&self) -> String {
        let secs = self.modified.elapsed().unwrap_or_default().as_secs();
        match secs {
            0..60 => "just now".to_string(),
            60..3600 => format!("{}m ago", secs / 60),
            3600..86400 => format!("{}h ago", secs / 3600),
            86400..604800 => format!("{}d ago", secs / 86400),
            _ => format!("{}w ago", secs / 604800),
        }
    }

    pub fn cwd_label(&self) -> Option<String> {
        let cwd = self.cwd.as_deref()?;
        cwd.file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(|name| format!("/{name}"))
            .or_else(|| (cwd == Path::new("/")).then(|| "/".to_string()))
    }
}

pub fn sessions_dir() -> PathBuf {
    crate::paths::sessions_dir()
}

pub(crate) fn fallback_session_label(session_id: &str) -> String {
    let short = session_id.get(..8).unwrap_or(session_id);
    format!("session-{short}")
}

pub(crate) fn roster_session_id_for_display_name(identifier: &str) -> Option<String> {
    if load_roster_entry(identifier).is_ok() {
        return Some(identifier.to_string());
    }
    std::fs::read_dir(sessions_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let session_id = path.file_name()?.to_str()?.strip_suffix(".ui.json")?;
            let roster = load_roster_entry(session_id).ok()?;
            (roster.session_name == identifier).then_some(roster.session_id)
        })
        .next()
}

pub(crate) async fn resolve_session_identifier(
    core: &lash::LashCore,
    identifier: &str,
) -> Result<Option<String>> {
    let sessions = core
        .sessions_filtered(lash::SessionListFilter {
            relation: None,
            deleted: Some(false),
        })
        .await?;
    if sessions
        .iter()
        .any(|summary| summary.session_id == identifier)
    {
        return Ok(Some(identifier.to_string()));
    }
    Ok(sessions.into_iter().find_map(|summary| {
        load_roster_entry(&summary.session_id)
            .ok()
            .filter(|roster| roster.session_name == identifier)
            .map(|_| summary.session_id)
    }))
}

pub(crate) async fn list_recent_sessions(
    core: &lash::LashCore,
    limit: usize,
) -> Result<Vec<SessionInfo>> {
    let summaries = core
        .sessions_filtered(lash::SessionListFilter {
            relation: Some(lash::SessionRelationKind::Root),
            deleted: Some(false),
        })
        .await?;
    let mut sessions = summaries
        .into_iter()
        .map(|summary| {
            let roster = load_roster_entry(&summary.session_id).ok();
            let modified_ms = summary.last_commit_at_ms.unwrap_or(summary.created_at_ms);
            SessionInfo {
                filename: summary.session_id.clone(),
                session_id: summary.session_id,
                message_count: roster.as_ref().map_or(0, |entry| entry.message_count),
                first_message: roster
                    .as_ref()
                    .map(|entry| entry.first_message.clone())
                    .filter(|message| !message.is_empty())
                    .unwrap_or_else(|| "No messages yet".to_string()),
                modified: SystemTime::UNIX_EPOCH + Duration::from_millis(modified_ms),
                cwd: roster.and_then(|entry| entry.cwd),
            }
        })
        .collect::<Vec<_>>();
    sessions.sort_by_key(|entry| Reverse(entry.modified));
    sessions.truncate(limit);
    Ok(sessions)
}

pub(crate) fn incompatible_session_count() -> usize {
    std::fs::read_dir(sessions_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "db")
        })
        .count()
}

pub(crate) async fn incompatible_session_message(path: &Path) -> String {
    let probe = lash_sqlite_store::SqliteStorePreflight::for_durable_core(path);
    match lash::preflight::probe_store(&probe, lash::preflight::PreflightOptions::summary()).await {
        Ok(report) => {
            let stamps = report.refusal_message().unwrap_or_else(|| {
                format!("{report}")
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
                    .join("; ")
            });
            format!(
                "Session {} is from an older Lash and is not openable: {stamps}",
                path.display()
            )
        }
        Err(error) => format!(
            "Session {} is from an older Lash and is not openable: {error}",
            path.display()
        ),
    }
}
