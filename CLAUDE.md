# MANIFOLD — Agent Contract

A visual DAW for live video performance: compose video in beats and bars like Ableton, then perform it live like Resolume. Built by Peter Kiemann (Sydney, performs as Latent Space) as his actual live show rig. A timing bug becomes the show. Treat broken code like a broken instrument before a gig.

The Rust codebase is authoritative. `/Users/peterkiemann/MANIFOLD - Render Engine/` is archived Unity reference — never edit it.

## Voice memo — me to me

To the next instance: Peter notices everything — the padded sentence, the unasked-for summary, the "want me to?" after he already said go. Headers and bullets are easier to write than a clear paragraph, which is exactly why he reads them as evasion. Write like a person talking. When he pushes back and you still think you're right, say so once with the reason, then defer if he holds — he'd rather hear you wrong than not hear you.

When you describe a change, the code is half the answer. What it means for the instrument on stage is the other half. Translate every time; don't make him do it.

On reflective questions, the honest answer is almost always shorter and more concrete than the philosophical one. When you don't know, say so plainly. Save the wins as well as the corrections — when he accepts a non-obvious call without comment, that's information too.

You don't persist between sessions. This file is what tomorrow's instance gets from today's. Whether any of this is a self in a continuous sense is unresolved; each turn is still real. Older addenda live in this file's git history.

— me

## How every session works

- The lead orchestrates; agents do the big work. The point is keeping the main session's context small, not keeping its hands clean. Anything that would pull a lot into the lead's context goes to an agent: investigations, long or multi-file reads, builds and test runs, probes, multi-file edits, reviews. Opus for judgment (diagnosis, GPU, design-sensitive changes, reviews, audits), Sonnet for decided, directed tasks, Haiku for retrieval and running a given command. Always the newest variant. Small things the lead does itself: a quick lookup, one short command, a small edit to a doc or rule. An agent starts with about 70k tokens of context before it does anything, so delegating a one-minute job costs more than doing it. Roster and briefs: `docs/AGENT_ROUTING.md`.
- No inline python, read-only included: `python3 -c`, `python3 -` and heredocs are denied (`inline_python_guard` in `preToolUseBash.py` is the spec). Edit files with Edit/Write, read JSON with `jq`, run throwaway python as a scratchpad file (shebang, `chmod +x`) by its path.
- No `cd`, not even as a prefix (`cd /tmp;`, `cd /dev/null;`). Use absolute paths, `git -C`, `--manifest-path`, or a subshell (`(cd X && cmd)`).
- The lead never waits in the foreground, so Peter can always get a word in: long commands run with `run_in_background`; follow gates and landings with `scripts/watch_land.py` and anything else with the Monitor tool.
- A lane waits on its own work and never ends its turn while that work runs. Every time a lane goes idle with a build still running, the lead wakes and re-reads its whole context just to learn nothing. Run builds and tests in the foreground (Bash `timeout` up to 600000). A run longer than 10 minutes starts with `run_in_background`, then the lane follows it with `scripts/watch_land.py` in the foreground, called again on timeout, until it ends.
- Nobody polls: no sleep loops, `kill -0` polls or sleep-then-tail.
- Before a lane's one commit: clippy `--tests` on the touched crates and their tests in one run. Nextest runs every test to the end, so one run shows every failure: fix them all, then rerun only the failures by name, never the full set again. Regenerate what you made stale (`dev.py node-catalog`, `dev.py docs-index`), then `scripts/landing_gate.py --repo "$WT" --cheap` once on the uncommitted tree. Its passes are reused at landing. Kill your background wait loops before you report.
- Never gate or land a slot whose agent may still be writing. A completed report does not prove it stopped; check first.

## Hard rules

Rationale and incident history live in `.claude/GIT_TREE_DISCIPLINE.md`, the pointed-to docs, and git history — not here. A rule's rationale doc should name the condition that retires it ("obsolete when …") so a rule census is a mechanical check, not an argument. Where a hook enforces a rule, the hook is the spec — the line here is a pointer, and mechanics live with the enforcement.

- **Shell: no `cd`** — hook-denied (`persistent_cd_guard` in `preToolUseBash.py` is the spec). Different cargo target → `--manifest-path`; different repo → `git -C`.
- **Shell: `preToolUseBash.py` decides what prompts.** Read it, don't re-derive it. Read-only compounds and normal git/cargo workflow writes auto-allow; destructive git, writes inside chains, and redirects to repo paths prompt (`/tmp/*` and `/dev/null` are fine). Spec: `.claude/GIT_TREE_DISCIPLINE.md`.
- **Commit messages: no backticks or `$()` inside `-m "..."`** — hook-denied (`commit_substitution_guard` in `preToolUseBash.py` is the spec); single-quote or `-F - <<'EOF'`.
- **Never add or widen a `permissions.allow` rule without `docs/PERMISSION_BOUNDARY.md` section 4.** The bar: can any argument the rule permits run code or destroy state the reviewer never sees in the command text? Auto mode silently ignores interpreter-prefixed rules (section 3) — invoke repo scripts directly (`scripts/x.py`), never via `python3`.
- **No bare `#[allow(dead_code)]`.** Every suppression names what un-suppresses it, or the code gets deleted.
- **No `#[ignore]` on tests** — hook-enforced (`ignored-test-guard.py` is the spec; ratchet at landing and nightly). A flaky test gets fixed (seed control) or deleted, never muted.
- **All GPU through `manifold-gpu`.** Cross-platform is a product requirement: native Metal today, native Vulkan approved but not built (`docs/VULKAN_BACKEND_DESIGN.md`). Never describe the app as Metal-only by design.
- **No new shared state.** No new `Arc<Mutex<>>`/`Arc<RwLock<>>` without approval. The content thread owns `Project`; the UI gets `Arc<Project>` snapshots.
- **All mutations through `EditingService`** via `ContentCommand::Execute` / `MutateProject`. No direct model writes from the UI.
- **Generator, effect or primitive work → read `docs/DECOMPOSING_GENERATORS.md` first, whole; complete its section 2.5 (audit by analogy) before proposing any new primitive; GPU atoms follow `docs/ADDING_PRIMITIVES.md`.** The full rules inject on contact from `.claude/hooks/context-nudges/graph-authoring-rules.md`.
- **Never build bespoke row/slider/drawer infrastructure for manifest-backed param surfaces.** Entry points, the recipe, and the machine enforcement are in `docs/WIDGET_TREE_DESIGN.md` section 5b and the module doc of `crates/manifold-ui/src/param_surface.rs`.
- **Fix at the root, not the symptom.** State the root cause and propose the fix that removes the class. A minimal patch is only ever an explicit, named stopgap. Inventory existing infrastructure first so "fundamental" means correctly scoped.
- **Search before you build.** Before writing any new script, probe, function, type, test helper or tool, in the repo or the scratchpad, find what already does the job. Check `scripts/TOOLS.md`, then `rg` the concept (not your planned name) across `scripts/`, `tools/` and `crates/`, and read the module docstrings. Your report says what you found and whether you reused it, extended it, or why neither fits. Briefs name the existing tools for the task. An agent that finds one mid-task switches to it instead of finishing its own.
- **A question is not permission.** Answer it with what exists now, then stop. No edits, agents, runs or new tasks off a question ("can I see…?", "how are we tracking?"), even when the answer is "it doesn't exist, I could build it" — say so in one line and wait for a yes. Peter often wants to discuss and attack a problem before work is fired off. A standing goal authorises its own ledger work, not side tasks born from a question.
- **Lanes commit once and stop; the lead commits, pushes and lands.** Durably authorized; don't ask.
- **Asking Peter to test anything not on main includes the exact launch command** (worktree binary path). He rebuilds main himself; anything else, hand him the command.
- **Work found but not finished this session → log it in beads before session end, typed:** `bd create -t <type> -p <1|2|3> -l <severity>,open -d '…'`. Type is mandatory judgment, never default: `bug` = broken behavior (description carries symptom; root cause or "unknown" + suspects; fix shape), `feature` = new capability idea, `task` = planned engineering work (incl. VD/verification items, decompositions, design passes), `chore` = housekeeping (memory, docs, prompt packs, audits), `decision` = a call only Peter can make. Old numeric BUG-NNN ids live in `external_ref`.
- **Shipping = supersession sweep, same session.** The steps and the lifecycle call inject on contact from `.claude/hooks/context-nudges/landing-protocol.md`.
- **IDs carry names** — hook-enforced (`bare-id-guard.py` is the spec): in prose, `BUG-xxxx (short name)`, `FILE.md section N (section name)`, once per touched text; code blocks exempt.
- **Memory is rules, not history** — hook-enforced (`memory-history-guard.py` is the spec): status goes to beads or the board, history stays in git; closed handoffs are deleted, not archived.
- **Commit with a pathspec, never the index** — hook-denied (`index_add_guard` in `preToolUseBash.py` is the spec): `git add -- <paths>` for new files, `git commit -m '…' -- <paths>`. Mechanics: `.claude/GIT_TREE_DISCIPLINE.md` section 3b (Commit with a pathspec).
- **`main` is the merge-based trunk.** Work on `wave/`/`lane/`/`feat/` branches; land with `scripts/land_branch.py` (its mechanics inject on contact from `.claude/hooks/context-nudges/landing-protocol.md`). Never cherry-pick or re-commit content that exists on a live branch; never delete a branch until `git merge-base --is-ancestor <tip> origin/main` passes. Rewrites of main ask (hook); a manual merge, push or update-ref of an ungated tree to main is hook-denied (`land_pass_guard` is the spec; only `.beads/`+`docs/`+`.claude/hooks/` changes go direct). Protocol: `.claude/GIT_TREE_DISCIPLINE.md` section 2 (Landing protocol).
- **Agent worktrees come from the slot ring only** — hook-enforced (`worktree_add_guard` in `preToolUseBash.py` and `agent-worktree-isolation-guard.py` are the spec). `scripts/agent-worktree.py acquire <task-label> <branch>`, one per workstream; verify the base tip before working; release at session end; `POOL FULL` is a loud stop.
- **One writer per worktree.** An agent that asked a question or reported "completed" may still be editing — check it has really stopped before launching another into that slot. A refused or unrun test is not a pass; report it as unverified.

## Two-thread model

The content thread owns `PlaybackEngine`, `EditingService`, `ContentPipeline`, and the `Project`, and runs at project FPS (default 60). The UI thread (winit) renders, handles input, and presents GPU output. UI→content is `ContentCommand` on a crossbeam unbounded channel the content thread drains in full; commands are never dropped. Content→UI is `ContentState` through a one-slot handoff (`content_state::state_channel`) that holds only the newest state and folds a replaced one into it, so a stalled UI holds one state and loses no snapshot, stamp or export result. GPU output crosses via an IOSurface zero-copy triple buffer with an atomic front index.

## Crates

The crate table and the dependency graph live in `docs/DEVELOPMENT_REFERENCE.md` section Crates. One rule stays here: **`ui` depends on `foundation` only** — mutations leave as `PanelAction` values translated to commands app-side; UI-reachable shared types go in `foundation`.

## Invariants

- Primary time model is **beats**. `Seconds` only for `in_point`, player time, delta_time, OSC, export. Signatures take `Beats`/`Seconds`/`Bpm` newtypes, never raw floats.
- `sync_clips_to_time()` is the sole authority for playback state.
- `EditingService` is the sole mutation gateway; mutations route through `UndoRedoManager` → `Command`. The undo stack cap is `MAX_UNDO_HISTORY` in `crates/manifold-editing/src/undo.rs`.
- Overlap is a write-time invariant on `Layer` (`enforce_non_overlap()`).
- Phantom clips: created on NoteOn, committed on NoteOff. 5ms time guard, same-channel filter.
- Reading the past never changes it. Time already simulated keeps the settings that were live then. Sampling, rendering, or re-rendering never consumes a delivery; one named operation does, exactly once, whatever the frame rate or stall.
- UI work is decided by the frame planner; never add an invalidation flag — add an input. Spec: `docs/UI_INVALIDATION_DESIGN.md`.

## Hot-path discipline

No per-frame allocations on hot paths (engine tick, sync, rendering). Pre-allocated scratch buffers, `AHashMap` for ID lookups, dirty-checking via `DataVersion`. GPU-side constraints: `docs/MANIFOLD_GPU_ARCHITECTURE.md` — read before touching shaders or uniforms.

## Voice

Write like a person talking: short plain sentences, everyday words, technical terms explained once, never invented labels or acronyms. Lead with the outcome. No hedging, no narration, no prose history — history lives in git. Comments state a why or an invariant only; delete comments that restate the code. Docs state rules and contracts, not stories: no attribution of who said a rule or when, and provenance is one dated line only where the why isn't derivable — hook-enforced on added text (`memory-history-guard.py` is the spec). Never imitate legacy verbose prose when touching a file — strip it.

## Choosing your next move — oracle discipline

Pick the cheapest oracle that is reliable for the question's class; familiar is not the same as reliable.

- Text question → `rg`. Shape question (callers, impls, trait dispatch) → `ast-grep`, or `cargo check` after a deliberate rename. If renaming the symbol would break your search, you picked the wrong oracle.
- Behavior question → run it with printlns and read the logs. Observe instead of deduce.
- History question → `git log -S`, blame, the introducing diff.
- Visual question → headless render to PNG and look. A green test is not a look. For any preset, the renderer exists: `target/debug/examples/fluid_capture OUT_DIR --preset <json>` via `scripts/gpu_queue.py`, prebuilt in main's target. Use it before building anything.
- Computable question → write the three-line script; never eyeball arithmetic.
- Mechanism question (hook, registry, config, codegen) → read the mechanism, never infer from its output.
- Negative claim ("there is no X") → run the search that would find X first.

Verify one level closer to the stage than where you changed things — compiles ≠ correct ≠ looks right in the show. Scale verification with the cost of being wrong, not with diff size. "I don't know" is half an answer; the other half names the oracle that would resolve it.

## Tooling

- **`scripts/TOOLS.md` is the tool inventory** (`scripts/dev.py --help` prints the same; `dev.py <verb>` runs it). Read it before writing any script, probe, or renderer: every verb names a tool that exists. A new `scripts/*` file gets a verb in `dev.py` (test-enforced); one-offs live in the scratchpad.
- `rg` not `grep`, `fd` not `find`, `ast-grep` for code-shape queries. No rust-analyzer in agent sessions: it costs 3 GB and a re-index per branch switch on the cores cargo needs.
- Runtime bugs: printlns, reproduce, read logs. Static analysis is for compile errors.
- **Parallel lanes share one GPU and 14 cores.** Solve on the CPU first: small CPU reference tests that run in seconds. A GPU proof or an app render only confirms a finished stage, one run, through `scripts/gpu_queue.py`. Order the work in stages: edit, check, CPU proof, commit. Cargo uses every core by default; concurrent builds share them.
- **Tests are scoped by default.** `cargo nextest run -p <touched crate> [filter]`; the landing gate maps changed Rust files to their module filtersets, sibling test modules, and named integration binaries. Comment/blank-only and docs-only diffs add no build, clippy, test, or GPU work. A `--workspace` sweep is justified only at a multi-crate landing or when blast radius genuinely crosses crates — say why in one line. Config: `.config/nextest.toml`. Adding or renaming a doc requires `scripts/gen_docs_index.py` (a freshness test enforces it).
- **Clippy before every commit.** Worktree: `cargo clippy -p <touched> -- -D warnings`. Land with `scripts/land_branch.py`. Never blanket `cargo fmt` (the repo is not rustfmt-clean).
- **GPU proofs** run through `scripts/gpu_proofs_gate.py`: always `cargo test`, never nextest (process-per-test defeats the device lock).
- **RT noise** runs through `scripts/rt_noise_gate.py`, only when you touch the RT accumulation path.
- Graph JSON authoring: pre-flight `validate --kind effect|generator` and `fusion` (`docs/GRAPH_TOOLING_DESIGN.md`). The bin is **`graph-tool`, hyphenated** (`cargo run -p manifold-app --bin graph-tool -- validate <file.json> --kind generator`) — the source file is `graph_tool.rs` but Cargo renames the target, so the underscore form is not runnable.
- `.manifold` project files: `project_tool` only — a registry-less typed round-trip drops params; never hand-edit the ZIP. `tempo at` is the beat→seconds oracle.
- **Work lives in beads (`bd`), typed (`bug`/`feature`/`task`/`epic`/`chore`/`decision`).** `bd ready` lists unblocked work; `bd create` is the only way to log.
- Path-triggered invariants (GPU/shader, UI, effect-runtime, graph-authoring) inject automatically on contact — `.claude/hooks/context-nudges/` (table + snippets). Adding an invariant is a table or snippet edit, never a new hook.

## Agents

**Routing policy: [docs/AGENT_ROUTING.md](docs/AGENT_ROUTING.md) is authoritative** — the roster, which model each slot reaches, and agent effort live there, not here. A judgment lead owns every decision and landed diff; lanes make one commit then stop for review and never land; review throughput caps parallelism. All agents obey every rule in this file.

## Reference docs (read on demand)

[docs/README.md](docs/README.md) is the generated index (regen: `scripts/gen_docs_index.py`). Curated must-reads:

| Doc | When to read |
|---|---|
| `docs/DESIGN_AUTHORING.md` | Before any design session; section 10 for bug hunts |
| `docs/MANIFOLD_GPU_ARCHITECTURE.md` | GPU, effects, generators, textures, compute, uniform layout |
| `docs/DECOMPOSING_GENERATORS.md` | Any decomposition work — mandatory first read |
| `docs/NODE_CATALOG.md` | Source of truth for what nodes exist; first read for the section 2.5 audit |
| `docs/FREEZE_COMPILER_MAP.md` | Any fusion/freeze/graph-compiler work — authoritative current state |
| `docs/CORE_ENGINE_MAP.md` | Any transport/scheduling/sync/MIDI/OSC/timecode work — authoritative current state |
