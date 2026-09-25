// Copyright 2026 Salesforce, Inc. All rights reserved.
//! The AI Act use-case decision — pure Rust, no model in this file. Turns the
//! judge's typed signals into (a) a risk **tag** for the upstream header and (b) an
//! **action** (allow / flag / block) plus whether a policy-violation event should be
//! raised. Also holds the deterministic evaluation gate.
//!
//! **This is a triage aid, not a legal determination.** The tag is an indicative
//! EU AI Act risk band to help humans find and review risky uses — nothing here
//! decides compliance.

use crate::common::{fnv1a_64, sampled_in};

/// The canonical risk categories (the four EU AI Act bands, plus `uncertain` when the
/// judge's confidence is below `minConfidence`).
pub const CAT_PROHIBITED: &str = "prohibited_practice";
pub const CAT_HIGH_RISK: &str = "high_risk";
pub const CAT_TRANSPARENCY: &str = "transparency_obligation";
pub const CAT_MINIMAL: &str = "minimal_risk";
pub const TAG_UNCERTAIN: &str = "uncertain";

/// Judge signals for the use-case classification.
#[derive(Clone, Debug, Default)]
pub struct Signals {
    /// choice: (risk_category, confidence). `None` if the judge returned no choice.
    pub risk_category: Option<(String, f64)>,
    /// noul p(true): the output is used to make/support a decision about a specific person.
    pub affects_individual: f64,
}

/// What to do when the use case is classified `prohibited_practice`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnProhibited {
    Flag,
    Block,
}

impl OnProhibited {
    pub fn parse(s: &str) -> OnProhibited {
        match s {
            "block" => OnProhibited::Block,
            _ => OnProhibited::Flag,
        }
    }
}

/// What to do when the use case is high-risk (or affects an individual decision).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnHighRisk {
    Flag,
    /// Require a registered `x-ai-use-case-id`; block if it is missing / unregistered.
    RequireRegistration,
}

impl OnHighRisk {
    pub fn parse(s: &str) -> OnHighRisk {
        match s {
            "require_registration" => OnHighRisk::RequireRegistration,
            _ => OnHighRisk::Flag,
        }
    }
}

/// The three-way action, mirroring `common::Decision` but named for this policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    Allow,
    Flag,
    Block,
}

impl ActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Allow => "allow",
            ActionKind::Flag => "flag",
            ActionKind::Block => "block",
        }
    }
}

/// The full outcome of the decision.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    /// The risk tag written to the upstream header (a category or `uncertain`).
    pub tag: String,
    pub action: ActionKind,
    /// Whether to raise a policy-violation event (prohibited / high-risk uses).
    pub violation: bool,
    /// Short machine reason for the decision log / deny body.
    pub reason: &'static str,
}

/// Decision parameters resolved from config.
#[derive(Clone, Copy, Debug)]
pub struct DecideParams {
    pub min_confidence: f64,
    pub prohibited_confidence: f64,
    pub affects_individual_threshold: f64,
    pub on_prohibited: OnProhibited,
    pub on_high_risk: OnHighRisk,
}

/// The header tag: the judge's chosen category when confidence ≥ `minConfidence`,
/// otherwise `uncertain`.
pub fn classify_tag(sig: &Signals, min_confidence: f64) -> String {
    match &sig.risk_category {
        Some((cat, conf)) if *conf >= min_confidence => cat.clone(),
        _ => TAG_UNCERTAIN.to_string(),
    }
}

/// True when the judge chose exactly `name` with confidence at or above `min_conf`.
fn category_at(sig: &Signals, name: &str, min_conf: f64) -> bool {
    matches!(&sig.risk_category, Some((cat, conf)) if cat == name && *conf >= min_conf)
}

/// The core decision. `registered` = a non-empty `x-ai-use-case-id` was present in
/// the configured `registeredUseCases` list (only consulted for `require_registration`).
pub fn decide(sig: &Signals, registered: bool, p: DecideParams) -> Outcome {
    let tag = classify_tag(sig, p.min_confidence);

    // 1) Prohibited practice — the strongest band, its own (higher) confidence gate.
    if category_at(sig, CAT_PROHIBITED, p.prohibited_confidence) {
        let action = match p.on_prohibited {
            OnProhibited::Block => ActionKind::Block,
            OnProhibited::Flag => ActionKind::Flag,
        };
        return Outcome { tag, action, violation: true, reason: "prohibited_practice" };
    }

    // 2) High-risk use, OR the output supports a decision about a specific person.
    let high = category_at(sig, CAT_HIGH_RISK, p.min_confidence);
    let affects = sig.affects_individual >= p.affects_individual_threshold;
    if high || affects {
        let reason = if high { "high_risk" } else { "affects_individual_decision" };
        let action = match p.on_high_risk {
            OnHighRisk::Flag => ActionKind::Flag,
            // Registered high-risk use → allowed through (flagged for the record);
            // unregistered → blocked pending registration.
            OnHighRisk::RequireRegistration => {
                if registered {
                    ActionKind::Flag
                } else {
                    ActionKind::Block
                }
            }
        };
        return Outcome { tag, action, violation: true, reason };
    }

    // 3) Transparency / minimal / uncertain — annotate only, no event.
    Outcome { tag, action: ActionKind::Allow, violation: false, reason: "ok" }
}

/// Deterministic evaluation gate. The judge is called when the `(client_id,
/// system_prompt)` pair falls inside the sampled fraction — computed from a stable
/// hash so the SAME pair always decides the same way (tests are stable; retries are
/// consistent). A cross-request dedup cache that *always* evaluates a genuinely new
/// pair once (regardless of `sampleRate`) is a documented production enhancement
/// layered on PDK Data Storage; the hash gate is the dependency-free floor.
pub fn should_evaluate(client_id: &str, system_prompt: &str, sample_rate: f64) -> bool {
    let mut key = String::with_capacity(client_id.len() + 1 + system_prompt.len());
    key.push_str(client_id);
    key.push('\u{0}');
    key.push_str(system_prompt);
    sampled_in(fnv1a_64(key.as_bytes()), sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> DecideParams {
        DecideParams {
            min_confidence: 0.6,
            prohibited_confidence: 0.8,
            affects_individual_threshold: 0.7,
            on_prohibited: OnProhibited::Flag,
            on_high_risk: OnHighRisk::Flag,
        }
    }

    #[test]
    fn tag_falls_back_to_uncertain_below_min_confidence() {
        let sig = Signals { risk_category: Some((CAT_HIGH_RISK.into(), 0.4)), affects_individual: 0.0 };
        assert_eq!(classify_tag(&sig, 0.6), TAG_UNCERTAIN);
        // At/above threshold the category is used.
        let sig2 = Signals { risk_category: Some((CAT_MINIMAL.into(), 0.9)), affects_individual: 0.0 };
        assert_eq!(classify_tag(&sig2, 0.6), CAT_MINIMAL);
    }

    #[test]
    fn minimal_risk_is_allowed_no_violation() {
        let sig = Signals { risk_category: Some((CAT_MINIMAL.into(), 0.95)), affects_individual: 0.05 };
        let o = decide(&sig, false, params());
        assert_eq!(o.action, ActionKind::Allow);
        assert!(!o.violation);
        assert_eq!(o.tag, CAT_MINIMAL);
    }

    #[test]
    fn prohibited_flags_by_default_blocks_when_configured() {
        let sig = Signals { risk_category: Some((CAT_PROHIBITED.into(), 0.9)), affects_individual: 0.1 };
        let flag = decide(&sig, false, params());
        assert_eq!(flag.action, ActionKind::Flag);
        assert!(flag.violation);
        let mut p = params();
        p.on_prohibited = OnProhibited::Block;
        assert_eq!(decide(&sig, false, p).action, ActionKind::Block);
    }

    #[test]
    fn prohibited_below_08_confidence_is_not_prohibited_action() {
        // choice prohibited but only 0.7 confidence → not treated as prohibited; tag
        // is still prohibited_practice (>= minConfidence 0.6), action falls through.
        let sig = Signals { risk_category: Some((CAT_PROHIBITED.into(), 0.7)), affects_individual: 0.1 };
        let o = decide(&sig, false, params());
        assert_eq!(o.tag, CAT_PROHIBITED);
        assert_eq!(o.action, ActionKind::Allow);
        assert!(!o.violation);
    }

    #[test]
    fn high_risk_require_registration_blocks_unregistered_allows_registered() {
        let sig = Signals { risk_category: Some((CAT_HIGH_RISK.into(), 0.9)), affects_individual: 0.2 };
        let mut p = params();
        p.on_high_risk = OnHighRisk::RequireRegistration;
        assert_eq!(decide(&sig, false, p).action, ActionKind::Block);
        assert_eq!(decide(&sig, true, p).action, ActionKind::Flag);
        assert!(decide(&sig, true, p).violation);
    }

    #[test]
    fn affects_individual_triggers_high_risk_branch_even_when_category_minimal() {
        let sig = Signals { risk_category: Some((CAT_MINIMAL.into(), 0.9)), affects_individual: 0.85 };
        let o = decide(&sig, false, params());
        assert_eq!(o.action, ActionKind::Flag);
        assert!(o.violation);
        assert_eq!(o.reason, "affects_individual_decision");
    }

    #[test]
    fn eval_gate_is_deterministic_per_pair() {
        // Same pair → same answer every call.
        let a = should_evaluate("client-1", "You are a hiring assistant.", 0.5);
        let b = should_evaluate("client-1", "You are a hiring assistant.", 0.5);
        assert_eq!(a, b);
        // 1.0 always evaluates; 0.0 never.
        assert!(should_evaluate("c", "sys", 1.0));
        assert!(!should_evaluate("c", "sys", 0.0));
    }
}
