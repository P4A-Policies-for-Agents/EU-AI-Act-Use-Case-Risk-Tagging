# C3 Provisioning Runbook (Phase B — outward actions)

> Placeholders in `<...>`. Real tenant ids / keys stay in local, gitignored files —
> never commit them. This policy is **inbound / request-leg**, so unlike an outbound
> policy it does **not** need `--upstreamId` when applied.

Prereqs: `anypoint-cli-v4` authed (Sandbox env), PDK 1.10 toolchain, `cargo-anypoint`
1.9.0, the demo LLM mock (A2D). Gateway: **omni-gw-small** (has a public URL; the
internal-only instance would 404).

## 1. Publish the policy (DEFINITION FIRST)

```bash
# Definition first — the flex build's config-gen fetches it.
make -C eu-ai-act-risk-tagging-definition release   # pdk policy-definition publish
make -C eu-ai-act-risk-tagging-flex       release   # build-asset-files + build + policy-wasm publish
```

Both assets: group `030e0aac-30d9-460f-9234-428c16a123c4`, version **1.0.0**.
Applyable policy asset = the definition assetId `eu-ai-act-risk-tagging`.

## 2. Stand up the mocked upstream LLM API (A2D)

Use the A2D MCP server to create a REST API from `demo/api-spec.yaml` — an
OpenAI-compatible `/v1/chat/completions` mock configured to **echo** the injected
`x-jev-ai-act-risk` header into the response body (`received_risk_tag`) so the tag is
visible in the demo. Publish it to Exchange as a REST asset and note its asset id.

## 3. Manage + deploy the API on the Flex Gateway

```bash
anypoint-cli-v4 api-mgr:api:manage --isFlex --type rest --withProxy \
  <group>/<mock-rest-asset-id>/<version>
# Deploy to omni-gw-small
anypoint-cli-v4 api-mgr:api:deploy --target <gwTargetId> \
  --gatewayVersion <gwVer> --overwrite <apiInstanceId>
```

`<gwTargetId>` / public host: see the **live-demo-gateway** memory (omni-gw-small).

## 4. Apply C3

```bash
# Inbound policy — no --upstreamId needed. Config from demo/config.json (mock judge,
# enforce, require_registration) or your real-judge config.
anypoint-cli-v4 api-mgr:policy:apply <apiInstanceId> \
  <group>/eu-ai-act-risk-tagging/1.0.0 \
  --config "$(cat demo/config.json)"
```

(No MCP Support policy is required — this is a REST route, not an MCP server.)

## 5. Verify live

```bash
source demo/env.local.sh      # C3_GW_URL = https://<omni-gw-small-host>/<route>/v1/chat/completions
./demo/demo.sh
```

Expect #1 → 200 (`received_risk_tag: minimal_risk`), #2 → 403 (`high_risk`,
unregistered), #3 → 200 (`high_risk`, registered), #4 → 403 (`prohibited_practice`).
Check API Manager for the policy-violation events raised on #2/#3/#4.

## Gotchas (from the S1 / CDGC builds)

- Never delete + recreate the same Exchange version → schema-not-found; **bump** instead.
- Impl publish rejects `metadata.labels.description` > 256 chars (folded YAML runs
  longer than it looks — C3's is ~242).
- `~typesafe/jev-latest` is a **decisions** model → `jevProvider: typesafe`, not
  `openrouter`. Set `jevTimeoutMs: 3000` for the real judge.
- CLI has no `--order` flag; order = apply sequence.
