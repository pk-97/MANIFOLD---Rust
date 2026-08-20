# Warmup — nothing initializes for the first time on stage

**Status:** IN PROGRESS — P1–P5 + budget fix on main; P6 (skip-mode removal, BUG-p8oe (warmup-p5-residue-cold-touches)) executing on lane/warmup-p6-skip-mode-removal · k3 (lead)
**Prerequisites:** none
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (phase briefs)–section 6 (seam briefs) before starting any phase.

The app's contract today is "everything is lazy," which means the audience pays for
initialization: the first play of a heavy generator builds its whole runtime on the
launch frame, and the first play of a 3D scene renders empty frames while its GLB
parses on a background thread. The new contract: **load time is free, play time is
sacred — nothing happens for the first time during a show.** The mechanism is a
pre-roll: at project load, every layer's generator is built and rendered offscreen
until all its async work (GLB parses, RT accel builds, model loads) has quiesced.
The project does not finish opening until that passes, with a progress bar while it
runs. Peter, 2026-08-20: "if the project only opens once we have that green light" —
blocking open replaces any separate readiness UI. And: "an honest thirty-second load
with a guarantee beats a three-second load with a hitch hiding in bar sixty-four."

Companion: `FREEZE_COMPILER_MAP.md` (fusion prewarm, untouched by this design),
`EFFECT_CHAIN_LIFECYCLE.md` (chain pool — orthogonal), BUG-93o6 (first-play-warmup-3d-scenes-heavy-generators — the originating bead, closes with P1).

## 1. Audit — what exists (verified 2026-08-20)

### Already warm — do not rebuild

| Piece | Where | Notes |
|---|---|---|
| Atom codegen pipeline sweep | `crates/manifold-renderer/src/generators/registry.rs:135` (`prewarm_all_atom_codegen_pipelines`) | Runs at boot from `GeneratorRenderer::new` (`generator_renderer.rs:207`) |
| Hand-written pipeline prewarms | `registry.rs:84-99` (RenderScene, GltfTextureSource, ScatterOnMesh, SeedParticlesFromTexture) | P4 deletes the ones pre-roll subsumes |
| Plugin effect prewarm | `crates/manifold-renderer/src/plugin_prewarm.rs:52`, called at `layer_compositor.rs:600` | Post-process FFI effects |
| Pipeline binary archive | `crates/manifold-app/src/app.rs:2382` load / `app.rs:2431` save, `~/Library/Caches/com.latentspace.manifold/pipeline_cache.metallib` | Cross-launch compile cache |
| Fusion segment prewarm | `prewarm_project_chain_segments`, called at `content_commands.rs:444` | Background worker, enqueue-only |
| Video decoder lookahead | `engine.rs:958` calls `compute_prewarm_candidates` (def `engine.rs:2517`) → `content_thread.rs:754` → `VideoRenderer::pre_warm_from_candidates` | **The house precedent**: engine computes what's about to play; decoders open ahead of activation |
| Audio/MIDI/OSC/Link init | `app.rs:2484-2529` | All eager at content-thread boot |

### Cold at first play — the warmup targets

| Piece | Where | First-play cost |
|---|---|---|
| Per-layer generator construction | `generator_renderer.rs:469` (`acquire_clip` → `install_layer_generator` → `PresetRuntime::from_json_str_with_device` + `pre_allocate_resources`) | Full runtime build + every Array buffer at declared capacity, on the launch frame, content thread |
| GLB mesh parse | `primitives/gltf_mesh_source.rs:368` — background thread spawned on first evaluate, scene renders **empty** until `pending_load` drains (`gltf_mesh_source.rs:379`) | Wrong frames, then RT accel build — the BUG-326 (rt-depth-snapshot-wrong-on-imported-glb-scenes) class |
| GLB texture decode | `primitives/gltf_texture_source.rs:205` — same thread+channel shape | Texture pops in late |
| RT accel + lazy RT surfaces | `render_scene.rs` — `rt_accel` async build (`:972`), D1-rule lazy textures (depth/velocity/AO/denoise/accumulators) | First RT-wired frame allocates a dozen full-res surfaces |
| HDRI decode | `primitives/hdri_source.rs` — decode already off-thread (spawn `:239`, `pending_load` `:158`, readiness query `io_pending()` `:207`); pipeline prewarmed via the registry hand list | Texture pops in late; launch-frame cost is upload/mips, not decode |
| Image clip decode | `crates/manifold-media/src/image_renderer.rs:37` — "decoded from disk exactly once" per clip activation | Disk decode on launch frame |
| AI model loads (MiDaS depth, person segment, optical flow) | `primitives/depth_estimate_midas.rs:121-128` (`ensure_depth_worker`), `person_segment.rs:145`, `optical_flow_estimate.rs:146` — ONNX load inside worker spawned at first evaluate | Effect outputs black until model resident (seconds) |
| LED tap runtime | `layer_compositor.rs:2322` (`led_tap`), `:2421` (`led_master_ec`) | PresetRuntime built on first LED-enabled frame |
| Per-primitive samplers/small buffers | ~15 atoms (`draw_gauge.rs:170`, `bokeh_gather.rs:130`, …) | Sub-ms each; pre-roll warms them free |

### Confirmed absent (searches run 2026-08-20)

- No GLB/scene asset cache anywhere (`rg -ln "GltfCache|scene_cache|asset_cache"` — zero hits).
- No project-load progress UI: `LoadProject` is fire-and-forget (`app_lifecycle.rs:886`) with snapshots suppressed until processed (`app.rs:99`); the project simply appears.
- No audio classifier model in the Rust app (BUG-dzr (classifier-rust-port) is open) — nothing to warm there yet.
- No lazy statics in manifold-media/audio/playback/led (`rg "OnceCell|LazyLock"` — zero non-test hits).
- `node.value_overlay`'s font atlas + pipeline (`render_value_overlay.rs:478`) is the one high-cost lazy site a full-crate sweep found beyond the table above — but no shipped preset wires it (`rg -l "node.value_overlay" assets/` — zero hits), and any user graph that does wire it lives inside a layer's generator, which pre-roll renders. Covered by construction; no special case.

## 2. Decisions

**D1 — Warmup runs inside the `LoadProject` content-command handler, blocking, on the
content thread.** The content thread renders nothing for the new project until the
command returns, so blocking it is nearly free; the UI thread stays live and shows
progress. The structural precedent is `run_export` (`content_thread.rs:393`,
`content_export.rs:25-28`): a multi-minute blocking content-thread operation that
takes `cmd_rx` + `state_tx` and pumps both from inside itself. Warmup takes the same
shape — per-layer progress publishes and a command-drain policy (section 3.2), not a
bare blocking loop. Two pumps are mandatory, discovered at review: a `Shutdown` poll
(quit during a long warm must not hang on budgets) and `ableton_bridge.update()`
(its connection timeout is 1.5s — `ableton_bridge.rs:26` — and `last_response` only
advances on content frames, so an unpumped warm forces a disconnect + rediscovery on
Ableton-synced sets; loading between songs while Ableton drives transport is a real
gesture). Rejected: background warmup workers handing warmed instances to the content
thread — new shared state and a handoff problem to save time on a thread that is
idle anyway. Pure-CPU decodes parallelize inside existing per-primitive worker
patterns, not a new warmup thread pool.

**D2 — The mechanism is pre-roll, not enumeration.** Warm = build each layer's
generator and render it offscreen at real canvas size until quiescent. This exercises
the exact code the stage exercises, so lazy paths added later are warmed by
construction. Rejected: per-feature warm calls as the primary mechanism (the
BUG-037 (gltf-first-render-stall) pattern and its successors) — whack-a-mole that
rots; each new primitive is a new cold spot nobody remembers to list. Enumeration
survives only as D4's quiescence query.

**D3 — Generator pre-build reuses the production path.** Warmup populates
`GeneratorRenderer::layer_generators` via the existing `install_layer_generator`
(`generator_renderer.rs:1021`) with the layer's real override/manifest/relight inputs.
`acquire_clip`'s `needs_create` check (`generator_renderer.rs:441`) then hits on first
launch — no parallel construction path, no warmup-only instance shape. Precedent for
reaching the renderer from the content thread: the downcast at `content_thread.rs:756`.

**D4 — Quiescence is a query, not a timer.** New method on the `Primitive` trait:
`fn warmup_pending(&self) -> bool { false }` — it must live on `Primitive`, not only
on `EffectNode`: the blanket `impl<P: Primitive> EffectNode for P`
(`primitive.rs:615`) forwards every method explicitly, so a method added to
`EffectNode` alone could never be overridden per-primitive. The blanket impl gains
one forward line; hand-written `EffectNode` impls (`boundary_nodes.rs:145`) keep the
default. Exactly five overrides — `gltf_mesh_source` (`pending_load` non-empty),
`gltf_texture_source` (same idiom, `io_pending()` at `gltf_texture_source.rs:169`),
`hdri_source` (same idiom, `io_pending()` at `hdri_source.rs:207` — the `io_pending`
naming is the foundation the override builds on), `render_scene` (rt_accel not ready
when RT wired), and one shared impl shape for the DNN atoms (`depth_estimate_midas`,
`person_segment`, `optical_flow_estimate`). The DNN atoms have no
first-response-delivered field today — that flag is new code, named here so no lane
discovers it mid-phase. `PresetRuntime` aggregates over its `effect_nodes`
(`preset_runtime/core.rs:96` — enumerable). Pre-roll per layer: render, query, repeat
until quiet or budget. Rejected: fixed frame counts or sleeps — a guess that is wrong
per project.

**D5 — Progress rides `ContentState`; the project opens warm.** New
`warmup: Option<WarmupProgress { done: u32, total: u32, label: String }>` snapshot
field, precedent `export_progress` (`content_state.rs:177`, added for BUG-083 (export-progress-restore)). Publishing must come from inside the warmup pass —
snapshots otherwise publish only between commands (`content_thread.rs:432/582`), so
a naive blocking handler shows a bar stuck at zero until it jumps to done; the
pass receives `state_tx` and publishes per layer, exactly as `run_export` does
(D1). The UI already suppresses snapshots until `LoadProject`
completes; it now additionally shows a load progress bar driven by this field, and
the open completes when warmup drains. Rejected: a separate post-open readiness
panel — superseded by blocking open (Peter's call, quoted in the intro).

**D6 — Budgets bound everything; exhaustion degrades honestly.** Per-layer render
budget and a total load budget (defaults set in P1 from the Liveschool fixture:
53 layers / 2928 clips — never assume small). A layer that exhausts its budget logs a
loud warning naming the layer and opens anyway — that layer may first-touch once,
exactly like today. Warmup must never make a project unopenable.

**D7 — Clips added during editing warm at add-time, transport-gated.** Adding a
generator to a layer while the transport is stopped warms that layer immediately
(same path as load). While the transport is playing, warmup is skipped — blocking the
content thread mid-show is the thing this design exists to prevent, and live edits
already render in preview. The guarantee is scoped: *a project that was opened (or
saved) warm plays warm.*

**D8 — Video decoders stay on lookahead.** Holding every decoder in a project open at
load is the wrong memory trade; the existing candidate prewarm (`engine.rs:958`)
already prevents black frames at clip start. Warmup covers what can be resident;
lookahead covers what can't. No change to the video path this design.

**D9 — A cold-touch detector is the regression net.** Pipeline compiles, GLB parses,
model loads, and effect-chain constructions already funnel through countable sites.
P1 adds a counter at those sites (chain construction included from day one — see the
scoped guarantee in section 3.2), sampled while the transport is playing: any cold
touch during playback logs a loud warning, and P4 wires the headless gate that
asserts zero. This is what keeps the policy true
as new primitives land — the invariant is machine-checked, not remembered.

**D10 — Disk caches are their own phase.** HDRI decodes and GLB preprocesses cache to
`~/Library/Caches/com.latentspace.manifold/` (the metallib cache's sibling, keyed by
content hash) in P3. Not in P1: correctness of the pre-roll must not depend on a cache.

## 3. Design body

### 3.1 Data model

```rust
// manifold-core — both types cross crate lines: ContentState is manifold-app,
// ClipRenderer is manifold-playback, and playback depends only on core.
pub struct WarmupProgress { pub done: u32, pub total: u32, pub label: String }
pub struct WarmupBudget { pub per_layer_frames: u32, pub total: std::time::Duration }
pub enum WarmupOutcome { Quiescent, BudgetExhausted }
```

`ContentState` gains `pub warmup: Option<WarmupProgress>` — `None` when no load is
warming. One item = one layer's pre-roll (assets and workers quiesce inside it), so
`total` = generator layers in the project. No new channels, no new shared state:
progress flows on the existing snapshot, published by the pass itself (D5).

### 3.2 The warmup pass (content thread, inside `LoadProject`)

Shaped like `run_export` (D1): the pass receives `state_tx` and `cmd_rx` and pumps
both from inside. After the existing steps (`content_commands.rs:412-459`) and after
the fusion prewarm enqueue, before the command returns:

1. Walk `project.timeline.layers`. For each layer with a generator: publish progress
   via `state_tx`, then call the renderer seam (3.3).
2. The seam builds the instance via `install_layer_generator` (D3) and pre-rolls it:
   render to a scratch `RenderTarget` at real canvas size, query
   `warmup_pending()`, repeat until quiet or the per-layer budget trips.
3. Per iteration of the pass: pump `ableton_bridge.update()` (D1) and poll `cmd_rx`
   for `Shutdown` — app quit during a long warm must not hang until budgets drain;
   abort the pass and shut down. Other commands stay queued; they address the new
   project and run right after load completes. (Verified 2026-08-20: the content
   thread is a single-threaded loop, `content_thread.rs:371-417` — nothing else pumps
   it, so a blocking handler is safe for everything except a queued shutdown and the
   Ableton heartbeat.)
4. Timeouts log and continue (D6). Progress clears to `None` at the end.
5. **After the pass, re-call the trigger-state reset.** `clear_generator_trigger_state`
   runs at the TOP of `LoadProject` (`content_commands.rs:428`, body `:67-74`), so
   pre-roll frames re-pollute `sample_and_hold`/`clip_trigger_cycle` latches AFTER
   that reset — the reset must run again post-warmup or the first live frame inherits
   warmup latches. One call; the mechanism exists.

**Scope of the P1 guarantee, stated honestly:** the walk covers layer generator
graphs only. Per-layer post-fx chains (`effect_chains`, `layer_compositor.rs:363`)
build on the layer's first ACTIVE frame (`ensure_chain_for_layer`, `:701`, only
pre-inserts `None`) — same cost class as generator construction, and the audit's DNN
atoms live in chains at least as often as in generator graphs. P2 extends the walk to
chains (section 5); until then a chain-hosted first-touch is still possible, and
D9's counter counts chain construction from P1 so the hole is visible, never silent.

Render precedents: `every_bundled_preset_executes_one_frame`
(`bundled_generator_presets.rs:223`) — offscreen render at small size with a
`PresetContext`; the production warm uses real canvas dims because
canvas-sized arrays allocate at construction (`registry.rs:154` doc — the
top-left-quadrant bug history). Scratch target is created once per warmup pass and
reused across layers.

### 3.3 Renderer seam

`ClipRenderer` gains a default no-op method:

```rust
fn prewarm_layer(&mut self, layer: &Layer, budget: WarmupBudget) -> WarmupOutcome;
```

No canvas-dim params: `GeneratorRenderer` already holds `self.width/self.height`
(`generator_renderer.rs`), and P2's `ImageRenderer` override holds its own.
`GeneratorRenderer` implements the build + pre-roll; every
`install_layer_generator` input is derivable from `&Layer` — production does exactly
that at `generator_renderer.rs:1265-1289`. Other renderers stay no-op (video: D8;
image: P2). Downcast plumbing is NOT extended — a trait method is the cleaner seam
and the video prewarm's downcast (`content_thread.rs:756`) is the acknowledged older
pattern, not the model to copy.

Why not the existing `on_project_loaded` hook (`renderer.rs:31`, called from
`engine.rs:526` inside `engine.initialize`)? It's project-level with no per-layer
granularity — progress (D5) and per-layer budgets (D6) both need the layer walk, and
`engine.initialize` runs before the pipeline resize that warmup must follow.

### 3.4 Quiescence overrides (the enumeration that survives)

Exactly four node impls override `warmup_pending()`: `gltf_mesh_source`,
`gltf_texture_source`, `render_scene`, and one shared impl shape for the DNN atoms
(`depth_estimate_midas`, `person_segment`, `optical_flow_estimate`). Everything else
is warmed incidentally by rendering. A new async primitive that forgets the override
is caught by D9's detector on first stage use, not by memory.

### 3.5 UI surface

During load the main window shows a centered progress bar + the current layer label,
driven by `ContentState.warmup`. The window is otherwise the normal load state
(snapshots already suppressed). No new panels, no settings, no opt-out.

### 3.6 Consequences, stated honestly

- **Load time goes up on first load of a project** — every generator built, every
  scene decoded, one+ frames rendered per layer. P3's disk caches amortize repeat
  loads; first load of a heavy show pays full price. Accepted (Peter's quote, intro).
- **Memory: the project's full generator working set becomes resident at open.**
  Liveschool-scale: 53 layers of particle/fluid buffers at canvas size. This is the
  show's real working set — the alternative is the audience paying it piecemeal — but
  it is a genuine RSS increase and low-memory machines will feel it. The BUG-olp9 (process-memory-leak-watchdog-panic) investigation measured 0.84GB peak for warming
  *all 45 bundled presets*; a project holds only its own layers, and P4's
  consolidation keeps it there.
- **Pre-roll runs generator code with `time=0`-ish contexts.** A generator with
  first-frame side effects (trigger counters, `sample_and_hold` latches) sees warmup
  frames as real frames — they are, and they run AFTER the load-time trigger-state
  reset (`content_commands.rs:428`), which is why section 3.2 step 5 re-clears that
  state post-warmup. Without the re-clear, the first live frame inherits warmup
  latches.
- **Ableton-connected sets pay a pump, not a disconnect.** Left unpumped, a >1.5s
  warm forces bridge disconnect + rediscovery + mapping revalidation (D1) — it
  self-heals, but mid-set that's a visible dropout. The pump is near-free; the cost
  is the warmup loop carrying one more call.
- **A pathological layer can still burn its full budget.** Bounded per layer, logged,
  show opens anyway (D6) — but total load time is worst-case budgets summed.

## 4. Invariants & enforcement

- **INV1 — Zero cold touches during playback.** Pipeline compile, GLB parse, HDRI
  decode, or model load while the transport plays = policy breach. *Enforcement:*
  D9's counter + headless test `warmup_gate_zero_cold_touches_during_playback`
  (load fixture project, warm, play 60 frames across scenes, assert zero) + loud log
  in app builds. Lands in P1 as the counter, P4 as the CI gate.
- **INV2 — Warmup never blocks open past budget.** *Enforcement:* per-layer and total
  budget constants; unit test with an artificially never-quiescent stub node asserts
  the pass terminates and logs.
- **INV3 — First launch does no construction.** After warmup, `acquire_clip` must not
  rebuild. *Enforcement:* test asserts `layer_generators` hit for every generator
  layer post-warmup (the `needs_create` inputs compared against the layer's current
  manifest/override versions).

## 5. Phasing

### P1 — Core pre-roll (one session)

- **Entry state:** this doc approved; anchors re-verified (`rg -n "fn install_layer_generator" crates/manifold-renderer/src/generator_renderer.rs` → 1 hit; `rg -n "prewarm_project_chain_segments" crates/manifold-app/src/content_commands.rs` → 1 hit).
- **Read-back:** D1–D6, section 3 whole, the `acquire_clip`/`needs_create` path
  (`generator_renderer.rs:400-520`), the test render precedent
  (`bundled_generator_presets.rs:223-280`), the LoadProject handler
  (`content_commands.rs:412-470`).
- **Deliverables:** `warmup_pending()` on the `Primitive` trait (default false) +
  the blanket-impl forward line (`primitive.rs:615`) + the five overrides (D4),
  including the new first-response-delivered flag on the DNN atoms;
  `PresetRuntime` aggregation; `ClipRenderer::prewarm_layer` + `GeneratorRenderer`
  impl; the `run_export`-shaped warmup pass in the `LoadProject` handler with
  budgets, per-layer progress publish, Ableton pump, and Shutdown poll; the
  post-warmup trigger-state re-clear (3.2 step 5); `WarmupProgress`/`WarmupBudget`/
  `WarmupOutcome` in manifold-core + `ContentState.warmup`; UI progress bar;
  cold-touch counter (log-only; sites: pipeline compile, GLB parse, HDRI decode,
  model load, **chain construction**); `preview_layer` cleared in
  `GeneratorRenderer::release_all` (one line — a stale preview id across projects
  would warm a layer watched/unfused and rebuild on first launch, an INV3 breach;
  removes the class independent of warmup); BUG-93o6 (first-play-warmup) closed at
  landing.
- **Gate:** positive — `warmup_gate_zero_cold_touches_during_playback` green on a
  project with a 3D scene and a heavy generator (build fixture from
  `tests/fixtures/rt/apricot_tl05.glb` + an existing heavy preset); INV2/INV3 unit
  tests green; `MANIFOLD_RENDER_TRACE=1` run of first-launch-after-warm shows no
  >20ms frame attributable to construction. Negative — `rg "fn prewarm_layer"` hits
  exactly the trait + one impl; no new `Arc<Mutex>` (`rg` zero hits in the diff).
- **Acceptance demo:** L3 — scripted flow (or headless harness run) that loads the
  fixture project, captures the progress bar mid-load, then triggers the 3D scene
  clip first-play and asserts the rendered frame is non-black within 2 frames of
  launch (today: empty until the GLB thread lands). Demo command + thresholds stated
  in the phase notes.
- **Performer gesture:** double-tap a cold scene launch the moment the project opens
  — the thing that hitches today.
- **Round-trip:** load → warm → play → save → reload → warm → play; second load's
  warmup must also pass INV1 (no state leaks between loads).
- **Forbidden moves:** a warmup-only construction path parallel to
  `install_layer_generator`; fixed sleeps/frame counts instead of the quiescence
  query; swallowing per-layer timeout logs; warming on a worker thread "for speed";
  touching the video decoder path.
- **Test scope:** `cargo nextest run -p manifold-renderer warmup` + `-p manifold-app`;
  GPU proofs gate (`scripts/gpu_proofs_gate.py`) since primitive code is touched.

### P2 — Chains + stragglers: post-fx, image decodes, LED tap, edit-time adds (one session)

- **Deliverables:** the warmup walk extended to per-layer post-fx chains (and group/
  clip/master chains where they exist — `layer_compositor.rs:363/432`,
  `ensure_chain_for_layer` `:701`), closing the scope hole section 3.2 names —
  chain-hosted DNN atoms get quiescence through the same override; `ImageRenderer::prewarm_layer` override warming disk decodes for
  the project's image clips (bounded LRU — full decode set may exceed memory; budget
  by total decoded bytes, warm most-likely-first by timeline order); LED tap runtime
  built at load when the project has LED layers; **D7 edit-time add-warmup** —
  transport-stopped generator assignment warms the layer through the same seam (one
  call site).
- **Gate:** INV1 test extended to a post-fx-chain project and an image-clip project
  (zero cold touches INCLUDING chain construction — the P1 counter site makes this
  assertable); LED-enabled fixture project
  warms the tap (assert `led_tap.is_some()` post-load); edit-time add with transport
  stopped leaves the layer warm (INV3-style assertion), with transport playing it
  does not block frames.
- **Demo:** none — L1 (no new visible surface).
- **Forbidden moves:** unbounded image cache; warming images for clips the project
  doesn't contain; a second warmup entry point for edit-time adds instead of reusing
  the P1 seam.

### P3 — Disk caches (one session)

- **Deliverables:** content-hash-keyed decode cache for HDRI and GLB preprocess
  output under `~/Library/Caches/com.latentspace.manifold/`; load path consults it;
  save-on-decode. Cache eviction: size cap with LRU, stated constant.
- **Gate:** second load of the 3D fixture project skips decode (counter assertion);
  corrupted cache entry fails loudly and re-decodes (round-trip rule — never open a
  project on silently-dropped cache data).
- **Demo:** none — L1, plus a measured second-load time reported in the phase notes.
- **Forbidden moves:** caches keyed by path alone (content must be hashed — same path,
  new file is a real workflow); cache correctness load-bearing for rendering (cache
  miss must equal cold behavior).

### P4 — Consolidation + regression gate (one session)

- **Deliverables:** audit the four hand-written prewarm calls in `registry.rs:84-99` against the atom codegen sweep; keep the ones the sweep cannot reach (`RenderScene`, `GltfTextureSource`, `ScatterOnMesh`, `SeedParticlesFromTexture` — all hand-written/exempt pipelines) and rewrite their comments to state why they survive; cold-touch counter wired to a CI/nextest gate via a CPU-runnable structural test in `manifold-foundation` plus the existing GPU `warmup_gate_zero_cold_touches_during_playback`; doc sweep (this doc's status, no stale `WARMUP_DESIGN` pointers found).
- **Gate:** `rg "prewarm_pipelines|prewarm_pipeline" crates/manifold-renderer/src/generators/registry.rs` returns the four hand-written calls plus the atom sweep documentation references; all four survive because none are on the codegen path. INV1 gate runs as the GPU test in `gpu-proofs` plus the new default-suite structural test in `manifold-foundation/src/cold_touch.rs`.
- **Demo:** none — L1.
- **Forbidden moves:** deleting a hand prewarm whose pipeline no pre-roll reaches
  (watched/graph-editor layer builds render unfused — check before deleting);
  removing the startup atom sweep.

### P5 — Clip-active pre-roll + fusion quiescence (one session; designed from the first real-project probe, 2026-08-20)

The Corrosion Music Video probe (rt-capture + cold-touch counter, `MANIFOLD_LOG_REBUILD_REASON=1`) proved P1–P4 insufficient on a real 3D/RT show: 65 cold touches during playback (39 pipeline compiles, 26 chain constructions), all on the first playback frames. Two root causes, both visible in the log:

- **RC1 — chains are topology-keyed on clip-active state, and warmup never activates a clip.** Playback rebuilds carry different effect sets than the load-time warm saw: an active clip's post-fx joins the layer's chain, so the chain the P2a warm built (bare layer, scratch input) is not the chain the first live frame needs. `dispatch_chain`'s `needs_rebuild` (`chain_dispatch.rs:197`) fires on `!is_compatible(effects, …)`.
- **RC2 — fused-kernel swap-in lands on stage.** The fusion segment prewarm is enqueue-only on a background worker; chains built during warmup build unfused, and when a segment finishes mid-playback the chain rebuilds to swap it in (`awaiting_fused_swap()` in the same `needs_rebuild`) — construction + compiles during playback even when every layer warmed "successfully." The warmup pass never waits for the fusion queue to drain.

Also in scope: one Corrosion layer (`cc0__oomurasaki_azalea…` a3a572d0) fails `install_layer_generator` during warmup and the failure surfaces as a misleading `BudgetExhausted{PerLayerFrames, 0.0ns}` log — diagnose the install failure (it presumably succeeds at first play, so something about the warmup context differs) and give install failure its own `WarmupOutcome` variant so it can never masquerade as a budget trip.

- **Deliverables:** D11 — pre-roll activates each layer's first clip through the production `acquire_clip`/`start_clip` path before rendering (clip post-fx enters the warmed topology), then `stop_clip` after; the existing post-pass trigger-state re-clear covers the latch pollution. D12 — the warmup pass waits for the fusion segment queue to drain (find or add a pending-count query on the prewarm worker; escalate rather than add shared state) BEFORE the per-layer loop pre-rolls chains, so swap-in rebuilds happen inside the warm. The `WarmupOutcome::InstallFailed` variant + the azalea install-failure diagnosis.
- **Gate:** the Corrosion probe is the acceptance — `RUST_LOG=info ./target/debug/manifold rt-capture '<Corrosion path>' --frames 360` reports `cold touches during playback: 0` (a named, explained residue is an escalation, not a pass); existing warmup tests stay green; landing_gate green.
- **Acceptance demo:** the probe log — L2.
- **Forbidden moves:** warming a parallel "clip-like" context instead of the production clip-activation path; making the fusion wait a fixed sleep instead of a queue query; deleting the synthetic-context warm (a layer with zero clips still warms bare).

### P6 — Skip-mode removal: amount is a value, the toggle is the structure (designed 2026-08-21 with Peter)

The P5 residue (BUG-p8oe (warmup-p5-residue-cold-touches): 70 cold touches = 30 chain constructions + their 40 pipeline compiles) is one root cause counted twice: chain segmentation reads live modulated param values through `is_skipped_for` (`SkipMode::OnZero` — skip when `amount <= 0.0`, exactly zero, no epsilon). Load-warmed topology diverges from playback topology the moment modulation crosses zero; warmup can never predict live audio/LFO values. Decision (Peter, 2026-08-21): **the `amount` slider is a performance control, never structure — an effect at amount 0 runs as a normal effect at amount 0; the on/off toggle (`PresetInstance.enabled`) is the only structural skip.** `SkipMode` is deleted end to end; the param-triggered rebuild path dies with it.

- **Deliverables:**
  - **D13 — delete the concept (Rust).** `SkipMode` enum + `is_skipped_for` (`chain_spec.rs`); the topology-hash line (`build.rs:97`); `SegmentMember::Transparent` classification (`segments.rs:58` — remove the variant if skip was its only producer); the worker-drop `continue` (`core.rs:870`); the generator-renderer parity copy (`generator_renderer.rs:725`); `skip_mode` field + `skip_mode_from_def` + the `Box::leak` (`loaded_preset_view.rs:211`); the `gltf_import/scene.rs:888` default; `SkipModeDef` from the preset-def schema. Parser must silently ignore the legacy `skipMode` key in old files (verify no `deny_unknown_fields`; add a regression test parsing a fixture that carries it). `topology_hash.rs` tests updated; new regression test: an `amount` sweep across zero must NOT change the topology hash.
  - **D14 — strip `"skipMode"` from all preset JSON** (57 files across `effect-presets/`, `reference-presets/`, `generator-presets/`). JSON stays valid; graph-tool validate passes on every touched file.
  - **D15 — amount-0 passthrough audit (hard gate).** gpu-proofs test: each formerly-skippable preset rendered at `amount = 0` asserts output == input (identity). A preset whose kernel is not identity at 0 is a kernel bug — fix the kernel, never keep a skip. Serial run behind the device lock (`scripts/gpu_proofs_gate.py`).
  - **D16 — warmup covers disabled chains.** The warm enumerates the project file's toggled-off effects too, so the mid-show toggle-on rebuild (the one remaining structural hitch) is also pre-built.
- **Gate:** Corrosion probe reports `cold touches during playback: 0`; gpu_proofs_gate green (incl. the new audit test); landing_gate green.
- **Acceptance demo:** the probe log + audit test report — L2.
- **Forbidden moves:** any new param-value-driven topology (the class dies here, not just this instance); failing old project/preset files on the removed key; a per-effect exemption list for D15; keeping a runtime alias-skip as part of this phase (steady-state cost of amount-0 dispatches is accepted; if profiling later shows it hurts, a dispatch-level — never topology-level — passthrough is a separate design).

Phasing-completeness: every intro/audit claim maps to a phase — generator pre-build +
pre-roll + progress + UI bar + detector (P1), post-fx chains + image/LED + edit-time
adds (P2), disk caches (P3), consolidation + CI gate (P4), clip-active pre-roll +
fusion quiescence (P5), skip-mode removal (P6). GPU clock ramp: no deliverable —
pre-roll provides it incidentally. Video: D8, no phase.

## 6. Decided — do not reopen

1. Blocking warmup inside `LoadProject` on the content thread (D1).
2. Pre-roll is the mechanism; enumeration survives only as the quiescence query (D2/D4).
3. Pre-build reuses `install_layer_generator`; no warmup-only instance shape (D3).
4. Project opens warm; progress bar is the green light (D5, Peter).
5. Budgets bound everything; timeout = log + open anyway (D6).
6. Edit-time warmup is transport-gated (D7).
7. Video stays on lookahead (D8).
8. `amount` never determines chain structure — the on/off toggle is the only structural skip; `SkipMode` is deleted (P6, Peter 2026-08-21).
8. Cold-touch detector is the policy's enforcement (D9).

## 7. Deferred

- **Group/master chain warmup** — scoped out at P2a execution (2026-08-20): both use
  the same lazy `dispatch_chain` pattern as per-layer chains, but warming them
  correctly needs active-clip/child context that doesn't exist at load, and no
  warmup-only construction path was acceptable. They first-touch once per project on
  stage (chain construction is a D9 counted site, so it's visible). Revive if the
  cold-touch log shows them firing in real shows.

- **Readiness as a separate post-open surface** — superseded by blocking open.
  Revive if load times prove unacceptable in rehearsal and a "rehearsal fast-open"
  mode gets designed; that mode is a product decision, Peter's call.
- **Next-project preloading (double-buffered projects)** — load project B while A
  plays. Real gig workflow, big memory/threading design of its own. Revive when P1–P3
  numbers exist; the warmup pass is its natural foundation.
- **Warming on project save** (save = warm checkpoint) — attractive (the file you
  rehearsed is provably the file you perform) but save-time hitches are their own
  complaint class. Revive if edit-time warming (D7) proves insufficient.
- **OS-level knobs** (App Nap, GPU clock pinning beyond what pre-roll provides) —
  no evidence they bind yet. Revive if the cold-touch gate is green and first frames
  still dip.
