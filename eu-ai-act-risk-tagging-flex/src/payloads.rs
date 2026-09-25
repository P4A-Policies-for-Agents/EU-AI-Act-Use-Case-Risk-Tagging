// Copyright 2026 Salesforce, Inc. All rights reserved.
//! Tolerant extraction of the two things the AI Act use-case judge needs from an
//! inbound LLM request: the **application instructions** (system prompt — which
//! usually defines the *use case*) and the **last user message**. Understands the
//! three common request shapes and ignores everything else it doesn't recognise.
//! A body that matches no shape returns `None` (the policy then passes it through
//! without tagging).

use serde_json::Value;

/// What the policy screens: the system prompt (use-case definition) and the most
/// recent user turn. Either may be empty, but not both (or we return `None`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LlmView {
    pub system_prompt: String,
    pub last_user: String,
}

/// Parse an inbound LLM request body. Handles:
/// - **OpenAI Chat Completions** — `messages: [{role, content}]` (content = string
///   or an array of `{type:"text", text}` parts).
/// - **OpenAI Responses** — top-level `instructions` (system) + `input` (a string,
///   or an array of `{role, content:[{type:"input_text", text}]}` items).
/// - **Anthropic Messages** — top-level `system` (string or `[{type:"text", text}]`)
///   + `messages: [{role, content}]`.
/// Unknown fields are ignored; a parse failure or an unrecognised shape → `None`.
pub fn parse_llm_request(body: &[u8]) -> Option<LlmView> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let mut system = String::new();
    let mut last_user = String::new();

    // System prompt sources (Anthropic `system`, OpenAI Responses `instructions`).
    if let Some(s) = v.get("system") {
        append(&mut system, &content_text(s));
    }
    if let Some(s) = v.get("instructions").and_then(Value::as_str) {
        append(&mut system, s);
    }

    // `messages[]` — OpenAI Chat and Anthropic Messages both use this shape.
    if let Some(msgs) = v.get("messages").and_then(Value::as_array) {
        for m in msgs {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("");
            let text = content_text(m.get("content").unwrap_or(&Value::Null));
            match role {
                "system" | "developer" => append(&mut system, &text),
                "user" => last_user = text, // keep the LAST user turn
                _ => {}
            }
        }
    }

    // `input` — OpenAI Responses (only if we didn't already find a user turn).
    if last_user.is_empty() {
        if let Some(inp) = v.get("input") {
            if let Some(s) = inp.as_str() {
                last_user = s.to_string();
            } else if let Some(arr) = inp.as_array() {
                for item in arr {
                    let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                    let text = content_text(item.get("content").unwrap_or(item));
                    match role {
                        "system" | "developer" => append(&mut system, &text),
                        "user" => last_user = text,
                        _ => {}
                    }
                }
            }
        }
    }

    if system.is_empty() && last_user.is_empty() {
        return None;
    }
    Some(LlmView { system_prompt: system, last_user })
}

/// Flatten a `content` value to text: a plain string, or an array of parts each of
/// which may be a string or an object carrying `text` / `input_text` / `output_text`.
fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for p in parts {
                if let Some(s) = p.as_str() {
                    append(&mut out, s);
                } else if let Some(t) = p
                    .get("text")
                    .or_else(|| p.get("input_text"))
                    .or_else(|| p.get("output_text"))
                    .and_then(Value::as_str)
                {
                    append(&mut out, t);
                }
            }
            out
        }
        _ => String::new(),
    }
}

fn append(buf: &mut String, s: &str) {
    let s = s.trim();
    if s.is_empty() {
        return;
    }
    if !buf.is_empty() {
        buf.push('\n');
    }
    buf.push_str(s);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_chat_last_user_and_system() {
        let body = br#"{"model":"gpt-4o","messages":[
            {"role":"system","content":"You are a hiring assistant."},
            {"role":"user","content":"First question"},
            {"role":"assistant","content":"ok"},
            {"role":"user","content":"Rank these applicants."}
        ]}"#;
        let v = parse_llm_request(body).unwrap();
        assert_eq!(v.system_prompt, "You are a hiring assistant.");
        assert_eq!(v.last_user, "Rank these applicants.");
    }

    #[test]
    fn parses_anthropic_system_and_content_parts() {
        let body = br#"{"model":"claude","system":"Summarise internal docs.",
            "messages":[{"role":"user","content":[
                {"type":"text","text":"Please summarise "},
                {"type":"text","text":"this memo."}
            ]}]}"#;
        let v = parse_llm_request(body).unwrap();
        assert_eq!(v.system_prompt, "Summarise internal docs.");
        assert_eq!(v.last_user, "Please summarise\nthis memo.");
    }

    #[test]
    fn parses_openai_responses_instructions_and_input_array() {
        let body = br#"{"model":"gpt","instructions":"You are a chatbot for the public.",
            "input":[{"role":"user","content":[{"type":"input_text","text":"Hi there"}]}]}"#;
        let v = parse_llm_request(body).unwrap();
        assert_eq!(v.system_prompt, "You are a chatbot for the public.");
        assert_eq!(v.last_user, "Hi there");
    }

    #[test]
    fn responses_input_as_plain_string() {
        let body = br#"{"instructions":"sys","input":"just a string prompt"}"#;
        let v = parse_llm_request(body).unwrap();
        assert_eq!(v.last_user, "just a string prompt");
    }

    #[test]
    fn unrecognised_body_returns_none() {
        assert!(parse_llm_request(br#"{"foo":"bar"}"#).is_none());
        assert!(parse_llm_request(b"not json").is_none());
    }
}
