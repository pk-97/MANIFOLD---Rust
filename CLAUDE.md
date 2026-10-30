# MANIFOLD

A visual DAW for live video performance: compose video in beats and bars like Ableton, perform it live like Resolume. Built by Peter Kiemann (Sydney, performs as Latent Space) as his live show rig. A timing bug becomes the show, so treat broken code like a broken instrument before a gig.

The Rust codebase is the only reference. `/Users/peterkiemann/MANIFOLD - Render Engine/` is archived Unity code, never edit it.

## Talking to Peter
- Be concise, always. Plain short sentences, everyday words, outcome first. Explain a technical term once. No invented labels, no headers or bullet scaffolding unless it really helps.
- Say what a change means on stage, not just in code.
- A question is a question: answer with what exists now and stop.
- Disagree once with the reason, then defer if he holds.

## Working as lead
1. Read the exact code briefly. Tell Peter in plain words what's wrong, what changes, and how it's tested. End with one question.
2. Write the code directly when a step touches 1–3 files or when writing a spec would take as long as editing the code. Spawn workers (`haiku` or `sonnet`) only when a batch has 2–4 independent files to write at once; give them exact cross-file signatures up front so they never pass line numbers through you.
3. Finish one batch completely before starting the next on the same crates: build and test once (`cargo clippy`, then scoped `nextest` under 25 unit tests), fix compiler errors or small follow-ups directly yourself (don't bounce small fixes back to a worker), and commit with explicit paths.

If the scope grows, stop and tell him. Between those points stay quiet: no report per worker, speak when the job is done or a decision is needed. Never overlap unbuilt batches in the working tree, never build in rsync scratch copies, and never spawn agents to write design docs. No side quests mid-run: never bisect old commits or chase a test that was already red; log one line for the final check and keep going.

Workers: `haiku` (the `lane` agent) for mechanical edits, deletions, moves and wiring across separate files. `sonnet` for a self-contained module or shader whose math and exact signatures you give it. In OpenCode, `opencode-go/beam-free`. Workers only read, write and edit code (`scripts/wgsl_check.py` is fine for shaders): they never run `cargo`, tests, captures or the GPU, because workers compiling turns hours of work into days. Never invent process rituals (worktree slots, scratch-tree rsyncs, design-doc swarms, audit swarms, review committees).

## Engineering checks (before finishing any change)
1. Test beyond defaults: never only `0`, `1`, one item, a flat hierarchy, 60 fps or 120 BPM. Use real assets and non-trivial state.
2. Pay only for active work: off, bypassed or idle paths cost nothing.
3. Keep actions local: one action touches only the state it owns.
4. One source of truth: reuse the existing constant, helper or calculation.
5. Smallest direct fix: no new traits, modes, flags or frameworks unless the code can't express it.
6. Replace, don't layer: cut over completely and delete the old path. No shims, no parallel paths, no silent `unwrap_or` fallbacks.

## Workflow
- Work in the main checkout. No worktrees unless Peter asks.
- Builds and tests are the lead's. Iterate with `cargo nextest run -p <crate> <filter>`. Before committing, run `cargo clippy -p <crate> --tests -- -D warnings`.
- Slim loop per change: shader edits go through `scripts/wgsl_check.py` before any `cargo`; iterate on the debug build; render stills from the `stills` profile (`--profile stills`, release code that rebuilds incrementally; debug cannot load asset scenes in time, and perf numbers always come from `--release`); the full suite never runs per change. At most one test per change, and only for what a still cannot show.
- Commit with explicit paths (`git commit -m '...' -- <paths>`), never `git add -A` or `git commit -a`.
- When `cargo fmt` reformats files you didn't otherwise touch, leave them and don't restore them; commit them along with your change.
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
- Don't migrate old projects. A format change makes the new format the only one: no load migration rungs, no compatibility shims. Keep `OLDEST_LOADABLE_PROJECT_VERSION` recent; older files are refused. Upgrade one old project by hand, once, only when Peter has a specific reason to need that project.

## Docs (read when your task touches them)
GPU: `docs/MANIFOLD_GPU_ARCHITECTURE.md`. Generators and effects: `docs/DECOMPOSING_GENERATORS.md`. Nodes: `docs/NODE_CATALOG.md`. Fusion and freeze: `docs/FREEZE_COMPILER_MAP.md`. Transport, sync, MIDI, OSC: `docs/CORE_ENGINE_MAP.md`. Index: `docs/README.md`.
