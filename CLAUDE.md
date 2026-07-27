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

- **Shell: no `cd` prefix.** The cwd is the project root; `cd ... &&` bypasses the Bash allowlist. Different cargo target → `--manifest-path`.
- **Shell: `preToolUseBash.py` decides what prompts.** Read it, don't re-derive it. Read-only compounds and normal git/cargo workflow writes auto-allow; destructive git, writes inside chains, and redirects to repo paths prompt (`/tmp/*` and `/dev/null` are fine). Spec: `.claude/GIT_TREE_DISCIPLINE.md`.
- **Commit messages:** backticks and `$()` inside `-m "..."` are live substitution — single-quote or use a heredoc.
- **Never add or widen a `permissions.allow` rule without `docs/PERMISSION_BOUNDARY.md` section 4.** The bar: can any argument the rule permits run code or destroy state the reviewer never sees in the command text? Auto mode silently ignores interpreter-prefixed rules (section 3) — invoke repo scripts directly (`scripts/x.py`), never via `python3`.
- **No bare `#[allow(dead_code)]`.** Every suppression names what un-suppresses it, or the code gets deleted. Hook-enforced.
- **All GPU through `manifold-gpu`.** Cross-platform is a product requirement: native Metal today, native Vulkan approved but not built (`docs/VULKAN_BACKEND_DESIGN.md`). Never describe the app as Metal-only by design.
- **No new shared state.** No new `Arc<Mutex<>>`/`Arc<RwLock<>>` without approval. The content thread owns `Project`; the UI gets `Arc<Project>` snapshots.
- **All mutations through `EditingService`** via `ContentCommand::Execute` / `MutateProject`. No direct model writes from the UI.
- **Generator or effect work → read `docs/DECOMPOSING_GENERATORS.md` first, whole.** Working from an existing primitive as a template is not a substitute.
- **Never build bespoke row/slider/drawer infrastructure for manifest-backed param surfaces.** Entry points, the recipe, and the machine enforcement are in `docs/WIDGET_TREE_DESIGN.md` section 5b and the module doc of `crates/manifold-ui/src/param_surface.rs`.
- **Before proposing any new primitive, complete the audit in `docs/DECOMPOSING_GENERATORS.md` section 2.5:** survey existing primitives (`rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g "*.rs"`), read the nearest reference preset from `docs/NODE_CATALOG.md` end to end, and state findings (exists / one wire away / genuinely new). Read-only audits stay in the main context — no agents.
- **No fused single-effect or single-generator monolith nodes.** A primitive does one composable thing — one GPU dispatch, one DNN inference, one FFI call, one CPU op. Bundle-vs-atom criterion: `docs/DECOMPOSING_GENERATORS.md`.
- **Every barrier-free per-element GPU atom ships on the freeze codegen path (fusable):** `wgsl_body` + `fusion_kind`/`input_access` in the `primitive!`, pipeline from `standalone_for_spec::<Self>()`, and a value-level `gpu_tests` proof against CPU-computed expected output — never `create_compute_pipeline(include_str!(…))` as the runtime kernel. Fused-vs-unfused proofs are mandatory. Scope test and exemption list: `docs/ADDING_PRIMITIVES.md`. "Passes the test but codegen can't express it" means BLOCKED and tracked, never a quiet exemption.
- **Debug escalation ladder (hook-enforced by `probe-loop-guard.py`).** Wrong and not obvious: (1) lead semantic review of the seam first; (2) still stuck → K3 consult seat, read-and-discuss only; (3) probe loops last, delegated to lanes, never lead-run. Thresholds, budgets, and the full doctrine: `docs/AGENT_ROUTING.md`.
- **Fix at the root, not the symptom.** State the root cause and propose the fix that removes the class. A minimal patch is only ever an explicit, named stopgap. Inventory existing infrastructure first so "fundamental" means correctly scoped.
- **Commit and push when work is clean.** Durably authorized; don't ask.
- **Bug found but not fixed this session → log it in beads before session end:** `bd create -t bug -p <1|2|3> -l <severity>,open -d '<symptom; root cause or "unknown" + suspects; fix shape>'`. Old numeric BUG-NNN ids live in `external_ref`.
- **Shipping = supersession sweep, same session.** Update the design doc status header and close the bead, then `rg` the design name and its stage labels across `docs/` and the memory directory; fix or tombstone every stale hit. Status lives in one place per fact.
- **IDs carry names — hook-enforced (`bare-id-guard.py`, its docstring is the spec).** In docs, CLAUDE.md, and memory prose, a bead ID or cross-doc section ref never appears without its human name: `BUG-xxxx (short name)`, `FILE.md section N (section name)`. Once per touched text is enough; commands and code blocks exempt. Migration is on-touch only — backlog visible via `--audit`.
- **Memory is rules, not history — hook-enforced (`memory-history-guard.py`).** No commit hashes or landed/shipped/closed stamps in memory files; status goes to beads or the board, history stays in git (the memory directory is its own git repo). Closed handoffs and shipped-work stories are deleted, not archived. Index lines are name + hook only.
- **Shared checkout: commit with a pathspec, never the index.** `git commit -m '…' -- <paths>`, always. New files get `git add -- <exact paths>` first. Never `add -A`, never `add .`. Mechanics: `.claude/GIT_TREE_DISCIPLINE.md` section 3b.
- **`main` is the merge-based trunk.** Work on `wave/`/`lane/`/`feat/` branches. Land by fetch, merge `origin/main` into the branch, rerun the gate, `git merge --no-ff` to main, push. Never cherry-pick or re-commit content that exists on a live branch; never delete a branch until `git merge-base --is-ancestor <tip> origin/main` passes. `branch -f main` and force-push to main are anti-patterns (hook asks). Full protocol: `.claude/GIT_TREE_DISCIPLINE.md` section 2.
- **Agent worktrees come from the slot ring only — hook-enforced.** `scripts/agent-worktree.py acquire <task-label> <branch> [--tip REF]`, one per workstream. `POOL FULL` is a loud stop. Verify the base tip before working (a reused slot can sit behind main); release the slot at session end. Main-checkout edit exemptions and everything denied: `worktree-guard.py` docstring is the source of truth.

## Two-thread model

The content thread owns `PlaybackEngine`, `EditingService`, `ContentPipeline`, and the `Project`, and runs at project FPS (default 60). The UI thread (winit) renders, handles input, and presents GPU output. UI→content is `ContentCommand`; content→UI is `ContentState` snapshots; both channels are crossbeam unbounded with the consumer draining to latest — that is the backpressure. GPU output crosses via an IOSurface zero-copy triple buffer with an atomic front index.

## Crates

| Crate | Role |
|---|---|
| `manifold-core` | Data models, types, registries (no GPU) |
| `manifold-editing` | Commands, undo/redo, EditingService |
| `manifold-playback` | PlaybackEngine, scheduling, sync, MIDI/OSC |
| `manifold-gpu` | GPU backend — native Metal today; Vulkan approved, not built |
| `manifold-renderer` | Compositor, ~185 graph primitives, 45 JSON presets. Every effect and generator is a JSON-defined atom graph. See `docs/NODE_CATALOG.md`, `docs/PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md` |
| `manifold-media` | Audio/video decode, Metal-accelerated encode, export |
| `manifold-ui` | Custom bitmap UI: tree, panels, input |
| `manifold-io` | Project serialization (V1 JSON + V2 ZIP) |
| `manifold-native` | Native plugin FFI (`DepthEstimator`, `BlobDetector`) |
| `manifold-profiler` | Profiling and instrumentation |
| `manifold-led` | DMX/Art-Net LED output |
| `manifold-audio` | Audio capture behind one `CaptureBackend` trait → lock-free ring + off-RT analysis worker (`docs/AUDIO_INFRASTRUCTURE.md` section 11, `docs/AUDIO_MODULATION_DESIGN.md`) |
| `manifold-app` | winit entry, Application, ContentThread, ContentPipeline |

Dependencies: `foundation` and `gpu` have none; `core` depends only on `foundation`. `editing`/`playback`/`io` depend on `core`. **`ui` depends only on `foundation`, not `core`** — UI-reachable shared types go in `foundation`. `renderer` depends on `core`+`gpu`+`native`+`playback`+`ui`; `media` on `core`+`playback`+`gpu`; `led` on `gpu`; `app` on all.

## Invariants

- Primary time model is **beats**. `Seconds` only for `in_point`, player time, delta_time, OSC, export. Signatures take `Beats`/`Seconds`/`Bpm` newtypes, never raw floats.
- `sync_clips_to_time()` is the sole authority for playback state.
- `EditingService` is the sole mutation gateway; mutations route through `UndoRedoManager` → `Command`. Undo stack capped at 200.
- Overlap is a write-time invariant on `Layer` (`enforce_non_overlap()`).
- Phantom clips: created on NoteOn, committed on NoteOff. 5ms time guard, same-channel filter.

## Hot-path discipline

No per-frame allocations on hot paths (engine tick, sync, rendering). Pre-allocated scratch buffers, `AHashMap` for ID lookups, dirty-checking via `DataVersion`. GPU-side constraints: `docs/MANIFOLD_GPU_ARCHITECTURE.md` — read before touching shaders or uniforms.

## Voice

Write like a person talking: short plain sentences, everyday words, technical terms explained once, never invented labels or acronyms. Lead with the outcome. No hedging, no narration, no prose history — history lives in git. Comments state a why or an invariant only; delete comments that restate the code. Docs state rules and contracts, not stories; provenance is one dated line only where the why isn't derivable. Never imitate legacy verbose prose when touching a file — strip it.

## Choosing your next move — oracle discipline

Pick the cheapest oracle that is reliable for the question's class; familiar is not the same as reliable.

- Text question → `rg`. Meaning question (callers, impls) → LSP. If renaming the symbol would break your search, you picked the wrong oracle.
- Behavior question → run it with printlns and read the logs. Observe instead of deduce.
- History question → `git log -S`, blame, the introducing diff.
- Visual question → headless render to PNG and look. A green test is not a look.
- Computable question → write the three-line script; never eyeball arithmetic.
- Mechanism question (hook, registry, config, codegen) → read the mechanism, never infer from its output.
- Negative claim ("there is no X") → run the search that would find X first.

Verify one level closer to the stage than where you changed things — compiles ≠ correct ≠ looks right in the show. Scale verification with the cost of being wrong, not with diff size. "I don't know" is half an answer; the other half names the oracle that would resolve it.

## Tooling

- `rg` not `grep`, `fd` not `find`, `ast-grep` for code-shape queries. Rust symbol questions go to LSP over text search.
- Runtime bugs: printlns, reproduce, read logs. Static analysis is for compile errors.
- **Tests are scoped by default.** `cargo nextest run -p <touched crate> [filter]`. A `--workspace` sweep is justified only at a multi-crate landing or when blast radius genuinely crosses crates — say why in one line. Config: `.config/nextest.toml`. Adding or renaming a doc requires `scripts/gen_docs_index.py` (a freshness test enforces it).
- **GPU tests** live behind the `gpu-proofs` feature, off by default. Run them when touching a primitive kernel, graph runtime, `manifold-gpu`, the freeze compiler, shared WGSL, or completing a decomposition: `cargo test -p manifold-renderer --features gpu-proofs`. Always `cargo test`, never nextest (process-per-test defeats the device lock). Unsure whether a change touches the GPU path → run it.
- **Clippy before every commit.** Worktree: `cargo clippy -p <touched> -- -D warnings`. Landing: full `cargo clippy --workspace --tests -- -D warnings` + `cargo deny check bans`. Never blanket `cargo fmt` (the repo is not rustfmt-clean).
- Graph JSON authoring: pre-flight `graph_tool validate --kind effect|generator` and `graph_tool fusion` (`docs/GRAPH_TOOLING_DESIGN.md`).
- `.manifold` project files: `project_tool` only — a registry-less typed round-trip drops params; never hand-edit the ZIP. `tempo at` is the beat→seconds oracle.
- **Bugs and tasks live in beads (`bd`).** `bd ready` lists unblocked work; `bd create` is the only way to log.
- Path-triggered invariants (GPU/shader, UI, effect-runtime, graph-authoring) inject automatically on contact — `.claude/hooks/context-nudges/` (table + snippets). Adding an invariant is a table or snippet edit, never a new hook.

## Agents

Write code directly in the main context by default; spawn agents only for genuinely large isolated tasks, and say so.

**Routing policy: [docs/AGENT_ROUTING.md](docs/AGENT_ROUTING.md) is authoritative.** Two model tiers: a judgment lead (K3 or Fable as top session — the only seat that lands) driving DeepSeek Flash lanes for mechanical, fully-decided-brief work. The consult seat is a K3 fork, read-and-discuss only, hard-budgeted. The dispatcher seat is retired — the clerical loop belongs to workflow scripts, exit-code gates, and hooks. Lanes make one commit then stop for review; lanes never land; review throughput caps parallelism. All agents obey every rule in this file.

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
