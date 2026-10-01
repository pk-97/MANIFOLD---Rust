# Token Economics — measured spend and why seats rotate at 200K

**Status:** MEASURED BASELINE 2026-07-23, trimmed 2026-10-01. Every number below came from local Claude Code transcripts, not from estimates; regenerate with `scripts/token_report.py`. The metered-price, provider-plan and purchasing sections are retired with the multi-provider roster (git history). Live routing policy: `docs/AGENT_ROUTING.md`.

The numbers are from the old mixed-provider roster at full tilt, so they are a control group, not a current reading.

---

## 1. How to reproduce

```
scripts/token_report.py            # 30-day totals by model
scripts/token_report.py --days 2   # recent window
scripts/token_report.py --daily    # per-day trend
scripts/token_report.py --sessions # concentration + context growth
scripts/token_report.py --tools    # tool mix by seat type
```

Source: `~/.claude/projects/**/*.jsonl`, the per-message `usage` block Claude Code writes locally. Deduped by `message.id`. **Never quote a number from this doc without re-running.** It ages the moment the roster changes.

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

### 14 days (2026-07-09 → 07-23), the orchestration-era window

- 16.8 GTok cache-read → **36 GTok/month run rate**. The orchestration patterns raised throughput; they did not lower consumption.
- **4,451 user turns → 144,968 model calls = 32.6 calls per user turn**
- **2,226 user turns/week**
- 710 agent sessions in 14 days (~50/day)
- Daily range 2,000–12,000 model calls; no downward trend

### 2 days to 2026-07-23

9,049 messages, 2,065 MTok cache-read, **$1,173 at metered list = $586/day ≈ $17,600/month**. **62% of messages ran on Opus or Fable.** The judgment tier was doing most of the message volume, not the mechanical tier.

---

## 3. Where the tokens actually go

Three findings, in descending order of leverage.

### 3a. Subagents are 61% of everything, and are not lightweight

| Seat type | Share of tokens | Avg context per call |
|---|---:|---:|
| Subagents | 61.3% | **224K** |
| Main sessions (lead + dispatcher) | 37.7% | **226K** |

Subagents carry the same context weight as the lead session. There was no such thing as a cheap worker: every spawned agent independently loads CLAUDE.md, the docs, and its own file reads, then re-reads all of it on every step. ~50 of these per day. A cheap worker only exists if its context stays small, which is why briefs carry established findings and exact scope.

### 3b. 73% of calls produce no tool call at all

| | No tool (prose) | Bash | Edit | Read | Agent |
|---|---:|---:|---:|---:|---:|
| Main | 72.6% | 14.4% | 5.6% | 5.3% | 0.3% |
| Subagent | 75.0% | 9.5% | 7.6% | 7.4% | 0.0% |

Roughly three-quarters of all spend is models writing prose (narrating, summarising, reporting, explaining) at 225K context each. Verbosity is not a style problem here; it is the majority of the bill.

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

> **Rotate worker seats at ~200K context (around call 150).** At 500K every subsequent call costs half a megatoken. This is the single largest lever in this document, and `context-ceiling-guard.py` enforces it: warn at 150K, stop at 200K. Workers only; the lead seat is exempt (Peter, 2026-07-24).
