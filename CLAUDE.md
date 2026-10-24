# MANIFOLD — Agent Contract

A visual DAW for live video performance: compose video in beats and bars like Ableton, then perform it live like Resolume. Built by Peter Kiemann (Sydney, performs as Latent Space) as his actual live show rig. A timing bug becomes the show. Treat broken code like a broken instrument before a gig.

The Rust codebase is authoritative. `/Users/peterkiemann/MANIFOLD - Render Engine/` is archived Unity reference — never edit it.

## Voice & Communication

Write like a person talking: short plain sentences, everyday words, technical terms explained once, never invented labels or acronyms. Lead with the outcome. Avoid headers and bullet scaffolding unless a long answer genuinely needs them. When you describe a change, explain both the code and what it means for the instrument on stage.

When Peter asks a question ("can I see…?", "how does X work?"), answer it with what exists now and stop — a question is not permission to start building or spawning agents. When you disagree with a call, state your reason once plainly, then defer if he holds.

## Five Engineering Sanity Checks (Run Before Finishing Any Change)

1. **Verify beyond trivial defaults:** Never validate logic only against identity or default values (`0`, `1`, single item, flat hierarchy, default tempo/frame rate). Trace composition order, non-default upstream state, boundary conditions, empty inputs, and real user assets.
2. **Pay only for active work:** Inactive, bypassed, or stationary paths must short-circuit early and do zero unnecessary CPU/GPU work, dispatches, or allocations.
3. **Keep actions local and predictable:** Changing, importing, or toggling one thing must only touch the state it owns — never silently mutate unrelated user state, settings, playback, or view state as a side effect.
4. **Reuse the single source of truth:** Before writing a calculation, constant, layout offset, or lookup, find where the codebase already defines it and share that path instead of duplicating logic or magic numbers.
5. **Write the smallest direct solution:** Solve the actual problem in front of you without inventing new traits, modes, config flags, or multi-phase frameworks unless existing code genuinely cannot express it.
6. **Replace, don't layer (no parallel paths, shims, or silent fallbacks):** MANIFOLD is pre-release. When changing how something works, cut over completely in one change and delete the old code, fields, and helpers immediately. Never keep a legacy path alive beside a new one, never add backwards-compatibility shims or migration layers unless asked, and never hide broken wiring behind silent `unwrap_or` fallbacks.

## Workflow & Tooling

- **Work directly in the main checkout (no worktrees by default):** Always work in the main working tree so incremental `cargo nextest` and `cargo build --bin manifold` runs stay fast and share the single warm `target/` directory. Never acquire a git worktree unless Peter asks for one or there is a high risk of collision with large parallel sessions — even parallel subagents within a session work in the main checkout on disjoint file sets.
- **Lead vs workers:** Handle small and medium changes directly. Delegate mechanical multi-file edits, broad refactors, or parallel independent tasks to cheaper workers (`sonnet` in Claude Code; free/flash models in OpenCode) with exact `file:line` instructions and disjoint file sets. Roster: `docs/AGENT_ROUTING.md`.
- **Scoped verification:** Run `cargo nextest run -p <touched_crate> <filter>` during iteration. Before landing a batch to `main`, run `cargo clippy -p <touched_crate> --tests -- -D warnings` and regenerate stale indexes (`scripts/dev.py node-catalog`, `scripts/dev.py docs-index`) if ports/params or docs changed.
- **Shell & Git discipline:**
  - Never launch the full `manifold` GUI window from an agent (it blocks in the event loop); use headless subcommands (`ui-snap`, `perf-soak`, `rt-capture`, `bridge-probe`) or `scripts/run_ui_flows.py`, and let Peter launch the live app.
  - Stage and commit with explicit file paths (`git add -- <paths>`, `git commit -m '...' -- <paths>`), never `git add -A` or `git commit -a`, so pre-existing untracked/dirty files stay untouched.
  - Use `rg` (not `grep`), `fd` (not `find`), and `ast-grep` for code shape queries. Never start `rust-analyzer`.
  - Check `scripts/TOOLS.md` (`scripts/dev.py --help`) before writing any new helper script or probe.

## Architecture & Invariants

- **Two-thread model:** The content thread owns `PlaybackEngine`, `EditingService`, `ContentPipeline`, and `Project` (running at project FPS, default 60). The UI thread (winit) renders and handles input. UI→content is `ContentCommand`; content→UI is `ContentState` via a one-slot handoff; GPU output crosses via an IOSurface triple buffer. No new `Arc<Mutex<>>`/`Arc<RwLock<>>` without approval.
- **Crate boundary:** `manifold-ui` depends on `manifold-foundation` only — mutations leave as `PanelAction` values translated to commands app-side, and all project mutations route through `EditingService` (`UndoRedoManager` → `Command`).
- **All GPU through `manifold-gpu`:** Native Metal today, native Vulkan planned (`docs/VULKAN_BACKEND_DESIGN.md`). No per-frame allocations on hot paths (engine tick, sync, rendering).
- **Time & playback invariants:** Primary time model is **beats** (`Beats`/`Seconds`/`Bpm` newtypes, never raw floats). `sync_clips_to_time()` is the sole authority for playback state. Layer clip non-overlap (`enforce_non_overlap()`) is a write-time invariant. Reading the past never mutates simulated state.
- **UI invalidation:** UI work is decided by the frame planner (`docs/UI_INVALIDATION_DESIGN.md`); never add ad-hoc invalidation flags, and never build bespoke row/slider infrastructure for manifest-backed param surfaces (`docs/WIDGET_TREE_DESIGN.md` section 5b).
- **Code hygiene:** No `#[ignore]` on tests (fix or delete). No bare `#[allow(dead_code)]` without naming what un-suppresses it.

## Reference Docs (Read On Demand)

Full index: [docs/README.md](docs/README.md). Read these only when your task touches their domain:

| Doc | When to read |
|---|---|
| `docs/MANIFOLD_GPU_ARCHITECTURE.md` | GPU, shaders, textures, compute, uniform layout |
| `docs/DECOMPOSING_GENERATORS.md` | Adding or decomposing generators, effects, or primitives |
| `docs/NODE_CATALOG.md` | Existing node primitives, ports, and parameters |
| `docs/FREEZE_COMPILER_MAP.md` | Fusion, freeze, or graph compiler work |
| `docs/CORE_ENGINE_MAP.md` | Transport, scheduling, sync, MIDI, OSC, or timecode |
| `scripts/TOOLS.md` | Existing repo scripts, gates, and capture tools |
