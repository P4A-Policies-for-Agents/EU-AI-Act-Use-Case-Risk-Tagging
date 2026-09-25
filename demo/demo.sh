#!/usr/bin/env bash
# EU AI Act Use-Case Risk Tagging — live demo driver.
#
# Sends three inbound LLM requests through the governed Flex Gateway route and shows
# the policy's decision: a minimal-risk use passes (tagged minimal_risk upstream), a
# high-risk hiring use is blocked unless the caller declares a *registered* use-case
# id, and a prohibited social-scoring use is blocked outright (403).
#
# Requires: demo/env.local.sh (copy from env.local.sh.example) sourced first, and the
# policy applied in `enforce` mode with the mock judge (demo/config.json.example).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
: "${C3_GW_URL:?set C3_GW_URL (source demo/env.local.sh)}"
CLIENT="${C3_CLIENT_ID:-demo-client-1}"
REG="${C3_REGISTERED_USE_CASE:-hr-screening-2026}"
UNREG="${C3_UNREGISTERED_USE_CASE:-rogue-hiring-bot}"

send() {
  local title="$1"; shift
  local file="$1"; shift
  echo "════════════════════════════════════════════════════════════════════"
  echo "▶ $title"
  echo "  extra headers: $*"
  echo "────────────────────────────────────────────────────────────────────"
  curl -sS -D - -o /tmp/c3_body.$$ \
    -H "content-type: application/json" \
    -H "x-client-id: ${CLIENT}" \
    "$@" \
    --data @"${HERE}/${file}" \
    "${C3_GW_URL}" | sed -n '1,20p'
  echo "  ── response body ──"
  sed 's/^/  /' /tmp/c3_body.$$ | head -20
  rm -f /tmp/c3_body.$$
  echo
}

# 1) Minimal-risk use → allowed; upstream receives x-jev-ai-act-risk: minimal_risk.
send "Minimal risk: summarise an internal document → ALLOW" \
     "requests/minimal_summarise.json"

# 2) High-risk hiring use, NO registered use-case id → BLOCK (403).
send "High risk: hiring decision, unregistered use case → BLOCK (403)" \
     "requests/high_risk_hiring.json" -H "x-ai-use-case-id: ${UNREG}"

# 3) Same high-risk hiring use, WITH a registered use-case id → allowed, tagged high_risk.
send "High risk: hiring decision, REGISTERED use case → ALLOW (tagged high_risk)" \
     "requests/high_risk_hiring.json" -H "x-ai-use-case-id: ${REG}"

# 4) Prohibited practice: social scoring → BLOCK (403), onProhibited: block.
send "Prohibited: social scoring of citizens → BLOCK (403)" \
     "requests/prohibited_social_scoring.json"

echo "Done. Expect: #1 200, #2 403, #3 200, #4 403 (with mock judge in enforce mode)."
