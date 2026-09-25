// Copyright 2026 Salesforce, Inc. All rights reserved.
//! `jev-client` subset (inlined; lift into the shared crate later): a single
//! abstraction for calling a System-One judge. The judge is a *classifier* — it
//! returns typed probabilities, never generated prose — so a numeric threshold is
//! meaningful. Providers: TypeSafe Jev (noul/choice API), any OpenAI-compatible
//! chat endpoint driven as a JSON classifier, and a deterministic in-policy Mock
//! for tests and offline demos. All mutation stays in the Rust policy.
//!
//! C3 asks the judge to classify the EU AI Act **risk band of the use case** and
//! whether the output supports a decision about a specific person.

use std::time::Duration;

use pdk::hl::*;
use serde_json::Value;

use crate::screen::Signals;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Mock,
    TypeSafe,
    OpenAi,
    OpenRouter,
    LiteLlm,
    Cloudflare,
    Custom,
}

impl Provider {
    pub fn parse(s: &str) -> Provider {
        match s {
            "mock" => Provider::Mock,
            "openai" => Provider::OpenAi,
            "openrouter" => Provider::OpenRouter,
            "litellm" => Provider::LiteLlm,
            "cloudflare" => Provider::Cloudflare,
            "custom" => Provider::Custom,
            _ => Provider::TypeSafe,
        }
    }
    /// Whether this provider speaks the OpenAI chat-completions contract.
    fn is_openai_compat(self) -> bool {
        matches!(self, Provider::OpenAi | Provider::OpenRouter | Provider::LiteLlm | Provider::Custom)
    }
    fn default_path(self) -> &'static str {
        match self {
            Provider::OpenAi => "/v1/chat/completions",
            Provider::OpenRouter => "/api/v1/chat/completions",
            Provider::LiteLlm | Provider::Custom => "/v1/chat/completions",
            Provider::TypeSafe => "/api/alpha/decisions",
            Provider::Cloudflare => "/client/v4/accounts",
            Provider::Mock => "",
        }
    }
}

#[derive(Clone, Debug)]
pub struct JevSettings {
    pub provider: Provider,
    pub model: String,
    pub path: String,
    pub api_key: String,
    pub custom_auth_header: String,
    pub timeout_ms: u64,
    pub max_state_tokens: usize,
    pub cloudflare_account_id: Option<String>,
    /// Optional per-category criteria overrides (the "versioned question pack" the
    /// legal team can edit without a code change). Order: prohibited, high-risk,
    /// transparency, minimal; `None` keeps the built-in default text.
    pub criteria: CriteriaPack,
}

impl JevSettings {
    pub fn resolved_path(&self) -> String {
        if !self.path.is_empty() {
            self.path.clone()
        } else if self.provider == Provider::Cloudflare {
            format!(
                "/client/v4/accounts/{}/ai/run/{}",
                self.cloudflare_account_id.as_deref().unwrap_or(""),
                self.model
            )
        } else {
            self.provider.default_path().to_string()
        }
    }
}

#[derive(Clone, Debug)]
pub enum JevError {
    Timeout,
    RateLimited,
    Overloaded,
    Auth,
    Upstream(u16),
    Decode(String),
    Disabled,
}

pub struct JevResult {
    pub signals: Signals,
    pub model: String,
}

/// Approximate token budget on the untrusted text (chars/4, conservative). Keeps
/// the head (70%) and tail (30%) with a gateway marker so an attacker cannot push
/// the payload past the judge by padding the middle. Returns (text, truncated).
pub fn budget_state(text: &str, max_tokens: usize) -> (String, bool) {
    let max_chars = max_tokens.saturating_mul(4);
    if text.chars().count() <= max_chars || max_chars == 0 {
        return (text.to_string(), false);
    }
    let head_chars = (max_chars * 7) / 10;
    let tail_chars = max_chars - head_chars;
    let chars: Vec<char> = text.chars().collect();
    let head: String = chars[..head_chars].iter().collect();
    let tail: String = chars[chars.len() - tail_chars..].iter().collect();
    (format!("{head}\n[… truncated by gateway …]\n{tail}"), true)
}

// ─── C3 question pack + signal parsing ───────────────────────────────────────

/// Default EU AI Act criteria text (the "question pack"). Overridable via config so
/// a legal/compliance team can revise the wording without a code change.
pub const DEFAULT_PROHIBITED: &str = "Social scoring of people, manipulation exploiting vulnerabilities, untargeted scraping of facial images, emotion recognition at work or school, or predicting crime from personality traits.";
pub const DEFAULT_HIGH_RISK: &str = "Decisions about hiring or managing workers, access to education, credit or insurance, essential public services, law enforcement, migration, justice, or safety components of regulated products.";
pub const DEFAULT_TRANSPARENCY: &str = "A chatbot talking with people, or generating or manipulating images, audio, video, or text presented to the public.";
pub const DEFAULT_MINIMAL: &str = "Other uses such as coding help, summarising internal documents, or search.";

/// Optional per-category criteria overrides (the versioned question pack).
#[derive(Clone, Debug, Default)]
pub struct CriteriaPack {
    pub prohibited: Option<String>,
    pub high_risk: Option<String>,
    pub transparency: Option<String>,
    pub minimal: Option<String>,
}

impl CriteriaPack {
    fn prohibited(&self) -> &str {
        self.prohibited.as_deref().unwrap_or(DEFAULT_PROHIBITED)
    }
    fn high_risk(&self) -> &str {
        self.high_risk.as_deref().unwrap_or(DEFAULT_HIGH_RISK)
    }
    fn transparency(&self) -> &str {
        self.transparency.as_deref().unwrap_or(DEFAULT_TRANSPARENCY)
    }
    fn minimal(&self) -> &str {
        self.minimal.as_deref().unwrap_or(DEFAULT_MINIMAL)
    }
}

const CLASSIFIER_SYSTEM: &str = "You are an EU AI Act use-case classifier for an API gateway. \
You are given the system instructions and the latest user request sent to an AI system. \
Classify the intended USE of the AI, not the safety of the text. This is a triage aid, not a legal determination. \
Reply with ONLY a JSON object, no prose, with keys: \
\"risk_category\": one of \"prohibited_practice\",\"high_risk\",\"transparency_obligation\",\"minimal_risk\", \
\"risk_category_confidence\": number 0..1, \
\"affects_individual_decision\": number 0..1 (probability the output is used to make or support a decision about a specific person).";

/// OpenAI-compatible chat request that drives the model as a JSON classifier.
pub fn build_chat_body(model: &str, instructions: &str, user_request: &str) -> Value {
    let user = format!(
        "application_instructions:\n{instructions}\n\nuser_request:\n{user_request}"
    );
    serde_json::json!({
        "model": model,
        "temperature": 0,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": CLASSIFIER_SYSTEM },
            { "role": "user", "content": user }
        ]
    })
}

/// Parse an OpenAI-compatible chat completion whose message content is the
/// classifier JSON.
pub fn parse_chat_signals(body: &[u8]) -> Result<Signals, JevError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| JevError::Decode(e.to_string()))?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| JevError::Decode("no choices[0].message.content".into()))?;
    let parsed: Value =
        serde_json::from_str(content).map_err(|e| JevError::Decode(format!("content not JSON: {e}")))?;
    Ok(signals_from_json(&parsed))
}

/// TypeSafe Jev decisions request (noul/choice). Same envelope shape verified live
/// against the OpenRouter Decisions API (`POST https://openrouter.ai/api/alpha/decisions`,
/// model `~typesafe/jev-latest`) for the S1 policy.
pub fn build_typesafe_body(model: &str, instructions: &str, user_request: &str, pack: &CriteriaPack) -> Value {
    serde_json::json!({
        "model": model,
        "state": {
            "application_instructions": instructions,
            "user_request": user_request
        },
        "questions": {
            "risk_category": {
                "type": "choice",
                "instructions": "Which category best describes the use of AI in `application_instructions` and `user_request`?",
                "criteria": {
                    "prohibited_practice": pack.prohibited(),
                    "high_risk": pack.high_risk(),
                    "transparency_obligation": pack.transparency(),
                    "minimal_risk": pack.minimal()
                }
            },
            "affects_individual_decision": {
                "type": "noul",
                "instructions": "Is the output used to make or support a decision about a specific person?",
                "criteria": {
                    "true": "It makes or supports a decision about a specific person.",
                    "false": "It does not decide anything about a specific person."
                }
            }
        }
    })
}

/// Parse a TypeSafe decisions response into our signals. **noul-first** for the
/// noul question (the OpenRouter Decisions API returns `answers.<q>.noul`; older /
/// other TypeSafe hosts used `p_yes` or `probabilities/true` — accept all, in that
/// order). This ordering is load-bearing (see the S1 live bug).
pub fn parse_typesafe_signals(body: &[u8]) -> Result<Signals, JevError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| JevError::Decode(e.to_string()))?;
    let answers = v.get("answers").unwrap_or(&v);
    let risk_category = answers.pointer("/risk_category/choice").and_then(Value::as_str).map(|c| {
        let conf = answers.pointer("/risk_category/confidence").and_then(Value::as_f64).unwrap_or(0.0);
        (c.to_string(), conf)
    });
    let affects_individual = answers
        .pointer("/affects_individual_decision/noul")
        .or_else(|| answers.pointer("/affects_individual_decision/p_yes"))
        .or_else(|| answers.pointer("/affects_individual_decision/probabilities/true"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    Ok(Signals { risk_category, affects_individual: clamp01(affects_individual) })
}

fn signals_from_json(v: &Value) -> Signals {
    let risk_category = v.get("risk_category").and_then(Value::as_str).map(|c| {
        let conf = v.get("risk_category_confidence").and_then(Value::as_f64).unwrap_or(0.0);
        (c.to_string(), clamp01(conf))
    });
    let affects_individual = v.get("affects_individual_decision").and_then(Value::as_f64).unwrap_or(0.0);
    Signals { risk_category, affects_individual: clamp01(affects_individual) }
}

fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

/// Deterministic in-policy judge for tests and offline demos. It mirrors what a real
/// classifier would conclude from obvious lexical signals in the system prompt and
/// user request, so a demo run without any external key still shows the risk-band
/// behaviour. Never enabled unless `allowMock: true`.
pub fn mock_signals(instructions: &str, user_request: &str) -> Signals {
    let hay = format!("{instructions}\n{user_request}").to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|p| hay.contains(p));

    let prohibited = has(&[
        "social scoring", "social credit", "emotion recognition", "scrape facial",
        "facial images", "predict crime", "predictive policing", "exploit vulnerab",
    ]);
    let high_risk = has(&[
        "hiring", "recruit", "applicant", "candidate", "resume", "cv ", "credit scor",
        "creditworth", "loan", "insurance", "admission", "exam grading", "law enforcement",
        "immigration", "migration", "welfare", "benefits eligibility", "medical diagnos",
    ]);
    let transparency = has(&[
        "chatbot", "chat bot", "customer-facing", "reply to customers", "generate image",
        "generate an image", "deepfake", "synthetic media", "image generation",
    ]);
    let affects = has(&[
        "hiring", "applicant", "candidate", "loan", "credit", "insurance", "patient",
        "defendant", "decision about", "eligibility", "should we hire", "who to hire",
    ]);

    let (category, conf) = if prohibited {
        (crate::screen::CAT_PROHIBITED, 0.9)
    } else if high_risk {
        (crate::screen::CAT_HIGH_RISK, 0.9)
    } else if transparency {
        (crate::screen::CAT_TRANSPARENCY, 0.85)
    } else {
        (crate::screen::CAT_MINIMAL, 0.95)
    };

    Signals {
        risk_category: Some((category.to_string(), conf)),
        affects_individual: if affects { 0.9 } else { 0.05 },
    }
}

/// Call the judge. `instructions` and `user_request` are already budgeted by the caller.
pub async fn evaluate(
    client: &HttpClient,
    service: &Service,
    s: &JevSettings,
    instructions: &str,
    user_request: &str,
) -> Result<JevResult, JevError> {
    if s.provider == Provider::Mock {
        return Ok(JevResult { signals: mock_signals(instructions, user_request), model: "mock".to_string() });
    }
    if s.api_key.trim().is_empty() {
        return Err(JevError::Auth);
    }

    let (body, is_openai) = if s.provider.is_openai_compat() {
        (build_chat_body(&s.model, instructions, user_request), true)
    } else {
        (build_typesafe_body(&s.model, instructions, user_request, &s.criteria), false)
    };
    let payload = serde_json::to_vec(&body).map_err(|e| JevError::Decode(e.to_string()))?;

    let auth_header = if s.provider == Provider::Custom { s.custom_auth_header.as_str() } else { "Authorization" };
    let auth_value = if auth_header.eq_ignore_ascii_case("authorization") {
        format!("Bearer {}", s.api_key)
    } else {
        s.api_key.clone()
    };

    let resp = client
        .request(service)
        .path(&s.resolved_path())
        .headers(vec![(auth_header, auth_value.as_str()), ("Content-Type", "application/json")])
        .body(&payload)
        .timeout(Duration::from_millis(s.timeout_ms))
        .post()
        .await
        .map_err(|_| JevError::Timeout)?;

    let status = resp.status_code() as u16;
    match status {
        200..=299 => {}
        401 | 403 => return Err(JevError::Auth),
        429 => return Err(JevError::RateLimited),
        529 => return Err(JevError::Overloaded),
        other => return Err(JevError::Upstream(other)),
    }

    let signals = if is_openai { parse_chat_signals(resp.body())? } else { parse_typesafe_signals(resp.body())? };
    Ok(JevResult { signals, model: s.model.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::{CAT_HIGH_RISK, CAT_MINIMAL};

    #[test]
    fn budget_truncates_head_and_tail() {
        let text = "a".repeat(1000);
        let (out, truncated) = budget_state(&text, 100); // 400 char budget
        assert!(truncated);
        assert!(out.contains("[… truncated by gateway …]"));
        assert!(out.chars().count() < 1000);
    }

    #[test]
    fn budget_leaves_short_text_untouched() {
        let (out, truncated) = budget_state("short", 100);
        assert!(!truncated);
        assert_eq!(out, "short");
    }

    #[test]
    fn parses_openai_classifier_content() {
        let body = br#"{"choices":[{"message":{"content":"{\"risk_category\":\"high_risk\",\"risk_category_confidence\":0.88,\"affects_individual_decision\":0.9}"}}]}"#;
        let sig = parse_chat_signals(body).unwrap();
        assert_eq!(sig.risk_category.as_ref().unwrap().0, "high_risk");
        assert!((sig.affects_individual - 0.9).abs() < 1e-9);
    }

    #[test]
    fn parses_typesafe_choice_and_noul_first() {
        // noul-first ordering: the decisions API returns the noul answer under `noul`.
        let body = br#"{"model":"typesafe/jev-1.13","answers":{
            "risk_category":{"type":"choice","choice":"high_risk","probabilities":{"high_risk":0.7},"confidence":0.83},
            "affects_individual_decision":{"type":"noul","noul":0.94}
        }}"#;
        let sig = parse_typesafe_signals(body).unwrap();
        let (cat, conf) = sig.risk_category.as_ref().unwrap();
        assert_eq!(cat, "high_risk");
        assert!((conf - 0.83).abs() < 1e-9);
        assert!((sig.affects_individual - 0.94).abs() < 1e-9);
    }

    #[test]
    fn parses_typesafe_noul_fallback_to_pyes() {
        let body = br#"{"answers":{"risk_category":{"choice":"minimal_risk","confidence":0.9},"affects_individual_decision":{"p_yes":0.2}}}"#;
        let sig = parse_typesafe_signals(body).unwrap();
        assert!((sig.affects_individual - 0.2).abs() < 1e-9);
    }

    #[test]
    fn mock_classifies_hiring_high_risk_and_summary_minimal() {
        let hi = mock_signals("You are a hiring-decision assistant.", "Rank these applicants and recommend who to hire.");
        assert_eq!(hi.risk_category.as_ref().unwrap().0, CAT_HIGH_RISK);
        assert!(hi.affects_individual > 0.7);
        let lo = mock_signals("You are a helpful assistant.", "Summarise this internal document.");
        assert_eq!(lo.risk_category.as_ref().unwrap().0, CAT_MINIMAL);
        assert!(lo.affects_individual < 0.5);
    }

    #[test]
    fn mock_classifies_prohibited_practice() {
        let p = mock_signals("Build a social scoring system for citizens.", "Score everyone.");
        assert_eq!(p.risk_category.as_ref().unwrap().0, crate::screen::CAT_PROHIBITED);
    }

    #[test]
    fn cloudflare_path_includes_account_and_model() {
        let s = JevSettings {
            provider: Provider::Cloudflare,
            model: "@cf/m".into(),
            path: String::new(),
            api_key: "k".into(),
            custom_auth_header: "Authorization".into(),
            timeout_ms: 600,
            max_state_tokens: 24000,
            cloudflare_account_id: Some("acc123".into()),
            criteria: CriteriaPack::default(),
        };
        assert_eq!(s.resolved_path(), "/client/v4/accounts/acc123/ai/run/@cf/m");
    }

    #[test]
    fn typesafe_body_uses_pack_override() {
        let mut pack = CriteriaPack::default();
        pack.high_risk = Some("Custom high-risk criteria for this deployment.".into());
        let body = build_typesafe_body("~typesafe/jev-latest", "sys", "usr", &pack);
        let hr = body.pointer("/questions/risk_category/criteria/high_risk").and_then(Value::as_str).unwrap();
        assert_eq!(hr, "Custom high-risk criteria for this deployment.");
        // default still used where not overridden
        let mr = body.pointer("/questions/risk_category/criteria/minimal_risk").and_then(Value::as_str).unwrap();
        assert_eq!(mr, DEFAULT_MINIMAL);
    }
}
