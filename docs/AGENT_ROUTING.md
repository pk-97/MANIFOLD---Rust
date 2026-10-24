# Agent Routing

Fast, anti-bureaucracy, warm-tree workflow. Authoritative guide for splitting work between the lead and workers.

## Core Principles

1. **Work in the main checkout:** Edit directly in the main checkout so incremental `cargo nextest` and `cargo build --bin manifold` runs complete in seconds against the single warm `target/` directory. Never acquire a git worktree unless Peter asks for one; parallel subagents share the main checkout on strictly disjoint file sets.
2. **Pre-chew, then delegate down (Haiku / Free-tier first, Sonnet second):** If a task seems to require an Opus subagent, the lead hasn't finished designing it yet. The lead does the 30–60 second `rg` + `Read` to find exact `file:line` seams, writes the exact struct/WGSL formula/signature in the prompt, and delegates implementation to **Haiku 5.5** (`haiku` in Claude Code) or **Free-tier workers** (`opencode-go/beam-free` in OpenCode), or **Sonnet** (`sonnet`) for larger self-contained modules/shaders.
3. **Don't spawn subagents for tiny lookups:** If a fix is under ~50 lines in 1–2 files, the lead edits it directly.
4. **Never fix a bug with a process ritual:** Forbidden in plans and workflows: multi-stage review committees, read-only audit swarms, planted-fault test theatre (breaking code just to watch a test fail), worktree/slot quotas, or "Opus-only" file/kernel bans. Quality comes from (a) an unambiguous `file:line` + formula spec before coding, and (b) the lead reading `git diff` + running scoped `cargo nextest` in the warm tree after coding.
5. **Plans are code specs, not management docs:** Implementation plans contain only target `file:line` anchors, data layouts/formulas, disjoint file groupings, and the exact verification command for each task. Zero git/worktree rules, zero reporting templates, zero review-committee schedules.

## Model Seats

| Seat | Model | Best For |
|---|---|---|
| Fast / Free Worker | `haiku` (Claude Code) / `opencode-go/beam-free` (OpenCode) | Default workhorse. File splits, dead-code deletions, moving helpers to includes, schema/UI/preset wiring, pattern replacements across files, and implementing functions/shaders when given the exact formula and `file:line`. |
| Standard Worker | `sonnet` (Claude Code) / flash/free worker (OpenCode) | Self-contained multi-file modules, shaders, and unit tests when the interface and math are specified in the brief. |
| Lead | Primary session — no subagent spawn | Fast `rg`+`Read` diagnosis, writing concrete `file:line` + formula briefs for Haiku/Sonnet, tricky core algorithmic seams, reviewing `git diff`, and landing. |

## Writing Effective Worker Prompts

- **Exact files to touch** (and explicit instruction not to touch anything else).
- **Pre-chewed `file:line` changes + exact structs/formulas** so a Haiku/free worker can execute without guessing or researching.
- **1–2 scoped verification commands** (`cargo nextest run -p <crate> <filter>`), returning a short (<100 word) summary.