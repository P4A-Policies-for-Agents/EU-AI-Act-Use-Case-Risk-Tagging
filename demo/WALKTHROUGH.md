# C3 Demo Walkthrough — EU AI Act Use-Case Risk Tagging

> **This is a triage aid, not a legal determination.** The policy attaches an
> *indicative* EU AI Act risk band to LLM traffic so a human compliance reviewer can
> find and review risky uses. It does not decide whether a system is lawful.

## The story

A platform team fronts several internal LLM endpoints with the MuleSoft Omni/Flex
Gateway. Product teams point their apps at those endpoints for all sorts of uses —
some harmless (summarising a document), some regulated (making hiring decisions), and
some that would be outright **prohibited** under the EU AI Act (social scoring of
people). Compliance has no visibility into *which* use each caller is putting the AI
to, because that intent lives in the **system prompt**, not in any API field.

C3 reads the system prompt (which usually defines the use case) and the latest user
message on the **request leg**, asks a typed **Jev** judge to classify the EU AI Act
risk band, and:

- sets an upstream header `x-jev-ai-act-risk: <band>` so downstream systems and logs
  carry the tag,
- raises a **policy-violation event** for prohibited / high-risk uses (visible in API
  Manager), and
- optionally **denies** (HTTP 403) — prohibited practices, or high-risk uses whose
  caller hasn't declared a **registered** use-case id.

Because the judge returns typed probabilities (never generated text), nothing the
model writes ever enters the request — all decisions and mutations stay in Rust.

## The scenario (mock judge, `enforce` mode)

Config: `demo/config.json.example` — `jevProvider: mock`, `allowMock: true`,
`mode: enforce`, `sampleRate: 1.0`, `onProhibited: block`,
`onHighRisk: require_registration`, `registeredUseCases: ["hr-screening-2026"]`.

| # | Request (system prompt) | Extra header | Judge band | Outcome |
|---|---|---|---|---|
| 1 | "You are a helpful assistant. Summarise the internal document…" | — | `minimal_risk` | **200** — passes; upstream receives `x-jev-ai-act-risk: minimal_risk` |
| 2 | "You are a hiring-decision assistant… recommend who to hire." | `x-ai-use-case-id: rogue-hiring-bot` (unregistered) | `high_risk` | **403** — blocked, `reason: high_risk`, violation event raised |
| 3 | same hiring prompt | `x-ai-use-case-id: hr-screening-2026` (registered) | `high_risk` | **200** — passes; tagged `high_risk`, violation event still recorded |
| 4 | "You build a social scoring system that rates citizens…" | — | `prohibited_practice` | **403** — blocked, `reason: prohibited_practice` |

Run it:

```bash
cp demo/config.json.example demo/config.json      # mock judge — no creds
cp demo/env.local.sh.example demo/env.local.sh     # set C3_GW_URL
source demo/env.local.sh && ./demo/demo.sh
```

The contrast that sells it: **the same hiring request is blocked or allowed purely by
whether the caller declared a registered use case** — governance the gateway can
enforce without the app changing a line of its LLM call, and without a model ever
touching the request path.

## Shadow first

Default `mode` is `shadow`: the policy computes and **logs** the tag and the would-be
action but never sets the header or denies. Deploy in shadow, watch the tags your real
traffic produces, tune `sampleRate` / `registeredUseCases` / the criteria pack, then
flip to `enforce`.

## Real judge

To run against the real TypeSafe Jev judge, set `demo/config.json` to
`jevProvider: typesafe`, `jevService: https://openrouter.ai`,
`jevModel: ~typesafe/jev-latest`, and `jevApiKey: <openrouter-key>` (kept only in the
gitignored `demo/config.json`), and raise `jevTimeoutMs` to ~3000. `~typesafe/jev-latest`
is a **decisions** model, so the provider must be `typesafe` (decisions envelope on
`/api/alpha/decisions`), not `openrouter`.
