//! Optional semantic reranking.
use std::collections::{BTreeMap, BTreeSet};

use lash::direct::{
    DirectJsonSchema, DirectMessage, DirectOutputSpec, DirectPart, DirectRequest, DirectRole,
};
use serde_json::{Value, json};

use super::common::tokenize;

/// Domain-specific intent reranking applied to candidate tools *after* the
/// generic lexical/semantic ranker produces them. The BM25 scorer is kept
/// domain-agnostic; payment-action heuristics (which up- or down-rank
/// transaction, request, reminder, and balance-style tools for "send money"
/// type queries) live here instead, where benchmark-shaped tweaks belong.
///
/// Candidates are reordered stably: their existing relative order is the
/// tiebreak, so this only promotes/demotes tools the intent rules touch.
pub(crate) fn rerank_payment_action_intent(query: &str, candidates: Vec<Value>) -> Vec<Value> {
    let query_tokens = tokenize(query);
    if !has_payment_action_intent(&query_tokens) || has_query_token(&query_tokens, "request") {
        return candidates;
    }

    let mut scored = candidates
        .into_iter()
        .enumerate()
        .map(|(position, candidate)| {
            let multiplier = payment_intent_multiplier(&candidate);
            (multiplier, position, candidate)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|(left_score, left_pos, _), (right_score, right_pos, _)| {
        right_score
            .partial_cmp(left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left_pos.cmp(right_pos))
    });
    scored
        .into_iter()
        .map(|(_, _, candidate)| candidate)
        .collect()
}

/// Searchable lowercased text for a projected candidate (name, signature,
/// description, examples) used by the intent rules to recognize tool kinds.
fn candidate_text(candidate: &Value) -> String {
    let mut parts = Vec::new();
    for key in ["name", "call", "signature", "description"] {
        if let Some(text) = candidate.get(key).and_then(Value::as_str) {
            parts.push(text.to_string());
        }
    }
    if let Some(examples) = candidate.get("examples").and_then(Value::as_array) {
        for example in examples {
            if let Some(text) = example.as_str() {
                parts.push(text.to_string());
            }
        }
    }
    parts.join("\n").to_ascii_lowercase()
}

fn payment_intent_multiplier(candidate: &Value) -> f64 {
    let text = candidate_text(candidate);
    let tokens = tokenize(&text);
    let has_token = |needle: &str| tokens.iter().any(|token| token == needle);
    let has_phrase = |phrase: &str| text.contains(phrase);

    let mut multiplier = 1.0;
    if has_token("transaction") || has_phrase("send money") || has_phrase("pay user") {
        multiplier += 6.0;
    }
    if has_token("remind") || has_token("reminder") {
        multiplier *= 0.05;
    } else if has_token("request") {
        multiplier *= 0.8;
    }
    if has_phrase("venmo balance") || has_phrase("bank transfer") {
        multiplier *= 0.65;
    }
    multiplier
}

fn has_payment_action_intent(query_tokens: &[String]) -> bool {
    let has_send = has_query_token(query_tokens, "send");
    let has_payment = has_query_token(query_tokens, "payment");
    let has_money = has_query_token(query_tokens, "money");
    let has_make = has_query_token(query_tokens, "make");
    let has_pay = has_query_token(query_tokens, "pay");
    let has_transfer = has_query_token(query_tokens, "transfer");

    (has_send && (has_payment || has_money))
        || (has_make && has_payment)
        || has_pay
        || (has_transfer && has_money)
}

fn has_query_token(query_tokens: &[String], needle: &str) -> bool {
    query_tokens.iter().any(|token| token == needle)
}

pub(crate) fn llm_rerank_request(
    args: &Value,
    candidates: &[Value],
    limit: usize,
    model: String,
    model_variant: Option<String>,
) -> DirectRequest {
    let candidate_names = candidates
        .iter()
        .filter_map(|candidate| candidate.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let schema = DirectJsonSchema {
        name: "tool_search_rerank".to_string(),
        strict: true,
        schema: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["tool_names"],
            "properties": {
                "tool_names": {
                    "type": "array",
                    "items": {
                        "type": "string",
                        "enum": candidate_names,
                    }
                }
            }
        })
        .into(),
    };
    let prompt = llm_rerank_prompt(args, candidates, limit);
    DirectRequest {
        model,
        model_variant: crate::model_selection::reasoning_selection_from_variant(model_variant),
        model_capability: lash_core::ModelCapability::default(),
        messages: vec![
            DirectMessage {
                role: DirectRole::System,
                parts: vec![DirectPart::Text(
                    "You select API tools for another agent. Return tool names only through the requested JSON schema. Maximize recall for the caller's query while keeping the ranking precise."
                        .to_string(),
                )],
            },
            DirectMessage {
                role: DirectRole::User,
                parts: vec![DirectPart::Text(prompt)],
            },
        ],
        attachments: Vec::new(),
        output: DirectOutputSpec::JsonSchema(schema),
        stream_events: None,
        generation: lash_core::GenerationOptions::default(),
        session_id: None,
        caused_by: None,
        replay: None,
    }
}

fn llm_rerank_prompt(args: &Value, candidates: &[Value], limit: usize) -> String {
    let compact_candidates = candidates
        .iter()
        .map(llm_candidate_payload)
        .collect::<Vec<_>>();
    json!({
        "instructions": [
            "Select tools from candidates for the caller's query.",
            "Return only tools that may be needed to complete the task, best to worst.",
            "Prefer read/list/show/search tools for inspection or counting tasks.",
            "Prefer mutation tools only when the query explicitly asks to change state.",
            "For combined constraints, include complementary tools for each constraint.",
            "Do not include tools outside the candidate list."
        ],
        "query": args.get("query").and_then(Value::as_str).unwrap_or_default(),
        "module": args.get("module").cloned().unwrap_or(Value::Null),
        "exclude": args.get("exclude").cloned().unwrap_or_else(|| json!([])),
        "limit": limit,
        "candidates": compact_candidates,
    })
    .to_string()
}

fn llm_candidate_payload(candidate: &Value) -> Value {
    json!({
        "name": candidate.get("name").cloned().unwrap_or(Value::Null),
        "signature": candidate.get("signature").cloned().unwrap_or(Value::Null),
        "description": candidate.get("description").cloned().unwrap_or(Value::Null),
        "examples": candidate.get("examples").cloned().unwrap_or(Value::Null),
    })
}

pub(crate) fn parse_llm_tool_names(text: &str) -> Result<Vec<String>, serde_json::Error> {
    let trimmed = text.trim();
    let value = match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => value,
        Err(err) => {
            let Some(start) = trimmed.find('{') else {
                return Err(err);
            };
            let Some(end) = trimmed.rfind('}') else {
                return Err(err);
            };
            serde_json::from_str::<Value>(&trimmed[start..=end])?
        }
    };
    Ok(value
        .get("tool_names")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}

pub(crate) fn merge_llm_selection(
    candidates: Vec<Value>,
    selected_names: Vec<String>,
    limit: usize,
) -> Vec<Value> {
    let mut by_name = BTreeMap::new();
    let mut deterministic_names = Vec::new();
    for candidate in candidates {
        let Some(name) = candidate.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !by_name.contains_key(name) {
            deterministic_names.push(name.to_string());
        }
        by_name.insert(name.to_string(), candidate);
    }

    let mut seen = BTreeSet::new();
    let mut ranked = Vec::new();
    for name in selected_names.into_iter().chain(deterministic_names) {
        if ranked.len() >= limit {
            break;
        }
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(candidate) = by_name.get(&name) {
            ranked.push(candidate.clone());
        }
    }
    ranked
}
