use lash::LashSession;
use lash::messages::{Message, MessageRole};
use lash::persistence::SessionNodeProjection;
use std::collections::{HashMap, HashSet};

use crate::app::{App, timeline_from_read_view};
use crate::overlay::TreeSelection;
use crate::session_log::SessionLogger;

#[derive(Clone, Debug)]
pub struct SessionMessageTreeNode {
    pub node_id: String,
    pub parent_message_node_id: Option<String>,
    pub message: Message,
    pub timestamp: String,
    pub children: Vec<SessionMessageTreeNode>,
    pub active: bool,
}

pub async fn current_message_tree(session: &LashSession) -> Vec<SessionMessageTreeNode> {
    let read_view = session.read_view();
    let graph = read_view.session_graph();
    let by_id = graph
        .nodes
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect::<HashMap<_, _>>();
    let mut active = HashSet::new();
    let mut cursor = graph.leaf_node_id.as_deref();
    while let Some(node_id) = cursor {
        active.insert(node_id.to_string());
        cursor = by_id
            .get(node_id)
            .and_then(|node| node.parent_node_id.as_deref());
    }
    let mut flat = Vec::new();
    for node in &graph.nodes {
        let Some(message) = node.message() else {
            continue;
        };
        let mut parent = node.parent_node_id.as_deref();
        let parent_message_node_id = loop {
            let Some(parent_id) = parent else { break None };
            let Some(parent_node) = by_id.get(parent_id) else {
                break None;
            };
            if parent_node.message().is_some() {
                break Some(parent_id.to_string());
            }
            parent = parent_node.parent_node_id.as_deref();
        };
        flat.push(SessionMessageTreeNode {
            node_id: node.node_id.clone(),
            parent_message_node_id,
            message,
            timestamp: node.timestamp.clone(),
            children: Vec::new(),
            active: active.contains(&node.node_id),
        });
    }
    build_tree(flat)
}

fn build_tree(nodes: Vec<SessionMessageTreeNode>) -> Vec<SessionMessageTreeNode> {
    let mut children_by_parent = HashMap::<Option<String>, Vec<SessionMessageTreeNode>>::new();
    for node in nodes {
        children_by_parent
            .entry(node.parent_message_node_id.clone())
            .or_default()
            .push(node);
    }
    fn children(
        parent: Option<String>,
        all: &mut HashMap<Option<String>, Vec<SessionMessageTreeNode>>,
    ) -> Vec<SessionMessageTreeNode> {
        let mut nodes = all.remove(&parent).unwrap_or_default();
        nodes.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));
        for node in &mut nodes {
            node.children = children(Some(node.node_id.clone()), all);
        }
        nodes
    }
    children(None, &mut children_by_parent)
}

#[allow(clippy::too_many_arguments)]
pub async fn switch_to_tree_selection(
    session: &LashSession,
    _logger: &SessionLogger,
    app: &mut App,
    history: &mut Vec<Message>,
    selection: TreeSelection,
) -> Result<(), String> {
    let target_leaf = if matches!(selection.message.role, MessageRole::User) {
        selection.parent_node_id.clone()
    } else {
        Some(selection.node_id.clone())
    };
    let seeded_input = if matches!(selection.message.role, MessageRole::User) {
        crate::overlay::tree_message_preview(&selection.message)
    } else {
        String::new()
    };

    // Fast path: if the target leaf already matches the runtime's
    // current leaf AND we have nothing to seed into the editor, the
    // branch is a no-op. Skip the full `branch_to_node` rebuild —
    // `from_state` walks the plugin host and re-projects the
    // transcript, which is expensive to pay for a visible no-op.
    let current_leaf = session.read_view().session_graph().leaf_node_id.clone();
    if current_leaf == target_leaf && seeded_input.is_empty() {
        return Ok(());
    }

    if current_leaf != target_leaf {
        return Err(
            "Lash does not expose a facade operation for changing the active history branch"
                .to_string(),
        );
    }
    let read_view = session.read_view();
    *history = read_view.messages().to_vec();
    app.timeline = timeline_from_read_view(&read_view, &app.ui_projection_state());
    app.set_input(seeded_input);
    app.update_suggestions();
    app.invalidate_height_cache();
    app.scroll_to_bottom();

    Ok(())
}
