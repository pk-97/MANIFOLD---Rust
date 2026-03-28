# MANIFOLD — Structural Refactor Plan

## Goal

Bring the codebase to "end game" quality: modern Rust, clean architecture,
correct idioms, no port artifacts. All refactors MUST be behavior-preserving.
No logic changes. This prepares the codebase for the stability audit.

## Codebase Stats (2026-03-28)

- 269 Rust files, ~99,270 lines, 12 crates
- Largest files: percussion_orchestrator (3078), metal/mod (2186), content_thread (2002), wireframe_depth (1981), app (1981), engine (1934)
- 400 `unsafe` blocks, 0 with `forbid(unsafe_code)` on any crate
- 354 `.unwrap()` / `.expect()`, 10 `#[must_use]`
- 1,735 numeric casts, 0 `panic::set_hook`
- Edition 2024 — but modern patterns may not be fully leveraged

## Rules

1. **Behavior-preserving ONLY.** No logic changes. Same inputs → same outputs.
2. **Compile and pass tests after each agent's work.** `cargo clippy --workspace -- -D warnings && cargo test --workspace`
3. **One concern per agent.** Don't mix type system changes with file splits.
4. **Preserve serialization compatibility.** Never change serde-visible field names or structure.
5. **Don't touch WGSL shaders.** Shader changes have GPU correctness implications — that's audit territory.

---

## Agent Assignments

Each agent has a specific structural concern, specific files to examine,
and specific transformations to make.

---

### STRUCT-1: Large File Decomposition

**Goal:** Split files >1000 lines into focused modules. No behavior change.

**Files to evaluate for splitting:**
- `crates/manifold-playback/src/percussion_orchestrator.rs` (3078 lines)
- `crates/manifold-gpu/src/metal/mod.rs` (2186 lines)
- `crates/manifold-app/src/content_thread.rs` (2002 lines)
- `crates/manifold-renderer/src/effects/wireframe_depth.rs` (1981 lines)
- `crates/manifold-app/src/app.rs` (1981 lines)
- `crates/manifold-playback/src/engine.rs` (1934 lines)
- `crates/manifold-ui/src/panels/viewport.rs` (1820 lines)
- `crates/manifold-app/src/ui_bridge/inspector.rs` (1584 lines)
- `crates/manifold-ui/src/panels/inspector.rs` (1541 lines)
- `crates/manifold-gpu/src/metal/mps.rs` (1451 lines)
- `crates/manifold-ui/src/panels/layer_header.rs` (1407 lines)
- `crates/manifold-renderer/src/layer_compositor.rs` (1229 lines)
- `crates/manifold-app/src/input_host.rs` (1201 lines)
- `crates/manifold-app/src/app_render.rs` (1192 lines)
- `crates/manifold-ui/src/panels/effect_card.rs` (1163 lines)
- `crates/manifold-editing/src/service.rs` (1076 lines)
- `crates/manifold-ui/src/interaction_overlay.rs` (1061 lines)

**Instructions:**
1. Read each file completely.
2. For each, identify natural split points — sections that handle distinct responsibilities.
3. Propose a split plan (new module names, what moves where).
4. DO NOT split files where the code is genuinely cohesive (e.g., a single complex algorithm).
5. Report your recommendations. Do not execute the splits — just plan them.

**Output format per file:**
```
## file_path (N lines)
RECOMMENDATION: Split / Keep / Borderline
REASON: ...
PROPOSED SPLITS:
- new_module_a.rs: lines X-Y (responsibility: ...)
- new_module_b.rs: lines X-Y (responsibility: ...)
```

---

### STRUCT-2: Type System Strengthening

**Goal:** Introduce newtypes and state enums to make illegal states unrepresentable.

**Files to examine:**
- `crates/manifold-core/src/types.rs` (909 lines) — all core types
- `crates/manifold-core/src/id.rs` — typed IDs
- `crates/manifold-core/src/project.rs`
- `crates/manifold-core/src/clip.rs`
- `crates/manifold-core/src/layer.rs` (554 lines)
- `crates/manifold-core/src/timeline.rs`
- `crates/manifold-core/src/tempo.rs`
- `crates/manifold-core/src/settings.rs`
- `crates/manifold-playback/src/engine.rs` (1934 lines) — playback state
- `crates/manifold-playback/src/transport_controller.rs`
- `crates/manifold-playback/src/sync_source.rs`

**Checklist:**
1. **Beats vs Seconds confusion:** Find every `f64` that represents beats and every `f64` that represents seconds. Could a `Beats(f64)` / `Seconds(f64)` newtype prevent mix-ups? List specific call sites where confusion is possible.
2. **BPM as raw f64:** Could `Bpm(f64)` with validated construction (rejects zero, negative, NaN) prevent invalid BPM values from entering the system?
3. **Boolean state machines:** Find structs with multiple related `bool` fields (e.g., `is_playing` + `is_paused`). Could they be a single enum where invalid combinations are impossible?
4. **Raw indices:** Are parameter indices raw `usize` where a newtype `ParamIndex(usize)` would add clarity?
5. **Stringly-typed lookups:** Find lookups by string where an enum would catch typos at compile time.
6. **Missing NonZero:** Find `u32` for values that can't be zero (resolution, FPS) where `NonZeroU32` would document and enforce.
7. **Tuple returns:** Find functions returning unnamed tuples. Should any be named structs?
8. **Report only.** List each opportunity with location, current type, proposed type, and impact. Do not implement changes.

---

### STRUCT-3: Resource Lifecycle & Cleanup Consolidation

**Goal:** Ensure resource cleanup is systematic, not ad-hoc and scattered.

**Files to examine:**
- `crates/manifold-app/src/content_pipeline.rs` (767 lines) — cleanup_stopped_clips
- `crates/manifold-renderer/src/effect_registry.rs` — effect state lifecycle
- `crates/manifold-renderer/src/effect_chain.rs`
- `crates/manifold-renderer/src/render_target_pool.rs`
- `crates/manifold-renderer/src/generator_renderer.rs` (646 lines)
- `crates/manifold-renderer/src/generators/stateful_base.rs`
- `crates/manifold-gpu/src/metal/mod.rs` — GPU resource lifecycle
- `crates/manifold-playback/src/engine.rs` — playback state cleanup
- `crates/manifold-playback/src/live_clip_manager.rs` (926 lines)

**Checklist:**
1. **Cleanup paths:** Map the full cleanup chain from `TickResult::stopped_clips` through to GPU resource release. Is every step covered? Any gaps?
2. **Drop vs manual cleanup:** List every type that requires a `.cleanup()` / `.destroy()` call instead of `Drop`. Could `Drop` be implemented instead?
3. **Initialization patterns:** List types where initialization is split across multiple methods (new + init + setup). Could they be single constructors?
4. **Reset correctness:** Find every `.reset()` method. Does it reset ALL fields? Use `Self { ..Default::default() }` pattern?
5. **State cleanup on project switch:** When a new project is loaded, what state from the old project survives? (GPU resources, effect state, scheduler state, undo history)
6. **Report with recommendations.** Don't implement.

---

### STRUCT-4: Modern Rust Idioms

**Goal:** Replace C#-isms and outdated Rust patterns with modern, idiomatic Rust.

**Scope:** All crates. Read files where Clippy or pattern search identifies issues.

**Patterns to find and propose fixes:**

1. **Index-based loops → iterators:**
   Search for `for i in 0..vec.len()` or `for i in 0..N` with `vec[i]` body.
   Replace with `for item in &vec` or `.iter().enumerate()`.

2. **Nested if-let → let-else:**
   Search for `if let Some(x) = expr { ... } else { return; }`.
   Replace with `let Some(x) = expr else { return; };`.

3. **Manual Option unwrap → combinators:**
   Search for `match opt { Some(x) => Some(f(x)), None => None }`.
   Replace with `opt.map(f)`.

4. **Missing Entry API:**
   Search for `if !map.contains_key(&k) { map.insert(k, v); }`.
   Replace with `map.entry(k).or_insert(v)`.

5. **Clone where borrow works:**
   Search for `.clone()` passed to functions that accept `&T`.
   Remove unnecessary clones.

6. **Manual string formatting on hot paths:**
   Search for `format!()` in per-frame code.
   Propose pre-allocation or elimination.

7. **Vec without capacity:**
   Search for `Vec::new()` followed by known-count pushes.
   Replace with `Vec::with_capacity(n)`.

8. **C#-style null check chains:**
   Search for nested `if let Some(...) { if let Some(...) { ... } }`.
   Propose `.and_then()` chains or `let ... else` chains.

9. **Manual From/Into conversions:**
   Search for explicit conversion functions that could be `impl From<T>`.

10. **Missing `is_some_and` / `is_ok_and`:**
    Search for `matches!(opt, Some(x) if cond)` or `opt.map_or(false, |x| cond)`.
    Replace with `opt.is_some_and(|x| cond)`.

**Output:** List each instance with file:line, current code, proposed code. Grouped by pattern type. Do not implement — report only.

---

### STRUCT-5: Error Handling Consistency

**Goal:** Establish consistent error handling patterns across the codebase.

**Files to examine:** All crates, focus on:
- `crates/manifold-app/src/` — application-level error handling
- `crates/manifold-playback/src/` — engine error paths
- `crates/manifold-gpu/src/` — GPU error paths
- `crates/manifold-media/src/` — decode error paths
- `crates/manifold-io/src/` — I/O error paths

**Checklist:**
1. **Panic audit on hot paths:** Every `.unwrap()` and `.expect()` in `manifold-playback`, `manifold-renderer`, `manifold-gpu`, `manifold-app` — classify as:
   - SAFE: impossible to fail (e.g., `Mutex::lock()` with parking_lot)
   - QUESTIONABLE: could fail under unusual runtime conditions
   - DANGEROUS: on a per-frame path and could fail from external state
2. **Swallowed errors:** Find `if let Ok(x) = ...` or `let _ = ...` patterns that silently ignore errors. List each.
3. **Mixed Result/panic patterns:** Find functions in the same module where some return `Result` and others panic for similar failures. Propose consistency.
4. **Missing error context:** Find `.map_err(|_| ...)` or `?` without `.context()` / `.with_context()`. Propose adding context.
5. **`#[must_use]` gaps:** Find functions that return important values (Result, Option, computed data) without `#[must_use]`. Propose adding it.
6. **Report only.** Classify each finding by severity and propose fix.

---

### STRUCT-6: Concurrency Pattern Audit

**Goal:** Ensure all concurrency patterns are correct and consistent.

**Files to examine:**
- `crates/manifold-app/src/content_thread.rs` (2002 lines)
- `crates/manifold-app/src/content_pipeline.rs` (767 lines)
- `crates/manifold-app/src/app.rs` (1981 lines)
- `crates/manifold-app/src/shared_texture.rs`
- `crates/manifold-playback/src/midi_input.rs` (794 lines)
- `crates/manifold-playback/src/midi_clock_sync.rs` (736 lines)
- `crates/manifold-playback/src/engine.rs` (1934 lines)
- `crates/manifold-renderer/src/background_worker.rs`
- Grep across all crates for: `Mutex`, `RwLock`, `Atomic`, `channel`, `thread::spawn`

**Checklist:**
1. **Lock inventory:** List every `Mutex` and `RwLock` in the codebase. What does each protect? Are any protecting unrelated state that should be split?
2. **Lock ordering:** For every path that acquires multiple locks, document the order. Are there any conflicting orders → deadlock risk?
3. **Lock scope:** Find locks held across long operations (I/O, GPU calls, channel sends). Could the scope be narrowed?
4. **Atomic ordering:** Every `Ordering::Relaxed` — is it actually safe? On ARM (Apple Silicon), Relaxed can reorder more than x86.
5. **Channel patterns:** Every channel — bounded or unbounded? What's the backpressure strategy? What happens when the other end drops?
6. **Thread lifecycle:** Every `thread::spawn` — is the JoinHandle stored? Is the thread joined on shutdown?
7. **Send/Sync correctness:** Any types crossing thread boundaries without explicit bounds?
8. **False sharing:** Any atomics or hot variables on the same cache line (within 64 bytes in the same struct)?
9. **Report only.** Map the full concurrency architecture.

---

### STRUCT-7: GPU Code Deduplication

**Goal:** Reduce boilerplate in effects and generators.

**Files to examine:**
- All `crates/manifold-renderer/src/effects/*.rs` (22 files)
- All `crates/manifold-renderer/src/generators/*.rs` (19 files)
- `crates/manifold-renderer/src/effects/fragment_blit_helper.rs`
- `crates/manifold-renderer/src/effects/compute_dual_blit_helper.rs`
- `crates/manifold-renderer/src/generators/compute_common.rs`
- `crates/manifold-renderer/src/generators/stateful_base.rs`

**Checklist:**
1. **Pipeline creation boilerplate:** How many effects/generators have similar pipeline creation code? Could a builder or helper reduce it?
2. **Uniform buffer setup:** How many have similar `#[repr(C)]` struct + buffer creation + binding? Is there a common pattern that should be extracted?
3. **Dispatch boilerplate:** How many compute effects have similar threadgroup calculation + dispatch code?
4. **Texture creation patterns:** Are effects creating textures with consistent flags, or is each ad-hoc?
5. **Per-owner state patterns:** How many effects use `AHashMap<i64, State>` with cleanup? Is the pattern consistent or each slightly different?
6. **Common shader code:** Read 5-6 WGSL shaders. Are there copy-pasted utility functions (color space, blend, noise)? Could they be shared?
7. **Report only.** Identify the top 5 deduplication opportunities ranked by lines-saved × occurrences.

---

### STRUCT-8: Numeric Safety

**Goal:** Make numeric operations explicit and safe.

**Scope:** Grep-based across all crates, focused on hot paths.

**Checklist:**
1. **Float-to-integer casts:** Every `as u32`, `as i32`, `as usize` from float. Categorize:
   - SAFE: value is bounded by prior logic
   - NEEDS_GUARD: could receive NaN/Inf/negative
   - NEEDS_ROUNDING: should use `.round()` first (Unity parity)
2. **Integer overflow:** Every arithmetic on counters, indices, sizes in `manifold-playback` and `manifold-renderer`. Could overflow in release mode (wraps silently).
   Propose `checked_add` / `saturating_add` where appropriate.
3. **Float precision:** Every `f32` that accumulates over time (beat position, phase, delta). Should it be `f64`?
4. **Float comparison:** Every `== 0.0` or `!= 0.0`. Should it use epsilon?
5. **Signed/unsigned mixing:** Every `i32 as usize` or `usize as i32`. Negative value → huge usize.
6. **Division guards:** Every `/` where divisor could be zero.
7. **Report only.** Categorize and prioritize.

---

### STRUCT-9: Dead Code, Port Artifacts & Hygiene

**Goal:** Remove noise that makes the codebase harder to read and audit.

**Scope:** Entire codebase.

**Checklist:**
1. **Dead code:** Run `cargo +nightly udeps` or equivalent. List unused dependencies.
   Search for `#[allow(dead_code)]` — is the code actually dead or just suppressed?
2. **Unused imports:** `cargo clippy` should catch these. List any.
3. **Stale comments:** Search for comments referencing Unity, MonoBehaviour, C#, TODO, FIXME, HACK, XXX. List each — are they still relevant?
4. **println/eprintln:** All 46 instances. Which are debug output that should be removed? Which are intentional logging?
5. **Unreachable code:** `unreachable!()` calls — are they actually unreachable?
6. **Duplicate implementations:** Any two functions that do the same thing in different files?
7. **Overly permissive visibility:** `pub` where `pub(crate)` would suffice. (May be too many to list — sample the core crate.)
8. **Missing `#[inline]` on trivial functions** used in hot paths (getters, newtypes).
9. **Report only.** Prioritize by impact on readability.

---

### STRUCT-10: Build & Lint Hardening

**Goal:** Strengthen compile-time safety guarantees.

**Files to examine:**
- `Cargo.toml` (workspace)
- All `crates/*/Cargo.toml`
- `clippy.toml`
- `rustfmt.toml`
- `.github/workflows/` (CI)
- `crates/manifold-core/src/lib.rs` — candidate for `#![forbid(unsafe_code)]`

**Checklist:**
1. **`#![forbid(unsafe_code)]`:** Can `manifold-core` enable this? It's described as "pure, no GPU." Any other pure crates?
2. **`#[deny(unsafe_op_in_unsafe_fn)]`:** Should be enabled workspace-wide. Forces explicit `unsafe` blocks inside unsafe functions.
3. **Clippy lints:** What's currently enabled? Propose enabling:
   - `clippy::cast_possible_truncation`
   - `clippy::cast_sign_loss`
   - `clippy::cast_precision_loss`
   - `clippy::float_cmp`
   - `clippy::missing_safety_doc`
   - `clippy::undocumented_unsafe_blocks`
4. **`#[must_use]`:** Only 10 in 99K lines. Propose adding to key functions.
5. **Unused dependencies:** List any in Cargo.toml that aren't used.
6. **Feature flag audit:** Are dependency features minimal? Any including more than needed?
7. **CI coverage:** Does CI run clippy with the proposed stricter lints? Does it run on macOS (required for Metal)?
8. **Report only.** Propose a linting configuration with rationale.

---

## Execution Order

**Batch 1 — Analysis only (no code changes, all independent):**
STRUCT-1 (file decomp), STRUCT-2 (types), STRUCT-3 (lifecycle),
STRUCT-9 (dead code), STRUCT-10 (build) — all in parallel

**Batch 2 — Analysis only (benefits from Batch 1 context):**
STRUCT-4 (idioms), STRUCT-5 (errors), STRUCT-6 (concurrency),
STRUCT-7 (GPU dedup), STRUCT-8 (numerics) — all in parallel

**Batch 3 — Implementation (after reviewing all reports):**
Prioritize findings, execute changes in order of:
1. Safety-critical (lint hardening, numeric guards, panic fixes)
2. Architecture (file splits, type system, lifecycle)
3. Quality (idioms, dedup, dead code)

## Output Format

Each agent produces a report:

```
## Summary
- Total issues found: N
- High priority: N
- Medium priority: N
- Low priority: N

## HIGH PRIORITY
- [file:line] Description — Proposed fix — Rationale

## MEDIUM PRIORITY
- [file:line] Description — Proposed fix — Rationale

## LOW PRIORITY
- [file:line] Description — Proposed fix — Rationale
```

## Constraint Reminder

ALL refactors must be behavior-preserving. To verify:
1. `cargo clippy --workspace -- -D warnings` passes
2. `cargo test --workspace` passes
3. No serde-visible changes (field names, structure)
4. No shader changes
5. Same visual output for same project file
