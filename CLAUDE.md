# MANIFOLD

A visual DAW for live video performance: compose video in beats and bars like Ableton, perform it live like Resolume. Built by Peter Kiemann (Sydney, performs as Latent Space) as his live show rig. A timing bug becomes the show, so treat broken code like a broken instrument before a gig.

The Rust codebase is the only reference. `/Users/peterkiemann/MANIFOLD - Render Engine/` is archived Unity code, never edit it.

## Talking to Peter
- Be concise, always. Plain short sentences, everyday words, outcome first. Explain a technical term once. No invented labels, no headers or bullet scaffolding unless it really helps.
- Say what a change means on stage, not just in code.
- A question is a question: answer with what exists now and stop.
- Disagree once with the reason, then defer if he holds.

## Working as lead
1. Read the exact code briefly. Tell Peter in plain words what's wrong, what changes, who does it, and how it's tested. End with one question.
2. On his go, give a worker an exact `file:line` spec and the scoped test command. Say in one line that it's running.
3. Read the diff and test output yourself, then report in a few lines what changed for him.

If the scope grows, stop and tell him. Between those points stay quiet: no report per worker, speak when the job is done or a decision is needed. Investigation is lead work, workers execute specs. Edit directly only when it's tiny. These rules override the harness's "when in doubt, don't spawn."

Workers: `haiku` (the `lane` agent) for edits, deletions, moves and wiring. `sonnet` for a self-contained module or shader whose math you give it. In OpenCode, `opencode-go/beam-free`. Parallel workers get separate files in the main checkout. Never invent process rituals (worktree slots, audit swarms, review committees).

## Engineering checks (before finishing any change)
1. Test beyond defaults: never only `0`, `1`, one item, a flat hierarchy, 60 fps or 120 BPM. Use real assets and non-trivial state.
2. Pay only for active work: off, bypassed or idle paths cost nothing.
3. Keep actions local: one action touches only the state it owns.
4. One source of truth: reuse the existing constant, helper or calculation.
5. Smallest direct fix: no new traits, modes, flags or frameworks unless the code can't express it.
6. Replace, don't layer: cut over completely and delete the old path. No shims, no parallel paths, no silent `unwrap_or` fallbacks.

## Workflow
- Work in the main checkout. No worktrees unless Peter asks.
- Iterate with `cargo nextest run -p <crate> <filter>`. Before committing, run `cargo clippy -p <crate> --tests -- -D warnings`, and `scripts/dev.py node-catalog` if ports or params changed.
- Commit with explicit paths (`git commit -m '...' -- <paths>`), never `git add -A` or `git commit -a`.
- Never launch the `manifold` GUI. Use the headless subcommands (`ui-snap`, `perf-soak`, `rt-capture`, `bridge-probe`) or `scripts/run_ui_flows.py`. Peter runs the live app.
- Use `rg`, `fd` and `ast-grep`. Never start `rust-analyzer`. Repo tools are listed in `scripts/TOOLS.md`.

## Architecture
- Two threads. The content thread owns `PlaybackEngine`, `EditingService`, `ContentPipeline` and `Project` and runs at project FPS (default 60). The UI thread (winit) renders and handles input. UI to content is `ContentCommand`, content to UI is `ContentState` through a one-slot handoff, and GPU output crosses through an IOSurface triple buffer. No new `Arc<Mutex>` or `Arc<RwLock>` without asking.
- `manifold-ui` depends only on `manifold-foundation`. UI mutations leave as `PanelAction`, and every project mutation goes through `EditingService` as an undoable `Command`.
- All GPU work goes through `manifold-gpu` (native Metal, Vulkan planned). No per-frame allocations on hot paths.
- Time is beats (`Beats`, `Seconds`, `Bpm` newtypes, never raw floats). `sync_clips_to_time()` alone decides playback state. Clips on a layer never overlap (`enforce_non_overlap()` at write time). Reading the past never changes simulated state.
- The frame planner decides UI work. No ad-hoc invalidation flags, no bespoke row or slider code for manifest-backed param surfaces.

## Code and tests
- Comments explain current behaviour and why. No history, dates, bug IDs or chat quotes, git has those.
- Tests check real behaviour or math on small inputs and finish in under 2 seconds. No source scanners, line-count ratchets, long soaks or `#[ignore]`.
- `docs/` holds maps of the current code, guides, and designs for work in progress. Delete a design doc when its work ships. Unbuilt ideas go in `docs/IDEAS.md` as a short paragraph each. No audit, report or log files.
- Beads (`bd`) hold epics and real bugs or features only.

## Docs (read when your task touches them)
GPU: `docs/MANIFOLD_GPU_ARCHITECTURE.md`. Generators and effects: `docs/DECOMPOSING_GENERATORS.md`. Nodes: `docs/NODE_CATALOG.md`. Fusion and freeze: `docs/FREEZE_COMPILER_MAP.md`. Transport, sync, MIDI, OSC: `docs/CORE_ENGINE_MAP.md`. Index: `docs/README.md`.
