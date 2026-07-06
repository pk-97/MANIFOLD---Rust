# Bug Backlog

<!-- index: Live, human-and-agent-facing tracker for known bugs not yet fixed. Each entry has a stable ID, a root-cause location, the user-visible symptom, a fix shape, and (when one exists) an #[ignore]'d test that goes green when fixed. -->

The repo had no bug tracker — bug knowledge lived only in agent memory, git history, and
session context. This file is the durable, in-repo home. It travels with the code, any agent
or human can read it, and it needs no external tool.

## How to use this file

- One entry per known bug, with a stable ID (`BUG-NNN`). Never renumber — IDs are referenced
  from commits, tests, and memory.
- The strongest form of an open entry is an **executable** one: an `#[ignore = "BUG-NNN"]`
  test that fails for the right reason. The bug is then self-documenting and self-closing —
  remove the `#[ignore]` when the fix lands and the suite enforces it forever.
- When you fix an entry, move it to **Fixed** with the commit SHA. Don't delete it — the
  history is the point.
- Severity is about the **instrument on stage**, not code aesthetics: `HIGH` = wrong output
  or silent data corruption a performer would hit; `MED` = reachable but narrow; `LOW` =
  latent / cosmetic / needs an unusual setup.
- **Escape analysis (added 2026-07-05):** a bug found in the app after an orchestrated
  landing carries one extra line in its entry — `Escaped: <wave/branch> · caught-by:
  <brief | gate | demo | held-out input | review>` — per `DESIGN_DOC_STANDARD.md` §10.
  Over time this is the empirical record of which orchestration stage leaks, so process
  fixes target the leaking stage instead of guessing.

---

## Index of open bugs (nickname → say this in chat)

| ID | Nickname | One line |
|---|---|---|
| BUG-039 | **saw-rotation-wrap** | angle params clamp instead of wrapping; saw LFO can't spin a full rotation (MED, mechanism pinned) |
| BUG-035 | **authoring-hitch** | ~59ms frame every ~5s: clip-atlas f16 convert on content thread (MED, root-caused) |
| BUG-037 | **glp-first-render-stall** | ~37ms warm-up on a glTF clip's first rendered frame (MED) |
| BUG-038 | **ableton-log-spam** | bridge warns every 1.5s forever when Live absent (LOW) |
| BUG-006 | **fused-param-noop** | param edits/undo on fused-away nodes silently no-op (HIGH) |
| BUG-007 | **fusion-exclusion-blind** | particle-loop exclusion misses configured wgsl_compute shapes (HIGH) |
| BUG-008 | **fused-buffer-oob** | mismatched array lengths read out of bounds in fused region (HIGH) |
| BUG-009 | **stateless-gate-miss** | harvest skip resets StateStore-held scalar state (HIGH) |
| BUG-010 | **wgsl-first-entry** | multi-entry wgsl_compute silently dispatches the first (MED) |
| BUG-011 | **fused-output-oversize** | fused output buffer sized to max of all inputs (MED) |
| BUG-015 | **inspector-overlap** | sections at stale offsets after scroll (MED, repro needed) |
| BUG-025 | **timeline-scissor-bleed** | clip content bleeds across row bounds (MED, repro needed) |
| BUG-026 | **popup-fade-freeze** | fix landed, running-app verification owed (MED) |
| BUG-033 | **ui-snapshot-broken** | headless UI harness feature doesn't build (MED) |
| BUG-012 | **tex-rename-corrupt** | fragment `tex_` port-rename corrupts `tex_*` scalars (LOW) |
| BUG-018 | **catalog-stale** | node_catalog.json out of sync test red (LOW) |
| BUG-031 | **audio-load-blip** | ~10ms of audio leaks when a voice is built (LOW) ⚠ id collides with the positional-layer-menu entry under Fixed — first free id is BUG-039 |
| BUG-034 | **atlas-uv-test-gap** | headless preview doesn't cover live atlas UV path (LOW) |
| BUG-014 / 030 | parked | NaN content-key hash · color-ratchet red |
| BUG-019 / 020 / 021 | deferred | group-fold gap · gen-card collapse · snap-back gap |

## Open

### BUG-039 (saw-rotation-wrap) — Angle params clamp at range ends, so a saw LFO / automation can't drive a smooth full rotation — MED (enhancement, performer-facing)

**Symptom** (Peter, 2026-07-06) — binding a saw LFO or an automation ramp to a rotation
param and sweeping 0→360° hitches at the wrap point: the effective value clamps at the
range end instead of wrapping, so continuous rotation — the most common motion move in a
VJ set — can't be played with a saw. Affects default card slider bindings across effects
and generators.

**Fix shape (mechanism pinned; Sonnet-executable, no design doc needed):**
- Add `wraps: bool` (serde default false) to `ParamSpecDef` — explicit tag, not inferred
  from `is_angle` (per `hidden-field-dependencies`; angle-typed ≠ periodic, e.g. FOV).
  Every existing project/preset loads unchanged.
- Apply wrap at the single point where modulation already post-processes effective values
  (where `whole_numbers` rounding lives): for wrapping params,
  `value = min + (v - min).rem_euclid(max - min)` instead of clamp. Base/undo semantics
  untouched — wrap applies to the effective only. Slider wrap-drag UX = later, not this pass.
- Mechanical sweep: every angle/degree-range param across primitive `ParamDef`s and the
  ~45 preset JSON card params; tag `wraps: true` ONLY where truly periodic (rotation,
  orbit, hue-angle, kaleidoscope angle). Clamped-for-a-reason params (FOV, ±89° tilt, arc
  extents) stay unwrapped. List every tag decision in the PR body.
- Gate: unit test on the wrap math (incl. negative saw), plus one preset smoke proving a
  saw 0→360 on a tagged param renders identical frames at phase 0 and phase 1.

**Sequencing** — AFTER the param-system post-refactor audit (Fable queue item 1): same
code region; land the audit's verified ground first.

### BUG-037 (glp-first-render-stall) — First render of a glTF scene layer stalls the content thread ~37ms (warm-up on the frame, not at load) — MED

**Symptom** — trace run 2026-07-06 (`meshImportTests.manifold`): the first frame after the
project's glp layer became active showed `generators=37.1ms` (RENDER_TRACE frame=421) —
one-off, distinct from the recurring BUG-035 spike. On stage this means launching a glp
clip mid-set drops ~2 frames on its first render.

**Root cause (probable, unmeasured beyond the one trace line)** — first-touch work in the
generator path: glTF texture decode hand-off / mesh buffer upload / pipeline+PSO creation
happens lazily on the first rendered frame instead of at load/schedule time. The repo
already has the machinery pattern for this class (`plugin_prewarm.rs`, generator pipeline
pre-warm at startup, pipeline archive).

**Fix shape** — pre-warm at project-load / clip-schedule time: when a glp generator clip
is loaded (or armed on a timeline), run its first-frame resource creation off the hot
path so frame 1 of the clip renders at steady-state cost. Verify with the same
MANIFOLD_RENDER_TRACE run: no >20ms frame on first clip render.

### BUG-038 (ableton-log-spam) — AbletonBridge retries + WARN-spams every ~1.5s forever when Live isn't running — LOW (log hygiene)

**Symptom** — any session without Ableton running logs
`[AbletonBridge] OSC send failed for /live/song/get/num_tracks: Connection refused` at
WARN level every ~1.5s indefinitely (see any 2026-07-06 trace-run log).

**Fix shape** — warn once on first failure, then downgrade repeats to debug until a send
succeeds (state flip logs "reconnected" at info). Optionally back off the poll while
refused. `manifold-playback/src/ableton_bridge.rs`, small.

### BUG-036 (dead-LFO-on-reload) — LFO on an imported-glb generator's card param is dead after project reload; re-importing the same .glb revives it — MED — FIXED 2026-07-06

**FIXED 2026-07-06** — both halves of the fix shape below, plus two siblings the audit
found in the same class:
- **Ordering (root):** `manifold_io::loader` gained `_with` variants that hand the file's
  `embeddedPresets` to an installer BEFORE the typed `Project` deserialize
  ([loader.rs](../crates/manifold-io/src/loader.rs) `EmbeddedPresetsPrePass`); the app
  passes `install_embedded_presets` so the overlay + core registry are populated when the
  V1.4 param loader resolves each instance ([project_io.rs](../crates/manifold-app/src/project_io.rs)).
- **Keep-don't-drop (class-kill):** `build_param_manifest` now only drops an unknown id
  when the template actually RESOLVED and says the id is gone (informed deprecation).
  With no template at all, the entry is kept on a placeholder spec — state is never lost
  to a missing template ([effects.rs](../crates/manifold-core/src/effects.rs)).
- **Sibling 1:** history-snapshot restore/open-copy never installed the snapshot's
  overlay at all (params dropped AND stale overlay left live) — now go through
  `load_project_snapshot_with` + an unconditional overlay install at the
  `apply_project_io_action` seam.
- **Sibling 2:** New Project never cleared the previous project's overlay (fork leak) —
  covered by the same apply-seam install.
Verified against the real repro: `meshImportTests.manifold` loads with all 17 imported
card params present and the saved `cam_orbit` driver resolving; regression test
`crates/manifold-app/tests/project_local_preset_reload.rs` proves both defenses
independently.

**Symptom** (Peter, 2026-07-06, `~/Downloads/meshImportTests.manifold`) — a project saved
with a glb auto-built graph (the `assemble_import_graph` door) reloads fine visually, but
an LFO bound to one of its card params (Camera Orbit) doesn't run. Deleting the layer and
re-creating it by dropping the SAME .glb makes the identical LFO run. So the modulation
path works against a freshly-imported instance and not against the deserialized one.

**Root cause — SMOKING GUN in the 2026-07-06 trace-run log.** On project load, EVERY card
param of the imported preset is dropped at deserialization:
`[manifold-core] dropping unknown param id "cam_orbit" on PresetTypeId(cc0_japanese_apricot_prunus_mume#2) load (no template descriptor, no inline spec)`
— same for cam_dist/cam_fov/cam_tilt, sun_int/x/y/z, metal_0..3, rough_0..3, env_bright.
The LFO is inert because its target param no longer exists in the loaded manifest. The
drop lines appear BEFORE `[presets] merging 4 project generator preset(s)` in the log:
the V1.4 param loader resolves specs against the template registry, and project-local
(imported) preset templates are merged into the registry only AFTER the project's layer
data deserializes — so every param keyed to a project-local preset type resolves to "no
template descriptor" and is dropped. Re-importing works because a fresh import registers
the template first. Almost certainly a param-storage-redesign (landed 2026-07-05)
load-ordering regression, cousin of the known-RED `expose_mirror` test.

**Fix shape** — order the loader so project-local preset templates register before layer
param deserialization; AND (class-kill, per `eliminate-bug-class-at-storage-layer`)
make the loader keep an unresolvable param as an inline spec instead of dropping it —
silent data loss on load is the storage-layer bug class this repo already decided to
eliminate. The drop log line should become a hard test assertion (load the repro project,
assert zero drops).

**Repro** — load `meshImportTests.manifold`, press play: Camera Orbit LFO inert. Delete
layer, drag the .glb back in, rebind: runs.

### BUG-035 (authoring-hitch) — 3D scenes hitch when a camera/light param is animated — MED — re-encode hypothesis MEASURED AND REFUTED 2026-07-06; cause is app-side, still open

**Measurement (2026-07-06, Fable)** — `freeze-profile scene <glb> [param] [frames]` (new bench
arm): drives the production import door (`assemble_import_graph`) + production
`PresetRuntime::render` on the azalea fixture, static params vs `cam_orbit` swept per frame
(the LFO shape), with a convergence gate (async texture decode means the first ~120 frames
render black — un-gated numbers are void) and a sweep-sanity readback (min→mid must change
pixels; min→max on an angle param is a full circle, a no-op).

Results (600 frames/arm, converged, sweep verified live):
- **CPU encode of the whole chain: ~70µs p50, 0.35ms max, zero >1ms frames in 2400** —
  static or animated, 1080p or 4K. The "full-chain re-encode grazes the 16ms deadline"
  hypothesis is off by three orders of magnitude. Incremental command encoding would
  recover ~0.07ms/frame — **do not build it for this bug.**
- **No static-vs-animated delta**: CPU 0.067 vs 0.065ms p50 (1080p); GPU 2.23 vs 2.18ms.
  The graph runtime prices an LFO'd scene identically to a static one.
- Also refuted along the way: there is NO held-when-static gate at the compositor/layer
  level (the occlusion skip is blend-only — content_pipeline.rs "Everything still
  RENDERS"); the static-scene smoothness the original diagnosis leaned on comes from the
  executor's pure-step memo, and render_scene/gltf_mesh_source re-run every frame anyway.
- The mesh re-blit + per-object rebind "smaller shaves" live inside that 70µs envelope —
  not worth building for this bug either.

**Surviving suspects (all app-side, only run when a param animates):** the modulation/LFO
evaluator on the content thread; UI redraw driven by visibly-changing values (inspector
sliders, graph-editor canvas + thumbnail dump_set when the editor is watching); content↔UI
GPU contention (see `ui-present-content-gpu-contention` memory); present/pacing path.

**In-app profiler sessions (2026-07-06, Peter, `meshImportTests.manifold`)** — the hitch is
now precisely characterized: baseline content frame ~0.09ms, with **isolated single frames
of ~59ms (58.6/58.7/59.2), entirely inside `render_content_ms`**, cadence roughly one per
5–6s, present in BOTH the static and the LFO run. LFO/animation is fully exonerated as a
cause (the original framing was wrong — a static scene hitches identically; you just see it
when something moves). The quantized ~59ms magnitude + slow cadence says periodic
maintenance work or a blocking wait inside `render_content_native`, not render cost.
Candidate: `pool.prune_stale(300)` every 300 frames (content_pipeline.rs:1584-1595) — frame
indices of the spikes (900, 1233, 3630) are ≡ 0/33/30 mod 300, consistent if the pool's
counter is offset from the profiler's frame index. Unproven.

**CAUGHT (2026-07-06, MANIFOLD_RENDER_TRACE run)** — five of five spikes land in the
`clip_atlas` section: `clip_atlas=57.9–61.6ms`, cadence ~360 frames, exactly the
CLIP_ATLAS_SAVE_DEBOUNCE=300 cycle. The culprit line is
[content_pipeline.rs:2225](../crates/manifold-app/src/content_pipeline.rs#L2225) —
`clip_atlas_readback.try_read()` on the completed persist readback. `try_read`
([gpu_readback.rs:99-115](../crates/manifold-renderer/src/gpu_readback.rs#L99)) converts
f16→u8 **per pixel, per channel, scalar, on the content thread**, and the clip atlas is
8192×1152 Rgba16Float (75MB, 9.4M pixels) — ~58ms of CPU once per debounce cycle. The
section's "all disk IO is off-thread" claim is true; the CPU conversion before the
hand-off is the stall. (The separate one-off `generators=37.1ms` spike on the first
frame after load is glTF texture/pipeline warm — not this bug.)

**Fix shape (root: no O(surface) CPU work on the content thread)** — switch the persist
path to `try_read_packed()` (plain memcpy, gpu_readback.rs:148) and move the f16→u8
conversion + `slice_atlas_for_store` into the existing clip-thumb disk worker: hand it
(raw bytes, layout snapshot, hashes) and let it slice/convert/store on its own thread.
No new threads, no format change on disk.

**Symptom** — animating a 3D scene's camera or sun/light via LFO produces a slight, visible
hitch — an uneven frame spike, not a clean framerate drop. Reported by Peter 2026-07-05 on
glTF ("glp") scenes; suspected across all `render_scene` / 3D-mesh output. A static 3D scene
is smooth, and the *same* LFO on a 2D effect param is smooth (Peter confirmed 2D is fine).

**Root cause (hypothesis, reasoned from code — NOT yet measured)** — when a layer is dirty
it re-executes its whole effect chain, re-encoding every node's GPU commands into a fresh
command buffer each frame. There is no incremental "encode once, patch the changed uniform"
path. A static scene is held/composited without re-running the chain (this held-when-static
behavior is *inferred* from observed smoothness — the exact gate was not located in code and
should be confirmed during design). An LFO makes the layer dirty every frame, so the full 3D
chain re-runs 60×/s. That re-encode is the suspected fixed per-frame cost that grazes the
16ms deadline on the heavier 3D path while staying invisible on cheap 2D chains.

Confirmed by reading:
- `render_scene` and `gltf_mesh_source` are both non-pure (`PURE` defaults false,
  [primitive.rs:104](../crates/manifold-renderer/src/node_graph/primitive.rs#L104);
  neither overrides it), so the executor's memo-skip
  ([execution.rs:189](../crates/manifold-renderer/src/node_graph/execution.rs#L189)) never
  spares them — they re-run every frame the chain runs. The still-scene savings are NOT at
  the node-memo level.
- Per animated frame `render_scene` recomposes each object's model matrix, rebuilds its
  uniform struct, looks up the pipeline, and re-binds all 8 texture/buffer slots
  ([render_scene.rs:605-680](../crates/manifold-renderer/src/node_graph/primitives/render_scene.rs#L605-L680)),
  and `gltf_mesh_source` re-blits the whole mesh buffer
  ([gltf_mesh_source.rs:213-222](../crates/manifold-renderer/src/node_graph/primitives/gltf_mesh_source.rs#L213-L222))
  even though geometry never changed.
- NOT the freeze compiler: render nodes are `Boundary` (non-fusable) and its recompile keys
  on structural content, "never per frame" ([freeze/install.rs:195-205](../crates/manifold-renderer/src/node_graph/freeze/install.rs#L195-L205)).
  Exposed-param modulation flows as runtime uniforms and never changes the content key.

**Fix shape** — incremental command encoding for the graph runtime: cache a layer's command
buffer and only re-record when the graph *structure* changes, patching camera/light (and
other exposed) uniforms in place between frames. System-wide upgrade (every animated layer
benefits; payoff concentrated on expensive chains — 3D scenes, long stacks, many bindings).
Orthogonal to, and layers on top of, the existing memo system (skips pure nodes) and freeze
compiler (fuses pointwise passes) — an *addition*, not a rewrite. It sits on the hot render
path where a stale-uniform bug becomes the show, so this is HIGH-risk-to-touch. Smaller
shaves that reduce (not eliminate) the re-encode cost: persistent mesh buffer to kill the
per-frame re-blit; trim `render_scene`'s per-object rebind.

**Before building** — confirm the CPU re-encode is actually where the ms go: add per-frame
timing around the 3D chain execution and watch it under a running LFO. Steady ~X ms → render
cost, optimize the render; sawtooth → scheduling/overhead, and incremental encoding is the
fix. (Not run this session — the app isn't headless and Peter didn't want the round-trip.)

**Design owner** — queued to Fable for a proper design doc (`docs/*_DESIGN.md`), per
[[fable-priority-queue]]. Reasoned diagnosis only; verify the measurement first.

### BUG-031 — Audible blip when an audio clip's voice is built (play-then-pause leaks ~10ms of the file's start) — LOW

**Symptom** — a very subtle pop/click from the speakers at the moment an audio file is
loaded onto the timeline (e.g. Finder drag-drop). Reported by Peter 2026-07-05.

**Root cause** — [audio_layer_playback.rs:171-179](../crates/manifold-playback/src/audio_layer_playback.rs#L171-L179):
`make_voice` calls `manager.play(data)` at full volume and only then
`handle.pause(Tween::default())`. kira's `pause` is a fade-out — and `Tween::default()`
is a **10ms** linear fade (kira-0.9.6 `tween.rs:110`), not instantaneous — so the first
~10ms of the file renders audibly before the voice reaches its "start paused at 0" state.
Any file whose first samples carry signal produces the blip. (The 5ms `declick()` tween
used everywhere else in this module doesn't apply here; this is the one edge built on
kira's default tween.)

**Fix shape** — build the voice silent instead of pausing it after the fact: apply
`.volume(0.0)` to the `StaticSoundData` before `manager.play`, keep the pause+seek. The
per-tick sync path already restores the real volume via `set_volume(volume, declick())`,
so activation is unaffected. This kills the whole class including the race where an audio
callback fires between play and pause. One-line-ish, `manifold-playback` only.

### BUG-029 — `profiling` feature doesn't compile: rotted against the Beats/Bpm newtypes — FIXED 2026-07-06

**Fix** — the three newtype casts (`.as_f32()` / `.0`) applied; `cargo check -p manifold-app
--features profiling` and clippy are clean, default build untouched. Un-parked because the
profiler is the next oracle for BUG-035 (per-frame content-thread phase breakdown, LFO on vs
off). Toggling the perf HUD starts/stops a session when built with `--features profiling`
(input_host.rs `toggle_performance_hud`); sessions land in `profiling_sessions/`. Note: GPU
pass-level numbers are still zero on native Metal (pre-migration profiler) — the CPU phase
breakdown (engine tick / render_content / gpu_poll) is the usable signal.

**Root cause** — the `#[cfg(feature = "profiling")]` blocks in `manifold-app` predate the
`Beats`/`Bpm`/`Seconds` newtype migration and still treat those values as raw `f32`/`u32`.
Three sites: [content_thread.rs:854](../crates/manifold-app/src/content_thread.rs#L854)
(`Beats as u32` — non-primitive cast), [content_thread.rs:988](../crates/manifold-app/src/content_thread.rs#L988)
(`expected f32, found Beats`), and [content_commands.rs:933](../crates/manifold-app/src/content_commands.rs#L933)
(`expected f32, found Bpm`).

**Symptom** — `cargo build -p manifold-app --features profiling` fails with 3 `E0308`/`E0605`
type errors. The default build (profiling off) is unaffected, which is why the rot went
unnoticed — the feature evidently hasn't been compiled since the newtype migration landed.

**Found during** — PARAM_STORAGE P2 (2026-07-05), while compile-checking the profiling path
after migrating its param readout from the deleted positional `param_values` to `ParamManifest`
(that param-side migration is done and correct; these 3 errors are unrelated newtype-cast rot
in the same blocks).

**Fix shape** — wrap each site in the Beats/Bpm accessor instead of a raw cast (~3 one-line
fixes). Unrelated to param storage, so parked here rather than folded into P2.

### BUG-033 — `ui-snapshot` feature build broken: `manifold_core::effects::resolve_param_in` no longer exists — MED (blocks the headless UI harness)

**Root cause** — [interact.rs:500](../crates/manifold-app/src/ui_snapshot/interact.rs#L500) (`lane_param_range`, an
automation-lane interact verb) calls `manifold_core::effects::resolve_param_in(&def, fx, param_id)`
to read a param's `(min, max)`. That function/module path is gone after the PARAM_STORAGE
refactor (the range now lives on the `ParamManifest`/spec, not a `resolve_param_in` helper).

**Symptom** — `cargo build --bin manifold --features ui-snapshot` fails with `E0425` (unknown
function) + a knock-on `E0433`. The DEFAULT build is unaffected, so it went unnoticed — but it
means the entire `ui-snap` headless harness (graph/editor/timeline PNG + `--script` driver) can't
compile on trunk. Found 2026-07-05 (Opus) while rendering a BUG-027 verification PNG; worked
around with a temporary local stub (reverted) to get the render.

**Fix shape** — resolve the param spec through the current manifest API and read its min/max
(mirror whatever `lane_param_range`'s live-app equivalent now does). Owner: PARAM_STORAGE P2 (its
refactor moved the range); ~1 site. Unrelated to the LayerId / node-preview work in this session.

### BUG-034 — Headless preview verification doesn't cover the live atlas UV path — LOW (test-coverage gap, follow-up to BUG-027)

**Gap** — the inline node-preview fix (BUG-027) is pixel-verified headless only through the
per-node-texture path (`ui_snapshot/render.rs`, whole-texture UV `[0,0,1,1]`). The LIVE app packs
every preview into one rotating atlas and samples a per-cell UV with letterbox/aspect trim; that
cell-picking math lives inline in [app_render.rs](../crates/manifold-app/src/app_render.rs) and is
NOT exercised by any headless render (the atlas is filled by the content thread). So a subtle cell
or aspect error would show wrong/offset/squashed previews in the running editor but pass every test.

**Fix shape** — (1) factor the atlas-cell-UV math out of `app_render.rs` into one shared helper;
(2) in the harness, pack the already-rendered per-node textures into a synthetic atlas + build the
matching `node_atlas_layout`, register it under the atlas handle, and drive previews through that
shared helper. Then a single graph PNG proves the live cell math, not a copy of it. Not large.
Gated behind BUG-033 (the `ui-snapshot` harness doesn't compile on trunk).

### BUG-030 — Design-token ratchet red on trunk: raw `Color32::new(` count 201 vs baseline 200 — LOW (parked, not param-storage)

**Root cause** — a UI landing added one raw `Color32::new(` literal in `crates/manifold-ui/src`
without tokenizing it or bumping the ratchet. [design_tokens.rs:40](../crates/manifold-ui/tests/design_tokens.rs#L40)
sets `COLOR_BASELINE = 200`; the actual scan count is 201.

**Symptom** — `cargo test -p manifold-ui --test design_tokens` fails (`no_new_raw_color_literals`,
201 > 200). **Fails identically on origin/main (58bc2d43)**: `crates/manifold-ui/src` is
byte-identical between that commit and the P2 branch, and `scan()` reads only that directory, so
the drift predates and is independent of P2.

**Found during** — PARAM_STORAGE P2 (2026-07-05), full-workspace sweep after merging origin/main.
Two pre-existing trunk failures surfaced (this + the stale node catalog, which P2 regenerated) —
a signal that a recent UI landing skipped the full workspace test.

**Fix shape** — the UI/design-token owner tokenizes the offending literal (a `color::` token, or
`// design-token-exempt: <reason>`); the ratchet then returns to green at 200. Left red on purpose
rather than bumping the baseline, which would silently bless the drift the ratchet exists to catch.
Unrelated to param storage.

BUG-006–014 come from the **freeze-compiler adversarial bug hunt, 2026-07-03**
(40-agent Sonnet workflow `wf_73bb4ddf-885`; 10 finder lenses → every finding attacked by 2
independent skeptics). BUG-006–012 were **confirmed by both skeptics** with line-level
evidence; BUG-013/014 got split verdicts (judgment recorded per entry). Full verifier
transcripts: the workflow journal at
`~/.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/18511d71-15ae-4119-81cc-894a3f83d247/subagents/workflows/wf_73bb4ddf-885/journal.jsonl`.
System context for all of them: [FREEZE_COMPILER_MAP.md](FREEZE_COMPILER_MAP.md).

### BUG-006 — Param edits/undo on fused-away nodes silently no-op until an unrelated rebuild — HIGH

**Root cause** — [bound_graph.rs:114-133](../crates/manifold-renderer/src/node_graph/bound_graph.rs#L114-L133):
`apply_inner_param_overrides` looks each node's `node_id` up in `slot.node_map` and silently
`continue`s on a miss. For a fused card, `node_map` is built from the FUSED def
([preset_runtime.rs:1285-1288](../crates/manifold-renderer/src/preset_runtime.rs#L1285-L1288)),
so fused-away members (e.g. `gain`) aren't in it. The path never consults the fused view's
`fused_retarget` map (which knows `gain.gain` → `fused_region_0.n0_gain`). Value-only edits
bump only `graph_version`, which is deliberately not in `compute_topology_hash`, so no rebuild
fires.

**Symptom** — edit a param in the editor, close it (re-fuses, bakes the value), then Undo
while viewing another effect: the def reverts but the fused kernel keeps rendering the OLD
value indefinitely, until a resize/editor-open/unrelated edit forces a rebuild. Live control
stranded, zero errors. `CHAIN_FUSION_DESIGN.md` §6 already flags this as an open item.

**Fix shape** — thread the fused view's `fused_retarget` into `apply_inner_param_overrides`
(or into `node_map` construction): on a `node_map` miss, translate `(node_id, param)` through
the retarget map to `(fused node, n{i}_field)` and apply there. Test: fuse, value-edit,
assert the fused node's param moved without a rebuild.

### BUG-007 — Particle-loop fusion exclusion is blind to configured `node.wgsl_compute` shapes — HIGH

**Root cause** — [region.rs:834](../crates/manifold-renderer/src/node_graph/freeze/region.rs#L834):
`cycle_contains_array` uses a bare `registry.construct(type_id)` — the ONE hold-out in the
file; every other classification call site uses `configured_construct`, whose own doc comment
states why the bare form is wrong. A full-kernel `node.wgsl_compute` with a
`var<storage, read_write> array<Particle>` output (StrangeAttractor's "simulate" node is a
shipped instance) introspects as the DEFAULT kernel (no Array output) under the bare
construct, so the cycle scan can't see the particle stage.

**Symptom** — a texture atom on a feedback loop whose only Array producer is such a node
passes cut rule 12 and fuses tier-A f16 in-loop, where the bit-exact induction argument does
not hold across a particle/scatter stage (FluidSim precedent: max_abs ~0.73 over ~31% of
pixels). Fused render visibly diverges from the editor.

**Fix shape** — one line: use `configured_construct(registry, node)` in
`cycle_contains_array`. Sweep the file for any other bare-construct hold-outs
(`node_is_buffer_atom` / `region_is_buffer` at
[region.rs:1885-1905](../crates/manifold-renderer/src/node_graph/freeze/region.rs#L1885-L1905)
have the same pattern — audit while there). Test: a loop through a configured wgsl_compute
particle node must classify its texture atoms Boundary.

### BUG-008 — Fused buffer region with mismatched array lengths reads out of bounds — HIGH

**Root cause** — [codegen.rs:1777-1813](../crates/manifold-renderer/src/node_graph/freeze/codegen.rs#L1777-L1813):
`generate_fused_buffer` anchors the dispatch guard to the FIRST array external's
`arrayLength`, then unconditionally pre-reads EVERY array external at that index. Nothing
anywhere (classify, union, `build_region`, `fused_def_builds`) checks that a buffer region's
array externals agree on length — the tier-6 uniformity gate is texture-only. The unfused
atom (e.g. `LerpInstanceFields`) explicitly clamps to `min(a_cap, b_cap, out_cap)`.

**Symptom** — two array inputs of different lengths fuse; for indices past the shorter
buffer the kernel does an out-of-bounds Metal storage read and writes garbage
instances/particles to the output — silent visual corruption. Shipped presets happen to share
lengths today; user graphs are unprotected.

**Fix shape** — either refuse at `build_region` when a buffer region has >1 array external
(conservative, fail-closed, cheapest), or emit a per-external in-bounds guard
(`idx < arrayLength(&src_e)` with a defined fallback element). Pair with BUG-011.

### BUG-009 — Segment "stateless" gate misses StateStore-held scalar state; harvest skip resets it — HIGH

**Root cause** — [segment.rs:153-171](../crates/manifold-renderer/src/node_graph/freeze/segment.rs#L153-L171):
`def_is_segment_stateless` checks only `state_capture_input_ports` + `aliased_array_io`.
Primitives that hold real cross-frame state in the StateStore without declaring either —
`sample_and_hold`, `envelope_decay`, `trigger_ease_to`, `compressor_envelope`,
`envelope_follower_ar`, `inject_burst` — pass as stateless. Segment member slots get
`def_content_key: 0` ([preset_runtime.rs:1105](../crates/manifold-renderer/src/preset_runtime.rs#L1105))
and `harvest_state_from` skips them
([preset_runtime.rs:1693](../crates/manifold-renderer/src/preset_runtime.rs#L1693)), so any
chain rebuild drops their state.

**Symptom** — AutoGain (shipped: `compressor_envelope` next to pointwise atoms) joins a
segment; any rebuild while it's a member — editor open/close elsewhere, an unrelated card
edit, or the fused-segment swap-in itself — resets the envelope: gain snaps to unity, a
visible/audible pop mid-show. Violates the chain-fusion design's own "never resets state"
invariant.

**Fix shape** — the root fix is a truthful statefulness signal: a `NodeRequires`-style
`uses_state_store` flag (or derive it from `ctx.state` usage) that `def_is_segment_stateless`
also checks. Stop-gap is a hard-coded exclusion list, which is exactly the pattern the freeze
module refuses everywhere else — prefer the flag.

### BUG-010 — `wgsl_compute` silently dispatches the first of multiple entry points — MED

**Root cause** — [wgsl_compute.rs:615-624](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L615-L624):
`introspect()` takes `module.entry_points[0]` with no `len() == 1` check (the module doc at
lines 29-31 claims multiple entry points fail validation — they don't). The pipeline compile
independently picks the same first entry. A fragment-form node embeds the author's raw text
BEFORE the synthesized `cs_main`, so any leftover `@compute fn` in the fragment becomes
entry 0 and is what actually runs. Verified empirically by a skeptic (scratch test:
`compile_failed=false`, `debug_pass` dispatched, real kernel never runs).

**Symptom** — a user kernel/fragment with a stray second `@compute` function (debug leftover,
copy-paste) renders stale/blank output with no warning; downstream wires read it as if it
worked. Authoring-time surface, so MED — but it's the exact silent-wrong-output class.

**Fix shape** — in `introspect()`: if the module has >1 compute entry point, prefer `cs_main`
by name; if absent, fail validation with the warning the doc already promises. Keep the
dispatch-side pick in lockstep.

### BUG-011 — Fused `@fused_output` buffer sized to max of ALL array inputs, not the member's own rule — MED

**Root cause** — [wgsl_compute.rs:1828-1829](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L1828-L1829):
the fresh-output branch of `array_output_capacity` returns
`input_capacities.max()` generically, overriding the fused output member's own semantic
capacity rule (e.g. `LerpInstanceFields` follows only input `a`). Downstream consumers
(`render_instanced_3d_mesh` computes capacity from physical buffer size) can then draw ghost
instances from the never-written tail.

**Symptom** — with mismatched input lengths (same shape as BUG-008), the fused output buffer
is larger than the unfused chain's, and its tail is uninitialized pooled VRAM — potential
stale-data ghosting across preset/frame boundaries.

**Fix shape** — falls out of BUG-008's decision: if multi-external buffer regions are
refused, this is unreachable; if guarded instead, size `dst` from the anchor external and
zero-fill or guard the tail.

### BUG-012 — Fragment `tex_` port-rename corrupts scalar params named `tex_*` — LOW

**Root cause** — [wgsl_compute.rs:544-548](../crates/manifold-renderer/src/node_graph/primitives/wgsl_compute.rs#L544-L548):
the fragment-form rename loop strips a literal `tex_` prefix from EVERY input port name with
no type filter (the sibling texture-binding rename at 549-561 IS filtered to
`SampledTexture`). A scalar `@param: tex_speed` exposes port `speed` while the uniform layout
and params stay keyed `tex_speed`; the dispatch-time wire lookup misses and the live wire is
silently ignored.

**Symptom** — a wired LFO/Ableton control on such a param renders as connected but never
moves the value. Latent — no shipped preset uses a `tex_`-prefixed param name.

**Fix shape** — filter the rename to texture-typed ports, mirroring lines 549-561. One-line.

### BUG-013 — `commit_and_wait_completed` never checks command-buffer status (likely the GPU-proof flake mechanism) — FIXED 2026-07-05

**Root cause** — [encoder.rs:1655-1662](../crates/manifold-gpu/src/metal/encoder.rs#L1655-L1662):
`waitUntilCompleted()` returns on ANY terminal state including `Error`; no caller checks
`status()`/`error()`. Every heavy freeze proof and `TextureDiff::compare` submit through this
call and read the result back as if it succeeded. Under cross-binary GPU contention
(documented in `.config/nextest.toml` and the `GPU_TEST_LOCK` comment; three call sites build
unlocked devices), a transiently failed buffer reads back stale/partial → spurious large diff.

**Status** — split verdict, judged REAL-as-flake-mechanism: it precisely explains the
observed signature (several heavy tests, random divergence sizes, never reproducing
isolated). It is test-infra, not a compiler miscompile — but it gates trust in the entire
oracle suite, so it blocks using the suite as a hard gate for agent work.

**Fix shape** — check the buffer's terminal status in `commit_and_wait_completed`; on error,
panic in tests (fail loudly, retryable) and log in production. Then re-baseline the flake:
if red runs now report command-buffer errors instead of pixel diffs, the mechanism is
confirmed; if divergences persist with clean status, keep hunting.

**FIXED 2026-07-05** — [encoder.rs](../crates/manifold-gpu/src/metal/encoder.rs) now calls a
`verify_completed()` helper after `waitUntilCompleted()`: if the buffer's status isn't
`Completed`, it reads `status`/`error()` and, in `debug_assertions` builds (tests + dev),
panics with the code+message; in release (the live show) it logs and continues rather than
crash mid-set. The dev-vs-release split via `cfg!(debug_assertions)` gives "loud in tests,
survivable on stage" without a test-only cfg (the helper lives in `manifold-gpu`, whose tests
aren't where the flake showed up). The `GPU_TEST_LOCK` "three unlocked sites" note above was
partly stale: the lock is a `parking_lot` reentrant mutex inside `test_device()`, and every
lib GPU test acquires it; the only unlocked device is the `gpu_proofs` integration binary's
own `GpuDevice::new()`, which runs in a separate process. That cross-process contention is now
self-reporting (a contended failure panics instead of reading stale pixels) rather than
silent, so a dedicated cross-process lock is no longer needed. Landed alongside the GPU-test
`gpu-proofs` feature gate (default `cargo test` is now GPU-free; run `--features gpu-proofs`
to exercise the proofs).

### BUG-014 — Content key collapses NaN/±Inf param values to one hash — LOW (parked)

**Root cause** — [install.rs:205-215](../crates/manifold-renderer/src/node_graph/freeze/install.rs#L205-L215):
`def_content_key` hashes `serde_json::to_vec(def)`, and serde_json writes non-finite floats
as `null`, so defs differing only in a non-finite param share a key while the fuse bakes the
raw f32.

**Status** — split verdict, judged UNREACHABLE today: the second skeptic traced every write
path into node params (scrub handlers clamp to finite ranges; JSON round-trips reject
non-finite). Parked as a hardening note — if a new param write path ever skips the clamp,
this becomes live. Cheapest closure: reject non-finite values at the `SerializedParamValue`
boundary (the eliminate-bug-class-at-storage-layer pattern).

### BUG-015 — Inspector sections render overlapping / at stale offsets after scroll — MED (repro needed)

**Symptom** — observed once by Peter, 2026-07-04, right after the timeline-P0 / multi-select
UX changes landed: the layer inspector drew its sections interleaved — the MIDI block
(MIDI / CHANNEL / DEVICE) and the audio-send block (send dropdown, +0.0 dB) overlapping
each other with a dead band between them, and the "No audio input" header clipped mid-panel.
Described as "a scrolling bug with the UI timeline updates". Screenshot lives in the
2026-07-04 session transcript.

**Root cause** — unknown. Suspect surface: inspector section Y-layout vs. scroll offset
(the `single-source-y-layout` invariant) or a stale subregion scissor
(`subregion-scissor-invariant`) going stale when timeline updates force a rebuild while the
inspector is scrolled.

**Repro** — not yet pinned. First step is reproducing: select a generator layer, scroll the
inspector, then trigger timeline churn (clip drag / multi-select updates) and watch for
section overlap.

**Fix shape** — TBD after repro. If it's the known invariant class, the fix is at the layout
single-source, not per-section patches.

### BUG-016 — Imported .glb layers are black boxes: no card params, no Model File picker, edit paths silently no-op — FIXED 2026-07-04 (`2d5e4dc6`)

**Resolution** — PRESET_LIBRARY P0 (D9) shipped: the drop now registers the assembled
graph as a project-embedded preset (`origin: Saved`) and the layer TRACKS it (`graph:
None`); the assembler emits a curated 13-slider card (camera/sun/envmap/per-object
material) with real bindings; the app installs the catalog overlay before the layer is
created, so the process-global preset registry seeds `init_defaults` consistently on both
threads. The `graph_def_mut` override install is deleted. verify-at-impl #4 resolved
(`bundled_preset_json` reads the overlay-merged catalog, no change needed). Assembler +
command tests + GPU render proofs green. **Still owed: the live drag-drop manual gate** in
a running app (card sliders move pixels, editor opens on the cog, save/reload intact) — the
one thing only Peter can eyeball. Original analysis below for reference.

**Root cause** — the glTF Stage-4 install mints a preset id that resolves in no catalog and
stashes the def only on the layer
([app_lifecycle.rs:506](../crates/manifold-app/src/app_lifecycle.rs#L506),
[layer.rs:100](../crates/manifold-editing/src/commands/layer.rs#L100)). Every type-keyed
surface then fails independently: the assembler emits empty `params`/`bindings`
([gltf_import.rs](../crates/manifold-renderer/src/node_graph/gltf_import.rs), metadata block)
so the card is empty; generator string params are sourced from the registry only
([inspector.rs:2251](../crates/manifold-app/src/ui_bridge/inspector.rs#L2251)) so the Model
File picker never shows; the editor's catalog default is `None`, which gates several edit
dispatch arms into silent no-ops (e.g. [app.rs:1356](../crates/manifold-app/src/app.rs#L1356)).
The reported empty editor canvas is NOT fully root-caused: `GraphSnapshot::from_def` on the
assembled def is proven good (12 nodes / 10 wires), so the entry path loses the watch target —
observe at repro.

**Fix shape** — `PRESET_LIBRARY_DESIGN.md` P0 (D9): the drop registers an `EmbeddedPreset`
and the layer tracks it; assembler emits curated performance bindings. Not per-consumer
fallbacks.

### BUG-017 — `docs_index_is_in_sync_with_docs_dir` red on main: two design docs never regenerated the index — FIXED 2026-07-05

**Symptom** — found 2026-07-04 running the full workspace sweep for the automation-P4
landing (unrelated to that work — pre-existing on origin/main before the landing branch
touched anything, confirmed via `git show 90ab8531:docs/README.md`).
`cargo test -p manifold-core --test docs_index_sync` fails:
`docs/README.md is out of sync with docs/. Missing from the index: ["AUDIO_SENDS_UX_DESIGN.md",
"TIMELINE_INGEST_DESIGN.md"]`.

**Root cause** — two sessions added design docs (`AUDIO_SENDS_UX_DESIGN.md`,
`TIMELINE_INGEST_DESIGN.md`) without re-running the generator afterward.

**Fix shape** — mechanical: `python3 scripts/gen_docs_index.py`, commit the regenerated
`docs/README.md`. Not fixed this session because other sessions were actively adding more
docs concurrently — regenerating now risked going stale again within the hour. Whichever
session next touches `docs/` and finds the tree quiet should run the generator and close
this out.

**Fixed 2026-07-05** — regenerated while adding `VERIFICATION_DEBT.md` (orchestration-quality
pass); `cargo test -p manifold-core --test docs_index_sync` green, 103 docs indexed.

### BUG-018 — `node_graph::catalog_gen::tests::regenerates_in_sync` red on main: `docs/node_catalog.json` stale against the node registry — LOW

**Symptom** — found 2026-07-04, same full-workspace sweep as BUG-017, same shape: confirmed
pre-existing on origin/main (`90ab8531`) before the automation-P4 landing branch touched
anything — reproduced standalone in a disposable worktree at that exact commit.
`cargo test -p manifold-renderer --lib node_graph::catalog_gen::tests::regenerates_in_sync`
fails with `docs/node_catalog.json is stale`.

**Root cause** — not investigated; some session added/changed a node-graph primitive without
re-running `cargo run -p manifold-renderer --bin gen_node_catalog` afterward. Given `node_count`
sits at 214 in the checked-in file, worth diffing against the live-generated output to see
which node(s) are missing/changed before just overwriting.

**Fix shape** — mechanical: `cargo run -p manifold-renderer --bin gen_node_catalog`, commit
the regenerated `docs/node_catalog.json`. Same reasoning as BUG-017 for not fixing it this
session (unrelated to the work at hand, and worth doing once rather than mid-churn).

### BUG-019 — Motion "group fold" (D17) has no UI surface to fold — DESIGN GAP (deferred)

**Symptom** — found 2026-07-04 completing UI motion P2. D17 lists "group fold: children
collapse into header," but the animation has nothing to animate: `EffectGroup.collapsed`
exists at the model layer (`crates/manifold-core/src/effects.rs:3194`) with zero rendering
surface — no group header, no collapse toggle, no child-card grouping by `group_id` in the
inspector (`rg EffectGroup crates/manifold-ui/src` → 0 hits).

**Root cause** — the design assumed a foldable effect-group UI in the inspector that was
never built. Group fold is a *new feature* (group header + child-card filtering + collapse
toggle), not an animation retrofit — correctly out of the motion layer's scope.

**Fix shape** — build the effect-group inspector UI first (own small design: header row,
`group_id`-keyed child filtering, collapse toggle), THEN the fold animation is a `FlipList`
+ exit-state retrofit like the other P2 collapses. Needs a design/build decision from Peter.

### BUG-020 — Card collapse animates effect cards but not generator cards — LOW (deferred)

**Symptom** — found 2026-07-04 (UI motion P2 batch 1). Effect cards collapse/expand with the
`collapse_anim` reflow; generator cards do not — their rows parent at root (`None`) in
`ParamCardPanel::build_generator`, so there is no `ClipRegion` seam to clip the collapsing
body the way `build_effect` has.

**Fix shape** — give `build_generator` the same parent/clip-region seam `build_effect` uses,
then reuse the existing `collapse_anim`. Small, localized to `param_card.rs`.

### BUG-021 — Value snap-back is Perform-inspector only, not the graph-editor param cards — LOW (deferred)

**Symptom** — found 2026-07-04 (UI motion P2 closer). Right-click value-reset eases the fill
(EASE_SNAP) on Perform-context inspector cards; the graph editor owns a separate
`ParamCardPanel` instance not reachable from the `ParamRightClick` dispatch site
(`ui_bridge/inspector.rs:1140`), so its value resets snap without the settle.

**Fix shape** — thread the snap-back trigger to the graph-editor's `ParamCardPanel` too, or
lift the reset-with-settle into shared `ParamCardPanel` logic both dispatch sites reach.

### BUG-022 — Main-window browser popup: Escape while the search field is focused cancels the text session but leaves the popup open — FIXED 2026-07-05

**Resolution** — applied the documented fix shape: in the main-window `text_input.active` Escape arm
(`window_input.rs`), when `field == SearchFilter`, also call
`self.ws.ui_root.browser_popup.handle_escape()` alongside `text_input.cancel()`, mirroring the
editor window's node-picker branch — one press now dismisses both the search field and the popup.
The closed-overlay pump reconciles the already-cancelled session next frame. Compiles + clippy clean.
Owed: the in-app one-press-closes confirmation (headless can't drive it), but the code mirrors the
proven editor branch exactly. Original analysis below.

**Symptom** — found 2026-07-04 auditing `window_input.rs`'s keyboard routing while
implementing `docs/OVERLAY_SESSIONS_AND_PICKER_DESIGN.md`. For the MAIN window (effect/
generator browser), once the search field has focus (`self.text_input.active &&
field == SearchFilter`), every keystroke is intercepted by the `if self.text_input.active { ... }`
block in `window_input.rs` (`primary_keyboard_input`, ~line 1593) before it ever reaches
`UIRoot::process_events`/`route_overlay_event`. Its `Key::Named(NamedKey::Escape)` arm calls
only `self.text_input.cancel()` — it never touches `self.ws.ui_root.browser_popup`. So Escape
while typing clears the search text and ends the text session, but the popup itself stays
open; a second Escape (now routed normally, since `text_input.active` is false) is needed to
actually dismiss it. This is plausibly the exact mechanism behind Peter's original report
("the search and text seems to stay after you search and need to click elsewhere again to
close it properly") — P1's stash-and-drain fix (`TextSessionOwner`/`take_closed_overlays`)
closes the *orphaned-session-after-popup-closes-elsewhere* class, but this is the inverse:
popup not closing when the session ends.

Note the EDITOR window's analogous bespoke branch (`window_input.rs` ~1145, node picker) does
NOT have this gap — its Escape arm already calls `browser_popup.handle_escape()` directly
alongside cancelling the text input (now also wired through `note_overlay_closed_if` as part
of this session's P1 work).

**Root cause** — the main-window `text_input.active` Escape arm was written before the browser
popup existed as an `Overlay`-driven modal; it only ever needed to cancel a plain text field.
Nothing updated it when `BrowserPopupPanel` started hosting a `SearchFilter` session.

**Fix shape** — in the main-window Escape arm, when `self.text_input.field == SearchFilter`,
also call `self.ws.ui_root.browser_popup.handle_escape()` (mirroring the editor's branch) instead
of only `self.text_input.cancel()`. Small, localized to `window_input.rs`'s
`if self.text_input.active` block — no design-doc scope change, since this is a pre-existing
gap outside P1/P2's stated deliverables (which target orphaned-session-on-close, not
missing-close-on-cancel).

### BUG-024 — Generator preset thumbnails render on a WHITE background (unrepresentative) — FIXED 2026-07-05

**Resolution** — root cause was (a) from the suspect list: generators leave their background
transparent (alpha 0), and `readback_tonemapped_rgba8` saved that alpha into the PNG, so viewers
showed the transparent background as white. Fixed by compositing over opaque black in the readback
(`rgb * a`, force alpha 255) — generators produce straight (non-premultiplied) alpha per
[[alpha-standardisation]], so `rgb * a` is the correct over-black composite, and opaque content
(effects, a=1) is byte-identical. Verified by regenerating + Reading the PNGs: StarField now reads
as stars on black, Lissajous as a clean curve on black, Bloom (effect) unchanged and correct.
**Residual (separate, minor):** a few full-frame generators still read low-saturation in their bare
state — Plasma is a grey blob on black (its background is now correct, but its bare/default output
without audio modulation or a colormap param is desaturated). Not the white-bg bug; a per-generator
"bare look" issue, low priority — leave for a thumbnail-polish pass if it matters on the picker.

### BUG-024-ORIG — original analysis (Generator thumbnails on WHITE background) — superseded by the FIXED note above

**Symptom** — found 2026-07-05 eyeballing the committed `assets/preset-thumbnails/generators/*.png`
after adding warm-up frames (PRESET_LIBRARY P6). Effect thumbnails (rendered over the gradient
fixture) look correct (Bloom reads right). But GENERATOR thumbnails render their content over a
WHITE background instead of the generator's own (usually dark) field: StarField is dark specks on
white (should be bright stars on black); Plasma is a grey blob on white. Warm-up frames (t advances,
state accumulates) did NOT fix it — so this is a render-path issue, not cold-start.

**Root cause** — unknown, not yet diagnosed. Suspects in
`crates/manifold-renderer/src/preset_thumbnail.rs::render_generator`: (a) the `Rgba16Float` render
target isn't cleared to the generator's expected background (black/transparent) before
`runtime.render`, so unwritten/low-alpha regions read as white after `readback_tonemapped_rgba8`;
(b) premultiplied-alpha / straight-alpha mismatch in the readback vs how generators composite
(cf. [[alpha-standardisation]] — compositor is premultiplied, producers aren't); (c) the tonemap
maps the clear/HDR default toward white. The live `GeneratorRenderer` path composites over the
correct background, so comparing its clear/blend setup against this one-shot path should localize it.

**Fix shape** — likely: clear the thumbnail target to the same background the live generator path
uses (black or transparent) before rendering, and match its alpha convention in the readback. Then
regenerate the 46 factory PNGs via `cargo run -p manifold-renderer --bin generate-preset-thumbnails`.
Effects are unaffected. Until fixed, generator thumbnails are present but not visually usable — the
P6 image-cell display infra is correct; the generator render output is not.

### BUG-023 — `no_new_raw_color_literals` red on main: real count (201) one above baseline (200) — FIXED 2026-07-05 (in the P6 landing)

**Resolution** — the extra raw literal was localized (not a "prior session" — it was THIS
orchestration's own P5 landing `0d6e857e`): `browser_popup.rs` carried
`const BADGE_TEXT: Color32 = Color32::new(130, 130, 134, 255)` for the origin-badge text,
added by P5 and missed because that phase ran clippy + focused tests but not the
`design_tokens` integration guard. Fixed by tokenizing it into `color::BROWSER_CELL_BADGE_TEXT`
(color.rs is the scan's exempt token home), dropping the counted set back to 200. Guard green.
Lesson for the orchestration: run `-p manifold-ui --test design_tokens` on any phase that
adds UI color, not just clippy. Original analysis below.

**Symptom** — found 2026-07-05 running the full gate for `PRESET_LIBRARY_DESIGN.md` P6
(thumbnails). `cargo test -p manifold-ui --test design_tokens no_new_raw_color_literals` fails:
`Raw Color32::new( count rose to 201 (baseline 200)`. Confirmed pre-existing and unrelated to
P6: re-ran the same scan logic against `git show HEAD:<path>` for every file under
`crates/manifold-ui/src` (a standalone Python re-implementation of `scan()`/`classify()`) and got
201 on HEAD alone, before any P6 edit — the P6 changes to `browser_popup.rs`/`color.rs` net to
**zero** new raw literals (three new cells' worth of `Color32::new(` were added to `color.rs`,
which the scan excludes as the token home, and the matching local consts in `browser_popup.rs`
were pointed at those new tokens instead of a raw literal — no net change to the counted set).

**Root cause** — not investigated; some prior session's commit added exactly one raw
`Color32::new(` line somewhere under `crates/manifold-ui/src` without bumping
`COLOR_BASELINE` in `crates/manifold-ui/tests/design_tokens.rs` (or without using a
`// design-token-exempt:` comment for a genuine one-off). `git bisect`/`git log -S"Color32::new("`
over the file list the scan touches would localize it quickly; not run this session since it's
orthogonal to P6 and risked burning session budget chasing an unrelated one-line drift.

**Fix shape** — mechanical, one of: (a) find the extra raw literal and tokenize it (count back to
200, no baseline change), or (b) if it's a genuine one-off, add `// design-token-exempt: <reason>`
on that line (count back to 200), or (c) bump `COLOR_BASELINE` to 201 if it's accepted debt. Not
fixed this session — the gate confirms the diff at hand is P6-clean; picking apart an unrelated
pre-existing count belongs to whoever next touches `manifold-ui/src`'s colour call sites.

### BUG-025 — Timeline layer/header scissoring: clip content bleeds across row bounds — MED (repro needed)

**Symptom** — reported by Peter 2026-07-05 (screenshot in session transcript) as "layer and
header scissoring": in the arrangement view, the bottom layer's purple clip body renders far
beyond its row — a solid block filling the timeline from its row down to the window edge —
while the layer-header column at bottom-left shows the Plasma MIDI drawer (MIDI / CHANNEL /
DEVICE) overlapping into that region. Clip content and header-column content are not being
mutually clipped to their rows/panes.

**Root cause** — unknown. Suspect surface: the per-row scissor rect for clip bodies (last or
expanded row), the `track-header-invariant` / `single-source-y-layout` class, or a stale
subregion scissor (`subregion-scissor-invariant`). Likely same family as BUG-015 (inspector
sections at stale offsets) — both smell like Y-layout/scissor divergence after the recent
timeline waves.

**Repro** — not pinned; NOT reproduced headless (2026-07-05 Opus). Snapshotted the `states`
and `timeline` scenes (both carry a selected generator layer with an open MIDI/CHANNEL/DEVICE
drawer, the closest fixtures to Peter's screenshot) — both render correctly: every clip body is
scissored to its row, every header drawer stays in the left column, group nesting clips fine.
A scroll-down + re-snapshot on `timeline` also did not reproduce (and scroll may not be fully
wired in the headless tracks path). So the general scissoring path is sound; the bug is
state-specific. Triage narrows it to a config the fixtures don't hit — most likely the
*last* row being a selected generator whose clip fills the remaining viewport height, and/or a
live scroll offset. Pin it with either a targeted fixture (selected generator as the final
layer) or a running-app repro from Peter's project.

**Fix shape** — TBD after repro. If it's the invariant class (likely, given BUG-015 is the same
family), fix at the single Y-layout source, not per-widget patches.

### BUG-026 — Batch-2 popups: entrance fade freezes at t=0 (transparent bg) until an input re-dirties the frame — MED — FIX LANDED, running-app verification owed

**Symptom** — reported by Peter 2026-07-05 (before/after screenshots): opening the Add Effect
browser renders the search field, filter chips, and preset cells floating directly over the
timeline — the popup's dark background panel is missing. Moving the mouse over the popup makes
the background appear and it then looks correct.

**Root cause (FOUND)** — not the alpha math, a missing animation-poll in the dirty-driven
renderer. The batch-2 popups (browser / ableton picker / settings) run a D17 entrance tween:
`enter_anim` starts at `t=0` and, while `t<0.999`, `BrowserPopupPanel::build` multiplies the
modal container's background + border alpha by `t` (browser_popup.rs:451,469-474) — so frame 0
draws the panel fully transparent while the cells (opaque, not `t`-gated) float on top. The
tween is ticked inside each popup's `update()`, which only re-runs while the frame stays dirty.
The inspector drawer + panel-split tweens self-sustain via a `needs_rebuild` poll after
`UIRoot::update()` (app_render.rs ~2927), but the batch-2 popups were added to `update()` and
never to that poll. Opening a popup dirties exactly one frame (drawing it invisible); nothing
re-dirties it, so the fade freezes at `t=0` until an unrelated input (mouseover) re-dirties the
frame — the "no background until mouseover" symptom.

**Fix (LANDED)** — added `is_animating()` to each batch-2 popup and the matching poll in the
app motion block, mirroring `drawer_anim_active` exactly. Gate: clippy `-D warnings` clean;
`manifold-ui --lib` 604/604. Commit `01c15213` (branch `fix/popup-enter-anim`).

**Verification owed (L4)** — the headless `--script` driver has no frame loop and its
`enter_anim` ticks off wall-clock, so it cannot exercise this timing bug; a running-app check
(open the Add Effect browser, confirm the background is present immediately without moving the
mouse) is the remaining proof. Tracked in VERIFICATION_DEBT (VD-006).

### BUG-027 — Graph-editor node previews composite on the wrong z-layer vs. node chrome — MED — FIXED 2026-07-05

**Fix** — node previews now draw INLINE via a new `Painter::draw_image_uv` primitive, emitted by
`GraphCanvas::draw_node` right after each node's body, with each node pushed to its OWN increasing
depth band (`CONTENT+1+i`); the renderer's per-depth loop draws that band's rects then its image,
and a node stacked above (higher band) occludes a lower node's preview. Both flat post-pass blits
(live `app_render.rs`, headless `ui_snapshot/render.rs`) are deleted; the live path registers the
rotating atlas front via `UIRenderer::register_external_texture` + a per-cell UV, the harness
registers each node's output texture. Verified: a deterministic depth-band unit test
(`node_previews_render_in_per_node_depth_bands`) proves the occlusion ordering, and a Kaleidoscope
effect-graph PNG confirms real previews render inline correctly. Full default suite green.

---
_Original analysis (kept for the record):_

**Symptom** — reported by Peter 2026-07-05 (screenshot in session transcript): node preview
thumbnails overlap neighbouring nodes inconsistently — a preview (e.g. Luma to Color) draws
OVER another node's body/ports while that node's own chrome draws over the preview, so
stacking order disagrees within a single node pair. Previews look like they live on a
separate layer that ignores node z-order.

**Root cause** — KNOWN (2026-07-05 Opus, deeper read; the earlier "unknown" was wrong). The
node preview thumbnails are NOT part of the depth-ordered chrome render at all — they're a
SEPARATE flat blit pass issued AFTER the whole chrome is composited, in `visible_node_thumbnails`
order (no depth). Both paths do it identically:
- Live app: [app_render.rs](../crates/manifold-app/src/app_render.rs) clears the offscreen to the
  canvas bg (a `clear`, not a drawn rect), renders chrome + black preview-screen placeholders via
  the depth-ordered tree/canvas pass, presents to the drawable, then blits each node's atlas cell
  over the drawable in a final flat loop (~L3668).
- Headless harness: [ui_snapshot/render.rs](../crates/manifold-app/src/ui_snapshot/render.rs)
  `render_graph_to_png` does the same — chrome first, then a `ui-snap-graph-thumbs` blit loop over
  each node's output texture (~L228).
Because every thumbnail is painted after every node body, no node body can occlude a preview, and
a lower node's preview lands over a higher node's body. The reason it's a bolt-on post-pass: the
immediate-mode `Painter` trait (`draw.rs`) has rect/line/text primitives but **no textured-quad
primitive**, so previews couldn't be drawn inline with the node bodies and were blitted separately.

**Repro** — IS headless-reachable (the earlier entry said it wasn't — wrong). `render_graph_to_png`
reproduces the exact flat-blit bug; render two overlapping preview-emitting nodes and the lower
node's thumbnail draws over the higher node's body. That gives a before/after PNG to verify a fix.

**Fix shape** — depth-interleave the previews instead of post-blitting them: add a thumbnail-draw
primitive to the `Painter` trait, have `canvas.render` emit each node's preview inline right after
its body (so occlusion follows node draw order), route it through the existing depth-interleaved
Image pipeline in `ui_renderer.rs` (which already draws per-depth: rects, then images, then text —
needs the rotating node atlas bound + a per-cell UV subrect for the live path; the harness feeds
per-node output textures with full UV), and delete BOTH flat blit passes. Real immediate-mode
renderer change (Painter trait + UIRenderer + canvas render + both blit-pass deletions), but
headless-verifiable. Not a "patch the overlap cases" job.

### BUG-028 — File-drop targeting can't read the live pointer during a Finder drag (both AppKit poll sources frozen) — MED — FIXED 2026-07-05 (`wave/timeline-drop`, landed on main 2026-07-05; Peter's live-drag verification still owed)

**Symptom** — dragging an audio file onto an existing audio lane lands it on a NEW lane
instead of the target lane. Verified 2026-07-05 (Peter, live drag test).

**Root cause** — the `DroppedFile` arms in `app.rs` resolve their target from `cursor_pos`,
which winit freezes for the whole drag (its macOS backend implements no `draggingUpdated:`
and emits no `CursorMoved` during a drag session). Both AppKit poll fallbacks were live-tested
and are ALSO frozen during an NSDragging session: `mouseLocationOutsideOfEventStream` and
`+[NSEvent mouseLocation]` both returned byte-identical values across dozens of frames while
the pointer was actively moving. The poll site (`about_to_wait`) runs during the drag, so the
loop isn't starved — the position APIs simply don't update while macOS owns the drag. Polling
is a dead end.

**Fix (as built)** — `crates/manifold-app/src/drag_interpose.rs`: winit's macOS drag
destination is its `NSWindow`'s window delegate (not a view), and that delegate implements
`draggingEntered:`/`performDragOperation:`/etc. but NOT `draggingUpdated:`. At startup we
`class_addMethod` a fresh `draggingUpdated:` onto the delegate's class (returns
`NSDragOperationCopy`) and swizzle the existing `performDragOperation:` (so the drop position
is captured even if the pointer never moves again after entry), both stashing
`[sender draggingLocation]` — converted window-point → view-point (`convertPoint:fromView:nil`)
→ flipped to `cursor_pos`'s logical top-left convention — into a UI-thread-only cell. New
`crates/manifold-app/src/drag_hover.rs` (`DragHoverTracker`) wraps it; all three `DroppedFile`
arms (audio/MIDI, image, glTF) in `app.rs` now read
`drag_tracker.drop_position().unwrap_or(cursor_pos)`. P2 (drop-target ghost): a full-length
translucent preview clip renders on the target audio lane during the drag
(`app_render.rs`, reusing the existing `ClipBody`/`emit_clips`/ghost-alpha pipeline that
in-app clip-move drags already use); the "New lane: ⟨filename⟩" label and a discrete beat-line
for the non-audio-lane case were **not** built — no existing floating-text-over-viewport
primitive to reuse, out of scope for this pass. Overrides TIMELINE_INGEST_DESIGN §2 D1 (see
its §3 for the full poll-failure writeup, now superseded).

**Verification** — clean compile + clippy (`-D warnings`) + full `manifold-app` test suite,
plus 4 new unit tests for the coordinate flip (`drag_interpose::macos::tests`). The one thing
that can't be verified headless: whether `NSWindow` actually forwards `draggingUpdated:` to a
delegate that only gained the method at runtime (documented AppKit behavior, `respondsToSelector:`
is checked per-message — but only a live drag proves it). Gate: drag a Finder audio file over an
existing audio lane → joins that lane at the pointer's beat, ghost clip shows lane+length before
drop; an image drop lands under the pointer.

### BUG-032 — glTF import: a model with >2 materials fails to load ("unknown parameter 'pos_x_2'") and renders black — HIGH — FIXED 2026-07-05 (`dc97bbe6`)

> Id note: originally logged as BUG-029 (commit `dc97bbe6`, commit-message and
> the `prove-render-path` memory still say 029). A concurrent PARAM_STORAGE P2
> session independently used BUG-029 for the profiling-compile bug (still Open,
> above) and added BUG-030. To resolve the collision without splitting that
> open sequential pair, this closed entry was renumbered to BUG-032. The
> `dc97bbe6` commit reference is immutable history — this entry is canonical.

**Symptom** — Peter, 2026-07-05: importing `cc0__japanese_apricot_prunus_mume.glb` (4 distinct
materials) produced a black viewport and a repeating log flood: `Generator … failed to load from
def: graph load error: node 4 (node.render_scene): unknown parameter 'pos_x_2'` +
`Generator type … not found in the preset catalog`. Escaped: glTF wave / PRESET_LIBRARY P0 ·
caught-by: **held-out input in the running app** (the VD-003 mesh-snapshot render harness looked
green because it exercises `gltf::import` directly, NOT the production `PresetRuntime::from_def`
load path where the failure lives — a wrong-path verification, see VERIFICATION_DEBT VD-003).

**Root cause** — `node.render_scene` is the first primitive whose PARAM set (not just its ports)
grows with a reconfigure param: per-object transforms `pos_x_N`/`pos_y_N`/… exist only after the
node reconfigures to `objects >= N+1`. The def loader (`graph_loader::instantiate_def`)
snapshotted the declared param surface ONCE at the node's default 2-object count, then validated
every def param against that stale snapshot — so `pos_x_2` (object index 2, present for the
apricot's 4 objects) was rejected as unknown before the node ever reconfigured. The runtime calls
`node.reconfigure(&params)` after every build (graph.rs, snapshot.rs, freeze/region.rs); the
loader was the one path that didn't. mux_texture/multi_blend hid the gap because their reconfigure
grows PORTS (validated at wire time), not params; the azalea dev fixture hid it because it has
exactly 2 objects.

**Fix** — call `boxed.reconfigure(&doc_params)` before the `param_defs` snapshot in the loader
(mirrors snapshot.rs: seed declared defaults, override with doc values, reconfigure). No-op for
static-shape nodes; general across every reconfigure-param node. Verified on the REAL path: the
apricot `.glb` (4 objects) now loads clean through `PresetRuntime::from_def`. Regression tests:
`render_scene_with_three_objects_loads_per_object_transform_params` (synthetic, portable) +
`held_out_gltf_generator_loads_through_from_def` (`#[ignore]`, env-gated on a >2-material `.glb`).

### BUG-031 — Layer context-menu + rename still address layers positionally — LOW (follow-up to the LayerId migration `877852a9`)

**Root cause** — the primary layer-header actions were migrated to carry a stable `LayerId`
(commit `877852a9`, kills the panel-index-vs-live-model collision). Two related clusters were
deliberately left positional to keep that diff bounded:
- The **`Context*Layer` right-click-menu family** (`ContextPasteAtLayer`, `ContextImportMidi`,
  `ContextAddVideoLayer/GeneratorLayer/AudioLayer`, `ContextDuplicateLayer`, `ContextUngroup`,
  `ContextDeleteLayer`, `DropdownContext::LayerContext`) still carry a `usize`. `LayerHeaderRightClicked`
  now carries the id and `ui_root` resolves it to the current row synchronously when the menu opens,
  so there's no regression — but the menu ITEMS bake in that index, leaving a (rare) stale window
  between menu-open and item-click.
- **`TextInputField::LayerName(usize)`** (layer rename): the enum derives `Copy`, and `LayerId`
  isn't `Copy`, so migrating it forces dropping `Copy` and cascades through the whole text-input
  subsystem (`app.rs` field handling). The double-click intercept resolves id→index locally, so the
  rename has the same (unchanged) stale window it always had.

**Symptom** — none observed; latent. A context-menu action or a rename committed after the layer
list changed under it (another command, undo/redo, MIDI phantom layer) could hit the wrong layer.
Same bug class as the migration killed for the primary controls.

**Fix shape** — carry `LayerId` in the `Context*Layer` family (thread it from
`LayerHeaderRightClicked` through the menu items) and switch `TextInputField::LayerName` to
`LayerId` (drop `Copy` from `TextInputField`, fix the fallout in `app.rs`). Mechanical, compiler-driven.

## Fixed

All five entries below were fixed 2026-06-23, with a test per path:
- BUG-001–004 — commit `2e3dc4f3` (`PresetInstance::duplicated()`, both paste paths, `Clip::clone_with_new_id`, `Layer::clone_with_new_ids`).
- BUG-005 — commit `9f43f183` (macros address effects by `EffectId`; versioned load migration).

The fresh-copy carry-rule (id always fresh; drop Ableton/MIDI + audio mods; drop cross-chain group; keep drivers/envelopes) is settled and lives in `PresetInstance::duplicated()`.

### BUG-001 — Pasting an effect shares the source's `EffectId` — HIGH — ✅ FIXED (`2e3dc4f3`)

Copy/paste of an effect card clones the `PresetInstance` verbatim and keeps the original's
`EffectId`. Nothing mints a fresh id. The two cards then share one identity, and the whole
system addresses effects by id with **first-match-wins** resolution, so they collide.

**Root cause**
- Clipboard clones verbatim: [clipboard.rs:32-34](../crates/manifold-editing/src/clipboard.rs#L32-L34) (`get_paste_clones` is a bare `.clone()`; `.clone()` copies the `id` field).
- Paste path 1: [input_host.rs:263-273](../crates/manifold-app/src/input_host.rs#L263-L273) (`handle_effect_paste`) — feeds the clone to `AddEffectCommand`, no `regenerate_id()`.
- Paste path 2: [app_render.rs:1907-1918](../crates/manifold-app/src/app_render.rs#L1907-L1918) (PanelAction paste) — same omission.

**Symptom (user-visible)**
- Move a slider on one card → the other card's value moves too.
- Undo/redo of an edit to one card hits the other (or the wrong one).
- The two cards share GPU/visual state (feedback trails, sim buffers) — see blast radius below.

**Why each symptom happens**
- Edits resolve via `Project::find_effect_by_id_mut` ([project.rs:921-947](../crates/manifold-core/src/project.rs#L921-L947)) and `set_base_param_by_id` — first match by id wins, so card B's edit lands on card A.
- Undo/redo commands store an `EffectId` and re-resolve the same way.
- The renderer's per-frame chain rebuild `harvest_state_from` ([preset_runtime.rs:1667-1743](../crates/manifold-renderer/src/preset_runtime.rs#L1667-L1743)) matches cards by first-match `EffectId` (lines 1684, 1697-1701). Two same-id slots in one chain both match the *same* prior slot → GPU node impls + `StateStore` buckets migrate to the wrong/shared card.

**Correct pattern to mirror**
`Layer::clone_with_new_ids` already does this right — it calls `effect.regenerate_id()` on
every cloned effect ([layer.rs:886-900](../crates/manifold-core/src/layer.rs#L886-L900)).
`PresetInstance::regenerate_id` is at [effects.rs:1768](../crates/manifold-core/src/effects.rs#L1768).

**Fix shape**
Call `fx.regenerate_id()` before building the `AddEffectCommand` in both paste paths. Decide
the `group_id` question (see BUG-003) and the carried-binding question (see BUG-004) in the
same pass. Add a paste test mirroring the graph-node one.

**Test:** none yet. Add `effect_paste_assigns_fresh_id` to `manifold-editing`.

---

### BUG-002 — `Clip::clone_with_new_id` doesn't regenerate nested effect ids — MED — ✅ FIXED (`2e3dc4f3`)

Same class as BUG-001, one layer down. `Clip::clone_with_new_id` mints a fresh `ClipId` but
bare-`.clone()`s everything else, including `effects: Vec<PresetInstance>`
([clip.rs:105](../crates/manifold-core/src/clip.rs#L105)). So a duplicated clip's effects keep
the **source clip's** `EffectId`s. Clip effects share the same first-match namespace
([project.rs:938-944](../crates/manifold-core/src/project.rs#L938-L944)).

**Root cause**
[clip.rs:168-172](../crates/manifold-core/src/clip.rs#L168-L172) — shallow clone of nested effects.

**Every clip-duplication path inherits it** (all funnel through that one function):
- Paste clip — [service.rs:452](../crates/manifold-editing/src/service.rs#L452)
- Duplicate clip — [service.rs:740](../crates/manifold-editing/src/service.rs#L740)
- Split clip (overlap-driven + explicit) — [layer.rs:616](../crates/manifold-core/src/layer.rs#L616), [SplitClipCommand](../crates/manifold-editing/src/commands/clip.rs#L599)
- Trim / copy-in-region — [service.rs:628](../crates/manifold-editing/src/service.rs#L628)
- Duplicate layer — [layer.rs:871](../crates/manifold-core/src/layer.rs#L871) (clones clips, never touches their effect ids)

**Symptom**
Editing an effect on a duplicated/split clip crosstalks with the source clip's effect.
**Split is the surprising trigger** — a user doesn't think of splitting a clip as
"duplicating," but it produces two clips silently sharing effect ids.

**Scope note:** only bites clips that carry effects (effects usually sit on layers, so this is
the less-traveled path — hence MED, not HIGH). Renderer state does **not** collide across
clips: clip chains have distinct `OwnerKey` per clip ([state_store.rs:30-34](../crates/manifold-renderer/src/node_graph/state_store.rs#L30-L34)), so the model-layer collision is the whole bug here.

**Fix shape**
Make `Clip::clone_with_new_id` deep-regenerate `cloned.effects[*].id` (and clip-effect
`group_id` if any). One function fixes all six entry points, including the layer-dup gap.

**Test:** none yet. Add `clip_clone_assigns_fresh_effect_ids` to `manifold-core`.

---

### BUG-003 — Duplicating a grouped effect leaves `group_id` pointing at the source's group — LOW — ✅ FIXED (`2e3dc4f3`)

A pasted/duplicated effect keeps its `group_id`, which still references a group on the
**source's** chain. `Layer::clone_with_new_ids` remaps this for layer effects
([layer.rs:889-893](../crates/manifold-core/src/layer.rs#L889-L893)), but the effect-paste
path (BUG-001) and the clip-effect path (BUG-002) don't. Fixing BUG-001/002 by regenerating
ids must also decide the `group_id` remap, or you trade an id collision for a dangling group
ref.

**Status:** rolled into the BUG-001/BUG-002 fix; tracked separately so it isn't forgotten.

---

### BUG-004 — Effect paste carries Ableton/automation bindings; generator paste drops them — LOW — ✅ FIXED (`2e3dc4f3`)

Effect paste clones the whole `PresetInstance`, so `ableton_mappings`, `drivers`, `envelopes`,
and `audio_mods` all ride along — a pasted effect ends up mapped to the **same Ableton
control** as the source, and one knob drives both. Generator paste does the opposite: its
`GeneratorSnapshot` carries `drivers` + `envelopes` but **not** `ableton_mappings` or
`audio_mods` ([clipboard.rs:54-95](../crates/manifold-editing/src/clipboard.rs#L54-L95)).

This is an inconsistency, not strictly a crash. Per the effect/generator binding-parity
principle the two paste paths should agree. Decide the intended behavior (most DAWs do **not**
carry hardware/MIDI mappings onto a paste) and make both paths match.

**Status:** design decision to settle alongside BUG-001.

---

### BUG-005 — Macro targets can't disambiguate two same-type effects on one layer — LOW — ✅ FIXED (`9f43f183`)

`MacroMappingTarget` addresses an effect param by `(layer_id | master, effect_type, param_id)`
([macro_bank.rs:64-82](../crates/manifold-core/src/macro_bank.rs#L64-L82)) — **not** by
`EffectId`. So duplicating an effect (trivially producing two `Blur`s on one layer) makes any
macro mapping to that `(layer, Blur, param)` ambiguous; resolution can't tell the copies
apart. Distinct from the id-collision class (macros are immune to that because they don't key
on `EffectId`), but the same root trigger — duplication — exposes it.

**Fix shape:** address macro targets by stable `EffectId` like single-card edits already do
(`docs/CARD_TARGET_UNIFICATION.md`). Larger than a one-liner; parked here so it's recorded.

---

## Checked and safe (coverage proof)

Audited during the 2026-06-23 duplication sweep; these duplicate correctly. Recorded so the
audit boundary is auditable.

- **Graph-node copy/paste** — `PasteNodesCommand` ([graph.rs:1985-2110](../crates/manifold-editing/src/commands/graph.rs#L1985-L2110)) mints fresh runtime ids + fresh `NodeId`s, remaps internal wires, starts pasted nodes un-exposed. Has regression tests (`paste_node_clones_with_fresh_identity_and_undo_removes`, `paste_remaps_internal_wires_to_the_new_node_ids`). **This is the reference implementation** for the BUG-001/002 fixes.
- **Generator paste** — `PasteGeneratorCommand` overwrites the target layer's single generator in place, addressed by `LayerId`. No id minted, no collision.
- **Markers** — created fresh via `TimelineMarker::new` (fresh `MarkerId`, [marker.rs:20-27](../crates/manifold-core/src/marker.rs#L20-L27)); no copy/paste/duplicate-marker path exists (markers are timeline-level, untouched by layer/clip dup).
- **New-clip-from-scratch paths** (MIDI/percussion/live-trigger/browser-drop) — construct fresh clips, not duplicates of existing ones.

## Blast radius — id-keyed resolvers that a duplicate `EffectId` breaks

All first-match-wins; all used by both editing and undo/redo:
- `Project::find_effect_by_id_mut` — [project.rs:921-947](../crates/manifold-core/src/project.rs#L921-L947) (master + layer + clip effects)
- `Project::find_effect_by_id` — [project.rs:711](../crates/manifold-core/src/project.rs#L711)
- `GraphTarget::Effect` / `set_base_param_by_id` paths that wrap them
- Renderer chain rebuild `harvest_state_from` — [preset_runtime.rs:1667](../crates/manifold-renderer/src/preset_runtime.rs#L1667) (per-card GPU state migration)

**Not** in the blast radius: macros (`(layer, type, param)`-addressed — see BUG-005),
markers, generators (`LayerId`-addressed).

## The pattern behind all of this

Duplicating an id-bearing entity must mint a fresh identity for itself **and** every nested
id-bearing child, or id-keyed first-match resolution collides. The graph-node path enforces
this with a test and never regressed; the paths without a test (effect paste, clip clone)
did. The durable fix for the class is a test per duplication path, not a doc note.

Related agent-memory notes: `feedback_hidden_field_dependencies` (the mirror — removing a
field silently breaks identity), and `project_invariant_audit` (its "Positional identity"
category is marked *already fixed*; BUG-001/002 are live counterexamples — correct that claim
when one is fixed).

