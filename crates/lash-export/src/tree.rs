//! Multi-session export — load catalogued descendants reachable from a
//! root session and classify cross-session edges created by `spawn_agent`.
//!
//! Lash main keeps sessions in one durable-core catalog. Each session carries
//! its full relation in `session_meta`. For subagents,
//! `SessionRelation::Child.caused_by` anchors the child to
//! the parent `spawn_agent` call without relying on model-authored names.
//!
//! One provider trace JSONL covers the whole `lash` invocation. Each
//! record carries `context.session_id`; we partition `LlmPromptSnapshot`s
//! by that field so each session in the tree gets its matching prompts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lash::persistence::{ChronologicalEntry, SessionRelation};

use crate::trace::{LlmPromptSnapshot, load_prompts_from_trace};
use crate::{
    LoadSessionError, LoadedSessionMetadata, load_session, open_session_read_only, preflight_store,
};

/// One session in the tree, ready to render.
pub struct LoadedSessionNode {
    pub meta: LoadedSessionMetadata,
    pub chronological: Vec<ChronologicalEntry>,
    pub model_id: Option<String>,
    pub context_window_tokens: Option<u64>,
    pub llm_prompts: Vec<LlmPromptSnapshot>,
    pub db_path: PathBuf,
    pub kind: NodeRelation,
    /// Sessions that this session spawned, in persisted relation order.
    pub subagent_children: Vec<SubagentEdge>,
}

/// What kind of edge points *into* this session from its parent.
#[derive(Clone, Debug)]
pub enum NodeRelation {
    Root,
    /// Parent called `spawn_agent` and waited for this session to finish.
    Subagent {
        parent_session_id: String,
        task: Option<String>,
        capability: Option<String>,
        /// The parent's causal tool call id, when available.
        parent_call_id: Option<String>,
    },
}

/// Records one `spawn_agent` edge from a parent to a child session.
#[derive(Clone, Debug)]
pub struct SubagentEdge {
    pub child_session_id: String,
    pub task: Option<String>,
    pub capability: Option<String>,
    pub call_id: Option<String>,
}

/// The full discovered tree.
pub struct LoadedSessionTree {
    pub root_id: String,
    pub trace_path: PathBuf,
    pub nodes: Vec<LoadedSessionNode>,
}

struct CandidateLoad {
    db_path: PathBuf,
    meta: LoadedSessionMetadata,
    chronological: Vec<ChronologicalEntry>,
    model_id: Option<String>,
    context_window_tokens: Option<u64>,
}

impl LoadedSessionTree {
    pub fn root(&self) -> &LoadedSessionNode {
        self.nodes
            .iter()
            .find(|n| n.meta.session_id == self.root_id)
            .expect("root must exist in tree")
    }

    pub fn get(&self, session_id: &str) -> Option<&LoadedSessionNode> {
        self.nodes.iter().find(|n| n.meta.session_id == session_id)
    }

    pub fn parent_of(&self, session_id: &str) -> Option<&LoadedSessionNode> {
        let node = self.get(session_id)?;
        let parent_id = match &node.kind {
            NodeRelation::Root => return None,
            NodeRelation::Subagent {
                parent_session_id, ..
            } => parent_session_id.as_str(),
        };
        self.get(parent_id)
    }

    /// Ancestor chain from root → … → `session_id` inclusive. Empty if not
    /// found.
    pub fn ancestors(&self, session_id: &str) -> Vec<&LoadedSessionNode> {
        let mut chain = Vec::new();
        let mut cur = self.get(session_id);
        while let Some(node) = cur {
            chain.push(node);
            cur = self.parent_of(&node.meta.session_id);
        }
        chain.reverse();
        chain
    }
}

/// Load the root and durable catalog, retaining descendants of root.
///
/// `trace_path` may cover any subset of sessions in the tree; prompts are
/// partitioned by `context.session_id`. Sessions for which no prompts are
/// found in the trace render fine — they just don't show LLM-call rows.
pub async fn load_tree_from_paths(
    store_root: &Path,
    root_session_id: &str,
    _session_ids: &[String],
    trace_path: &Path,
) -> Result<LoadedSessionTree, LoadSessionError> {
    preflight_store(store_root).await?;
    let prompts_all = load_prompts_from_trace(trace_path).map_err(LoadSessionError::Trace)?;
    let mut prompts_by_session: HashMap<String, Vec<LlmPromptSnapshot>> = HashMap::new();
    let mut prompts_unkeyed: Vec<LlmPromptSnapshot> = Vec::new();
    for prompt in prompts_all {
        match prompt.session_id.clone() {
            Some(sid) => prompts_by_session.entry(sid).or_default().push(prompt),
            None => prompts_unkeyed.push(prompt),
        }
    }

    let mut candidates: Vec<CandidateLoad> = Vec::new();
    let factory = lash_sqlite_store::SqliteSessionStoreFactory::new(store_root);
    let summaries = lash::persistence::SessionStoreFactory::list_sessions(
        &factory,
        &lash::SessionListFilter {
            relation: None,
            deleted: Some(false),
        },
    )
    .await
    .map_err(LoadSessionError::Store)?;
    for summary in summaries {
        let session_id = summary.session_id;
        let relation = summary
            .durable_relation
            .ok_or_else(|| LoadSessionError::SessionNotFound(session_id.clone()))?;
        let read_view = open_session_read_only(store_root, &session_id).await?;
        let meta = LoadedSessionMetadata {
            session_id: session_id.clone(),
            relation,
        };
        let loaded = load_session(&read_view, &session_id, Some(meta.clone()))?;
        candidates.push(CandidateLoad {
            db_path: store_root.join("durable-core.db"),
            meta,
            chronological: loaded.chronological,
            model_id: loaded.model_id,
            context_window_tokens: loaded.context_window_tokens,
        });
    }

    let root_idx = candidates
        .iter()
        .position(|candidate| candidate.meta.session_id == root_session_id)
        .ok_or_else(|| LoadSessionError::SessionNotFound(root_session_id.to_string()))?;
    let root_id = candidates[root_idx].meta.session_id.clone();

    // Walk the parent chain to keep only sessions reachable from root.
    let by_id: HashMap<String, usize> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| (c.meta.session_id.clone(), i))
        .collect();

    let mut keep: Vec<bool> = vec![false; candidates.len()];
    keep[root_idx] = true;
    // BFS-style: a candidate stays if any ancestor is the root.
    loop {
        let mut changed = false;
        for (i, c) in candidates.iter().enumerate() {
            if keep[i] {
                continue;
            }
            let SessionRelation::Child {
                parent_session_id, ..
            } = &c.meta.relation
            else {
                continue;
            };
            let Some(&parent_idx) = by_id.get(parent_session_id) else {
                continue;
            };
            if keep[parent_idx] {
                keep[i] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Build child-of-parent index from persisted relations. The parent call
    // id survives as causal metadata, but detached invocation history is no
    // longer part of the session graph.
    let mut node_kinds: HashMap<String, NodeRelation> = HashMap::new();
    let mut subagent_edges: HashMap<String, Vec<SubagentEdge>> = HashMap::new();

    for (i, c) in candidates.iter().enumerate() {
        if !keep[i] || c.meta.session_id == root_id {
            continue;
        }
        match &c.meta.relation {
            SessionRelation::Root => {}
            SessionRelation::Child {
                parent_session_id,
                caused_by,
            } => {
                node_kinds.insert(
                    c.meta.session_id.clone(),
                    NodeRelation::Subagent {
                        parent_session_id: parent_session_id.clone(),
                        task: None,
                        capability: None,
                        parent_call_id: tool_call_id_from_cause(caused_by),
                    },
                );
            }
            SessionRelation::Fork { .. } => {}
        }
    }

    for (i, c) in candidates.iter().enumerate() {
        if !keep[i] {
            continue;
        }
        let parent_sid = c.meta.session_id.clone();
        let mut edges: Vec<SubagentEdge> = Vec::new();
        for (child_idx, child) in candidates.iter().enumerate() {
            if !keep[child_idx] {
                continue;
            }
            let SessionRelation::Child {
                parent_session_id,
                caused_by,
            } = &child.meta.relation
            else {
                continue;
            };
            if parent_session_id != &parent_sid {
                continue;
            }
            if edges
                .iter()
                .any(|edge| edge.child_session_id == child.meta.session_id)
            {
                continue;
            }
            edges.push(SubagentEdge {
                child_session_id: child.meta.session_id.clone(),
                task: None,
                capability: None,
                call_id: tool_call_id_from_cause(caused_by),
            });
        }
        subagent_edges.insert(parent_sid, edges);
    }

    // Assemble the final node list.
    let mut nodes = Vec::new();
    for (i, c) in candidates.into_iter().enumerate() {
        if !keep[i] {
            continue;
        }
        let sid = c.meta.session_id.clone();
        let kind = if sid == root_id {
            NodeRelation::Root
        } else {
            node_kinds
                .remove(&sid)
                .expect("retained non-root sessions are child relations")
        };
        let llm_prompts = prompts_by_session.remove(&sid).unwrap_or_else(|| {
            if sid == root_id {
                std::mem::take(&mut prompts_unkeyed)
            } else {
                Vec::new()
            }
        });
        nodes.push(LoadedSessionNode {
            meta: c.meta,
            chronological: c.chronological,
            model_id: c.model_id,
            context_window_tokens: c.context_window_tokens,
            llm_prompts,
            db_path: c.db_path,
            kind,
            subagent_children: subagent_edges.remove(&sid).unwrap_or_default(),
        });
    }

    Ok(LoadedSessionTree {
        root_id,
        trace_path: trace_path.to_path_buf(),
        nodes,
    })
}

fn tool_call_id_from_cause(caused_by: &Option<lash::process::CausalRef>) -> Option<String> {
    match caused_by {
        Some(lash::process::CausalRef::ToolCall { call_id, .. }) => Some(call_id.clone()),
        _ => None,
    }
}
