# Fleet stack handover — how to rebuild this harness setup elsewhere

Written to be handed to someone else's Claude so it can reimplement the same
thing on their machine and repo. Everything here is as-built on a Mac
(homebrew, launchd) as of 2026-07-30. No secrets. Repo-side files referenced
by path so they can be copied.

The MANIFOLD-internal versions of this material are `docs/PROVIDER_OPERATIONS.md`
(runbook) and `docs/AGENT_ROUTING.md` (who sits in which seat and why). This
doc is the portable summary of both plus the observability stack.

## What the setup buys you

Claude Code panes that run on non-Anthropic models (Kimi, z.ai GLM, DeepSeek
via OpenCode) with per-seat accounting, per-seat access control, and live
dashboards for spend, tokens, latency and error rates. One local proxy is the
only choke point, so every question about "what did last night cost, on which
model, for which session" has one answer.

Two things it does **not** buy: real cost control (all the accounts here are
flat-rate subscriptions, so every dollar figure is notional list-rate
equivalent, not money spent), and better routing decisions (routing is decided
by a human or the lead session at brief time, not by a router).

## Shape

```
Claude Code pane
  └─ cc-fleet profile env (ANTHROPIC_BASE_URL + ANTHROPIC_DEFAULT_{HAIKU,SONNET,OPUS,FABLE}_MODEL)
      └─ litellm proxy  127.0.0.1:4000   (launchd, KeepAlive)
          ├─ upstream: api.kimi.com/coding        (Anthropic wire)
          ├─ upstream: open.bigmodel.cn/api/anthropic  (Anthropic wire)
          └─ upstream: opencode.ai/zen/go/v1      (OpenAI wire)
          ├─ SpendLogs ──▶ Postgres :5432 ──▶ Grafana :3000
          └─ /metrics  ──▶ Prometheus :9090 ─────▶
Claude Code transcripts ──(hourly launchd export)──▶ Postgres (Anthropic path)
```

Five always-on services, all user launchd jobs, all bound to 127.0.0.1:
litellm proxy, Postgres 16, Grafana, Prometheus, and an hourly usage-export
job. `launchctl list | rg 'manifold|mxcl'` is the liveness check; the three
homebrew ones also answer to `brew services list`.

## Layer 1 — cc-fleet (the pane launcher)

`cc-fleet` is a Go CLI (v0.3.2 here, `~/.local/bin/cc-fleet`) that manages
third-party provider profiles for Claude Code. It generates
`~/.claude/profiles/<provider>.json`, holds API keys in a pluggable secret
backend, and launches Claude Code sessions against a provider.

Commands that matter: `add`, `edit`, `repair`, `list`, `models`, `keyget`,
`run` (interactive pane on a provider), `subagent` (one-shot headless job),
`codex-proxy` (its OpenAI↔Anthropic conversion daemon), `doctor`.

Durable source of truth is `~/.config/cc-fleet/providers.toml`. Each provider
entry maps the four Claude Code model slots:

- `fast_model` → `ANTHROPIC_DEFAULT_HAIKU_MODEL`
- `default_model` → `ANTHROPIC_DEFAULT_SONNET_MODEL`
- `strong_model` → `ANTHROPIC_DEFAULT_OPUS_MODEL`

**Never hand-edit the generated profile JSON** — `cc-fleet repair` silently
rewrites it from `providers.toml` and wipes anything you added, including env.
There is no env passthrough. Deliver extra env another way (a shell alias
around `cc-fleet run` works; that is what we do).

Two entries per account here: a live one pointed at the proxy
(`base_url = "http://127.0.0.1:4000"`), and a disabled `<name>-upstream` one
holding the real provider URL and the real key. `cc-fleet keyget <name>`
returns the litellm *virtual* key (only the proxy accepts it);
`cc-fleet keyget <name>-upstream` returns the provider key, which is what any
provider-side usage API needs. Virtual keys 401 upstream.

Real example (keys live in the secret backend, not the file):

```toml
[kimi]
base_url = "http://127.0.0.1:4000"
default_model = "glm-4.7"      # sonnet slot
strong_model  = "glm-5.2"      # opus slot
fast_model    = "deepseek-v4-flash"   # haiku slot
effort = "low"
default_permission = "auto"

[opencode]                     # OpenAI-wire provider
base_url = "http://127.0.0.1:18923"   # cc-fleet's conversion daemon
protocol = "openai-chat"
upstream_url = "http://127.0.0.1:4000/v1"
default_model = "deepseek-v4-flash"
```

## Layer 2 — litellm proxy

Install into its own venv (`~/.local/litellm-venv`), plus `prometheus-client`.
Run it from a launchd plist (`com.manifold.litellm-proxy`, RunAtLoad +
KeepAlive) that execs a small launcher script. The launcher's whole job is to
pull provider keys into the process environment and nothing else — keys never
land on disk:

```sh
export PATH="$HOME/.local/bin:/opt/homebrew/bin:$PATH"   # launchd's PATH is bare
export ZAI_KEY="$(cc-fleet keyget zai-upstream)"
export OPENCODE_KEY="$(cc-fleet keyget opencode-upstream)"
export KIMI_KEY="$(cc-fleet keyget kimi-upstream)"
export LITELLM_MASTER_KEY="sk-$(<~/.config/litellm/master_key)"
export LITELLM_DB_URL="postgresql://litellm:litellm-local@localhost:5432/litellm"
exec ~/.local/litellm-venv/bin/litellm --config ~/.config/litellm/config.yaml \
  --host 127.0.0.1 --port 4000
```

Restart: `launchctl kickstart -k gui/$(id -u)/com.manifold.litellm-proxy`.
Boot takes 15–20 s — a test call fired immediately gets connection-refused,
which is not the same as a broken config. Health:
`curl 127.0.0.1:4000/health/liveliness`. Log: `~/.config/litellm/proxy.log`.

`config.yaml` essentials:

```yaml
general_settings:
  master_key: os.environ/LITELLM_MASTER_KEY
  database_url: os.environ/LITELLM_DB_URL

litellm_settings:
  callbacks: ["prometheus"]
  use_chat_completions_url_for_anthropic_messages: true   # see gotchas

router_settings:
  model_group_retry_policy:
    deepseek-v4-flash: {BadRequestErrorRetries: 3}
  fallbacks: [{"deepseek-v4-flash": ["glm-4.7"]}]

model_list:
  - model_name: k3
    litellm_params:
      model: anthropic/k3
      api_base: https://api.kimi.com/coding/
      api_key: os.environ/KIMI_KEY
      input_cost_per_token: 0.000003        # provider list rate
      output_cost_per_token: 0.000015
      cache_read_input_token_cost: 0.0000003
    model_info:
      max_input_tokens: 1048576
      max_output_tokens: 131072
      supports_reasoning: true              # see gotchas
  - model_name: glm-4.7
    litellm_params: {model: anthropic/glm-4.7, api_base: https://open.bigmodel.cn/api/anthropic, api_key: os.environ/ZAI_KEY, ...}
  - model_name: deepseek-v4-flash
    litellm_params: {model: openai/deepseek-v4-flash, api_base: https://opencode.ai/zen/go/v1, api_key: os.environ/OPENCODE_KEY, ...}
```

Price every model at its own provider's public list rate. For subscription
accounts that makes Spend a notional "what this traffic would cost on the
metered API" number — useful for comparing seats and for judging whether a
subscription is worth its monthly fee, useless as a budget. Never attach
`max_budget` or alerts to a subscription seat; invoices are ground truth.

**Virtual keys are the access control.** One key per seat, each carrying a
`models` allow-list, so a worker key is physically incapable of reaching an
expensive tier. That is a strictly harder guarantee than any regex or prompt
rule. Create via `POST /key/generate` with the master key; keep a local
`key-<seat>.json` copy for reference. A new model in `model_list` is invisible
to a seat until it is added to that seat's allow-list — `POST /key/update`.
Note the master-key file may store the key without its `sk-` prefix; the API
wants the prefix.

## Layer 3 — Postgres, Prometheus, Grafana

Postgres 16 via homebrew, database `litellm`, user `litellm`. litellm creates
its own schema; `LiteLLM_SpendLogs` is the all-time ground truth. Useful
columns: `model` (the deployment that actually *served* — a request for flash
showing `anthropic/glm-4.7` is a live fallback reroute), `session_id`
(litellm lifts the real Claude Code session UUID out of the `metadata.user_id`
CC sends), `metadata.usage_object` (full input/output/cache-read/cache-write/
reasoning counts).

Prometheus: `brew install prometheus`. litellm's `/metrics` 307-redirects and
requires bearer auth, so scrape with
`bearer_token_file: /opt/homebrew/etc/litellm_scrape_token` (the master key,
chmod 600). Bind to 127.0.0.1 in `prometheus.args`. Useful labels: `model`,
`requested_model`, `api_key_alias`, `status_code`. Counters start at install
and reset on proxy restart, so Prometheus answers ops questions only — never
history.

Grafana: `brew install grafana`, `http_addr = 127.0.0.1`, dark theme, default
login admin/admin (localhost-only). Provision datasources and dashboards from
files; keep the JSON in the repo and deploy by copy:

- repo `scripts/grafana/datasource-{prometheus,postgres}.yaml`
  → `/opt/homebrew/etc/grafana/provisioning/datasources/`
- repo `scripts/grafana/*.json` → `/opt/homebrew/etc/grafana/dashboards/`
  (30 s auto-reload)

Three dashboards here, all tagged `manifold` with a links-by-tag block so they
render as nav tabs in each other's headers — a new dashboard joins the tabs by
carrying the tag:

- **fleet** (Prometheus) — spend 1h/24h/all-time, $/h, token in/out/cache/
  reasoning rates, req/min by model+status, p50/p95 latency and TTFT.
- **value** (Postgres) — is each subscription worth its fee. Plan costs are
  editable dashboard textbox variables, not hardcoded. Each provider's traffic
  is repriced at *that provider's own* list rate; no cross-provider
  counterfactual.
- **daily** — day-shaped rollup.

Grafana provisioning has two traps that cost us real time. First, **pin
datasource uids in the provisioning yaml** — dashboards reference datasources
by uid, and a dangling uid fails *silently*: health check passes, nothing in
`grafana.log`, every panel just says "No data". Grafana also cannot rewrite an
existing provisioned datasource's uid in place (it refuses to start with
"data source not found"); the change must be `deleteDatasources:` plus create
in the same yaml. Second, raw-SQL panels need `datasource` and
`editorMode: "code"` on **each target**, not only on the panel. With no image
renderer installed there is no screenshot oracle, so verify a dashboard change
by executing every stored target through `/api/ds/query`.

## Layer 4 — the Anthropic path

Claude Code talking to Anthropic never crosses the proxy, so the biggest
subscription is invisible on every dashboard unless you export it. Our
`scripts/claude_usage_export.py` reads the per-message `usage` blocks out of
`~/.claude/projects/*/*.jsonl`, prices them against a local `RATES` table, and
upserts into a `manifold_anthropic_usage` table. An hourly launchd job runs it
(`--since-days 1`, ~0.3 s). Two views give every provider one row shape: a
union view over SpendLogs + Anthropic rows, and a second adding a `work_class`
guessed from token shape (SpendLogs stores no request body, so token shape is
the only work-type signal available).

**Dedupe on message uuid is load-bearing.** Resumed and forked sessions copy
earlier turns verbatim into new transcript files; a naive sum over-counts by
about 9%.

## Layer 5 — repo-side glue worth copying

- **`scripts/seat_tool.py`** — the only sanctioned way to rotate which model
  fills a slot: edits `providers.toml`, runs `cc-fleet repair`, verifies the
  regenerated profile, and warns when the litellm config or a guard hook is
  out of sync. `show` is the read oracle. Slot rotation by hand-editing
  profiles reverts silently.
- **`scripts/fleet_seats.toml`** plus `seat_tool check` — a seat-name registry
  and drift gate. Seat names are *accounts* (`kimi`, `zai`, `opencode`), never
  model names, because the quota that binds is per-account and several models
  share one. `seat_tool rename` migrates every consumer and secret file in one
  transaction.
- **`.claude/hooks/seat-identity.py`** — injects "which seat am I" into each
  session. Resolve it by matching `ANTHROPIC_DEFAULT_OPUS_MODEL` against the
  provider registry, **never** by `ANTHROPIC_BASE_URL`: post-proxy every seat
  shares `127.0.0.1:4000`, and first-match there once told lead sessions they
  were workers — an authority inversion injected straight into the lead.
  Invariant that makes it work: no two seats share a strong slot.
- **Tier guards** — `agent-tier-spawn-guard.py` (caller's tier read from the
  transcript's `message.model`; a worker-tier caller may spawn nothing) and
  `cc-fleet-tier-guard.py` (blocks `cc-fleet subagent|run|workflow` from
  worker seats). Worker sessions have Bash, and a bash `cc-fleet` call was
  otherwise one step from worker-spawning-worker.
- **`.claude/hooks/context-ceiling-guard.py`** — warn at 150K tokens, hard
  stop at 200K for worker seats, with commit/handoff/report-up still allowed.
  The lead seat is exempt, and so is any seat a human is actively typing into
  (detect via `origin.kind == "human"` in the transcript — present in every
  interactive pane, absent in every headless lane).
- **`.claude/hooks/oneshot`** — a one-shot API call as a shell tool
  (`oneshot "prompt"`, `-f file`, stdin, `--model`, auto-escalating token
  budget) against the proxy. Cheap for index triage and bounded mechanical
  chores. It has no repo access, so never use it for code review: measured, it
  fabricates citations to code that does not exist.
- **statusline** — provider seats render identity plus a small table whose
  **columns are providers, not models**, since quota is per-account. Rows: 5h
  window and 7d, expressed as % of quota where a usage API exists and as
  notional $ over the same window where none does. Per-session spend comes
  from `SELECT sum(spend) ... GROUP BY model_group WHERE session_id = <mine>`
  — the session UUID arrives on statusline stdin as `.session_id`. Do not use
  `/key/info`: it is all-time-per-key, and since lanes bill under the spawning
  seat's key it reads absurd on a fresh pane. A rolling-window variant was
  tried and reverted (reads $0 after 5h idle).

## Gotchas that will bite during a rebuild

- **litellm venv patches revert on every upgrade.** We carry local patches in
  the venv and a script to reapply them (`.claude/hooks/litellm_patches_reapply.py`).
  Upgrades revert them silently, and we have caught drift that way. Run the
  script after any pip touch, restart, then re-run a canary (`claude -p`
  headless through the proxy with `--model <worker model>`). Re-pip
  `prometheus-client` too.
- **What those patches are, in case you hit the same walls:** empty-choices
  keepalive handling; dropping `reasoning_content` from responses (one upstream
  reasons unconditionally, ignores reasoning-off params, and its
  empty-signature thinking block triggers "Content block is not a text block"
  retry storms in Claude Code); and downgrading OpenAI-style `json_schema`
  response_format to `json_object` (one upstream 400s on json_schema, and CC's
  session-title call sends it on every user prompt — 18 failures in 6 hours and
  no session titles). Every structured-output prompt CC sends self-instructs
  JSON, so nothing is lost.
- **`use_chat_completions_url_for_anthropic_messages: true`** is required to
  reach an OpenAI-wire upstream via `/v1/messages`; its `/responses` shape
  breaks litellm's Responses parser. Anthropic-wire upstreams are unaffected.
- **`supports_reasoning: true` in `model_info`** stops the proxy dropping the
  harness's adaptive thinking/effort. Missing it looks like the model ignoring
  effort settings.
- **The permission classifier is coupled to the slot map.** Claude Code's
  auto-mode classifier resolves its model as: server-pushed config, else
  `ANTHROPIC_DEFAULT_SONNET_MODEL` if that value passes CC's validity check,
  else (when the main model equals the fable slot) reroute to the opus slot,
  else the main model itself. Branch 2 is **session-sticky**: the first
  classifier call locks in on success, and any non-401 error demotes the
  session permanently. A demoted pane never recovers — restart it, don't debug
  it. The classifier also **fails closed**, so an unreliable model in the
  sonnet slot freezes permissions. Dedicate that slot to your most reliable
  cheap model and spawn nothing on it. Cost floor is ~27k prompt tokens per
  classified action (cached); `permissions.allow` entries bypass it entirely
  and are the only free lever. Debug oracle:
  `claude --debug -p '…' --permission-mode auto`, then grep
  `~/.claude/debug/latest` for `classifier_request_started`.
- **Fallbacks are load-bearing.** One upstream here wobbles in minutes-long
  capacity waves, and fail-closed permissions turned each wave into a frozen
  pane. A one-directional fallback to another subscription fixed it — which
  means that second subscription is paying for fallback duty, not just its own
  traffic. Price that when evaluating whether to cancel it. Verify a fallback
  by forced failure only: point the primary's `api_base` at a dead port,
  restart, fire a call, confirm SpendLogs shows the fallback deployment
  serving, restore.
- **Some gateways wrap upstream 5xx as 400.** litellm won't retry a 400 by
  default; `BadRequestErrorRetries` scoped to that one model group fixes it
  without weakening fail-fast elsewhere.
- **Spend on one virtual key is a slice, not the whole.** Providers meter the
  single upstream key account-wide, but if the lead spawns lanes under the
  lead's key, a non-lead seat's own key reads ~$0 while the provider console
  shows real usage. Whole-account truth is
  `sum(spend) FROM LiteLLM_SpendLogs WHERE model LIKE '%<family>%'`.
- **Native subagents inherit the parent's session_id**, so lane spend rolls
  into the spawning seat. Headless one-shot jobs are separate processes and
  get their own ids.
- **Not every subscription has a usage API.** We re-probed one plan
  exhaustively — every plausible usage/limits/quota/balance path 404s, a live
  completion returns no `ratelimit-*` headers, and the docs describe none. For
  that account, notional ledger dollars are the best available proxy. Probe
  once, write down the result, don't re-probe.

## Routing doctrine, in three lines

The parts worth copying are not technical. One judgment-tier model is the only
orchestrator and the only seat that lands work — never worker-orchestrates-
worker at any depth, which is the failure that killed our first unattended
runs. Workers get fully-decided briefs in prescriptive imperatives ("run this
exact command", "read this file:line"), make exactly one commit, then stop and
report for review. Review throughput is the cap on parallelism, not how many
workers you can launch. Weak-model briefs must carry the operational recipe
inline, because hooks deny but do not teach.

One measured caveat on cheap tiers: one-shot calls to reasoning models are an
analysis and triage tier, not a code-generation tier. Two of ours burn the
entire completion budget on reasoning and return empty content on code tasks.
Code with a structural crux goes to a tool-using session that iterates against
the compiler.

## Rebuild order

1. Postgres 16, database + user.
2. litellm in a venv, `config.yaml` with one model, master key, launchd plist,
   launcher script pulling keys from wherever your secrets live. Verify
   liveliness and one real completion.
3. Virtual key per seat with `models` allow-lists.
4. cc-fleet: install, `add` each provider pointed at the proxy, plus a
   disabled `-upstream` entry per account holding the real URL and key.
   `cc-fleet run <provider>` and confirm the pane answers and that SpendLogs
   gets a row.
5. Prometheus with the bearer token; Grafana with provisioned datasources
   (pinned uids) and one dashboard.
6. The transcript export job if you also run on Anthropic.
7. Seat identity injection, tier guards, context ceiling — last, once the
   supply chain is stable.

Total moving parts: one proxy, one database, two web UIs, one CLI, one
transcript exporter, and a handful of hooks. The hard-won part is the gotcha
list above, not the wiring.
