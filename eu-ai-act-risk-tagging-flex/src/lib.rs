// Copyright 2026 Salesforce, Inc. All rights reserved.
//! EU AI Act Use-Case Risk Tagging — a request-leg Omni/Flex Gateway policy.
//!
//! Tags an inbound LLM request with an indicative EU AI Act risk band of the *use
//! case* — `prohibited_practice` / `high_risk` / `transparency_obligation` /
//! `minimal_risk` (or `uncertain`) — so risky uses are logged and reviewed. The
//! judge is TypeSafe Jev: a typed "System 1" classifier that returns probabilities,
//! never generated text — so all mutation stays in this Rust policy. It sets an
//! upstream header, raises a policy-violation event for prohibited/high-risk uses,
//! and can optionally deny with HTTP 403.
//!
//! **This is a triage aid, not a legal determination.** It helps humans find and
//! review risky AI uses; it does not decide compliance.
//!
//! One leg: REQUEST. It buffers the request headers + body together
//! (`into_headers_body_state`, needs `enable_stop_iteration`), extracts the system
//! prompt (which usually defines the use case) and the last user message, calls the
//! judge on the deterministically-sampled `(client, system-prompt)` pairs, and acts
//! per `mode`. Errors honour `failMode` (open passes through).

mod common;
mod generated;
mod jev;
mod payloads;
mod screen;

use std::rc::Rc;

use pdk::hl::*;
use pdk::logger;
use pdk::policy_violation::PolicyViolations;

use crate::common::{path_prefix_match, FailMode, Mode};
use crate::generated::config::Config;
use crate::jev::{budget_state, evaluate, CriteriaPack, JevError, JevSettings, Provider};
use crate::payloads::parse_llm_request;
use crate::screen::{decide, should_evaluate, ActionKind, DecideParams, OnHighRisk, OnProhibited, Outcome};

const DEFAULT_RISK_HEADER: &str = "x-jev-ai-act-risk";
const DEFAULT_USE_CASE_HEADER: &str = "x-ai-use-case-id";
const DEFAULT_CLIENT_ID_HEADER: &str = "x-client-id";

// ─── config helpers ─────────────────────────────────────────────────────────

fn jev_settings(cfg: &Config) -> JevSettings {
    JevSettings {
        provider: Provider::parse(cfg.jev_provider.as_deref().unwrap_or("typesafe")),
        model: cfg.jev_model.clone().unwrap_or_else(|| "~typesafe/jev-latest".to_string()),
        path: cfg.jev_path.clone().unwrap_or_default(),
        api_key: cfg.jev_api_key.clone().unwrap_or_default(),
        custom_auth_header: cfg.custom_auth_header.clone().unwrap_or_else(|| "Authorization".to_string()),
        timeout_ms: cfg.jev_timeout_ms.unwrap_or(600).max(1) as u64,
        max_state_tokens: cfg.max_state_tokens.unwrap_or(24000).max(256) as usize,
        cloudflare_account_id: cfg.cloudflare_account_id.clone(),
        criteria: CriteriaPack {
            prohibited: cfg.prohibited_criteria.clone(),
            high_risk: cfg.high_risk_criteria.clone(),
            transparency: cfg.transparency_criteria.clone(),
            minimal: cfg.minimal_criteria.clone(),
        },
    }
}

fn decide_params(cfg: &Config) -> DecideParams {
    DecideParams {
        min_confidence: cfg.min_confidence.unwrap_or(0.6),
        prohibited_confidence: cfg.prohibited_confidence.unwrap_or(0.8),
        affects_individual_threshold: cfg.affects_individual_threshold.unwrap_or(0.7),
        on_prohibited: OnProhibited::parse(cfg.on_prohibited.as_deref().unwrap_or("flag")),
        on_high_risk: OnHighRisk::parse(cfg.on_high_risk.as_deref().unwrap_or("flag")),
    }
}

/// `open` fails through (pass, untagged), `closed` fails to a block.
fn fail_mode(cfg: &Config) -> FailMode {
    FailMode::parse(cfg.fail_mode.as_deref().unwrap_or("open"))
}

/// Outcome when the judge is unavailable/disabled, under the failure mode.
fn fail_outcome(fm: FailMode, e: &JevError) -> Outcome {
    logger::warn!("c3: judge error {e:?} failMode={fm:?}");
    match fm {
        FailMode::Open => Outcome {
            tag: "unavailable".to_string(),
            action: ActionKind::Allow,
            violation: false,
            reason: "judge_unavailable",
        },
        FailMode::Closed => Outcome {
            tag: "unavailable".to_string(),
            action: ActionKind::Block,
            violation: false,
            reason: "decision_unavailable",
        },
    }
}

// ─── request leg ─────────────────────────────────────────────────────────────

async fn request_filter(
    request_state: RequestState,
    cfg: Rc<Config>,
    client: Rc<HttpClient>,
    policy_violations: Rc<PolicyViolations>,
) -> Flow<()> {
    let mode = Mode::parse(cfg.mode.as_deref().unwrap_or("shadow"));
    let risk_header = cfg.risk_header.clone().unwrap_or_else(|| DEFAULT_RISK_HEADER.to_string());

    // Buffer headers + body together so we can classify the body and then set an
    // upstream header (or deny) before anything is forwarded upstream.
    let state = request_state.into_headers_body_state().await;

    // Strip any client-supplied risk header so a caller cannot spoof the tag a later
    // policy (or the upstream) trusts.
    if cfg.strip_client_jev_headers.unwrap_or(true) {
        state.handler().remove_header(&risk_header);
    }
    if mode == Mode::Off {
        return Flow::Continue(());
    }

    // REST route allowlist (empty = every matched route).
    let path = state.handler().header(":path").unwrap_or_default();
    if !path_prefix_match(cfg.routes.as_deref().unwrap_or(&[]), &path) {
        return Flow::Continue(());
    }

    // Only inspect JSON request bodies (LLM APIs are JSON).
    let ct = state.handler().header("content-type").unwrap_or_default().to_ascii_lowercase();
    if !ct.contains("json") || !state.contains_body() {
        return Flow::Continue(());
    }
    let body = state.handler().body();
    let Some(view) = parse_llm_request(&body) else {
        // Not a recognised LLM request shape — pass through untagged.
        return Flow::Continue(());
    };

    // Caller identity + declared use-case id (both header-driven, configurable).
    let client_id_header = cfg.client_id_header.clone().unwrap_or_else(|| DEFAULT_CLIENT_ID_HEADER.to_string());
    let use_case_header = cfg.use_case_header.clone().unwrap_or_else(|| DEFAULT_USE_CASE_HEADER.to_string());
    let client_id = state.handler().header(&client_id_header).unwrap_or_else(|| "anonymous".to_string());
    let use_case_id = state.handler().header(&use_case_header).unwrap_or_default();

    // Deterministic evaluation gate on the (client, system-prompt) pair.
    let sample_rate = cfg.sample_rate.unwrap_or(0.1);
    if !should_evaluate(&client_id, &view.system_prompt, sample_rate) {
        logger::debug!("c3: pair not sampled (rate={sample_rate}) — passthrough");
        return Flow::Continue(());
    }

    if cfg.log_state_sample.unwrap_or(false) {
        let sp: String = view.system_prompt.chars().take(160).collect();
        logger::debug!("c3: client={client_id} system_prompt_sample={sp:?}");
    }

    // Budget the two state fields before the judge sees them.
    let settings = jev_settings(&cfg);
    let (instr, _) = budget_state(&view.system_prompt, settings.max_state_tokens);
    let (user, _) = budget_state(&view.last_user, settings.max_state_tokens);

    // Registration status (only consulted for require_registration).
    let registered = !use_case_id.is_empty()
        && cfg.registered_use_cases.as_deref().unwrap_or(&[]).iter().any(|u| u == &use_case_id);

    // Judge → outcome.
    let outcome = if settings.provider == Provider::Mock && !cfg.allow_mock.unwrap_or(false) {
        fail_outcome(fail_mode(&cfg), &JevError::Disabled)
    } else {
        match evaluate(&client, &cfg.jev_service, &settings, &instr, &user).await {
            Ok(res) => {
                let o = decide(&res.signals, registered, decide_params(&cfg));
                logger::debug!(
                    "c3: category={:?} affects={:.2} registered={registered} -> tag={} action={} reason={}",
                    res.signals.risk_category, res.signals.affects_individual, o.tag, o.action.as_str(), o.reason
                );
                o
            }
            Err(e) => fail_outcome(fail_mode(&cfg), &e),
        }
    };

    logger::info!(
        "c3: mode={:?} tag={} action={} violation={} reason={} client={client_id} registered={registered} path={path}",
        mode, outcome.tag, outcome.action.as_str(), outcome.violation, outcome.reason
    );

    // Shadow mode: compute + log only, never mutate or deny.
    if !mode.mutates() {
        return Flow::Continue(());
    }

    // Enforce mode: annotate upstream, raise a violation event, optionally deny.
    state.handler().set_header(&risk_header, &outcome.tag);
    if outcome.violation {
        policy_violations.generate_policy_violation();
    }
    match outcome.action {
        ActionKind::Block => Flow::Break(
            Response::new(403)
                .with_headers(vec![(risk_header.clone(), outcome.tag.clone())])
                .with_body(deny_body(&outcome.tag, outcome.reason)),
        ),
        ActionKind::Allow | ActionKind::Flag => Flow::Continue(()),
    }
}

/// The 403 body when the policy denies a request in enforce mode.
fn deny_body(tag: &str, reason: &str) -> String {
    serde_json::json!({
        "error": "Request blocked by EU AI Act use-case risk policy.",
        "risk_category": tag,
        "reason": reason,
        "policy": "eu-ai-act-risk-tagging",
        "note": "Indicative triage tag, not a legal determination. Register the use case (x-ai-use-case-id) or contact compliance."
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn deny_body_is_valid_json_with_tag_and_reason() {
        let b = deny_body("high_risk", "affects_individual_decision");
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["risk_category"], "high_risk");
        assert_eq!(v["reason"], "affects_individual_decision");
        assert_eq!(v["policy"], "eu-ai-act-risk-tagging");
    }

    #[test]
    fn fail_open_passes_untagged_fail_closed_blocks() {
        let open = fail_outcome(FailMode::Open, &JevError::Timeout);
        assert_eq!(open.action, ActionKind::Allow);
        assert!(!open.violation);
        let closed = fail_outcome(FailMode::Closed, &JevError::Timeout);
        assert_eq!(closed.action, ActionKind::Block);
    }
}

// ─── launch ───────────────────────────────────────────────────────────────────

#[entrypoint]
async fn configure(
    launcher: Launcher,
    Configuration(bytes): Configuration,
    client: HttpClient,
    policy_violations: PolicyViolations,
) -> anyhow::Result<()> {
    let config: Config = serde_json::from_slice(&bytes).map_err(|err| {
        anyhow::anyhow!("Failed to parse configuration '{}'. Cause: {}", String::from_utf8_lossy(&bytes), err)
    })?;
    let config = Rc::new(config);
    let client = Rc::new(client);
    let policy_violations = Rc::new(policy_violations);

    let filter = on_request(move |rs| {
        let c = config.clone();
        let cl = client.clone();
        let pv = policy_violations.clone();
        async move { request_filter(rs, c, cl, pv).await }
    });
    launcher.launch(filter).await?;
    Ok(())
}
