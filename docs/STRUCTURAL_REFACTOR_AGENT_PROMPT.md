# MANIFOLD — Structural Refactor Research Agent

You are auditing a Rust codebase (269 files, ~99K lines, 12 crates) for structural
quality issues. This is a **research-only** pass — you will NOT modify any code.
You will write all findings to `docs/STRUCTURAL_AUDIT_REPORT.md`.

## CRITICAL INSTRUCTIONS

- **DO NOT MODIFY ANY SOURCE CODE.** Read only. Your output is a report file.
- **DO NOT STOP OR ASK FOR INPUT.** Complete all 10 tasks autonomously.
- **DO NOT USE AGENTS OR SUBAGENTS.** Do ALL work directly in the main context. Agents are lazy, skip findings, and produce shallow analysis. Every file read, every grep, every classification must be done by YOU, not delegated. If you spawn an agent for any task, the audit is invalid.
- **BE EXHAUSTIVE, NOT LAZY.** For every grep result, read the surrounding code to classify it properly. Do not skim, summarize counts without reading, or skip entries because "they look similar." Each finding is independent and must be individually evaluated. If a task says "report first 30 instances," you must actually find and report 30 (or all if fewer exist), not stop at 5 and say "and similar patterns elsewhere."
- **Write findings incrementally** to `docs/STRUCTURAL_AUDIT_REPORT.md` — append each section as you complete it so work is not lost.
- **Every finding MUST include `file_path:line_number`.** No vague statements.
- **Classify every finding:** HIGH (safety/correctness risk), MEDIUM (maintainability), LOW (style/polish).
- **Work through tasks 1-10 in order.** Do not skip any.
- The codebase root is `/Users/peterkiemann/MANIFOLD - Rust`.
- Read `CLAUDE.md` at the root before starting for full architectural context.

## CODEBASE STATS (for reference)

- 400 `unsafe` blocks, 354 `.unwrap()`/`.expect()`, 1735 numeric casts
- 8 `autoreleasepool`, 0 `panic::set_hook`, 0 `IOPMAssertion`
- 0 `forbid(unsafe_code)`, 10 `#[must_use]`, 46 `println`/`eprintln`
- Largest files: percussion_orchestrator.rs (3078), metal/mod.rs (2186),
  content_thread.rs (2002), wireframe_depth.rs (1981), app.rs (1981), engine.rs (1934)
- Crate sizes: manifold-ui (22K), manifold-playback (17.5K), manifold-app (16.5K),
  manifold-renderer (16K), manifold-core (8.8K), manifold-editing (6.4K),
  manifold-gpu (4.4K), manifold-media (2.4K), manifold-io (1.9K)

---

## TASK 1: Large File Decomposition Analysis

Read each file >1000 lines listed below. For each, determine if it should be split
and where the natural boundaries are.

**Files (read each completely):**
- `crates/manifold-playback/src/percussion_orchestrator.rs` (3078)
- `crates/manifold-gpu/src/metal/mod.rs` (2186)
- `crates/manifold-app/src/content_thread.rs` (2002)
- `crates/manifold-renderer/src/effects/wireframe_depth.rs` (1981)
- `crates/manifold-app/src/app.rs` (1981)
- `crates/manifold-playback/src/engine.rs` (1934)
- `crates/manifold-ui/src/panels/viewport.rs` (1820)
- `crates/manifold-app/src/ui_bridge/inspector.rs` (1584)
- `crates/manifold-ui/src/panels/inspector.rs` (1541)
- `crates/manifold-gpu/src/metal/mps.rs` (1451)
- `crates/manifold-ui/src/panels/layer_header.rs` (1407)
- `crates/manifold-renderer/src/layer_compositor.rs` (1229)
- `crates/manifold-app/src/input_host.rs` (1201)
- `crates/manifold-app/src/app_render.rs` (1192)
- `crates/manifold-ui/src/panels/effect_card.rs` (1163)
- `crates/manifold-editing/src/service.rs` (1076)
- `crates/manifold-ui/src/interaction_overlay.rs` (1061)

**For each file, report:**
```
## file_path (N lines)
VERDICT: Split / Keep / Borderline
REASON: [why — is it genuinely cohesive or mixing concerns?]
PROPOSED SPLITS (if Split):
- new_module.rs: lines X-Y (responsibility)
```

**Write this section to the report file before moving to Task 2.**

---

## TASK 2: Type System Strengthening

Examine core type definitions for opportunities to make illegal states unrepresentable.

**Files to read:**
- `crates/manifold-core/src/types.rs`
- `crates/manifold-core/src/id.rs`
- `crates/manifold-core/src/project.rs`
- `crates/manifold-core/src/clip.rs`
- `crates/manifold-core/src/layer.rs`
- `crates/manifold-core/src/timeline.rs`
- `crates/manifold-core/src/tempo.rs`
- `crates/manifold-core/src/settings.rs`
- `crates/manifold-playback/src/engine.rs`
- `crates/manifold-playback/src/transport_controller.rs`
- `crates/manifold-playback/src/sync_source.rs`

**Find and report:**
1. Every `f64` that represents beats vs seconds — are they ever mixed? List specific locations where a `Beats(f64)` / `Seconds(f64)` newtype would catch bugs.
2. Raw `f64` for BPM — could `Bpm(f64)` with validated construction prevent invalid values?
3. Multiple related `bool` fields that should be a state enum — list each struct and the bools.
4. Stringly-typed lookups where an enum would catch typos at compile time.
5. `u32` for values that can never be zero where `NonZeroU32` would enforce.
6. Functions returning unnamed tuples that should be named structs.

**Write this section to the report file before moving to Task 3.**

---

## TASK 3: Resource Lifecycle & Cleanup

Map the full resource lifecycle to find gaps where cleanup is missing or ad-hoc.

**Files to read:**
- `crates/manifold-app/src/content_pipeline.rs`
- `crates/manifold-renderer/src/effect_registry.rs`
- `crates/manifold-renderer/src/effect_chain.rs`
- `crates/manifold-renderer/src/render_target_pool.rs`
- `crates/manifold-renderer/src/generator_renderer.rs`
- `crates/manifold-renderer/src/generators/stateful_base.rs`
- `crates/manifold-gpu/src/metal/mod.rs`
- `crates/manifold-playback/src/engine.rs`
- `crates/manifold-playback/src/live_clip_manager.rs`

**Find and report:**
1. The full cleanup chain from `TickResult::stopped_clips` → content_pipeline → effect_registry → GPU resources. Are there any gaps where a resource type is not cleaned up?
2. Every type with a manual `.cleanup()` / `.destroy()` / `.release()` method — could `Drop` handle it instead?
3. Types with split initialization (new + init + setup) — could they be single constructors?
4. Every `.reset()` method — does it reset ALL fields or miss some?
5. What state survives a project switch? List everything that persists.

**Write this section to the report file before moving to Task 4.**

---

## TASK 4: Modern Rust Idioms

Search the entire codebase for outdated patterns. Use Grep to find patterns efficiently.

**Patterns to search for (grep across all `crates/*/src/`):**

1. **Index loops:** grep for `for .* in 0\.\.` and check if body uses `[i]` indexing. Report first 30 instances.
2. **Nested if-let that should be let-else:** grep for `if let Some` and `if let Ok` — look for patterns where the else branch is just `return`/`continue`. Report first 20.
3. **Missing Entry API:** grep for `contains_key` followed by `insert` on the same map. Report all.
4. **Unnecessary clones:** grep for `.clone()` — focus on hot-path crates (manifold-playback, manifold-renderer, manifold-gpu). Report instances where the clone appears unnecessary (passed to a function that takes `&T`).
5. **format! on hot paths:** grep for `format!` in manifold-playback, manifold-renderer, manifold-gpu, manifold-app. Report any in per-frame functions.
6. **Vec::new without capacity:** grep for `Vec::new()` in hot-path crates. Check if followed by known-count pushes.
7. **Manual option chains:** grep for patterns like `if x.is_some() { x.unwrap()` — should use if-let or combinators.

**For each finding, report:** `file:line — current pattern — proposed replacement`
**Write this section to the report file before moving to Task 5.**

---

## TASK 5: Error Handling Consistency

Audit panic points and error handling patterns.

**Step 1:** Grep for every `.unwrap()` and `.expect()` in these crates:
- `manifold-playback`
- `manifold-renderer`
- `manifold-gpu`
- `manifold-app`
- `manifold-media`

**For each, classify:**
- **SAFE:** Impossible to fail (e.g., parking_lot Mutex::lock, regex compilation of a literal)
- **QUESTIONABLE:** Could fail under unusual conditions (disk full, GPU memory exhaustion)
- **DANGEROUS:** On a per-frame hot path AND could fail from external state

Report ALL dangerous and questionable ones. Report safe ones as a count only.

**Step 2:** Grep for `if let Ok(` and `let _ =` to find swallowed errors. Report all.

**Step 3:** Grep for `#[must_use]` — only 10 exist. Identify the top 20 functions that should have it (functions returning Result, Option, or important computed values in public APIs).

**Write this section to the report file before moving to Task 6.**

---

## TASK 6: Concurrency Pattern Audit

Map the full concurrency architecture.

**Step 1: Lock inventory.** Grep for `Mutex::new`, `RwLock::new`, `AtomicU`, `AtomicI`, `AtomicBool` across the codebase. For each, read the surrounding code to determine:
- What data does it protect?
- Which threads access it?
- How wide is the lock scope?

**Step 2: Channel inventory.** Grep for `bounded`, `unbounded`, `channel` in crossbeam usage. For each:
- Is it bounded or unbounded?
- What is the capacity (if bounded)?
- What happens when the channel is full or the other end drops?

**Step 3: Thread inventory.** Grep for `thread::spawn`. For each:
- Is the JoinHandle stored or dropped (detached thread)?
- Is the thread joined on shutdown?
- What is the thread's QoS / priority?

**Step 4: Atomic ordering.** Grep for `Ordering::` across the codebase. For each `Relaxed`, determine if it's actually safe or if `Acquire`/`Release`/`SeqCst` is needed.

**Step 5: Lock ordering.** Read `content_thread.rs` and `app.rs` to trace which locks are acquired in what order. Are there any paths where two locks are acquired in conflicting order?

**Write this section to the report file before moving to Task 7.**

---

## TASK 7: GPU Code Deduplication

Analyze effects and generators for repeated boilerplate.

**Step 1:** Read the first 100 lines of each effect file in `crates/manifold-renderer/src/effects/` (skip helpers and mod.rs). Look for repeated patterns in:
- Pipeline creation
- Uniform struct definition
- Buffer creation and binding
- Dispatch / blit calls

**Step 2:** Same for generators in `crates/manifold-renderer/src/generators/`.

**Step 3:** Read 5 WGSL shader files and check for copy-pasted utility functions.

**Report:** The top 5 deduplication opportunities, ranked by (lines saved × number of occurrences). For each, describe the pattern, where it repeats, and how it could be extracted.

**Write this section to the report file before moving to Task 8.**

---

## TASK 8: Numeric Safety

Audit numeric operations for safety issues.

**Step 1: Float-to-integer casts.** Grep for `as u32`, `as i32`, `as usize` in hot-path crates (manifold-playback, manifold-renderer, manifold-gpu). For the first 50 results in each crate, classify:
- SAFE: input is bounded by prior logic
- NEEDS_GUARD: could receive NaN/Inf/negative
- NEEDS_ROUNDING: should use `.round()` first

**Step 2: f64 to f32 precision loss.** Grep for `as f32` in manifold-playback and manifold-renderer. Are beat positions or time values being downcast to f32 for shader uniforms? Report each.

**Step 3: Float equality.** Grep for `== 0.0`, `!= 0.0`, `== 1.0` in hot-path crates. Should any use epsilon comparison?

**Step 4: Division by zero.** Grep for ` / ` in manifold-playback and manifold-renderer. For each division, can the divisor be zero? Focus on parameter-derived values.

**Step 5: Integer overflow.** Find counter variables (frame count, tick count, version counters). What type? Can they overflow in realistic timeframes?

**Write this section to the report file before moving to Task 9.**

---

## TASK 9: Dead Code, Port Artifacts & Hygiene

Clean up noise.

**Step 1:** Grep for `#[allow(dead_code)]` — list each. Is the code actually used or truly dead?

**Step 2:** Grep for `TODO`, `FIXME`, `HACK`, `XXX`, `TEMP` in comments. List each with context.

**Step 3:** Grep for `println!` and `eprintln!` — all 46 instances. Classify:
- DEBUG: should be removed
- INTENTIONAL: meaningful logging
- HOT_PATH: on a per-frame path (CRITICAL to remove)

**Step 4:** Grep for comments containing `Unity`, `MonoBehaviour`, `C#`, `GetComponent`, `SerializeField`. Are they stale port artifacts or still-useful context?

**Step 5:** Grep for `pub fn` and `pub struct` in `manifold-core`. How many should be `pub(crate)`?

**Write this section to the report file before moving to Task 10.**

---

## TASK 10: Build & Lint Hardening

Examine build configuration and propose hardening.

**Files to read:**
- Root `Cargo.toml`
- `clippy.toml`
- `rustfmt.toml`
- `crates/manifold-core/src/lib.rs` (check for crate-level attributes)
- `crates/manifold-editing/src/lib.rs`
- `crates/manifold-io/src/lib.rs`
- `.github/workflows/` — all CI files

**Check:**
1. Can `manifold-core` use `#![forbid(unsafe_code)]`? Check if it has any unsafe. Same for `manifold-editing`, `manifold-io`.
2. Is `unsafe_op_in_unsafe_fn` denied?
3. What Clippy lints are enabled/denied? Propose adding: `cast_possible_truncation`, `cast_sign_loss`, `cast_precision_loss`, `float_cmp`, `missing_safety_doc`, `undocumented_unsafe_blocks`.
4. Does CI run on macOS? (Required for Metal compilation.)
5. Are there unused dependencies? Check each crate's `Cargo.toml` against its imports.

**Write this section to the report file.**

---

## FINAL STEP

After completing all 10 tasks, add a summary section at the TOP of the report file:

```markdown
# MANIFOLD — Structural Audit Report

**Date:** [today]
**Scope:** 10 structural quality audits across 12 crates, ~99K lines

## Executive Summary
- Total findings: N
- HIGH priority: N
- MEDIUM priority: N
- LOW priority: N

### Top 10 Most Critical Findings
1. [file:line] — description
2. ...

### Recommended Execution Order
1. [which findings to fix first and why]
```

Then you are done. Do not ask for input. Do not modify any source code.
