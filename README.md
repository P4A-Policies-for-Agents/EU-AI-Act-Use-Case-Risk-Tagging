# EU AI Act Use-Case Risk Tagging — MuleSoft Omni/Flex Gateway Policy

An **inbound (request-leg) policy** for the MuleSoft Omni/Flex Gateway that reads the
**system prompt and latest user message** of an LLM request, asks a typed **Jev**
judge which **EU AI Act risk band** the *use case* falls in, and tags the request so
risky uses are logged and reviewed:

- **`prohibited_practice`** — social scoring, manipulation of vulnerabilities,
  untargeted facial-image scraping, emotion recognition at work/school, crime
  prediction from personality.
- **`high_risk`** — hiring/worker management, education access, credit/insurance,
  essential public services, law enforcement, migration, justice.
- **`transparency_obligation`** — a chatbot talking with people, or generating/
  manipulating public-facing media.
- **`minimal_risk`** — coding help, summarising internal documents, search, etc.
- **`uncertain`** — the judge's confidence was below `minConfidence`.

> ## ⚠️ This is a triage aid, not a legal determination.
> The tag is an **indicative** signal to help humans find and review risky AI uses.
> It does **not** decide whether a system is lawful under the EU AI Act. Treat every
> tag — especially `prohibited_practice` and `high_risk` — as a prompt for human
> review, not as a compliance verdict.

The system prompt usually *defines* the use case, but it lives in the request body,
invisible to ordinary API governance. This policy puts that classification on the
request path — at the gateway, before the call reaches the model — as a header, a
policy-violation event, and (optionally) a block.

Built with the PDK, Rust → `wasm32-wasip1`, split-model. Applies to **REST** and
**HTTP** LLM routes (`assetTypes: rest,http`). It understands OpenAI Chat Completions,
OpenAI Responses, and Anthropic Messages request shapes.

It is one of the **TypeSafe-Jev** gateway policy family: a typed **"System 1" judge**
returns *probabilities*, never generated text — so there is **no model in the request
path** and nothing generated is ever inserted. All decisions and mutations stay in Rust.

---

## How it tags — parse → sample → judge → decide → act

On the **request leg**, for a JSON LLM request on an in-scope route:

1. **Strip** any client-supplied `riskHeader` (so a caller can't spoof the tag), then
   **parse** the request to extract the system prompt (`application_instructions`) and
   the last user message (`user_request`).
2. **Sample** deterministically: the judge is called for the fraction of
   `(client_id, system_prompt)` pairs selected by a stable hash (`sampleRate`, default
   **0.1**), so the same pair always decides the same way — retries are consistent and
   tests are stable. *(A cross-request dedup cache that always evaluates a genuinely
   new pair once, with `cacheTtlSeconds`, is a documented Data Storage enhancement;
   the hash gate is the dependency-free floor — see [VERIFY](#verify--notes).)*
3. **Judge** — send the two state fields to a typed Jev judge that returns a
   `risk_category` **choice** (+ confidence) and an `affects_individual_decision`
   **noul** probability. Never generated text.
4. **Decide**:
   - `risk_category` tag = the choice when confidence ≥ `minConfidence`, else `uncertain`.
   - `prohibited_practice` with confidence ≥ `prohibitedConfidence` (0.8) → `onProhibited`
     (`flag` | `block`, default `flag`).
   - `high_risk`, **or** `affects_individual_decision ≥ affectsIndividualThreshold`
     (0.7) → `onHighRisk` (`flag` | `require_registration`). `require_registration`
     denies unless the request carries a `useCaseHeader` (`x-ai-use-case-id`) value
     present in `registeredUseCases`.
5. **Act** (enforce mode): set upstream `x-jev-ai-act-risk: <tag>`; raise a
   **policy-violation event** for prohibited/high-risk uses; deny with **HTTP 403**
   (+ explanation) when the decision is `block`.

`mode` is **`enforce`** (act) / **`shadow`** (compute + log, never mutate or deny —
the safe first deployment) / **`off`**. `failMode` is **`open`** (pass through
untagged on judge error/timeout) / **`closed`** (deny). Defaults: **`shadow`**,
**`open`**.

## Judge providers

Identical to the rest of the Jev family: `jevProvider` = `mock` (deterministic
in-policy, needs `allowMock: true`) / `typesafe` (decisions API, default) / `openai` /
`openrouter` / `litellm` / `custom` (OpenAI-compatible chat, driven as a JSON
classifier) / `cloudflare`. Default model `~typesafe/jev-latest`.

**`~typesafe/jev-latest` is a *decisions* model** — serve it with
`jevProvider: typesafe` (decisions envelope on `/api/alpha/decisions`), even when
fronted by OpenRouter (`jevService: https://openrouter.ai`). Setting
`jevProvider: openrouter` for it fails (that's the chat endpoint).

## Configuration (highlights)

| Property | Default | Purpose |
|---|---|---|
| `mode` | `shadow` | `enforce` / `shadow` / `off` |
| `failMode` | `open` | `open` / `closed` on judge error/timeout |
| `jevProvider` | `typesafe` | judge provider (`mock` for offline) |
| `jevService` | — (required) | judge base URL (`format: service`) |
| `sampleRate` | `0.1` | fraction of `(client, system-prompt)` pairs judged (hash-based) |
| `onProhibited` | `flag` | `flag` / `block` for `prohibited_practice` (conf ≥ 0.8) |
| `onHighRisk` | `flag` | `flag` / `require_registration` for high-risk uses |
| `registeredUseCases` | `[]` | allowlisted `x-ai-use-case-id` values (for `require_registration`) |
| `useCaseHeader` | `x-ai-use-case-id` | header carrying the declared use-case id |
| `riskHeader` | `x-jev-ai-act-risk` | upstream tag header (also stripped inbound) |
| `clientIdHeader` | `x-client-id` | caller id for the sampling/cache key |
| `minConfidence` | `0.6` | below this, the tag is `uncertain` |
| `prohibitedConfidence` | `0.8` | confidence gate for the prohibited action |
| `affectsIndividualThreshold` | `0.7` | noul gate for the high-risk branch |
| `routes` | `[]` | path-prefix allowlist (empty = all matched routes) |
| `prohibited/highRisk/transparency/minimalCriteria` | built-in | criteria-pack overrides (legal-editable) |
| `cacheTtlSeconds` | `86400` | advisory; reserved for the dedup-cache enhancement |

Full schema: [`eu-ai-act-risk-tagging-definition/gcl.yaml`](eu-ai-act-risk-tagging-definition/gcl.yaml).
Criteria text is a **versioned question pack** — a compliance team can revise the four
category descriptions via the `*Criteria` properties without a code change.

## Layout & build (split-model)

```
eu-ai-act-risk-tagging-definition/   # gcl.yaml + exchange.json — the applyable policy asset
eu-ai-act-risk-tagging-flex/          # Rust/wasm implementation
  src/ common.rs    # Mode/FailMode/Decision/Bands, glob, fnv1a, deterministic sampling
      payloads.rs   # OpenAI Chat / Responses + Anthropic Messages request parsing
      screen.rs     # Signals, tag classification, decision logic, eval gate
      jev.rs        # Provider / JevSettings / evaluate + chat/typesafe/mock signal parsing
      lib.rs        # request-leg threading (headers+body), header set / 403 / violation, entrypoint
demo/                                 # live A2D + Flex Gateway demo (see demo/PROVISION.md)
```

```bash
# Publish the DEFINITION FIRST — `make release` on the flex impl runs config-gen against it.
make -C eu-ai-act-risk-tagging-definition release   # pdk policy-definition publish
make -C eu-ai-act-risk-tagging-flex       release   # build-asset-files + build + policy-wasm publish
```

Requires PDK 1.10 (feature `enable_stop_iteration`, MIN_FLEX_VERSION 1.9.3),
cargo-anypoint, anypoint-cli-v4. **27 unit tests** (`make -C eu-ai-act-risk-tagging-flex unit`).

## Live demo

A mocked OpenAI-compatible `/v1/chat/completions` upstream behind a Flex Gateway route
on **omni-gw-small**, C3 in `enforce` with the deterministic `mock` judge. The demo
shows the **same hiring request blocked or allowed by whether the caller declares a
registered use case**, a minimal-risk use passing through tagged, and a prohibited
social-scoring use blocked outright. See [`demo/PROVISION.md`](demo/PROVISION.md) to
stand it up and [`demo/WALKTHROUGH.md`](demo/WALKTHROUGH.md) for the story.

```bash
cp demo/config.json.example demo/config.json      # mock judge — no creds
cp demo/env.local.sh.example demo/env.local.sh     # set C3_GW_URL
source demo/env.local.sh && ./demo/demo.sh
```

## VERIFY / notes

- **Per-pair dedup cache:** the plan calls for *always* evaluating a genuinely new
  `(client_id, system_prompt_sha256)` pair once and caching the decision for
  `cacheTtlSeconds`. This build ships the **deterministic hash sampling** floor (same
  pair → same decision, no state) so behaviour is stable and dependency-free; the
  cross-request cache is layered on PDK **Data Storage** (`DataStorageBuilder`,
  `store(..., StoreMode::Absent, ...)` keyed by the pair hash) as a production
  enhancement. `cacheTtlSeconds` is declared for that path and is otherwise advisory.
- **Sampling is hash-based, not random** (acceptance criterion) so unit tests are
  deterministic — see `screen::should_evaluate` and its test.
- Tag is an indicative triage signal — see the disclaimer above; also stated in the
  403 body and the walkthrough.
