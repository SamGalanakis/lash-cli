//! Machine-readable JSON dump of a loaded session.

use serde_json::json;

use crate::LoadedSession;

pub fn render(session: &LoadedSession) -> String {
    let meta = session.meta.as_ref().map(|meta| {
        json!({
            "session_id": meta.session_id,
            "parent_session_id": meta.parent_session_id(),
            "relation": meta.relation,
        })
    });

    let document = json!({
        "meta": meta,
        "model_id": session.model_id,
        "trace_path": session.trace_path.display().to_string(),
        "context_window_tokens": session.context_window_tokens,
        "chronological": session.chronological,
        "llm_prompts": session.llm_prompts,
    });

    serde_json::to_string_pretty(&document).unwrap_or_else(|_| document.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lash::persistence::{ChronologicalEntry, ChronologicalPayload};
    use lash_core::{Message, MessageRole};
    use std::path::PathBuf;

    #[test]
    fn json_export_uses_chronological_entries() {
        let session = LoadedSession {
            meta: None,
            chronological: vec![ChronologicalEntry {
                index: 0,
                payload: ChronologicalPayload::Message(Message {
                    id: "m1".to_string(),
                    role: MessageRole::User,
                    parts: std::sync::Arc::new(vec![lash_core::Part::text(
                        "p1".to_string(),
                        "hello".to_string(),
                        None,
                    )]),
                    origin: None,
                }),
            }],
            trace_path: PathBuf::from("session.trace.jsonl"),
            model_id: None,
            context_window_tokens: None,
            llm_prompts: Vec::new(),
        };

        let rendered = render(&session);
        assert!(rendered.contains("\"chronological\""));
        assert!(rendered.contains("\"kind\": \"message\""));
        assert!(!rendered.contains("\"messages\""));
        assert!(!rendered.contains("\"tool_calls\""));
    }
}
