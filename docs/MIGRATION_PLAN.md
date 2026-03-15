# MANIFOLD Rust Migration Plan — Comprehensive Multi-Phase Execution Plan

> **Date:** 2026-03-16
> **Supersedes:** `ROADMAP.md` (original 10-phase plan)
> **Input documents:**
> - `UNITY_PARITY_AUDIT.md` — every gap between Unity and Rust (the "what's wrong")
> - `INTERACTION_CONTRACT.md` — exact behavioral specification (the "how it must work")
> - `PORTING_STRATEGY.md` — testing and verification approach (the "how to verify")
> **Unity reference:** `Assets/Docs/USER_GUIDE.md` in the Unity project (canonical spec)
>
> **Guiding principle:** The Rust port must be pixel-identical and behavior-identical to the Unity version. No approximations, no "close enough."

---

## Current State (2026-03-16)

### What's Done
- Core data models (all types, enums, serialization) — **complete**
- Editing service + all commands (clip, layer, effect, driver, envelope, settings) — **complete**
- Undo/redo manager — **complete**
- Clipboard (clips + effects) — **complete**
- PlaybackEngine + ClipScheduler — **complete**
- LiveClipManager (phantom clip logic) — **complete** (not wired)
- SyncArbiter structure — **complete** (no implementations)
- wgpu GPU context, window, event loop — **complete**
- Custom bitmap UI system (UITree, UIInputSystem, panels) — **complete**
- All UI panels built (transport, header, footer, layer headers, inspector, viewport, dropdown) — **complete**
- Project loading (V1 JSON, V1.0→V1.1 migration) — **complete**

### What's Broken or Wrong (from Audit)
- Ruler height: 40px instead of 24px
- Track background color too bright (gray 36 vs 26)
- Default inspector width: 500px instead of ~280px
- Monitor button label: "Monitor" instead of "OUT"
- Insert cursor: 1px without triangle instead of 2px
- Clip vertical padding: 4px instead of 12px
- Primary text color has blue tint (230 vs 224)
- Inter font not loaded (uses system default)
- Startup creates test project instead of empty video layer
- Plasma generator param_defs don't match renderer indices

### What's Missing (Major Systems)
- 35 of 40 effects (GPU shaders + param definitions)
- 18 of 19 generators (GPU shaders + param definitions)
- All blend mode GPU implementations (13 modes)
- Driver/envelope runtime evaluation
- Video decode and playback
- MIDI input pipeline
- All sync sources (Link, MIDI Clock, OSC)
- Project save
- File dialogs (open/save)
- Text input fields
- Export pipeline
- LED/ArtNet/DMX output
- Audio/percussion pipeline
- 20+ missing keyboard shortcuts
- 15+ missing interaction behaviors
- Effect browser popup (search + categories)
- Performance HUD
- OSC remote control
- Toast notifications

---

## Phase Overview

```
Phase 0: Correctness Fixes ──────────────────── (immediate, ~1 day)
Phase 1: Interaction Test Harness ────────────── (~2-3 days)
Phase 2: Compositor + Blend Modes ────────────── (~1 week)
Phase 3: Generator Definitions + Shaders ─────── (~2-3 weeks)
Phase 4: Effect Definitions + Shaders ──────────  (~3-4 weeks)
Phase 5: Driver/Envelope Runtime ─────────────── (~1 week)
Phase 6: Missing UI Behaviors ────────────────── (~1-2 weeks)
Phase 7: File I/O + Text Input ───────────────── (~1 week)
Phase 8: Video Decode + Playback ─────────────── (~2-3 weeks)
Phase 9: MIDI + Sync Sources ─────────────────── (~1-2 weeks) [parallel with 8]
Phase 10: Export Pipeline ────────────────────── (~2 weeks)
Phase 11: LED/ArtNet + Audio/Percussion ──────── (~2-3 weeks) [parallel with 10]
Phase 12: Polish + Performance HUD + OSC ─────── (~1-2 weeks)
```

**Critical path:** 0 → 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8 → 10 → 12
**Parallel tracks:** Phase 9 with Phase 8; Phase 11 with Phase 10

---

## Phase 0: Correctness Fixes

**Goal:** Fix every known deviation identified in `UNITY_PARITY_AUDIT.md` that requires only constant/config changes (no new systems).

**Duration:** ~1 day

### 0.1 Fix Layout Constants

All changes in `crates/manifold-ui/src/color.rs` unless noted:

| Fix | File | Change |
|-----|------|--------|
| Ruler height | `color.rs` | `RULER_HEIGHT: 40.0 → 24.0` |
| Clip vertical padding | `panels/viewport.rs` | `CLIP_VERTICAL_PAD: 4.0 → 12.0` |
| Default inspector width | `layout.rs` | `inspector_width: 500.0 → 280.0` in `ScreenLayout::new()` |
| Monitor button label | `panels/header.rs` | `"Monitor" → "OUT"` |
| Insert cursor width | `panels/viewport.rs` | `INSERT_CURSOR_WIDTH: 1.0 → 2.0` |

### 0.2 Fix Colors

| Fix | File | Change |
|-----|------|--------|
| Track background | `color.rs` | `TRACK_BG: (36,36,37) → (26,26,27)` |
| Track alt background | `color.rs` | `TRACK_BG_ALT: (27,27,28) → (20,20,21)` (proportional) |
| Primary text color | `color.rs` | `TEXT_NORMAL: (224,224,230) → (224,224,224)` |
| Primary text C32 | `color.rs` | `TEXT_PRIMARY_C32: (224,224,230) → (224,224,224)` |
| Export marker green | `color.rs` | `EXPORT_MARKER_COLOR green: 140 → 141` |

### 0.3 Fix Startup

In `crates/manifold-app/src/app.rs`, replace the test project creation:

**Current:** Creates a generator layer with a Plasma clip at beat 0, duration 10000, starts Playing.
**Required:** Create one empty video layer named "Layer 1", no clips, stopped at beat 0, show MasterInspector.

### 0.4 Fix Plasma Param Mismatch

In `crates/manifold-core/src/types.rs`, update `GeneratorType::Plasma` `param_defs()`:
- Must return 5 params matching the renderer's index order: Pattern, Complexity, Contrast, Speed, Scale
- Read Unity's `GeneratorDefinitionRegistry.cs` for exact names, ranges, defaults

### 0.5 Load Inter Font

In `crates/manifold-renderer/src/ui_renderer.rs` or font loading code:
- Bundle `Inter-Regular.ttf` as a compiled-in resource
- Load into glyphon font database at startup
- Use as the default font for all text rendering

### 0.6 Write Parity Tests

Create `crates/manifold-ui/tests/parity_constants.rs`:

```rust
#[test]
fn test_layout_constants_match_unity() {
    assert_eq!(RULER_HEIGHT, 24.0);
    assert_eq!(TRACK_HEIGHT, 140.0);
    assert_eq!(TRANSPORT_BAR_HEIGHT, 36.0);
    assert_eq!(HEADER_HEIGHT, 40.0);
    assert_eq!(FOOTER_HEIGHT, 29.0);
    assert_eq!(LAYER_CONTROLS_WIDTH, 200.0);
    assert_eq!(MIN_INSPECTOR_WIDTH, 196.0);
    assert_eq!(MAX_INSPECTOR_WIDTH, 500.0);
    // ... every dimension from UNITY_PARITY_AUDIT.md
}

#[test]
fn test_color_constants_match_unity() {
    assert_eq!(PLAY_ACTIVE, Color32::new(64, 184, 82, 255));
    assert_eq!(STOP_RED, Color32::new(128, 51, 51, 255));
    assert_eq!(ACCENT_BLUE, Color32::new(89, 148, 235, 255));
    // ... every color from UNITY_PARITY_AUDIT.md §32
}
```

**Exit criteria:** All constant tests pass. Visual inspection confirms correct ruler height, track colors, text color, font, and startup behavior.

---

## Phase 1: Interaction Test Harness

**Goal:** Build a test framework that can verify interaction behaviors from `INTERACTION_CONTRACT.md` without a window or GPU. Extract interaction logic into pure testable functions.

**Duration:** ~2-3 days

### 1.1 Extract Magnetic Snap as Pure Function

Create `crates/manifold-ui/src/snap.rs`:

```rust
pub fn magnetic_snap_beat(
    raw_beat: f32,
    grid_interval: f32,
    neighbor_edges: &[f32],
    ppb: f32,
    snap_threshold_px: f32,  // 12.0
    max_snap_beats: f32,     // 0.5
) -> f32;

pub fn grid_interval_for_zoom(ppb: f32, beats_per_bar: f32) -> f32;
```

### 1.2 Extract Selection State Machine

Create `crates/manifold-ui/src/selection_state.rs`:

```rust
pub struct SelectionState {
    pub selected_clip_ids: HashSet<String>,
    pub primary_clip_id: Option<String>,
    pub primary_layer_index: Option<usize>,
    pub selected_layer_ids: HashSet<String>,
    pub region: SelectionRegion,
    pub insert_cursor: Option<InsertCursor>,
    pub version: u64,
}

impl SelectionState {
    pub fn select_clip(&mut self, clip_id: &str, layer: usize);
    pub fn toggle_clip(&mut self, clip_id: &str, layer: usize);
    pub fn clear(&mut self);
    pub fn set_insert_cursor(&mut self, beat: f32, layer: usize);
    pub fn set_region(&mut self, region: SelectionRegion);
    // Each method enforces mutual exclusivity
}
```

### 1.3 Extract Trim Logic as Pure Functions

Create `crates/manifold-ui/src/trim.rs`:

```rust
pub struct TrimResult {
    pub new_start_beat: f32,
    pub new_duration_beats: f32,
    pub new_in_point: f32,
}

pub fn compute_left_trim(
    mouse_beat: f32,
    original_start: f32,
    original_end: f32,
    original_in_point: f32,
    spb: f32,
    is_generator: bool,
    min_duration: f32,  // 0.25
) -> TrimResult;

pub fn compute_right_trim(
    mouse_beat: f32,
    clip_start: f32,
    original_in_point: f32,
    spb: f32,
    is_generator: bool,
    is_looping: bool,
    video_length: Option<f32>,
    min_duration: f32,
) -> TrimResult;
```

### 1.4 Extract Overlap Enforcement as Pure Function

Already exists in `manifold-editing/src/service.rs` as `enforce_non_overlap`. Add comprehensive tests:

```rust
#[test] fn overlap_full_cover_deletes_existing() { ... }
#[test] fn overlap_cover_start_trims_existing() { ... }
#[test] fn overlap_cover_end_trims_existing() { ... }
#[test] fn overlap_middle_splits_existing() { ... }
#[test] fn no_overlap_produces_no_commands() { ... }
```

### 1.5 Extract Arrow Key Navigation

Create `crates/manifold-ui/src/cursor_nav.rs`:

```rust
pub enum NavResult {
    SelectClip(String),      // auto-selected a clip at position
    SetCursor(f32, usize),   // set insert cursor
    NoChange,                // at boundary
}

pub fn navigate_cursor(
    direction: Direction,
    current_beat: f32,
    current_layer: usize,
    grid_interval: f32,
    is_fine: bool,           // Shift held → 1/16 beat
    layers: &[LayerInfo],    // heights for skipping collapsed
    clips: &[ClipInfo],      // for auto-select
) -> NavResult;
```

### 1.6 Write Interaction Tests

Create `crates/manifold-ui/tests/interactions.rs` with tests from `INTERACTION_CONTRACT.md` Appendix:

```rust
#[test] fn clip_move_preserves_offset() { ... }
#[test] fn clip_trim_minimum_duration() { ... }
#[test] fn magnetic_snap_to_neighbor_edge() { ... }
#[test] fn magnetic_snap_to_grid() { ... }
#[test] fn magnetic_snap_threshold_in_pixels() { ... }
#[test] fn video_left_trim_clamp() { ... }
#[test] fn generator_left_trim_extend() { ... }
#[test] fn cross_layer_type_block() { ... }
#[test] fn escape_priority_chain_menu_first() { ... }
#[test] fn escape_priority_chain_inspector_second() { ... }
#[test] fn cmd_c_context_sensitive_inspector() { ... }
#[test] fn cmd_c_context_sensitive_timeline() { ... }
#[test] fn slider_right_click_reset() { ... }
#[test] fn insert_cursor_arrow_autoselect() { ... }
#[test] fn selection_mutual_exclusivity() { ... }
#[test] fn region_partial_overlap_inclusion() { ... }
#[test] fn grid_interval_at_zoom_levels() { ... }
#[test] fn quarter_note_snap_loop_slider() { ... }
```

**Exit criteria:** All interaction tests pass. Pure functions are extracted and tested independently from UI panels.

---

## Phase 2: Compositor + Blend Modes

**Goal:** Complete GPU compositor with all 13 blend modes rendering correctly.

**Duration:** ~1 week

### 2.1 Implement Blend Mode Shaders

Create WGSL shaders for each blend mode. Reference: Unity's `BlendMaterial.shader`.

```
crates/manifold-renderer/src/shaders/
  blend_normal.wgsl
  blend_additive.wgsl
  blend_multiply.wgsl
  blend_screen.wgsl
  blend_overlay.wgsl
  blend_stencil.wgsl
  blend_opaque.wgsl
  blend_difference.wgsl
  blend_exclusion.wgsl
  blend_subtract.wgsl
  blend_color_dodge.wgsl
  blend_lighten.wgsl
  blend_darken.wgsl
```

### 2.2 Implement BlendMaterialCache Equivalent

Create `crates/manifold-renderer/src/blend.rs`:
- Cache wgpu RenderPipelines per blend mode
- Each pipeline reads source texture + blend texture, outputs blended result
- Lazy initialization on first use

### 2.3 Fix LayerCompositor

Update `crates/manifold-renderer/src/layer_compositor.rs`:
- Apply layer blend mode when compositing each layer
- Support per-layer opacity
- Correct compositing order (higher indices = base, lower = top)

### 2.4 Fix copy_texture_to_texture

In `crates/manifold-renderer/src/effect_chain.rs`:
- Replace the clear stub with a real texture-to-texture copy
- Use a simple blit pipeline or `encoder.copy_texture_to_texture()`

### 2.5 Implement Wet/Dry Lerp Shader

Create `crates/manifold-renderer/src/shaders/wet_dry_lerp.wgsl`:
- Simple `lerp(dry, wet, amount)` fragment shader
- Wire into EffectChain's group processing

### 2.6 Implement ACES Tonemapping

Create `crates/manifold-renderer/src/shaders/aces_tonemap.wgsl`:
- ACES filmic tonemap for HDR → SDR
- Optional PQ encoding for HDR10 output

**Exit criteria:** Load a project with multiple layers using different blend modes. All 13 modes render correctly. Group wet/dry blend works. Effect chain input copy works.

---

## Phase 3: Generator Definitions + Shaders

**Goal:** All 19 generators fully defined (correct param names, ranges, defaults) and rendering via GPU shaders.

**Duration:** ~2-3 weeks

### 3.1 Complete All Generator Param Definitions

Read Unity's `Assets/Scripts/Data/GeneratorDefinitionRegistry.cs` and update every `GeneratorType::param_defs()` in `crates/manifold-core/src/types.rs`.

**Must match exactly:** param name, min, max, default, wholeNumbers flag, param index mapping.

All 19 generators:

| Priority | Generator | Category | Shader Complexity |
|----------|-----------|----------|-------------------|
| 1 | Plasma | Fragment | Simple (already done, fix params) |
| 2 | Basic Shapes Snap | Fragment | Simple |
| 3 | Concentric Tunnel | Fragment | Simple |
| 4 | Fractal Zoom | Fragment | Medium (Mandelbrot/Julia) |
| 5 | Number Station | Fragment | Medium |
| 6 | Tesseract | Vertex+Fragment | Medium (4D projection) |
| 7 | Duocylinder | Vertex+Fragment | Medium |
| 8 | Lissajous | Vertex+Fragment | Simple |
| 9 | Wireframe Zoo | Vertex+Fragment | Medium (5 polyhedra) |
| 10 | Oscilloscope XY | Vertex+Fragment | Medium |
| 11 | Reaction-Diffusion | Compute | Complex (ping-pong sim) |
| 12 | Flowfield | Compute | Complex (particle system) |
| 13 | Mycelium | Compute | Complex (physarum) |
| 14 | Fluid Simulation | Compute | Complex (2D Navier-Stokes) |
| 15 | Fluid Simulation 3D | Compute | Very Complex (3D volume) |
| 16 | Parametric Surface | Compute | Complex (5 surfaces) |
| 17 | Strange Attractor | Compute | Complex (5 attractors) |
| 18 | Strange Attractor GPU | Compute | Complex (compute version) |

### 3.2 Port Generator Shaders (HLSL → WGSL)

For each generator, reference the Unity shader file and translate to WGSL:

```
Unity: Assets/Scripts/Generators/{Name}Generator.cs + Assets/Resources/Shaders/Gen_{Name}.shader
Rust:  crates/manifold-renderer/src/generators/{name}.rs + shaders/gen_{name}.wgsl
```

**Shared infrastructure:**
- `GeneratorParticleSplat.shader` → `gen_particle_splat.wgsl` (shared by ALL particle generators)
- ComputeShaderCache equivalent for wgpu compute pipelines
- Ping-pong render target management for stateful generators

### 3.3 Update GeneratorRegistry

In `crates/manifold-renderer/src/generators/registry.rs`:
- Map every `GeneratorType` to its concrete `Generator` implementation
- Remove the "log warning and return None" fallback

### 3.4 Write Generator Param Tests

```rust
#[test]
fn all_generators_have_param_defs() {
    for gen_type in GeneratorType::ALL {
        let defs = gen_type.param_defs();
        assert!(!defs.is_empty(), "{:?} has no param definitions", gen_type);
    }
}

#[test]
fn generator_param_count_matches_unity() {
    // From GeneratorDefinitionRegistry.cs
    assert_eq!(GeneratorType::Plasma.param_defs().len(), 5);
    assert_eq!(GeneratorType::FluidSimulation3D.param_defs().len(), 26);
    assert_eq!(GeneratorType::Mycelium.param_defs().len(), 12);
    // ... all 19
}
```

**Exit criteria:** All 19 generator types render. Every param slider matches Unity's name, range, and default. Loading a generator-only project shows all generators correctly.

---

## Phase 4: Effect Definitions + Shaders

**Goal:** All 40 effects fully defined (correct params) and rendering via GPU shaders.

**Duration:** ~3-4 weeks

### 4.1 Complete All Effect Param Definitions

Read Unity's `Assets/Scripts/Data/EffectDefinitionRegistry.cs` and update every `EffectType::param_defs()` in `crates/manifold-core/src/types.rs`.

**Must match exactly:** param name, min, max, default, wholeNumbers flag, format function.

Prioritize by category:

| Priority | Effect | Params | Shader Complexity |
|----------|--------|--------|-------------------|
| 1 | Fix ColorGrade (4→9 params) | 9 | Already implemented, extend |
| 2 | Fix Mirror (add Amount) | 2 | Already implemented, extend |
| 3 | Fix Bloom (match Unity params) | 1+ | Already implemented, align |
| 4 | Fix InvertColors (name Amount) | 1 | Already implemented, rename |
| 5 | Pixel Sort | 2+ | Compute (bitonic sort) |
| 6 | Kaleidoscope | 2+ | Fragment |
| 7 | Edge Stretch | 3+ | Fragment |
| 8 | Voronoi Prism | 2+ | Fragment |
| 9 | Quad Mirror | 1+ | Fragment |
| 10 | Dither (6 algorithms) | 2 | Fragment |
| 11 | Strobe (3 modes) | 3 | Fragment |
| 12 | Stylized Feedback | 4+ | Stateful |
| 13 | CRT | 5 | Fragment |
| 14 | Chromatic Aberration | 4 | Fragment |
| 15 | Glitch | 4 | Fragment |
| 16 | Film Grain | 3 | Fragment |
| 17 | Halation | 4 | Fragment |
| 18 | Slit Scan | 3 | Stateful |
| 19 | Datamosh | 2 | Stateful |
| 20 | Infinite Zoom | 2+ | Stateful |
| 21 | Fluid Distortion | 4+ | Compute (2D Navier-Stokes) |
| 22 | Edge Glow (3 modes) | 3+ | Fragment |
| 23 | Gradient Map | 7 | Fragment |
| 24 | Blob Tracking | 4+ | Compute |
| 25 | Wireframe Depth | 14 | Compute + Fragment |
| 26 | Microscope | 11 | Fragment |
| 27 | Corruption | 6 | Fragment |
| 28 | Infrared (10 palettes) | 6 | Fragment |
| 29 | Surveillance | 7 | Fragment |
| 30 | Redaction (3 styles) | 7 | Fragment |

### 4.2 Port Effect Shaders (HLSL → WGSL)

For each effect, reference the Unity shader and translate:

```
Unity: Assets/Scripts/Compositing/Effects/{Name}FX.cs + Assets/Resources/Shaders/FX_{Name}.shader
Rust:  crates/manifold-renderer/src/effects/{name}.rs + shaders/fx_{name}.wgsl
```

**Stateful effects require special handling:**
- Feedback, Stylized Feedback: temporal buffer (ping-pong RT)
- Slit Scan: temporal line buffer
- Datamosh: frame hold/blend buffer
- Infinite Zoom: recursive zoom buffer
- Fluid Distortion: 2D Navier-Stokes simulation buffers

### 4.3 Investigate Missing Effect IDs (2-9)

The Rust enum skips IDs 2-9. Determine whether Unity has effects at these IDs:
- Check Unity `EffectDefinitionRegistry.cs` for all registered effect types
- If effects exist at IDs 2-9 in Unity, add them to the Rust enum
- If they are legacy/removed, document why

### 4.4 Write Effect Param Tests

```rust
#[test]
fn all_effects_have_param_defs() {
    for fx_type in EffectType::ALL {
        let defs = fx_type.param_defs();
        assert!(!defs.is_empty(), "{:?} has no param definitions", fx_type);
    }
}

#[test]
fn effect_param_count_matches_unity() {
    assert_eq!(EffectType::ColorGrade.param_defs().len(), 9);
    assert_eq!(EffectType::WireframeDepth.param_defs().len(), 14);
    assert_eq!(EffectType::Microscope.param_defs().len(), 11);
    // ... all 40
}

#[test]
fn param0_is_always_amount() {
    for fx_type in EffectType::ALL {
        if fx_type == EffectType::Transform { continue; }
        let defs = fx_type.param_defs();
        assert_eq!(defs[0].name, "Amount", "{:?} param0 should be Amount", fx_type);
    }
}
```

**Exit criteria:** All 40 effects render correctly. Every param slider matches Unity. Effect groups with wet/dry blend work. param0 = Amount for all non-Transform effects.

---

## Phase 5: Driver/Envelope Runtime Evaluation

**Goal:** LFO drivers and ADSR envelopes modulate effect and generator parameters in real-time during playback.

**Duration:** ~1 week

### 5.1 Implement Driver Evaluator

Create `crates/manifold-playback/src/driver_evaluator.rs`:

```rust
pub fn evaluate_driver(
    driver: &ParameterDriver,
    current_beat: f32,
    bpm: f32,
) -> f32;
```

Waveform evaluation:
- Sine: `sin(phase * 2π) * 0.5 + 0.5`
- Triangle: `1 - abs(fract(phase) * 2 - 1)`
- Sawtooth: `fract(phase)`
- Square: `if fract(phase) < 0.5 { 0 } else { 1 }`
- Random: seeded from floor(phase), stable per period

Phase calculation: `phase = (current_beat / beat_division_beats) + driver.phase`

Trim mapping: `output = trim_min + output * (trim_max - trim_min)`

Reverse: `if reversed { 1.0 - output } else { output }`

### 5.2 Implement Envelope Evaluator

Create `crates/manifold-playback/src/envelope_evaluator.rs`:

```rust
pub fn evaluate_envelope(
    envelope: &ParamEnvelope,
    clip_local_beat: f32,  // beat relative to clip start
    clip_duration: f32,
) -> f32;
```

ADSR phases relative to clip start:
- Attack: 0 → attack_beats (ramp 0→1)
- Decay: attack_beats → attack_beats + decay_beats (ramp 1→sustain)
- Sustain: after decay, until clip_duration - release_beats (hold at sustain)
- Release: last release_beats (ramp sustain→0)

Output = `envelope_value * target_normalized`

### 5.3 Apply Modulation in Compositor

In the compositor's per-clip and per-layer processing:
1. For each effect with drivers: evaluate each driver, add to base param value
2. For each effect with envelopes: evaluate each envelope, multiply with param value
3. Pass modulated param values to the effect shader

### 5.4 Driver Pause During Manual Drag

When a slider drag begins on a parameter with an active driver:
- Set a flag to pause the driver for that parameter
- On drag end: unpause

This prevents the LFO from fighting the user's manual adjustment.

### 5.5 Visual Feedback on Modulated Sliders

When a parameter has an active driver:
- Show base value position (gray) on the slider track
- Show current modulated value position (colored thumb) — updates per frame

**Exit criteria:** Load a project with LFO-driven effects. Parameters visibly modulate in sync with beats. Envelopes ramp parameters during clip playback. Manual slider drag pauses driver.

---

## Phase 6: Missing UI Behaviors

**Goal:** Implement every missing interaction behavior from `INTERACTION_CONTRACT.md` §30.

**Duration:** ~1-2 weeks

### 6.1 Keyboard Shortcuts

Wire all missing shortcuts from `INTERACTION_CONTRACT.md` §28:

| Shortcut | Action | Priority |
|----------|--------|----------|
| Home | Seek to beat 0 | High |
| End | Seek to end of timeline | High |
| F | Zoom to fit (10% padding) | High |
| Shift+Arrow | Fine nudge (1/16 beat) | High |
| Up/Down Arrow | Layer navigation with auto-select | High |
| Cmd+D | Duplicate selected clips | High |
| Cmd+G | Group (context-sensitive) | Medium |
| Cmd+Shift+G | Ungroup (context-sensitive) | Medium |
| I / O | Set export in/out markers | Medium |
| Alt+I / Alt+O | Clear export markers | Medium |
| ` (backtick) | Toggle Performance HUD | Low (Phase 12) |
| Cmd+Shift+I | Import percussion | Low (Phase 11) |

### 6.2 Context-Sensitive Shortcuts

Implement `inspector_has_focus` tracking:
- Set to true when user clicks within inspector panel
- Set to false when user clicks in timeline viewport
- Route Cmd+C/X/V/Delete/G based on this flag

### 6.3 Escape Priority Chain

Implement 4-level priority:
1. If dropdown/context menu open → dismiss
2. If monitor output active → no-op (or close monitor)
3. If inspector has focus → clear effect selection, clear focus
4. Otherwise → clear all selection + insert cursor

### 6.4 Neighbor Clip Edge Snap

Update viewport clip drag to use `magnetic_snap_beat()` from Phase 1:
- Collect start/end beats of all clips on same layer (excluding self)
- Pass as neighbor_edges to snap function
- 12px threshold, 0.5 beat cap

### 6.5 Cursor Changes

Implement cursor system:
- `SetDefault()` on empty space
- `SetResizeHorizontal()` on trim handle hover/drag
- `SetMove()` on clip body hover/drag
- `SetBlocked()` on incompatible cross-layer drag

Use `winit::window::Window::set_cursor()` with `CursorIcon` variants.

### 6.6 Shift+Click Range Select

On clips: extend selection from anchor (primary selected clip) to clicked clip.
On empty space: extend region from anchor to clicked position.

### 6.7 Auto-Scroll During Playback

- Trigger: playhead within 50px of right viewport edge
- Target: scroll so playhead is at 25% from left

### 6.8 Focus Loss Drag Cancel

On `winit::event::WindowEvent::Focused(false)`:
- Synthesize PointerUp event
- Cancel any in-progress drag

### 6.9 Missing Context Menu Items

Add to layer context menu:
- Paste (when clipboard has content)
- Import MIDI File
- Group Selected Layers (when 2+ layers selected)
- Ungroup (when clicked layer is a group)
- Delete Layer (when >1 layer exists)

### 6.10 Multi-Select Inspector

When multiple clips are selected:
- Show ClipInspector with common properties
- Properties that differ across selected clips show `*` placeholder
- Changing a value applies to all selected clips

### 6.11 Region Operations

Implement region copy/cut/delete:
- Split straddling clips at region boundaries
- Interior segments are the operation targets
- For paste: place pattern at insert cursor with gap preservation

### 6.12 Effect Browser Popup

Replace flat dropdown with proper browser popup:
- 480px wide grid, max 440px tall
- Search bar with real-time filtering
- Category chips (Spatial, Post-Process, Filmic, Surveillance, etc.)
- Click adds effect to end of rack

**Exit criteria:** Every interaction test from Phase 1 passes. All keyboard shortcuts work. Effect browser has search and categories. Region operations work. Inspector shows multi-select correctly.

---

## Phase 7: File I/O + Text Input

**Goal:** Users can save projects, open files via dialog, and edit text fields (BPM, FPS, layer names).

**Duration:** ~1 week

### 7.1 Native File Dialogs

Add `rfd` (Rust File Dialog) crate:
- Open: `.manifold` and `.json` filter
- Save As: `.manifold` filter
- Folder picker for video library paths

### 7.2 Project Save

In `crates/manifold-io/src/`:
- `save_project(project: &Project, path: &Path) -> Result<()>`
- Serialize to JSON (matching Unity's camelCase field names)
- For `.manifold` (V2): write JSON into ZIP archive
- Update `saved_at_version` in EditingService

### 7.3 Text Input System

Create `crates/manifold-ui/src/text_input.rs`:
- Overlay a native text field at the correct screen position
- Commit on Enter/focus loss
- Cancel on Escape
- Suppress all other keyboard input while active

Wire to:
- BPM field (valid range 20-300)
- FPS field
- Layer name (double-click)
- Effect param value text (click on value label)
- Rack name (double-click)

### 7.4 Open Recent

Store last opened project path in a config file (`~/.config/manifold/recent.json`).

### 7.5 Wire File Operations

Connect transport bar buttons:
- NEW → create blank project
- OPEN → show file dialog → load
- OPEN RECENT → load from stored path
- SAVE → save to existing path (or Save As if new)
- SAVE AS → show save dialog → save

### 7.6 Drag-and-Drop from OS

Handle `winit::event::WindowEvent::DroppedFile(path)`:
- `.manifold` / `.json` → load project
- `.mp4` / `.mov` / `.webm` / `.avi` → create clips at cursor position (Phase 8)
- `.mid` / `.midi` → import MIDI notes to layer (Phase 9)

**Exit criteria:** Can save and reload a project. BPM, FPS, and layer names are editable via text input. File dialogs work on macOS.

---

## Phase 8: Video Decode + Playback

**Goal:** Load and play back video clips on the timeline alongside generators.

**Duration:** ~2-3 weeks

### 8.1 Video Decoder

Add `ffmpeg-next` crate. Create `crates/manifold-video/`:
- `VideoDecoder`: opens video file, seeks to time, decodes frames to RGBA
- `VideoPlayerPool`: pre-allocated pool of decoders (default 10)
- Pending pause pattern: Play briefly for decoder init, Pause after 40ms
- Recently-started exclusion: 50ms delay before compositor includes new player

### 8.2 Video ClipRenderer

Implement `ClipRenderer` trait for video:
- `start_clip`: acquire decoder from pool, seek to InPoint
- `stop_clip`: return decoder to pool
- `pre_render`: decode frame, upload to GPU texture via `wgpu::Queue::write_texture()`
- Handle looping, playback rate, seeking

### 8.3 Video Library

- Scan `VideoFolderPath` directories for supported formats
- Cache metadata (duration, resolution, file size)
- Relative path resolution on project load

### 8.4 Thumbnail Generation

- Extract ~12 frames at even intervals from each video
- Compose into horizontal atlas texture (72px height)
- LRU cache (max 80 clips)
- Async generation (rayon thread pool)

### 8.5 Drag-and-Drop Video Files

Complete the drag-drop handler from Phase 7:
- Import dropped videos to library if not present
- Create clips at drop position (default 4 beats)
- Grid-snap placement

**Exit criteria:** Load a Unity project with video clips. Videos play back in sync. Thumbnails show in timeline clip rects. Pool recycles under load.

---

## Phase 9: MIDI + Sync Sources (parallel with Phase 8)

**Goal:** Full MIDI performance and external sync.

**Duration:** ~1-2 weeks

### 9.1 MIDI Input

Add `midir` crate. Create `crates/manifold-midi/`:
- Enumerate MIDI input devices
- Route NoteOn/NoteOff to LiveClipManager
- Channel filtering, time guards (5ms NoteOff debounce)
- Device selection dropdown

### 9.2 Ableton Link

Add Rust bindings for the official Ableton Link C++ library:
- Tempo sync (MANIFOLD always slaves)
- Transport sync (optional)
- Show peer count in UI

### 9.3 MIDI Clock

Implement `MidiClockSyncSource`:
- Receive MIDI Clock ticks + Song Position Pointer
- 24 PPQN timing
- Auto-play on clock receipt, auto-pause on 0.5s timeout
- BPM derivation from tick intervals

### 9.4 OSC

Add `rosc` crate:
- Input: `/livemtc` timecode (H:M:S:F)
- Output: `/manifold/play`, `/manifold/transport`, `/manifold/position`
- Parameter routing: `/master/{effect}`, `/layer/{id}/{effect}`, `/layer/{id}/gen/{param}`

### 9.5 Wire LiveClipManager

Connect MIDI input → LiveClipManager:
- NoteOn → `trigger_live_clip()` / `trigger_live_generator_clip()`
- NoteOff → `commit_live_clip()`
- Quantized launch with configurable grid

### 9.6 Add OSC to ClockAuthority Enum

Currently: Internal, Link, MidiClock. Add: OSC.

**Exit criteria:** MIDI notes trigger live clips. Link syncs tempo. MIDI Clock follows external DAW. OSC parameters route correctly.

---

## Phase 10: Export Pipeline

**Goal:** Export video files (H.264 SDR and HEVC HDR10).

**Duration:** ~2 weeks

### 10.1 Frame Capture

- GPU readback via `wgpu::Buffer::map_async()` + staging buffer
- Double-buffered readback for pipelining

### 10.2 Video Encoding

Option A (recommended): ffmpeg subprocess pipe
- Pipe raw RGBA frames to `ffmpeg -f rawvideo -pix_fmt rgba -s WxH -r FPS -i - -c:v libx264 -pix_fmt yuv420p output.mp4`
- HDR: `-c:v libx265 -pix_fmt yuv420p10le -color_primaries bt2020 ...`

Option B: Metal VideoToolbox (macOS, hardware accelerated)
- GPU blit from RenderTexture → CVPixelBuffer
- No CPU readback needed

### 10.3 Frame Pacing

- Generator-only content: `Time.captureFramerate` equivalent (offline, faster than real-time)
- Has video clips: real-time playback (video must play at correct speed)
- All generators use `GeneratorContext.DeltaTime` (consistent with offline mode)

### 10.4 Export Range

Wire I/O keyboard shortcuts (from Phase 6):
- In/Out markers define export range
- If no range: auto-range to clip content bounds
- Display in transport bar: `"IN: X OUT: Y"`

### 10.5 Audio Muxing

- Optional audio file path
- ffmpeg post-mux: `ffmpeg -i video.mp4 -i audio.wav -c:v copy -c:a aac output.mp4`

### 10.6 Export UI

- Progress bar (frame %, ETA)
- EXPORT button changes to cancel during export
- Status text: "Encoding frame 1234/5000..."

### 10.7 FCPXML Export

Port `ResolveFcpxmlExporter` (263 LOC, pure string generation):
- Generate DaVinci Resolve-compatible FCPXML from timeline

**Exit criteria:** Export a project to MP4. SDR and HDR modes work. Audio muxing works. FCPXML imports into DaVinci Resolve.

---

## Phase 11: LED/ArtNet + Audio/Percussion (parallel with Phase 10)

**Goal:** LED output and percussion analysis pipeline.

**Duration:** ~2-3 weeks

### 11.1 LED/ArtNet/DMX

Create `crates/manifold-led/`:
- `LedSettings` ScriptableObject equivalent (config struct)
- Edge-extend shader (fill letterbox regions)
- Blit to tiny RenderTexture (stripCount x ledsPerStrip)
- Async GPU readback (1-frame latency)
- Pack to DMX universes (512 channels = 170 RGB pixels)
- Send via UDP Art-Net protocol (port 6454)
- Energy gating: percussion energy envelope → brightness modulation

### 11.2 Audio Pipeline

Add `cpal` + `symphonia` (or `rodio`) crates:
- Stem audio playback synced to timeline
- Waveform rendering (peak data → GPU texture)

### 11.3 Percussion Analysis

Decision: Keep Python subprocess (proven accuracy, least work):
- Bundle `percussion_json_pipeline.py` + Python runtime
- Or: native Rust via `tract`/`candle` for ML inference (1.5x faster)

Implement:
- PERC button → file dialog → run analysis → place clips
- Per-instrument re-analysis (DRUMS, BASS, SYNTH, VOCAL buttons)
- Beat-indexed energy envelope for LED gating
- ~85 tuning parameters in PercussionPipelineSettings

### 11.4 Waveform/Stem Display

- Render imported audio waveform in timeline
- Beat grid visualization with downbeat markers
- Per-stem re-analysis buttons

**Exit criteria:** ArtNet DMX drives LED strips. Percussion analysis produces correct onset/BPM data. Stems play in sync.

---

## Phase 12: Polish + Performance HUD + OSC Remote

**Goal:** Final feature completion and polish.

**Duration:** ~1-2 weeks

### 12.1 Performance HUD

Implement overlay panel (backtick toggle):
- FPS counter, frame time (ms)
- Memory usage
- Current beat, BPM, time
- Active clock authority
- MIDI status: last note, velocity, device
- Compositor state: clips playing, pool availability
- Log button for frame diagnostics

### 12.2 Toast Notifications

- Duration: ≤3 seconds, not user-dismissible
- One at a time: new toast replaces previous
- Position: bottom of timeline viewport
- Examples: "Undid: Delete 2 clips", "Applied BPM: 128 bpm"
- Zero cost when idle (`is_animating` guard)

### 12.3 Undo/Redo Visual Flash

- Brief flash on affected elements after undo/redo
- Highlight color briefly applied then fades

### 12.4 Duration Tooltip

- Show tooltip near cursor during clip trim/resize
- Displays current duration in beats

### 12.5 Monitor Output

- Toggle via OUT button in header bar (blue when active)
- Create second winit window on secondary display
- Route compositor output to secondary window

### 12.6 Insert Cursor Triangle

- Add small triangle indicator above the insert cursor line
- Match Unity's visual (if it exists in latest build)

### 12.7 Envelope ADSR Undo

Currently ADSR changes are live without undo. Wire proper snapshot/commit:
- Snapshot 4 values (A/D/S/R) on drag start
- Commit `ChangeEnvelopeADSRCommand` on drag end

### 12.8 Mute/Solo Undo

Currently mute/solo toggle directly without undo. Add commands.

### 12.9 Final Interaction Contract Verification

Run all interaction tests from Phase 1. Manually verify:
- Every cursor change works
- Every visual feedback (ghost, dimming, insertion line) works
- Effect browser search and categories work
- Toast notifications appear and dismiss correctly

### 12.10 OSC Remote Control

Wire OSC address space for all parameters:
```
/master/{effectPrefix}              → param 0 (Amount)
/master/{effectPrefix}{paramSuffix} → param N
/layer/{layerId}/opacity
/layer/{layerId}/{effectPrefix}
/layer/{layerId}/gen/{genPrefix}/{paramName}
```

**Exit criteria:** Feature-complete application matching Unity in every user-facing behavior. All interaction tests pass. All effects and generators render. Video plays. MIDI triggers. Sync works. Export works.

---

## Testing Strategy (Applied Across All Phases)

### Per-Phase Testing

| Phase | Test Type | What to Test |
|-------|-----------|--------------|
| 0 | Constant comparison | Colors, dimensions match Unity |
| 1 | Unit tests | Pure interaction functions |
| 2 | Visual inspection | Blend modes render correctly |
| 3 | Unit + visual | Param counts match; generators render |
| 4 | Unit + visual | Param counts match; effects render |
| 5 | Integration | Driver/envelope values modulate correctly |
| 6 | Integration | Keyboard shortcuts dispatch correctly |
| 7 | Integration | Save/load roundtrip preserves project |
| 8 | Integration | Video plays in sync with timeline |
| 9 | Integration | MIDI triggers create clips |
| 10 | Integration | Export produces valid MP4 |
| 11 | Integration | ArtNet packets contain correct data |
| 12 | Manual | Visual polish, feel, performance |

### Continuous Testing Commands

```bash
# Run all tests
cargo test --workspace

# Run only interaction tests
cargo test -p manifold-ui interactions

# Run only parity tests
cargo test -p manifold-ui parity_constants

# Run with output for debugging
cargo test --workspace -- --nocapture
```

---

## Dependency Map

```
Phase 0 ─── Phase 1 ─── Phase 2 ─── Phase 3 ─── Phase 4 ─── Phase 5
                                                              │
                                                    Phase 6 ──┤
                                                              │
                                                    Phase 7 ──┤── Phase 10 ── Phase 12
                                                              │
                                                    Phase 8 ──┘   Phase 11 ──┘
                                                    (parallel     (parallel
                                                     with 9)      with 10)
                                                    Phase 9 ──┘
```

### Phase Dependencies

| Phase | Hard Dependencies | Can Parallel With |
|-------|-------------------|-------------------|
| 0 | None | — |
| 1 | 0 | — |
| 2 | 0 | 1 |
| 3 | 2 | — |
| 4 | 2 | 3 (different shader files) |
| 5 | 3, 4 | — |
| 6 | 1 | 3, 4, 5 |
| 7 | 0 | 3, 4, 5, 6 |
| 8 | 2 | 6, 7, 9 |
| 9 | 5 | 8 |
| 10 | 2, 3, 4 | 11 |
| 11 | 2, 5 | 10 |
| 12 | All | — |

---

## Reference Files

### Rust Crate Locations
```
crates/manifold-core/src/      — Data models, types, enums
crates/manifold-editing/src/    — Commands, undo, EditingService
crates/manifold-playback/src/   — PlaybackEngine, scheduler, sync
crates/manifold-io/src/         — Project load/save, migration
crates/manifold-renderer/src/   — GPU: compositor, effects, generators
crates/manifold-ui/src/         — UI tree, panels, input, colors
crates/manifold-app/src/        — Application entry, event loop, bridge
```

### Unity Reference Files (canonical truth)
```
Assets/Docs/USER_GUIDE.md                          — Feature specification
Assets/Scripts/Data/EffectDefinitionRegistry.cs     — All 40 effect params
Assets/Scripts/Data/GeneratorDefinitionRegistry.cs  — All 19 generator params
Assets/Scripts/UI/Timeline/Core/UIConstants.cs      — All color constants
Assets/Scripts/UI/Timeline/InteractionOverlay.cs    — Clip drag/trim/select
Assets/Scripts/UI/Timeline/InputHandler.cs          — Keyboard shortcuts
Assets/Scripts/UI/Timeline/ClipHitTester.cs         — Trim handle hit areas
Assets/Scripts/UI/Bitmap/UIInputSystem.cs           — Input state machine
Assets/Scripts/UI/Bitmap/EffectCardBitmapPanel.cs   — Slider/card interactions
Assets/Scripts/UI/Bitmap/BitmapSlider.cs            — Slider math
Assets/Scripts/Compositing/CompositorStack.cs       — Compositing pipeline
Assets/Scripts/Compositing/Effects/                 — All effect shaders
Assets/Scripts/Generators/                          — All generator implementations
```

### Audit Documents (this repo)
```
docs/UNITY_PARITY_AUDIT.md     — Gap analysis (what's wrong/missing)
docs/INTERACTION_CONTRACT.md   — Behavioral specification (how it must work)
docs/PORTING_STRATEGY.md       — Testing strategy (how to verify)
docs/MIGRATION_PLAN.md         — This document (execution plan)
```
