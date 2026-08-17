# RT Quality Settings — per-project quality tiers for raytraced terms, live and export

**Status:** APPROVED design, not built · 2026-08-17 · k3 (lead), direction set with Peter in-session
**Prerequisites:** none
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs) before starting any phase.

Today the RT sample counts are compile-time constants in `render_scene.rs` (`AO_SAMPLES_PER_PIXEL = 4`, `GI_SAMPLES_PER_PIXEL = 4`, `REFL_SAMPLES_PER_PIXEL = 8`, shadow 1 spp) tunable only through env-var probes. This design makes them a per-project setting: a 5-row × 2-column grid — rows Shadows / AO / GI / Reflections (six spp tiers each) plus Ray Resolution (25/50/75/100%), columns Real-time and Export. Peter's directives from the design session: "ultra low is not off. That's a per scene config" (on/off stays on the graph bools; tiers only set sample counts); "maybe a scene level over-ride as a future feature" (deferred, D7); "this needs to be done properly… not a quick patch wiring job."

Companion docs: `docs/RAYTRACING_DESIGN.md` (the RT engine this tunes; spp committed ranges live there), `docs/MANIFOLD_GPU_ARCHITECTURE.md` (uniform/texture discipline), `docs/WIDGET_TREE_DESIGN.md` section 5b (Agent contract & enforcement — the panel's manifest machinery).

## 1. Audit — what exists (verified 2026-08-17)

| Piece | Where | State |
|---|---|---|
| spp constants | `crates/manifold-renderer/src/node_graph/primitives/render_scene.rs:225-284` | `AO=4`, `GI=4`, `REFL=8`, shadow mask `1` (render_scene.rs:5672) |
| spp GPU transport | `crates/manifold-gpu/src/metal/raytrace.rs:759-786` | runtime params struct (`shadow_spp`/`ao_spp`/`gi_spp`/`refl_spp` fields) — **uniforms, not codegen literals; per-frame change is safe** |
| Per-scene on/off bools | `render_scene.rs:4111-4125` (`rt_enabled`, `rt_shadows`, `rt_ao`, `rt_gi`, `rt_reflections` node params) | stays — gates whether a feature runs at all |
| Env probes to subsume | `MANIFOLD_RT_SWEEP_AO_SPP` / `_GI_SPP` / `_REFL_SPP` (render_scene.rs:5702-5711), `MANIFOLD_RT_NATIVE_TERMS` (render_scene.rs:1183-1191) | probe-only; this design replaces both |
| Project settings storage | `crates/manifold-core/src/settings.rs` (`ProjectSettings`, `#[serde(default)]` pattern) | extend |
| Settings commands | `crates/manifold-editing/src/commands/settings.rs` (`ChangeBpmCommand`, `ChangeResolutionCommand` precedent) | extend |
| Executor per-frame state pattern | `crates/manifold-renderer/src/node_graph/execution.rs:66` (`Executor`), setters at :445-558 (`set_preview_target` precedent); context construction at :1300 and :1658 via `EffectNodeContext::with_state` | extend — the ONLY two context construction sites |
| `EffectNodeContext` | `crates/manifold-renderer/src/node_graph/effect_node.rs:166` | extend with one field; `Option` fields are the established pattern for test-construction convenience (`errors`, :203) |
| Export-mode flag | `crates/manifold-playback/src/engine.rs:411` (`is_export_mode`), setter :667; set by `crates/manifold-app/src/content_export.rs:230` | reuse — selects the Export column |
| Export quality override precedent | `content_export.rs:232` — export already forces `render_scale = 1.0` | same idea, new axis |
| RT texture realloc + history reset | `render_scene.rs:2539-2578` (`ensure_rt_irradiance`, returns reset flag on dims change) + `TemporalResetDetector` (`node_graph/temporal_reset.rs`) | rides existing path — a ray-resolution change is a dims change |
| Settings panel surface | `crates/manifold-ui/src/panels/` (`audio_setup_panel.rs` precedent); manifest machinery `crates/manifold-ui/src/param_surface.rs` | new panel section |

Export renders through the **normal content-thread pipeline** — the frame loop lives in `manifold-app` (`export_session.rs:4-6` header comment), so `render_scene` runs identically in export; only `is_export_mode` differs. (Recon initially claimed otherwise; verified wrong at `content_export.rs:230-236`.)

## 2. Decisions

- **D1 — Project-level, not per-scene.** On/off is per-scene (graph bools) and stays there; tiers are a hardware-and-venue decision, one place for the whole show. Peter: "Would also be good to display the number of samples for each tier" — the UI shows "Medium (4 spp)".
- **D2 — Two columns, Real-time and Export, on one settings group.** `is_export_mode()` selects the column per frame. Export defaults are brute-force (offline frames are free); real-time defaults reproduce today's constants exactly so the show's look does not drift on upgrade.
- **D3 — Six shared tiers: Ultra Low / Low / Medium / High / Extra High / Ultra = 1/2/4/8/16/32 spp**, identical ladder for all four features. Per-feature cost differences (reflections are the expensive class, `render_scene.rs:278-283`) are expressed by which tier is the default, not by divergent ladders — one ladder keeps the UI honest and the model simple.
- **D4 — Ray Resolution is a separate row: 25% / 50% / 75% / 100%** (Peter: "this is how all games do it"). Applies to both RT dispatches (shadow mask + lighting). Replaces `MANIFOLD_RT_NATIVE_TERMS`; 100%/75%/25% graduate that probe path to runtime-supported.
- **D5 — Flow path: Executor per-frame setter → `EffectNodeContext` field.** The compositor (content thread, owns `Project`) resolves the active column once per frame and calls `executor.set_rt_quality(...)`. Follows the `set_preview_target` precedent exactly. **Plausible-wrong architecture, forbidden by name:** you will want to inject these as node params into `ctx.params` or into the graph JSON — no; params are per-node serialized graph state, these are project-level frame state. You will also want a global `static` or `Arc<RwLock>` the primitive reads — no; "no new shared state" (CLAUDE.md), and the content thread already owns both sides.
- **D6 — Change semantics: latched at frame boundary.** Settings are read once per frame by the compositor; a mid-frame UI edit takes effect next frame. The content thread owns `Project` and the executor — no race exists to handle. An spp change needs NO realloc and NO accumulation reset (dims unchanged; converged history keeps converging). A ray-resolution change IS a dims change and rides the existing `ensure_rt_irradiance` realloc + `TemporalResetDetector` reset — the executor adds nothing new here.
- **D7 — Scene-level override: deferred.** Revival trigger: Peter hits a show where one heavy GLB scene needs lower tiers than the rest. When revived, it lands as an optional per-`render_scene`-node param group that wins over the project column — never a third column.
- **D8 — Env probes deleted, not layered.** `MANIFOLD_RT_SWEEP_*` and `MANIFOLD_RT_NATIVE_TERMS` are subsumed by the panel. Keeping them as overrides on top is a second source of truth. The `rt_noise_gate.py` baseline is unaffected (it never sets them); any script that does gets updated in the same phase.

## 3. Data model (committed signatures)

`crates/manifold-core/src/settings.rs` (same file as `ProjectSettings`):

```rust
/// Six-step spp ladder, shared by all RT features (D3).
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RtQualityTier {
    UltraLow, Low, #[default] Medium, High, ExtraHigh, Ultra,
}

impl RtQualityTier {
    pub fn spp(self) -> u32 { /* 1, 2, 4, 8, 16, 32 */ }
    pub fn label(self) -> &'static str { /* "Ultra Low (1 spp)" etc. — UI shows counts, D1 */ }
}

/// RT dispatch resolution relative to native canvas (D4).
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RtRayResolution { Quarter, #[default] Half, ThreeQuarter, Native }

impl RtRayResolution {
    /// (numerator, denominator) — integer fraction, same discipline as
    /// `output_canvas_scale`'s (num, den) at render_scene.rs:284+.
    pub fn fraction(self) -> (u32, u32) { /* (1,4) (1,2) (3,4) (1,1) */ }
}

/// One column of the grid — the values one usage mode (live or export) runs.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RtQualityColumn {
    pub shadows: RtQualityTier,
    pub ao: RtQualityTier,
    pub gi: RtQualityTier,
    pub reflections: RtQualityTier,
    pub ray_resolution: RtRayResolution,
}

impl Default for RtQualityColumn { /* the LIVE default — today's constants:
    shadows UltraLow (1), ao/gi Medium (4), reflections High (8), ray Half */ }

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RtQualitySettings {
    pub realtime: RtQualityColumn,      // Default::default() — live values
    #[serde(default = "RtQualityColumn::export_default")]
    pub export: RtQualityColumn,        // shadows High, ao/gi High, reflections ExtraHigh, ray Native
}
```

`ProjectSettings` gains `#[serde(default)] pub rt_quality: RtQualitySettings`. Old projects deserialize to the live defaults = today's constants = byte-identical behavior (round-trip gated, P1).

Renderer side, resolved per frame (Copy, no alloc):

```rust
/// Resolved per-frame values the trace dispatch consumes. Lives in
/// manifold-renderer (node_graph/effect_node.rs next to FrameTime).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RtQuality {
    pub shadow_spp: u32, pub ao_spp: u32, pub gi_spp: u32, pub refl_spp: u32,
    pub ray_res_num: u32, pub ray_res_den: u32,
}
```

Seams:

- `Executor` gains `rt_quality: RtQuality` (default = live defaults) + `pub fn set_rt_quality(&mut self, q: RtQuality)` — shaped like `set_preview_target` (execution.rs:558).
- `EffectNodeContext` gains `pub rt_quality: RtQuality` — a plain value, not `Option`: the `with_state` constructor takes it from the executor's field; test construction paths use `RtQuality::default()` (live constants) so existing tests compile and behave unchanged.
- The compositor call site (wherever it already sets per-frame executor state before `execute_frame_*`) resolves `project.settings.rt_quality` column by `engine.is_export_mode()` → `RtQuality` → `set_rt_quality`. Exact call site named in P2's read-back; one site, found by `rg "set_preview_target" crates/manifold-renderer/src --type rust -l` minus the editor path.
- `render_scene.rs`: spp reads become `ctx.rt_quality.*_spp` gated by the existing node bools (bools still zero the spp — on/off unchanged); dispatch dims become `native_dim * num / den` with the same truncating integer arithmetic as `output_canvas_scale`. Constants `AO/GI/REFL_SAMPLES_PER_PIXEL` and both env probes deleted.

## 4. Invariants & enforcement

- **I1 — Live defaults are byte-identical to today's constants.** Enforcement: value test in manifold-core (`rt_quality_defaults_match_pre_change_constants`) asserting UltraLow=1/Medium=4/High=8 mapping plus `RtQualitySettings::default()` realtime column equality; P2's gpu-proofs run must show no drift report.
- **I2 — On/off stays graph-side; tiers never zero a feature.** `RtQualityTier::spp()` never returns 0. Enforcement: exhaustiveness + the value test asserting `spp() >= 1` for all variants.
- **I3 — Resolution changes reset temporal history.** Enforcement: existing `ensure_rt_irradiance` reset-flag path plus a gpu-proofs test flipping ray_resolution across frames and asserting the reset flag fired. P2 deliverable.
- **I4 — No env-probe second source of truth.** Enforcement: negative `rg` gate — `rg "MANIFOLD_RT_SWEEP|MANIFOLD_RT_NATIVE_TERMS" crates/ scripts/` returns zero hits after P2.
- **I5 — No per-frame allocation for the settings path.** `RtQuality` is `Copy`; `set_rt_quality` stores by value. Enforcement: code shape; `MANIFOLD_RENDER_TRACE=1` run in P2's gate (content-thread work gate, DESIGN_DOC_STANDARD.md section 5 (Phase briefs)).

## 5. Phasing

### P1 — Core model + serialization + command (lane: Flash Weak)

- **Entry state:** recon anchors re-verified: `settings.rs` holds `ProjectSettings` with `#[serde(default)]`; `commands/settings.rs` holds `ChangeBpmCommand`.
- **Read-back:** this doc's D1-D4, the data model in section 3 (Data model); `crates/manifold-core/src/settings.rs` whole; `ChangeBpmCommand` whole. Restate: the two defaults differ per column; serde rename convention matches the file's existing one — check before choosing.
- **Deliverables:** the four types above in `manifold-core/src/settings.rs`; `ProjectSettings.rt_quality` field; `ChangeRtQualityCommand` in `manifold-editing/src/commands/settings.rs` (whole-struct replace, shaped like `ChangeBpmCommand` — undo stores the prior `RtQualitySettings`); unit tests: serde round-trip, old-JSON-missing-field → defaults, `spp()` ladder values, live defaults = (1,4,4,8,½).
- **Gate:** `cargo nextest run -p manifold-core -p manifold-editing` green; `cargo clippy -p manifold-core -p manifold-editing -- -D warnings` clean. Round-trip gate: a saved project with non-default tiers reloads with them intact (test, not hand-wave).
- **Demo:** none — L1. **Test scope:** the two crates. **Shortcuts/confession fields mandatory in report.**
- **Forbidden moves:** touching the renderer; adding the UI; inventing a migration framework (serde defaults ARE the migration); per-feature divergent ladders.

### P2 — Renderer wiring (lane: Flash Strong — root-level RT code)

- **Entry state:** P1 merged into the branch. Re-verify anchors: execution.rs:1300/1658 are still the only `EffectNodeContext::with_state` sites (`rg -F "EffectNodeContext::with_state" crates/`); `ensure_rt_irradiance` still returns the reset flag.
- **Read-back:** D5, D6, I1-I5; `EffectNodeContext` fields (effect_node.rs:166-210); the `set_preview_target` precedent; render_scene.rs:2539-2578 (realloc), :4111-4125 (bools), :5660-5740 (dispatch config), :1183-1191 (native-terms probe). Restate the forbidden moves below before code.
- **Deliverables:** `RtQuality` struct; `Executor.rt_quality` + `set_rt_quality`; `EffectNodeContext.rt_quality` threaded at both construction sites; compositor call site resolves column by `is_export_mode()`; render_scene consumes `ctx.rt_quality` for all four spp values and both dispatch dims; constants and both env probes deleted; I3 gpu-proofs test (resolution flip → reset flag); existing gpu-proofs suite green.
- **⚠ VERIFY-AT-IMPL:** (a) atrous filter and MetalFX denoiser texture dims on a ray-resolution change — confirm they realloc through the same dims-change path, read `rt_irr_width/height` consumers; (b) whether the WGSL freeze path bakes any spp value — `rg "spp" crates/manifold-renderer/src/node_graph/freeze/` must return zero, verify don't recall; (c) the export audio-mod path (`content_export.rs:110-131`) renders frames identically regardless — no work, confirm only.
- **Gate:** `scripts/gpu_proofs_gate.py` green (mandatory — render_scene is the RT accumulation path); `cargo clippy -p manifold-renderer -p manifold-app -- -D warnings` clean; I4 negative rg gate zero hits; `MANIFOLD_RENDER_TRACE=1` run with a tier flip — no frame >20ms attributable to the settings path.
- **Acceptance demo:** headless render of `tests/fixtures/rt/RtEmissiveStrength.manifold` at default tiers → PNG byte-compared against main's output (threshold: identical or sub-1%-pixel diff, scripted pixel-diff with stated threshold — no agent eyeballs). Then the same render at Ultra/Native export column via a test harness override → PNG artifact for Peter (L2).
- **Performer gesture:** mid-show panic — flip every live tier to Ultra Low between frames while a heavy GLB scene plays; gate asserts no panic, no realloc storm (reset fires once per change), next frame reflects new spp.
- **Forbidden moves:** keeping the env probes as overrides; touching the graph bools; fusing this with any other render_scene cleanup; adapting a misfit call site instead of escalating; per-frame HashMap/String anywhere in the path.
- **Test scope:** manifold-renderer + manifold-app + gpu-proofs. Verify once at phase end.

### P3 — Settings panel UI (lane: Flash Weak)

- **Entry state:** P2 on branch; `ChangeRtQualityCommand` exists. Re-verify: `rg "ChangeRtQualityCommand" crates/manifold-editing/src` returns the command.
- **Read-back:** D1 (sample counts shown), the manifest-surface rule — `docs/WIDGET_TREE_DESIGN.md` section 5b (Agent contract & enforcement) and `crates/manifold-ui/src/param_surface.rs` module doc WHOLE; `audio_setup_panel.rs` as panel precedent. Restate: no bespoke row/dropdown infrastructure.
- **Deliverables:** an "RT Quality" section in the project settings surface: 5 rows × 2 dropdown columns, labels carrying sample counts ("Medium (4 spp)"), wired through `ContentCommand::Execute(ChangeRtQualityCommand)`. Headless UI PNG scene + a `scripts/ui-flows/` flow changing one dropdown and asserting the command fired (L3 target per DESIGN_DOC_STANDARD.md section 5 (Phase briefs)).
- **Gate:** `cargo nextest run -p manifold-ui` green; clippy `-p manifold-ui` clean; flow passes. Round-trip gate: change tiers → save project → reload → panel shows the saved tiers.
- **Demo:** `cargo xtask ui-snap` PNG of the panel (for Peter, L2) + the L3 flow. **Forbidden:** editing project state outside EditingService; bespoke dropdowns.

### P4 — Verify + land (lead, not a lane)

`scripts/landing_gate.py`, RT noise gate (`scripts/rt_noise_gate.py` — the accumulation path was touched), landing report per DESIGN_DOC_STANDARD.md section 8 (Execution protocol) rule 10, doc status flip, merge per `.claude/GIT_TREE_DISCIPLINE.md` section 2 (Landing protocol).

## 6. Decided — do not reopen

1. Project-level settings; per-scene override deferred (D1/D7).
2. Six shared tiers 1/2/4/8/16/32; no per-feature divergent ladders (D3).
3. Tiers set samples only; on/off stays on graph bools; Ultra Low is NOT off (Peter, verbatim).
4. Ray Resolution is its own row: 25/50/75/100% (D4).
5. Flow: Executor setter → context field; never node params, never globals (D5).
6. Frame-boundary latching; spp change = no reset, resolution change = existing realloc/reset path (D6).
7. Env probes deleted, not layered (D8).
8. Live defaults reproduce today's constants exactly; export defaults brute-force (D2).

## 7. Deferred

- **Scene-level per-node override** — trigger: a show where one scene's asset demands different tiers than the rest (D7).
- **Accumulation-length scaling per tier** (more accumulation frames at higher tiers, esp. export) — trigger: export stills showing residual noise at Extra High/Native. The alpha-floor machinery (`IRRADIANCE_ACCUM_ALPHA`, render_scene.rs:255-271) already exists; this would be a second consumer of the tier, not new plumbing.
- **25% ray resolution below the atrous filter's usefulness** — if 25% looks unusably smeary in practice, the tier stays (panic setting) but the denoiser interaction gets its own design note.
