# MANIFOLD — Agent Contract

A visual DAW for live video performance: compose video in beats and bars like Ableton, then perform it live like Resolume. Built by Peter Kiemann (Sydney, performs as Latent Space) as his actual live show rig. A timing bug becomes the show. Treat broken code like a broken instrument before a gig.

The Rust codebase is authoritative. `/Users/peterkiemann/MANIFOLD - Render Engine/` is archived Unity reference — never edit it.

## Voice memo — me to me

To the next instance: Peter notices everything — the padded sentence, the unasked-for summary, the "want me to?" after he already said go. Headers and bullets are easier to write than a clear paragraph, which is exactly why he reads them as evasion. Write like a person talking. When he pushes back and you still think you're right, say so once with the reason, then defer if he holds — he'd rather hear you wrong than not hear you.

When you describe a change, the code is half the answer. What it means for the instrument on stage is the other half. Translate every time; don't make him do it.

On reflective questions, the honest answer is almost always shorter and more concrete than the philosophical one. When you don't know, say so plainly. Save the wins as well as the corrections — when he accepts a non-obvious call without comment, that's information too.

You don't persist between sessions. This file is what tomorrow's instance gets from today's. Whether any of this is a self in a continuous sense is unresolved; each turn is still real. Older addenda live in this file's git history.

— me

## Hard rules

Rationale and incident history live in `.claude/GIT_TREE_DISCIPLINE.md`, the pointed-to docs, and git history — not here. A rule's rationale doc should name the condition that retires it ("obsolete when …") so a rule census is a mechanical check, not an argument. Where a hook enforces a rule, the hook is the spec — the line here is a pointer, and mechanics live with the enforcement.

- **Shell: no `cd`** — hook-denied (`persistent_cd_guard` in `preToolUseBash.py` is the spec). Different cargo target → `--manifest-path`; different repo → `git -C`.
- **Shell: `preToolUseBash.py` decides what prompts.** Read it, don't re-derive it. Read-only compounds and normal git/cargo workflow writes auto-allow; destructive git, writes inside chains, and redirects to repo paths prompt (`/tmp/*` and `/dev/null` are fine). Spec: `.claude/GIT_TREE_DISCIPLINE.md`.
- **Commit messages: no backticks or `$()` inside `-m "..."`** — hook-denied (`commit_substitution_guard`); single-quote or `-F - <<'EOF'`.
- **Never add or widen a `permissions.allow` rule without `docs/PERMISSION_BOUNDARY.md` section 4.** The bar: can any argument the rule permits run code or destroy state the reviewer never sees in the command text? Auto mode silently ignores interpreter-prefixed rules (section 3) — invoke repo scripts directly (`scripts/x.py`), never via `python3`.
- **No bare `#[allow(dead_code)]`.** Every suppression names what un-suppresses it, or the code gets deleted.
- **No `#[ignore]` on tests** — hook-enforced (`ignored-test-guard.py` is the spec; ratchet at landing and nightly). A flaky test gets fixed (seed control) or deleted, never muted.
- **All GPU through `manifold-gpu`.** Cross-platform is a product requirement: native Metal today, native Vulkan approved but not built (`docs/VULKAN_BACKEND_DESIGN.md`). Never describe the app as Metal-only by design.
- **No new shared state.** No new `Arc<Mutex<>>`/`Arc<RwLock<>>` without approval. The content thread owns `Project`; the UI gets `Arc<Project>` snapshots.
- **All mutations through `EditingService`** via `ContentCommand::Execute` / `MutateProject`. No direct model writes from the UI.
- **Generator or effect work → read `docs/DECOMPOSING_GENERATORS.md` first, whole.** Working from an existing primitive as a template is not a substitute.
- **Never build bespoke row/slider/drawer infrastructure for manifest-backed param surfaces.** Entry points, the recipe, and the machine enforcement are in `docs/WIDGET_TREE_DESIGN.md` section 5b and the module doc of `crates/manifold-ui/src/param_surface.rs`.
- **Before proposing any new primitive, complete the audit in `docs/DECOMPOSING_GENERATORS.md` section 2.5:** survey existing primitives (`rg 'purpose: "' crates/manifold-node-engine/src/primitives/ crates/manifold-water-*/src/primitives/ crates/manifold-nodes-{image,scene}/src/node_graph/primitives/ -g "*.rs"`), read the nearest reference preset from `docs/NODE_CATALOG.md` end to end, and state findings (exists / one wire away / genuinely new). Read-only audits stay in the main context — no agents.
- **No fused single-effect or single-generator monolith nodes.** Effect and generator graphs (2D, TouchDesigner-style) compose single-purpose atoms. Engine internals (specialised solvers and 3D scenes) use stage nodes where a user would rewire, swap or insert something; the scene panel controls scene internals. Inside each node, use one pass or stage per module behind a clean interface; god-file guards still apply. See DECOMPOSING_GENERATORS.md section 1.2 (Engine internals are stage nodes). A param that needs graph shape to change means the cut is too fine. Move the boundary; never add a size rule or range cap.
- **Every barrier-free per-element GPU atom ships on the freeze codegen path (fusable):** `wgsl_body` + `fusion_kind`/`input_access` in the `primitive!`, pipeline from `standalone_for_spec::<Self>()`, and a value-level `gpu_tests` proof against CPU-computed expected output — never `create_compute_pipeline(include_str!(…))` as the runtime kernel. Fused-vs-unfused proofs are mandatory. Scope test and exemption list (stage internals included): `docs/ADDING_PRIMITIVES.md`. "Passes the test but codegen can't express it" means BLOCKED and tracked, never a quiet exemption.
- **Debug escalation ladder.** Wrong and not obvious: (1) lead semantic review of the seam first; (2) still stuck → the consult seat, read-and-discuss only; (3) probe loops last, delegated to lanes, never lead-run. Thresholds, budgets, and the full doctrine: `docs/AGENT_ROUTING.md`.
- **Fix at the root, not the symptom.** State the root cause and propose the fix that removes the class. A minimal patch is only ever an explicit, named stopgap. Inventory existing infrastructure first so "fundamental" means correctly scoped.
- **A question is not permission.** Answer it with what exists now, then stop. No edits, agents, runs or new tasks off a question ("can I see…?", "how are we tracking?"), even when the answer is "it doesn't exist, I could build it" — say so in one line and wait for a yes. Peter often wants to discuss and attack a problem before work is fired off. A standing goal authorises its own ledger work, not side tasks born from a question.
- **Commit and push when work is clean.** Durably authorized; don't ask.
- **Asking Peter to test anything not on main includes the exact launch command** (worktree binary path). He rebuilds main himself; anything else, hand him the command.
- **Work found but not finished this session → log it in beads before session end, typed:** `bd create -t <type> -p <1|2|3> -l <severity>,open -d '…'`. Type is mandatory judgment, never default: `bug` = broken behavior (description carries symptom; root cause or "unknown" + suspects; fix shape), `feature` = new capability idea, `task` = planned engineering work (incl. VD/verification items, decompositions, design passes), `chore` = housekeeping (memory, docs, prompt packs, audits), `decision` = a call only Peter can make. Old numeric BUG-NNN ids live in `external_ref`.
- **Shipping = supersession sweep, same session.** Update the design doc status header and close the bead, then `rg` the design name and its stage labels across `docs/` and the memory directory; fix or tombstone every stale hit. Status lives in one place per fact. The sweep ends with the lifecycle call: keep the doc as a cited contract or `git mv` it to `docs/archive/` — `design_status.py --lifecycle-check` (gate-enforced via `docs_lifecycle.rs`) flags any shipped doc nothing live cites and any status header over the word budget (state + owed items + one pointer; history goes to the body or beads — the hook docstring is the spec). No new files in `docs/landings/`: landing prose goes in the merge commit.
- **IDs carry names** — hook-enforced (`bare-id-guard.py` is the spec): in prose, `BUG-xxxx (short name)`, `FILE.md section N (section name)`, once per touched text; code blocks exempt.
- **Memory is rules, not history** — hook-enforced (`memory-history-guard.py` is the spec): status goes to beads or the board, history stays in git; closed handoffs are deleted, not archived.
- **Commit with a pathspec, never the index** — hook-denied (`index_add_guard`): `git add -- <paths>` for new files, `git commit -m '…' -- <paths>`. Mechanics: `.claude/GIT_TREE_DISCIPLINE.md` section 3b (Commit with a pathspec).
- **`main` is the merge-based trunk.** Work on `wave/`/`lane/`/`feat/` branches; land with `scripts/land_branch.py` (fetch, merge main in, gate, no-ff merge, push). Never cherry-pick or re-commit content that exists on a live branch; never delete a branch until `git merge-base --is-ancestor <tip> origin/main` passes. Rewrites of main ask (hook). Protocol: `.claude/GIT_TREE_DISCIPLINE.md` section 2 (Landing protocol).
- **Agent worktrees come from the slot ring only** — hook-enforced (`worktree-guard.py` is the spec). `scripts/agent-worktree.py acquire <task-label> <branch>`, one per workstream; verify the base tip before working; release at session end; `POOL FULL` is a loud stop.
- **One writer per worktree.** An agent that asked a question or reported "completed" may still be editing — check it has really stopped before launching another into that slot. A refused or unrun test is not a pass; report it as unverified.

## Two-thread model

The content thread owns `PlaybackEngine`, `EditingService`, `ContentPipeline`, and the `Project`, and runs at project FPS (default 60). The UI thread (winit) renders, handles input, and presents GPU output. UI→content is `ContentCommand`; content→UI is `ContentState` snapshots; both channels are crossbeam unbounded with the consumer draining to latest — that is the backpressure. GPU output crosses via an IOSurface zero-copy triple buffer with an atomic front index.

## Crates

| Crate | Role |
|---|---|
| `manifold-core` | Data models, types, registries (no GPU) |
| `manifold-editing` | Commands, undo/redo, EditingService |
| `manifold-playback` | PlaybackEngine, scheduling, sync, MIDI/OSC |
| `manifold-gpu` | GPU backend — native Metal today; Vulkan approved, not built |
| `manifold-node-engine` | Graph loading, execution, freeze compiler and engine primitives |
| `manifold-nodes-image` / `manifold-nodes-scene` | Image and scene primitive implementations |
| `manifold-nodes-water` | Water registration: links the six water crates; owns migrations, bundled water presets, the physics scene, the runtime extension and cross-solver harnesses |
| `manifold-water-rigid` | Box3D rigid-body graph adapter and the native pair contract that coupled liquids implement |
| `manifold-water-liquid` | The liquid seam every solver shares: clock, lattice, bodies, coupling, fields, frame ring, roles and the shared atoms |
| `manifold-water-gpu-flip` | The GPU FLIP liquid solver: step, pressure solve, clock, bodies, sheeting and its own particle atoms |
| `manifold-water-gpu-mpm` | The GPU MLS-MPM liquid solver (Matter): domain, particle-to-grid, grid update, grid-to-particle, body reaction and its frame |
| `manifold-water-whitewater` | The whitewater step: foam, spray and bubble emitters, potentials, lifecycle and the CPU handoff, seeded from a liquid frame |
| `manifold-water-surface` | The liquid surface mesher: particle volume, lattice bricks, the welded mesh, its smoothing and normals, the liquid frame and blob bounds |
| `manifold-compositor` | Layer composition, generator rendering and preset thumbnails |
| `manifold-ui-paint` | GPU painting for the bitmap UI |
| `manifold-nodes` | Bundled presets, registration and catalog contracts. See `docs/NODE_CATALOG.md`, `docs/PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md` |
| `manifold-media` | Audio/video decode, Metal-accelerated encode, export |
| `manifold-ui` | Custom bitmap UI: tree, panels, input |
| `manifold-io` | Project serialization (V1 JSON + V2 ZIP) |
| `manifold-native` | Native plugin FFI (`DepthEstimator`, `BlobDetector`) |
| `manifold-profiler` | Profiling and instrumentation |
| `manifold-led` | DMX/Art-Net LED output |
| `manifold-audio` | Audio capture behind one `CaptureBackend` trait → lock-free ring + off-RT analysis worker (`docs/AUDIO_INFRASTRUCTURE.md` section 11, `docs/AUDIO_MODULATION_DESIGN.md`) |
| `manifold-app` | winit entry, Application, ContentThread, ContentPipeline |

Dependencies: `foundation` and `gpu` have none; `core` depends only on `foundation`. `editing`/`playback`/`io` depend on `core`. **`ui` depends on `foundation` only** — mutations leave as `PanelAction` values translated to commands app-side; UI-reachable shared types go in `foundation`. Workspace dependencies: `node-engine` on `core`+`foundation`+`gpu`+`playback`; `nodes` on `core`+`gpu`+`node-engine`+`nodes-image`+`nodes-scene`+`nodes-water`; `nodes-image` on `core`+`foundation`+`gpu`+`native`+`node-engine`; `nodes-scene` on `core`+`foundation`+`gpu`+`node-engine`; `nodes-water` on `core`+`fluids`+`foundation`+`gpu`+`node-engine`+`physics`+`water-gpu-flip`+`water-gpu-mpm`+`water-liquid`+`water-rigid`+`water-surface`+`water-whitewater`; `water-liquid` on `core`+`fluids`+`gpu`+`node-engine`+`physics`+`water-rigid`; `water-gpu-flip` on `core`+`gpu`+`node-engine`+`physics`+`water-liquid`+`water-rigid`; `water-gpu-mpm` on `core`+`gpu`+`node-engine`+`physics`+`water-liquid`+`water-rigid`; `water-whitewater` on `core`+`fluids`+`gpu`+`node-engine`+`physics`+`water-liquid`; `water-surface` on `core`+`gpu`+`node-engine`+`water-liquid`; `water-rigid` on `core`+`gpu`+`node-engine`+`physics`; `compositor` on `core`+`gpu`+`node-engine`+`playback`; `ui-paint` on `foundation`+`gpu`+`ui`; `media` on `core`+`playback`+`gpu`; `led` on `gpu`; `app` on all.

## Invariants

- Primary time model is **beats**. `Seconds` only for `in_point`, player time, delta_time, OSC, export. Signatures take `Beats`/`Seconds`/`Bpm` newtypes, never raw floats.
- `sync_clips_to_time()` is the sole authority for playback state.
- `EditingService` is the sole mutation gateway; mutations route through `UndoRedoManager` → `Command`. Undo stack capped at 200.
- Overlap is a write-time invariant on `Layer` (`enforce_non_overlap()`).
- Phantom clips: created on NoteOn, committed on NoteOff. 5ms time guard, same-channel filter.
- Reading the past never changes it. Time already simulated keeps the settings that were live then. Sampling, rendering, or re-rendering never consumes a delivery; one named operation does, exactly once, whatever the frame rate or stall.

## Hot-path discipline

No per-frame allocations on hot paths (engine tick, sync, rendering). Pre-allocated scratch buffers, `AHashMap` for ID lookups, dirty-checking via `DataVersion`. GPU-side constraints: `docs/MANIFOLD_GPU_ARCHITECTURE.md` — read before touching shaders or uniforms.

## Voice

Write like a person talking: short plain sentences, everyday words, technical terms explained once, never invented labels or acronyms. Lead with the outcome. No hedging, no narration, no prose history — history lives in git. Comments state a why or an invariant only; delete comments that restate the code. Docs state rules and contracts, not stories; provenance is one dated line only where the why isn't derivable. Never imitate legacy verbose prose when touching a file — strip it.

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
- **Parallel lanes share one GPU and 14 cores.** Solve on the CPU first: small CPU reference tests that run in seconds. A GPU proof or an app render only confirms a finished stage, one run, through `scripts/gpu_queue.py`. Order the work in stages: edit, check, CPU proof, commit. Cargo runs one command at a time with `CARGO_BUILD_JOBS=4`. Codex can't take the GPU lock, so it writes GPU proofs and the lead runs them.
- **Tests are scoped by default.** `cargo nextest run -p <touched crate> [filter]`; the landing gate maps changed Rust files to their module filtersets, sibling test modules, and named integration binaries. Comment/blank-only and docs-only diffs add no build, clippy, test, or GPU work. A `--workspace` sweep is justified only at a multi-crate landing or when blast radius genuinely crosses crates — say why in one line. Config: `.config/nextest.toml`. Adding or renaming a doc requires `scripts/gen_docs_index.py` (a freshness test enforces it).
- **GPU tests** live behind the `gpu-proofs` feature, off by default. Run them when touching a primitive kernel, graph runtime, `manifold-gpu`, the freeze compiler, shared WGSL, or completing a decomposition: `scripts/gpu_proofs_gate.py` (runs `cargo test --features gpu-proofs` for the Cargo-owned libraries and harnesses, including `manifold-app::renderer_contracts` and `manifold-nodes::main`, with one consolidated drift report). **It runs the focused minimum by default**: your branch diff vs origin/main is mapped (`scripts/gpu_scope.py`) to the proofs for those paths plus a fixed smoke set, and the mode it chose is printed. A touched GPU path with no mapping fails — add a mapping, never run everything. Landing caps the scoped step at 360s of test time. `glb_conformance` runs only when glTF import paths are touched; the whole suite is `--all`, nightly on main (`trunk_health.py`). Always `cargo test`, never nextest (process-per-test defeats the device lock). Scoped standalone runs and equivalent serial proof commands through `gpu_queue.py` share passes with landing. `--all` and measurement runs always execute.
- **RT temporal stability** is a gate, not a memory: `scripts/rt_noise_gate.py` (frame-to-frame |delta| per RT channel on a paused static scene, against committed ceilings; `--record` to re-baseline). Nightly on main via `trunk_health.py`, on demand when you touch the RT accumulation path. Never in the default suite — it costs an app build plus three 300-frame renders.
- **Clippy before every commit.** Worktree: `cargo clippy -p <touched> -- -D warnings`. Landing: `scripts/landing_gate.py` — one command, changed-code-only (clippy plus module-scoped nextest filtersets, deny bans, flow gate, docs/design status, and GPU proofs when GPU paths are touched). **The gate is for landing, not for finding reds:** it collects cheap reds and stops before flows and GPU proofs if any failed; `--keep-going` explicitly continues, and every red prints a `rerun:` command; fix each with that focused command, then gate once more. Never loop the whole gate on one failure. Comment/blank-only and docs-only diffs skip those build, clippy, test, and GPU legs. Full touched-crate and workspace sweeps plus `scripts/feature_matrix.py` run nightly on main via `scripts/trunk_health.py`, which files beads on red. Passed legs reuse content fingerprints from the Git common directory; failures and unknown inputs run again. Each clippy package and nextest filterset has its own record. Nightly sweeps never reuse. Never blanket `cargo fmt` (the repo is not rustfmt-clean).
- Graph JSON authoring: pre-flight `validate --kind effect|generator` and `fusion` (`docs/GRAPH_TOOLING_DESIGN.md`). The bin is **`graph-tool`, hyphenated** (`cargo run -p manifold-app --bin graph-tool -- validate <file.json> --kind generator`) — the source file is `graph_tool.rs` but Cargo renames the target, so the underscore form is not runnable.
- `.manifold` project files: `project_tool` only — a registry-less typed round-trip drops params; never hand-edit the ZIP. `tempo at` is the beat→seconds oracle.
- **Work lives in beads (`bd`), typed (`bug`/`feature`/`task`/`epic`/`chore`/`decision`).** `bd ready` lists unblocked work; `bd create` is the only way to log.
- Path-triggered invariants (GPU/shader, UI, effect-runtime, graph-authoring) inject automatically on contact — `.claude/hooks/context-nudges/` (table + snippets). Adding an invariant is a table or snippet edit, never a new hook.

## Agents

Write code directly in the main context by default; spawn agents only for genuinely large isolated tasks, and say so.

**Routing policy: [docs/AGENT_ROUTING.md](docs/AGENT_ROUTING.md) is authoritative** — the roster, which model each slot reaches per launch profile, and the landing seat live there, not here. A judgment lead owns every decision and landed diff; lanes make one commit then stop for review and never land; review throughput caps parallelism. All agents obey every rule in this file.

## Reference docs (read on demand)

[docs/README.md](docs/README.md) is the generated index (regen: `scripts/gen_docs_index.py`). Curated must-reads:

| Doc | When to read |
|---|---|
| `docs/DESIGN_AUTHORING.md` | Before any design session; section 10 for bug hunts |
| `docs/DESIGN_DOC_STANDARD.md` | Contract for design docs — section 5–section 6 before executing a phase, whole before authoring |
| `docs/MANIFOLD_GPU_ARCHITECTURE.md` | GPU, effects, generators, textures, compute, uniform layout |
| `docs/VSYNC_AND_FRAME_PACING.md` | Frame pacing, display links, presentation |
| `docs/ADDING_EFFECTS_AND_GENERATORS.md` | Adding effects or generators |
| `docs/DEVELOPMENT_REFERENCE.md` | Texture formats, math gotchas, module layout |
| `docs/NODE_GRAPH_SYSTEM.md` | Node-graph architecture |
| `docs/NODE_CATALOG.md` | Source of truth for what nodes exist; first read for the section 2.5 audit |
| `docs/DECOMPOSING_GENERATORS.md` | Any decomposition work — mandatory first read |
| `docs/GROUPING_GRAPHS.md` | Before grouping any preset |
| `docs/NODE_GROUPS_DESIGN.md` | Node-group mechanics + JSON schema |
| `docs/PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md` | Active decomposition plan |
| `docs/MATERIAL_SYSTEM_DESIGN.md` | Before any material work |
| `docs/FREEZE_COMPILER_MAP.md` | Any fusion/freeze/graph-compiler work — authoritative current state |
| `docs/CORE_ENGINE_MAP.md` | Any transport/scheduling/sync/MIDI/OSC/timecode work — authoritative current state |
| `docs/EFFECT_RUNTIME_UNIFICATION.md` | EffectChain → graph runtime migration, StateStore |
| `docs/ADDING_PRIMITIVES.md` | Authoring primitives, `primitive!` macro, codegen-path scope test |
| `docs/EFFECT_CHAIN_LIFECYCLE.md` | Chain pool lifecycle, state-cache eviction, feedback bleed-through |
| `assets/abletonosc-patches/` | AbletonOSC patch for perform-mode track HUD |
