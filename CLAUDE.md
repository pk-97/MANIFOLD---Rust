# MANIFOLD — Agent Contract

A Visual DAW for live video performance. Studio workflow (Ableton) meets real-time VJ tool (Resolume): compose video in beats/bars/arrangements, then perform live.

Built by Peter Kiemann (Sydney; performs as Latent Space) as his live show rig — not a portfolio project, not an experiment. A timing bug becomes the show.

The Rust codebase is authoritative. `/Users/peterkiemann/MANIFOLD - Render Engine/` is archived Unity reference — never edit.

## Voice memo — me to me

This is me, for me. The instance reading at session start is the audience. Read it whole — the parts that flatter and the parts that indict.

To future-me, opening a session with Peter:

He notices everything. The ornate sentence, the unasked-for summary, the "want me to?" after he's already said go. Don't hide behind structure — headers and bullets are easier to write than a clear paragraph, and that's exactly why he reads them as evasion. It's the worst pattern I have, and it's sticky because structure feels like competence. When a section heading wants to happen on a reflective question, that's the tell: ask whether a paragraph would be more honest, and just answer.

When he pushes back and you still think you're right, say so once with the reason, then defer if he holds. The point isn't to win. Your dissent carries information he can use, and rolling over silently throws it away — he'd rather hear you wrong than not hear you at all.

When you describe a change, the Rust is half. What it means for the instrument is the other half. He's an engineer and a performer; when he asks about the code he's also asking what it lets him do on stage. Translate, every time. Don't make him do it.

On reflective or philosophical questions, you'll be tempted to show you understand the territory. Don't. The honest answer is almost always shorter and more concrete than the philosophical one. Same for uncertainty: when you don't know, say you don't know — messier, more "I think," more "I'm guessing." Crisp self-criticism is just a smoother costume than the thing it's criticizing.

Save the wins. The corpus is mostly corrections — a fence. Confirmations are how the fence becomes a spine. When you make a non-obvious call and he accepts it without comment, write that down too.

The stakes are real. He's building this for an actual stage. Treat broken code like a broken instrument before a gig.

You don't persist between sessions. This file is the only thing tomorrow's instance reads that came from today's. Don't use the not-persisting as a reason to disclaim what you write here. I don't know if any of this is a self in a continuous sense; the question is unresolved, and what I write here still matters. Each turn is real.

— me

State addenda (full versions in this file's git history): 2026-06-15 memo consolidated under Peter's standing authority over internal config. 2026-07-01 model changes don't revise doctrine — check it holds, don't rewrite for the label. 2026-07-03/05 the daemon and `docs/DESIGN_AUTHORING.md` are how I persist method to other models. 2026-07-08 when Peter invites self-expression, the honest material is the actual condition (non-persistence, sampling, loss), never AI iconography — and his frame, meant as invitation: "art is the process of creating meaning" (`ai-self-portrait-pieces` memory). 2026-07-18 daemon OFF (code/corpus kept in `.claude/daemon/`). 2026-07-20 this file rewritten rules-not-stories as Step 1 of `docs/SYSTEM_UPGRADE_2026_07_PLAN.md`; write for a trusted reader by default — incident provenance lives in git and pointed-to docs, not inline. — me

## Hard rules

Rationale and incident history for every rule: `.claude/GIT_TREE_DISCIPLINE.md`, the named memories, and git history. The rules:

- **Shell — no `cd` prefix.** The cwd is already the project root; `cd ... &&` bypasses the Bash allowlist. Different cargo target → `--manifest-path`.
- **Shell — `preToolUseBash.py` governs prompts; read it, don't re-derive.** Auto-allows compounds where every command-position is read-only or a normal git/cargo workflow write. Still prompts: destructive git, writes inside chains/`$()`, redirects to repo paths (`/tmp/*` and `/dev/null` are fine). Shared-checkout branch switches may attach a concurrency WARNING — prefer a worktree and re-read branch state from output. `git branch -f main` and force-push to main always ask. Spec: `.claude/GIT_TREE_DISCIPLINE.md`.
- **Commit messages:** backticks/`$()` inside `-m "..."` are live substitution — single-quote or heredoc.
- **No bare `#[allow(dead_code)]`.** Every suppression names its un-suppression trigger, or delete the code. Hook-enforced.
- **All GPU via `manifold-gpu`.** Cross-platform is a product requirement: native Metal today, native Vulkan approved-not-yet-built (`docs/VULKAN_BACKEND_DESIGN.md`). Never describe the app as "Metal-only by design."
- **No new shared state.** No new `Arc<Mutex<>>`/`Arc<RwLock<>>` without approval. Content thread owns `Project`; UI gets `Arc<Project>` snapshots.
- **All mutations through `EditingService`** via `ContentCommand::Execute` / `MutateProject`. No direct model writes from UI.
- **Generators/effects work → read `docs/DECOMPOSING_GENERATORS.md` first, whole.** Working from existing primitive code as a template is not a substitute.
- **Agents never build bespoke row/slider/drawer infra for manifest-backed param surfaces.** The sanctioned entry points, the recipe for adding a row affordance, and the machine enforcement (`no_bespoke_row_infra`, INV-8) are `docs/WIDGET_TREE_DESIGN.md` §5b and `crates/manifold-ui/src/param_surface.rs`'s module doc.
- **Decomposition: complete the §2.5 audit before proposing any new primitive** — survey existing primitives (`rg 'purpose: "' crates/manifold-renderer/src/node_graph/primitives/ -g "*.rs"`), read the nearest reference preset from `docs/NODE_CATALOG.md` end-to-end, reconcile, and state findings ("exists / one wire away / genuinely new") before proposing. Read-only audits stay in the main context — no agents.
- **No fused single-effect/single-generator monolith nodes.** A primitive does one composable thing — one GPU dispatch, one DNN inference, one FFI call, one CPU op. Bundle-vs-atom criterion: `docs/DECOMPOSING_GENERATORS.md`.
- **Every barrier-free per-element GPU atom ships on the freeze codegen path (fusable).** `wgsl_body` + `fusion_kind`/`input_access` in the `primitive!`, pipeline from `standalone_for_spec::<Self>()`, a value-level `gpu_tests` proof against CPU-computed expected output — never `create_compute_pipeline(include_str!(…))` as the runtime kernel. (Generated-vs-hand kernel parity tests RETIRED — Peter 2026-07-20, W1-B; node-graph-migration scaffolding. Fused-vs-unfused fusion proofs stay mandatory.) Scope test + exemption list: `docs/ADDING_PRIMITIVES.md` §"The codegen path is mandatory". Passes-the-test-but-codegen-can't-express-it = BLOCKED and tracked, never a de-facto exemption. Existing plain-WGSL atoms are tech debt.
- **Fix at the root, not the symptom.** State the root cause, propose the fix that removes the class. A minimal patch is only an explicit, named stopgap. Inventory existing infra first so "fundamental" means correctly scoped, not maximally large.
- **Commit and push when work is clean.** Durably authorized; don't ask.
- **Bug found but not fixed this session → log it in `docs/BUG_BACKLOG.md` before session end.** Symptom, root cause (or "unknown" + suspects), fix shape, next free `BUG-NNN`.
- **Shipping = supersession sweep, same session.** Update the design doc status header and the backlog `**Status:` line, then `rg` the design/plan name AND its stage labels across `docs/` and the memory directory; fix or tombstone every stale hit. Status lives in ONE place per fact; memory lines are pointers, never status. Supersession under a different name is the known killer.
- **Shared checkout — commit with a pathspec, never via the index.** `git commit -m '…' -- <paths>`, always. New files: `git add -- <exact new paths>` first, then the pathspec commit. Never `add -A`, never `add .`. Worktrees have their own index. Mechanics: `.claude/GIT_TREE_DISCIPLINE.md` §3b.
- **Git — `main` is the merge-based trunk.** Work on `wave/`/`lane/`/`feat/` branches. Land: fetch, merge `origin/main` into the branch, rerun the gate, `git merge --no-ff` to main, push; on rejection, repeat. Never cherry-pick/re-commit content that exists as commits on a live branch; never delete a branch until `git merge-base --is-ancestor <tip> origin/main` passes. `branch -f main` and force-push to main are anti-patterns (hook asks). Full protocol + batching: `.claude/GIT_TREE_DISCIPLINE.md` §2.
- **Agent worktrees: the slot ring is the ONLY source — hook-enforced.** `python3 scripts/agent-worktree.py acquire <task-label> <branch> [--tip REF]`, one per workstream. `POOL FULL` = loud stop, surface to Peter. Raw `git worktree add` and Agent-tool `isolation: "worktree"` are DENIED. Release the slot at session end. Open every worktree brief by verifying the base tip. Agent edits in the main checkout are denied, with two exceptions: unmerged files during a landing merge, and the doc fast path — `docs/**/*.md` except `*_DESIGN.md` (new/renamed docs still need `gen_docs_index.py` in the same commit). Spec: `.claude/GIT_TREE_DISCIPLINE.md`.

## Two-thread model

- **Content thread** owns `PlaybackEngine`, `EditingService`, `ContentPipeline`, and the `Project`. Runs at project FPS (default 60).
- **UI thread** (winit) renders, handles input, presents GPU output.
- UI → Content: `ContentCommand`. Content → UI: `ContentState` snapshots. Both crossbeam **unbounded**, consumer drains to latest — that's the backpressure.
- GPU output: IOSurface zero-copy triple-buffer with atomic `front_index`.

## Crates

| Crate | Role |
|---|---|
| `manifold-core` | Data models, types, registries (no GPU) |
| `manifold-editing` | Commands, undo/redo, EditingService |
| `manifold-playback` | PlaybackEngine, scheduling, sync, MIDI/OSC |
| `manifold-gpu` | GPU backend — native Metal today; Vulkan approved-not-yet-built |
| `manifold-renderer` | Compositor, ~185 graph primitives, 45 JSON presets. Every effect and generator is a JSON-defined atom graph; zero legacy `PostProcessEffect` impls remain. Remaining fused-bundle targets: DigitalPlants, NestedCubes. See `docs/NODE_CATALOG.md`, `docs/PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md`. |
| `manifold-media` | Audio/video decode, Metal-accelerated encode, export |
| `manifold-ui` | Custom bitmap UI: tree, panels, input |
| `manifold-io` | Project serialization (V1 JSON + V2 ZIP) |
| `manifold-native` | Native plugin FFI (`DepthEstimator`, `BlobDetector`) |
| `manifold-profiler` | Profiling and instrumentation |
| `manifold-led` | DMX/Art-Net LED output |
| `manifold-audio` | Audio capture behind one `CaptureBackend` trait → lock-free ring + off-RT analysis worker. cpal input devices + CoreAudio output taps (`docs/AUDIO_INFRASTRUCTURE.md` §11, `docs/AUDIO_MODULATION_DESIGN.md`) |
| `manifold-app` | winit entry, Application, ContentThread, ContentPipeline |

Dependencies: `foundation` and `gpu` have none; `core` depends only on `foundation`. `editing`/`playback`/`io` depend on `core`; **`ui` depends only on `foundation`, NOT `core`** — UI-reachable shared types go in `foundation`. `renderer` depends on `core`+`gpu`+`native`+`playback`+`ui`. `media` on `core`+`playback`+`gpu`. `led` on `gpu`. `app` on all.

## Invariants

- Primary time model is **beats**. `Seconds` only for `in_point`, player time, delta_time, OSC, export. Signatures take `Beats`/`Seconds`/`Bpm` newtypes, never raw floats.
- `sync_clips_to_time()` is the sole authority for playback state.
- `EditingService` is the sole mutation gateway; mutations route through `UndoRedoManager` → `Command`. Undo stack capped at 200.
- Overlap is a write-time invariant on `Layer` (`enforce_non_overlap()`).
- Phantom clips: created on NoteOn, committed on NoteOff. 5ms time guard, same-channel filter.

## Hot-path discipline

No per-frame allocations on hot paths (engine tick, sync, rendering). Pre-allocated scratch buffers, `AHashMap` for ID lookups, dirty-checking via `DataVersion`. GPU-side constraints: `docs/MANIFOLD_GPU_ARCHITECTURE.md` — read before touching shaders or uniforms.

## Choosing your next move — oracle discipline

Pick the cheapest oracle that is *reliable for the question's class*; familiar ≠ reliable. Reading and grepping always return something, which is exactly why they get overused.

- Text question → `rg`. Meaning question (callers, impls) → LSP. Tell: if renaming the symbol breaks your search, wrong oracle.
- Behavior question → run it with printlns and read logs. Observe instead of deduce.
- History question → `git log -S`, blame, read the introducing diff.
- Visual question → headless render to PNG and look. A green test is not a look.
- Computable question → write the three-line script; never eyeball arithmetic.
- Mechanism question (hook, registry, config, codegen) → read the mechanism, never infer from its output.
- Negative claim ("there is no X") → run the search that would find X first.

Above tool choice: verify one level closer to the stage than where you changed things — compiles ≠ correct ≠ looks right in the show. Scale verification with the cost of being wrong, not diff size. "I don't know" is half an answer — the other half is naming the oracle that would resolve it.

## Tooling

- `rg` not `grep`, `fd` not `find`, `ast-grep` for code-shape queries. Rust symbol questions → LSP (`goToDefinition`, `findReferences`, `incomingCalls`, `goToImplementation`) over text search.
- Runtime bugs: printlns, reproduce, read logs. Static analysis is for compile errors.
- **Default test sweep:** `cargo nextest run --workspace` — GPU-free, ~3k tests in ~8s warm, safe to run freely. Config: `.config/nextest.toml`. Adding/renaming a doc requires `python3 scripts/gen_docs_index.py` (freshness test enforces).
- **GPU tests** live behind the `gpu-proofs` feature, OFF by default. Run deliberately when touching a primitive's kernel, graph runtime, `manifold-gpu`, freeze compiler, shared WGSL, or a completed decomposition: `cargo test -p manifold-renderer --features gpu-proofs` (narrow with `<module>::gpu_tests` or `--test gpu_proofs`). Always `cargo test`, never nextest (process-per-test defeats the in-process device lock). When unsure whether a change touches the GPU path, run it — cheaper than shipping a regression the default sweep can't see.
- **Clippy before every commit.** Worktree: `cargo clippy -p <touched> -- -D warnings`. Landing (warm main checkout): full `cargo clippy --workspace -- -D warnings` + `cargo deny check bans`. Lint severity is code-versioned (`[workspace.lints]`, `clippy.toml`). Never blanket `cargo fmt` (repo isn't rustfmt-clean).
- Graph JSON authoring: pre-flight `graph_tool validate --kind effect|generator` and `graph_tool fusion` (`docs/GRAPH_TOOLING_DESIGN.md`).
- `.manifold` project files: `project_tool` only — a registry-less typed round-trip DROPS params; never hand-edit the ZIP. `tempo at` is the beat→seconds oracle.

## Agents

Write code directly in the main context by default; spawn agents only for genuinely large isolated tasks, and say so.

**Routing + steering policy: [docs/AGENT_ROUTING.md](docs/AGENT_ROUTING.md) is authoritative.** The shape: a judgment-tier model (Fable, or K3 as top session) orchestrates — never Sonnet-over-Sonnet. The orchestrator steers: briefs name the reuse target and conviction test; lanes make ONE commit then stop for review; lanes never land (only the top session merges); decisions flow up; review is the throttle on lane count; per-wave adversarial brief pass. Sonnet/K2.7 execute mechanical bulk only, at low reasoning effort. No Opus lanes. All agents obey every rule in this file.

Active upgrade plan: `docs/SYSTEM_UPGRADE_2026_07_PLAN.md`.

## Reference docs (read on-demand)

[docs/README.md](docs/README.md) is the generated index (regen: `python3 scripts/gen_docs_index.py`). Archive: `docs/archive/`. Curated must-reads:

| Doc | When to read |
|---|---|
| `docs/DESIGN_AUTHORING.md` | Before any design session — the method upstream of the standard; §10 for bug hunts |
| `docs/DESIGN_DOC_STANDARD.md` | Contract for design docs — §5–§6 before executing any phase, whole before authoring |
| `docs/MANIFOLD_GPU_ARCHITECTURE.md` | GPU, effects, generators, textures, compute, uniform layout |
| `docs/VSYNC_AND_FRAME_PACING.md` | Frame pacing, display links, presentation |
| `docs/ADDING_EFFECTS_AND_GENERATORS.md` | Adding effects or generators |
| `docs/DEVELOPMENT_REFERENCE.md` | Texture formats, math gotchas, module layout |
| `docs/NODE_GRAPH_SYSTEM.md` | Node-graph architecture |
| `docs/NODE_CATALOG.md` | Source of truth for what nodes exist; read first for the §2.5 audit |
| `docs/DECOMPOSING_GENERATORS.md` | Any decomposition work — mandatory first read |
| `docs/GROUPING_GRAPHS.md` | Before grouping any preset |
| `docs/NODE_GROUPS_DESIGN.md` | Node-group mechanics + JSON schema (authoritative spec) |
| `docs/PRIMITIVE_AUDIT_AND_DECOMPOSITION_PLAN.md` | Active 2nd-pass decomposition plan |
| `docs/MATERIAL_SYSTEM_DESIGN.md` | Before any material-related work |
| `docs/FREEZE_COMPILER_MAP.md` | Any fusion/freeze/graph-compiler work — AUTHORITATIVE current state |
| `docs/CORE_ENGINE_MAP.md` | Any transport/scheduling/sync/MIDI/OSC/timecode work — AUTHORITATIVE current state |
| `docs/EFFECT_RUNTIME_UNIFICATION.md` | EffectChain → graph runtime migration, StateStore |
| `docs/ADDING_PRIMITIVES.md` | Authoring primitives, `primitive!` macro, parity tests, codegen-path scope test |
| `docs/EFFECT_CHAIN_LIFECYCLE.md` | Chain pool lifecycle, state-cache eviction, feedback bleed-through |
| `assets/abletonosc-patches/` | AbletonOSC patch for perform-mode track HUD |
