# Provider Operations — seats, upstreams, fallbacks, key rotation

Operational runbook for changing anything in the fleet's model supply chain.
Roster doctrine (who sits in which seat and why) lives in
`docs/AGENT_ROUTING.md`; spend analysis in `docs/TOKEN_ECONOMICS.md`;
classifier mechanics in `docs/PERMISSION_BOUNDARY.md`. This doc owns the
*how* — config layers, procedures, verification.

## Architecture

```
CC pane → cc-fleet profile env (ANTHROPIC_DEFAULT_*_MODEL)
        → litellm proxy 127.0.0.1:4000 (launchd com.manifold.litellm-proxy)
        → upstream provider (z.ai / opencode Zen / kimi)
```

The proxy is the single choke point: every seat's traffic, the permission
classifier, and the one-shot tool all cross it. A proxy restart blips every
pane; a bad `config.yaml` fails boot and freezes everything until fixed.

## Config layers — what is source of truth for what

| Layer | File | Owns |
|---|---|---|
| Seat names + consumer registry | `scripts/fleet_seats.toml` (repo) | seat = subscription account (`kimi`/`zai`/`opencode`, never a model name); every file that carries a seat token. `seat_tool check` is the drift gate; `seat_tool rename` migrates all consumers + secret files in one transaction |
| Seat slot map | `~/.config/cc-fleet/providers.toml` | which model fills each slot (haiku/sonnet/opus) per profile |
| Upstreams + routing | `~/.config/litellm/config.yaml` | model_list (api_base, key env, pricing), router_settings (retries, fallbacks) |
| Virtual keys | `~/.config/litellm/key-*.json` | per-key model allow-lists (e.g. k3-lead) |
| Upstream API keys | cc-fleet secret backend | fetched by `start-proxy.sh` via `cc-fleet keyget <name>`; never on disk |
| Proxy runtime | `~/.local/litellm-venv/` + `start-proxy.sh` + plist | process, port, DB, log (`~/.config/litellm/proxy.log`) |

None of this is in the repo. The config files carry their own rationale
comments; this doc is the procedure layer.

**Hand-editing cc-fleet profile JSON is reverted silently by `cc-fleet
repair`** (2026-07-25 drift incident). `providers.toml` is the durable
source; edit it only via `scripts/seat_tool.py`.

**A new model is invisible to a seat until it is in that key's allow-list**
(BUG-lng (Add deepseek-v4-flash to k3-lead key allow-list)). The model_list entry alone is not enough.

## Procedures

### Swap which model fills a seat slot

`scripts/seat_tool.py assign <slot> <model>` — edits providers.toml, runs
repair, verifies the profile, warns on litellm/tier-guard/SHORT_LABEL gaps.
The teammate naming guard needs no sync: it derives the slot map from the
session env (`ANTHROPIC_DEFAULT_<TIER>_MODEL`) at spawn time. Never
hand-edit profiles. `seat_tool.py show` is the read oracle.

Slots are semantic, not provider-shaped: `sonnet` = default work model,
`haiku` = fast/classifier-adjacent, `opus` = strong consult. The auto-mode
classifier resolves off these slots (PERMISSION_BOUNDARY.md section 2 (Which model runs it)), so a slot
swap changes what gates every permission decision — say so in the commit.

### Add or repoint an upstream

Adding a MODEL to an existing provider is `scripts/seat_tool.py onboard
<model> --provider <seat> [--label L] [--slot S] [--costs IN OUT CACHE]` —
it does the config entry, retry policy, key allow-lists, proxy restart, live
verification call, and guard maps in one run; `offboard <model>` reverses it.
The steps below remain the procedure for a NEW provider upstream.

1. `config.yaml` model_list: copy the nearest sibling entry; set `api_base`,
   `api_key: os.environ/<KEY>`, list-rate pricing, `supports_reasoning` if
   the model reasons.
2. If the key env var is new: add it to the cc-fleet secret backend
   (`cc-fleet keyget` namespace) and to `start-proxy.sh`.
3. Add the model to every virtual key's allow-list that should reach it.
4. `launchctl kickstart -k gui/501/com.manifold.litellm-proxy`.
5. Verify: startup log lists the model under "Set models"; a
   `/v1/chat/completions` call returns 200; SpendLogs shows the deployment
   (`model` column = the deployment that actually served, not the request).

### Add or change a fallback

`router_settings.fallbacks: [{<group>: [<fallback-group>]}]`. One direction
per line; primaries stay primaries. Fallbacks fire on timeout/connection/
429/5xx and absorb upstream capacity waves that would otherwise freeze
seats (the classifier fails closed — an unavailable classifier blocks every
unreviewed action).

Verify by forced failure, never by assumption: point the primary's
`api_base` at a dead port, restart, fire a test call, confirm SpendLogs
shows the *fallback* deployment serving it, restore, restart, confirm the
primary serves again. Two restart blips; do it when no wave is mid-land.

### Rotate an upstream API key

Update the cc-fleet secret backend entry, then restart the proxy —
`start-proxy.sh` re-fetches on boot. No config edit.

### Update pricing

Subscription seats log notional list-rate spend. On any provider price
change: `input/output_cost_per_token` (+ cache rates) in config.yaml, the
plan-cost variables on the fleet-value Grafana dashboard, and the `RATES`
table in `scripts/claude_usage_export.py` for the Anthropic path.

### Change reasoning effort (lead vs lanes)

The lead seat runs K3 via the `k3m` alias; lanes are native Agent subagents
on `kimi-for-coding`. The two are pinned at different layers:

- **Lead**: `cc-fleet edit kimi --effort high` (values low|medium|high|
  xhigh|max) — writes `effort` in providers.toml, which cc-fleet regenerates
  into the profile's `effortLevel`. cc-fleet OWNS effort: it scrubs
  `CLAUDE_CODE_EFFORT_LEVEL` from the spawned process env, so an env-var
  override in the alias or profile is dead on arrival (verified 2026-09-03 —
  var present in the launch env, absent in the claude process).
- **Lanes**: pinned at the PROXY, not the harness — the `kimi-for-coding`
  litellm entry deliberately has no `supports_reasoning`, so the proxy
  strips thinking/effort from every lane request before it reaches Kimi
  (added 2026-08-25 for the 64-token classifier call). No harness setting
  can raise lane effort through this entry; a lane agent definition
  (`~/.claude/agents/lane.md`, `effort: low` frontmatter) is belt-and-
  braces only. To give lanes real reasoning: add `supports_reasoning: true`
  to the entry — but then the classifier's tiny calls reason too and burn
  budget, so gate that decision on the classifier separately.
- **K3 API accepts only low/high/max** (platform.kimi.ai reasoning-effort
  guide); medium/xhigh from the harness need proxy mapping if ever used.

Verify a change with the session header ("k3 with high effort") and, for
lane isolation, `ps eww` on the lead process for `effortLevel` inheritance
is NOT the oracle — the proxy strip is; confirm via a lane call's
SpendLogs reasoning-token count.

## Always-running services — the observability stack

Five background services keep the fleet observable. All run as user launchd
jobs (`launchctl list | rg 'manifold|mxcl'` is the liveness oracle; the
three homebrew ones also answer to `brew services list`). Data flow:

```
CC transcripts ──(hourly export)──┐
litellm proxy ──(SpendLogs)───────┤→ postgres :5432 ──→ grafana :3000 (dashboards)
              └─(/metrics)──→ prometheus :9090 ────────↗
```

| Service | launchd label | If it dies | Restart |
|---|---|---|---|
| litellm proxy :4000 | `com.manifold.litellm-proxy` | **every seat freezes** (classifier fails closed); log `~/.config/litellm/proxy.log` | `launchctl kickstart -k gui/501/com.manifold.litellm-proxy` |
| Postgres 16 :5432 | `homebrew.mxcl.postgresql@16` | proxy loses SpendLogs, dashboards empty | `brew services restart postgresql@16` |
| Grafana :3000 | `homebrew.mxcl.grafana` | dashboards unreachable; data unharmed | `brew services restart grafana` |
| Prometheus :9090 | `homebrew.mxcl.prometheus` | health metrics gap (metrics are scrape-time; the gap is permanent) | `brew services restart prometheus` |
| Claude usage export (hourly) | `com.manifold.claude-usage-export` | Anthropic rows go stale — value dashboard freshness row turns red at 2 days | `launchctl kickstart -k gui/501/com.manifold.claude-usage-export` |

Grafana provisions datasources and dashboards from files: repo
`scripts/grafana/*` is the source, deployed by copy to
`/opt/homebrew/etc/grafana/provisioning/{datasources,dashboards}/` and
`/opt/homebrew/etc/grafana/dashboards/` (30 s auto-reload). Edit in the
repo, copy out — never edit only the deployed copy.

## Verification oracles

- **Which deployment served a call:** SpendLogs `model` column
  (`psql postgresql://litellm:litellm-local@localhost:5432/litellm`).
  Requested-flash-but-served-`anthropic/glm-4.7` = a live fallback reroute.
- **Upstream health:** `proxy.log` — "Error from provider" strings name the
  upstream's own failure, distinguishing provider wobble from proxy trouble.
- **Fleet rates/errors/latency:** Grafana `manifold-fleet` dashboard
  (Prometheus, from 2026-07-25 only); SpendLogs is all-time ground truth.
- **Anthropic rows on `manifold-value`:** exported hourly by launchd job
  `com.manifold.claude-usage-export` (plist versioned in `scripts/grafana/`,
  installed copy in `~/Library/LaunchAgents/`; log
  `~/.config/litellm/claude-usage-export.log`). The dashboard's top
  freshness row goes orange at 1 day stale, red at 2 — red means the feed
  stopped, not that a seat idled.

## Hazards

- **litellm upgrades wipe the venv patches** — reapply
  `.claude/hooks/litellm_patches_reapply.py` after any upgrade (and re-pip
  `prometheus-client` into the venv).
- **Classifier coupling:** the classifier is a session-sticky resolution
  off the slot map (PERMISSION_BOUNDARY.md section 2 (Which model runs it)). A demoted pane never
  recovers — restart it, don't debug it.
- **Fallback legs are load-bearing subscriptions.** As of 2026-07-26 the
  z.ai/GLM plan is the fallback for both deepseek groups; cancelling it
  re-exposes every seat to Zen waves. When evaluating provider value,
  price the fallback duty, not just seat traffic.
- **Proxy boot takes ~15–20 s**; test calls fired immediately after
  kickstart race the bind (connection refused ≠ config broken — check the
  startup log first).
