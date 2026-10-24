# Agent Routing

Authoritative guide for splitting work between the lead session and worker subagents.

## Core Principles

1. **Work in the main checkout:** Edit directly in the main checkout so incremental `cargo nextest` and `cargo build --bin manifold` runs complete in seconds against the single warm `target/` directory. Never acquire a git worktree unless Peter asks for one or there is a high risk of collision with large parallel sessions; parallel subagents within a session share the main checkout on disjoint file sets.
2. **Don't spawn subagents for small work:** Subagent spawns carry context startup overhead and lose conversational nuance. If a fix is under ~100 lines across 1–3 files, or requires a 30-second `rg` + `Read` lookup, the lead does it directly.
3. **Use workers for mechanical bulk and parallel tasks:** Once the lead has diagnosed the exact `file:line` seam and decided the fix shape, delegate mechanical multi-file edits, boilerplate wiring, and scoped test runs to cheaper workers (`sonnet` in Claude Code; free/flash models in OpenCode). When running workers in parallel in the warm tree, assign strictly disjoint file sets.
4. **Verify substance, not just exit codes:** The lead reviews `git diff` and verifies that tests check real state and behavior (never placeholder tests or identity-only defaults) and that the five engineering sanity checks in `CLAUDE.md` hold.

## Model Seats

| Seat | Model | Best For |
|---|---|---|
| Lead | Opus / Primary Lead | Diagnosis, architecture/UX calls, small/medium direct fixes, diff review, landing. |
| Fast Worker | `sonnet` (Claude) / Free-tier worker (OpenCode) | Decided multi-file edits with `file:line` specs, UI/schema wiring, test runs, mechanical refactors. |
| Deep Worker | `opus` | Isolated complex GPU/physics/shader tasks or deep multi-crate audits when parallelizing. |
| Retrieval | `haiku` | Fast read-only command execution or mechanical searches when needed. |

## Writing Effective Worker Prompts

Keep worker briefs short and concrete:
- **Exact files to touch** (and explicit instruction not to touch anything else or run `cargo fmt`).
- **Concrete `file:line` changes** with the target data structures and reuse paths already identified.
- **1–3 scoped verification commands** (`cargo nextest run -p <crate> <filter>`), with instructions to return a concise (<150 word) summary of what changed and what passed.
