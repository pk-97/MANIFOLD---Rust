# Token Economics — measured spend, provider options, routing rules

**Status:** MEASURED BASELINE 2026-07-23 (Fable session, final night of the Claude roster). Every number below came from local Claude Code transcripts, not from estimates — regenerate with `python3 scripts/token_report.py`. Companion to `docs/AGENT_ROUTING.md` §0 (R5 window economy, R6 executor shape, R7 CLAUDE.md size, R8 LiteLLM proxy). **Read this before making any purchasing or routing decision.**

Written the night before the K3/GLM5.2/DeepSeek roster change, so the baseline is the *old* roster at full tilt. That is the point: it is the control group.

---

## 1. How to reproduce

```
python3 scripts/token_report.py            # 30-day totals by model
python3 scripts/token_report.py --days 2   # recent window
python3 scripts/token_report.py --daily    # per-day trend
python3 scripts/token_report.py --sessions # concentration + context growth
python3 scripts/token_report.py --tools    # tool mix by seat type
```

Source: `~/.claude/projects/**/*.jsonl`, the per-message `usage` block Claude Code writes locally. Deduped by `message.id`. **Never quote a number from this doc without re-running** — it ages the moment the roster changes, which is the whole reason it exists.

---

## 2. Measured baseline

### 30 days to 2026-07-23

| Model | Messages | Cache-read MTok | Output MTok |
|---|---:|---:|---:|
| claude-sonnet-5 | 70,064 | 15,631 | 12.26 |
| claude-opus-4-8 | 25,335 | 5,896 | 22.23 |
| claude-fable-5 | 22,527 | 4,718 | 18.57 |
| k3 (Kimi) | 1,852 | 196 | 0.78 |
| claude-haiku-4.5 | 1,925 | 78 | 0.09 |
| kimi-for-coding | 555 | 75 | 0.21 |
| **TOTAL** | **122,387** | **26,598** | **54.17** |

**26.6 billion cache-read tokens per month.** Per message: **217,325 cache-read tokens in, 443 tokens out.** Every turn re-reads a near-full context window to emit a couple of paragraphs.

### 14 days (2026-07-09 → 07-23) — the orchestration-era window

- 16.8 GTok cache-read → **36 GTok/month run rate** (higher than the 30-day average; the orchestration patterns raised throughput, they did not lower consumption)
- **4,451 user turns → 144,968 model calls = 32.6 calls per user turn**
- **2,226 user turns/week**
- 710 agent sessions in 14 days (~50/day)
- Daily range 2,000–12,000 model calls; no downward trend

### 2 days to 2026-07-23

9,049 messages, 2,065 MTok cache-read, **$1,173 at metered list = $586/day ≈ $17,600/month**. **62% of messages ran on Opus or Fable** — the judgment tier was doing most of the message volume, not the mechanical tier.

---

## 3. Where the tokens actually go

Three findings, in descending order of leverage.

### 3a. Subagents are 61% of everything — and are NOT lightweight

| Seat type | Share of tokens | Avg context per call |
|---|---:|---:|
| Subagents | 61.3% | **224K** |
| Main sessions (lead + dispatcher) | 37.7% | **226K** |

Subagents carry the *same* context weight as the lead session. There is currently no such thing as a cheap worker — every spawned agent independently loads CLAUDE.md, the docs, and its own file reads, then re-reads all of it on every step. ~50 of these per day.

This is the empirical basis for `AGENT_ROUTING.md` R6 option A: the 8x worker saving is available precisely *because* the gap between worker and lead context is currently zero.

### 3b. 73% of calls produce no tool call at all

| | No tool (prose) | Bash | Edit | Read | Agent |
|---|---:|---:|---:|---:|---:|
| Main | 72.6% | 14.4% | 5.6% | 5.3% | 0.3% |
| Subagent | 75.0% | 9.5% | 7.6% | 7.4% | 0.0% |

Roughly three-quarters of all spend is models **writing prose** — narrating, summarising, reporting, explaining — at 225K context each. Not reading, not editing, not running anything. Verbosity is not a style problem here; it is the majority of the bill.

### 3c. Cost inside a session grows without limit

Average context per call, by position in the session:

| Call # | Avg context | Call # | Avg context |
|---:|---:|---:|---:|
| 0–49 | 76K | 550–599 | 460K |
| 100–149 | 176K | 900–949 | 534K |
| 300–349 | 321K | 1100–1149 | 749K |
| 400–449 | 378K | 1150–1199 | 791K |

Call 600 costs **6x** what call 20 cost, for identical work. Session totals:

- sessions under 100 calls: **4 MTok average**
- sessions of 400+ calls: **176 MTok average**
- **a long session costs 41x a short one**

**12% of sessions burn 50% of all tokens.**

> **The existing doctrine threshold is wrong.** `AGENT_ROUTING.md` §Overnight says seats rotate at "~500K observed as the sensible ceiling." At 500K every subsequent call costs half a megatoken. **Rotate at ~200K (around call 150).** This is the single largest lever in this document and it is a one-line change.

---

## 4. What it would cost metered

30-day measured volume at published per-MTok rates:

| Model | Metered cost | Cache-read share of it |
|---|---:|---:|
| Sonnet 5 | $3,724 | 84% |
| Opus 4.8 | $4,355 | 68% |
| Fable 5 | $3,643 | 65% |
| K3 + Kimi | $227 | ~97% |
| **TOTAL** | **~$11,962/mo** | |

Recent-window run rate is higher: **~$17,600/month**.

**Caveat, and it matters:** this is what *Anthropic* would charge. It is not what the work is worth. Priced against models that can actually do the job (GLM 5.2 cached $0.26, Flash cached $0.028), the same tokens cost a fraction. The figure was inflated twice — by list price, and by wrong-tier routing (62% of messages on the two most expensive models). Do not quote "$17K of value" as a defence of any subscription.

---

## 5. Published per-token prices (per MTok, 2026-07-23)

| Model | Input | Output | Cached |
|---|---:|---:|---:|
| Claude Opus 4.8 | $5.00 | $25.00 | $0.50 |
| Claude Sonnet 5 | $2.00 | $10.00 | $0.20 |
| Claude Haiku 4.5 | $1.00 | $5.00 | $0.10 |
| GLM 5.2 | $1.40 | $4.40 | $0.26 |
| DeepSeek V4 Pro | $1.74 | $3.48 | $0.145 |
| **DeepSeek V4 Flash** | **$0.14** | **$0.28** | **$0.028** |
| Qwen3.7 Max | $2.50 | $7.50 | $0.50 |
| MiniMax M3 | $0.30 | $1.20 | $0.06 |
| Kimi K3 | — | — | **~$0.80** (measured 2026-07-18) |

Source: opencode Zen published rates; Kimi from the measurement recorded in `AGENT_ROUTING.md` §Provider facts.

**K3's cache-read rate ($0.80) is higher than Opus 4.8's ($0.50).** A heavy K3 top session is the single most expensive configuration available. This is not a reason to avoid K3 — it is the reason the lead seat must stay small and rare.

---

## 6. The insight that governs every purchasing decision

**Which currency a plan meters in decides its value for this workload.**

Peter's profile: **few user turns (2,226/week), each expanding into ~33 model calls and 200K+ tokens of context.** That shape is:

- **worst case for token metering** — giant turns drain a token window fast (this is why the Anthropic Max 20x plan runs dry in ~3 days)
- **best case for prompt metering** — a 217K-token turn costs exactly the same as a one-line question

This is not vendor malice. It is a pricing-model mismatch. The fix is to buy plans metered in the currency that favours the workload, and to stop comparing across currencies without normalising.

---

## 7. Provider options evaluated

| Option | Price | Meters in | Verdict |
|---|---|---|---|
| **Kimi Allegretto** | held | flat window | **KEEP** — covers the K3 lead seat |
| **z.ai GLM Coding Plan** | Lite $18 / Pro $72 / Max $160 | **prompts** (1 prompt = 1 user query ≈ 15–20 model calls) | **BUY Pro** — right currency for this workload |
| **opencode Go** | $5 first month, then $10 | dollars ($12/5h, $30/wk, $60/mo) | **BUY** — cheapest route to Flash; overflows to Zen credits rather than blocking |
| **opencode Zen** | pay-as-you-go, $20 balance | dollars, published rates, "zero markup" | **KEEP as overflow** behind Go |
| **Anthropic Max 20x** | $200 | tokens | **KEEP through transition**, revisit with data |
| **Ollama Cloud** | Pro $20 / Max $100 | GPU-time | **REJECT** — no K3 in the cloud library (stops at K2.7); limits published only as relative multipliers ("5x more than Pro", "50x more than Free") with no absolute figure, the same unfalsifiable framing that produced the 20x/1.7x gap |
| **Entelligence router** | enterprise | — | **REJECT** — headline "same pass count as Opus at half cost" rests on 45 Terminal-Bench tasks with a 0-task difference (25 vs 25), inside binomial noise; routes frontier Claude only; proxies transcripts to a third party |
| RouteLLM / vLLM Semantic Router / Portkey / LangGraph | OSS | — | **REJECT** — route single-turn chat (benchmarked on MT-Bench/MMLU/GSM8K); no notion of task shape, which is where routing is actually decided here |

### opencode Go allowances vs measured demand

| Model | Go allowance | Note |
|---|---:|---|
| Kimi K3 | 490 req/month | ~16/day — a consult, not a lead seat |
| GLM-5.2 | 4,300 req/month | short of a dispatcher tier |
| DeepSeek Flash | 158,150 req/month | ≈ the entire current monthly message count |

Go's quota is dollar-denominated, so **oversized requests exhaust it long before the request count**. Full-context Flash lanes would cost ~$345/month against a $60 cap. Flash used *as R6 specifies* (one small task, no repo context) fits comfortably. **That distinction is the difference between $10 and $400.**

### Recommended stack

| Seat | Provider | Cost |
|---|---|---|
| K3 — lead | Kimi Allegretto | already held |
| GLM 5.2 — dispatcher | z.ai Coding Plan **Pro** | $72/mo |
| DeepSeek Flash — workers | opencode Go | $10/mo |
| overflow | opencode Zen credits | as used |

**~$82/month added.** Start at zAI Pro, not Max: the roster is unproven and R6 should move volume down to Flash. Upgrade only on a real cap-out.

**Watch item:** z.ai sizes a prompt at 15–20 model invocations. Measured here: **32.6**. Treat effective capacity as roughly half the headline — Pro's 2,000/week may behave like ~1,000. Also unverified: whether z.ai's terms carry a context-length or fair-use clause that bites at 217K-token turns, and whether proxying a subscription endpoint through LiteLLM is permitted by Kimi's and z.ai's terms.

---

## 8. Optimisation target

| | Now (measured) | Optimised (estimate) |
|---|---:|---:|
| Subagents | 22.1 GTok | 2.8 GTok |
| Lead + dispatcher | 13.6 GTok | 6.8 GTok |
| **Total** | **36.0 GTok/mo** | **9.5 GTok/mo (27%)** |
| Metered, correct tiers | ~$11,200 | ~$3,050 |

Two assumptions, **stated as estimates, not measurements**:

1. **Workers cut 8x** — R6 option A gives Flash one small task with no repo context (~400K → ~50K). *This is the load-bearing assumption.* If workers still haul full context, the saving mostly vanishes. §3a says the gap is currently zero, so the headroom is real; whether it is captured depends entirely on R6 being implemented as written.
2. **Seats cut 2x** — rotation at 200K instead of 500K. Grounded in the §3c curve; higher confidence.

**After optimising, K3 becomes the largest line item** ($1,792 of $3,051) despite being a third of seat volume, because of its $0.80 cache rate. The cheapest configuration is one where the lead thinks hard and rarely and everything else happens below it — i.e. the old Fable window-economy posture was structurally correct, not merely a workaround for Anthropic's pricing. **This answers R5 with data.**

---

## 9. Routing rules — sort on cost drivers, not seniority

A lead→middle→worker waterfall sorts by *seniority of the task*. The measured cost drivers are **context size, output length, and verifiability**. Three questions per task:

**1. Can a machine check the result?**
If clippy, a test, or an exit code catches the error, use the cheapest model *regardless of task difficulty* — the gate is the reviewer. If only judgment catches it (wrong reuse target, bad seam, semantics), it needs an expensive seat *regardless of how trivial the edit looks*. This supersedes "mechanical vs judgment," which is a lossy proxy for it.

**2. How much context does it need?**
This matters more than model choice. A cheap model at 225K costs more than an expensive one at 20K. Everything currently runs at ~225K regardless of task (§3a) — worker tasks are being charged lead-seat prices. Sizing context per task beats switching models.

**3. Does the answer need to be prose?**
73% of calls currently say yes (§3b). Most shouldn't. Structured returns below the lead; "commit `<sha>`, gate green, 3 files" instead of five paragraphs.

Consequences:

- **Retrieval never happens in an expensive seat.** Read/Bash are ~20% of lead calls and each permanently inflates the most expensive context for the rest of the session. A cheap agent that reads 2,000 lines and returns 20 is the best value construct available.
- **Verification is cheap, not senior.** Running a gate and reading an exit code doesn't need the lead. Only interpreting an *ambiguous* failure does.
- **Judgment tasks get SMALL context, not big.** The instinct is to hand the smart model everything; a K3 turn at 400K costs 20x one at 20K. Brief precisely instead.
- The target shape is **cheap retrieval → one small precise expensive turn → cheap gated execution.** The expensive model should see the *least* text in the system. Today it sees the most.

---

## 10. Enforcement

Your own tier-spawn guard records why this section exists: the rule "was policy, not machinery." Three classes, and only one holds:

- **Mechanical** — a hook or the proxy refuses. Cannot be violated.
- **Structural** — the cheap path is the only convenient path.
- **Convention** — the model reads a doc and complies. Works with strong models. **Will not work with DeepSeek Flash.** Every rule currently held by convention must be promoted to mechanism or it stops holding under the new roster.

| Rule | Class | Mechanism |
|---|---|---|
| Rotate seats at ~200K context | **Mechanical** | `.claude/hooks/context-ceiling-guard.py` — reads `transcript_path` (same trick as `agent-tier-spawn-guard.py`), sums recent `cache_read_input_tokens`, warns then denies past the ceiling |
| Worker keys cannot reach expensive models | **Mechanical** | LiteLLM virtual key `models` allow-list (R8) — strictly harder than R2's regex, which fails open on unrecognised ids |
| Per-seat spend/rate caps | **Mechanical** | LiteLLM `max_budget`, `rpm_limit`, `max_parallel_requests` (R8) |
| Briefs name a gate, a scope, a reuse target | **Structural** | generate lane briefs from a script that refuses to emit one missing them |
| Terse/structured returns | **Convention + partial** | schema-constrained returns where supported; otherwise prescriptive text repeated in *every* brief — doctrine already found that repetition is the only thing that works |
| Which tasks need judgment | **Human/lead** | not mechanisable; one decision per lane, not per turn |

---

## 11. Open items

- The §8 optimised figure is a **target, not a result.** Re-run `token_report.py` after the first full wave on the new roster and record the actual against it here.
- Pair spend with an outcome number before optimising on cost alone (R8): landed diffs that survived, lanes rejected at review, bugs that returned. The most expensive line will be the lead's review pass, and that pass is what prevents the failure the steering model exists to stop. Cost-only optimisation points straight at cutting it.
- Currency: AUD display, USD enforcement, rate + date stamped on every figure (R8). Applies to the metered tier only; Kimi and z.ai are subscriptions where notional dollars are a within-endpoint usage proxy, not money.
