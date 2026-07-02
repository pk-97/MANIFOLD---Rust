# Multi-Display / Totem Canvas Model

**Status: APPROVED design, not implemented.** Written 2026-07-02 (Fable). Execution is a
Sonnet apply pass — every decision needed to build this is in here; don't reopen §11.

The driving use case: two vertical LED totems on stage, meters apart. Content chases
between them, bounces off them, spans the gap — the physical setup is part of the
instrument, not a routing detail.

---

## 1. Goal

MANIFOLD today renders one canvas and presents it to one output window. This design
extends that to N physical outputs (displays, projectors, LED processors) with:

1. **One composition** — the performer authors a single piece of content, not N.
2. **Physical-space awareness** — content can know where the displays physically are
   and use that: chase across the gap at real-world speed, strobe totems alternately,
   mirror a pattern per-totem.
3. **Zero mapping busywork** — no slice editors, no pixel-rect entry, no output routing
   matrices. The user arranges pictures of their displays on a stage plan. Everything
   else is derived.

Non-goals: projection mapping onto 3D geometry, hardware genlock, per-display
independent compositions (that's just running the content twice).

## 2. What already exists (inventory)

The presentation layer is mostly built. Do not redesign these; extend them.

| Piece | Where | State |
|---|---|---|
| Multi-window registry with `WindowRole::Output` | `manifold-app/src/window_registry.rs` | N windows supported today |
| Direct present: content thread acquires drawable + blits + presents in its own CB | `content_pipeline.rs` (`render_content_native`, "Direct present to output drawable" block) | Single `output_surface: Option<GpuSurface>`, triple-buffered, `presents_with_transaction(false)` |
| Content-thread pacing | `mach_wait_until` + 2ms spin at project FPS | SETTLED — do not touch |
| UI vsync | `UiDisplayLink` (CVDisplayLink per workspace window) | Content thread does NOT use display links |
| Canvas dimensions | `ProjectSettings::output_width/output_height` + `render_scale` (internal render res → output res upscale exists) | The canvas already has a scale relief valve |
| EDR per-display headroom | `edr_surface.rs` (event-driven headroom re-query on screen change) | Already per-window |
| LED output samples the final texture | `manifold-led` (blit + readback → Art-Net) | Already "a sampler of the canvas" |
| Display ID lookup | `display_link.rs::display_id_for_window` (CGDirectDisplayID via NSScreen) | Runtime only — IDs are NOT stable across reboots |

The missing piece is the **mapping model**: one canvas → N physical outputs, plus the
physical layout as data the content can read.

## 3. Core model

### D1 — One virtual canvas; outputs are samplers of it

The content thread renders one canvas per tick, exactly as today. Every output —
display window, LED strip, future NDI/recording — owns a mapping: *source rect on the
canvas → transform → device*. There are no per-display pipelines: they would double GPU
cost, duplicate state, and make boundary-spanning effects impossible.

### D2 — The canvas is a pixel rect; gaps are rendered

Two candidate models were weighed:

- **(a) Resolume model (CHOSEN):** the canvas is one big pixel rect. Displays are crops
  placed on it, with real gaps between them. Gap pixels are rendered and never shown.
  Screen-space effects (blur, feedback, convolution) work across the boundary because
  the boundary isn't one — it's just canvas.
- **(b) Packed atlas (REJECTED):** render only display pixels, remap coordinates per
  region. Saves the gap pixels but screen-space effects break at atlas seams — which
  kills exactly the boundary-spanning behavior this design exists to enable.

The waste in (a) is bounded, honest, and user-visible (§9). Density control is the
mitigation, not topology tricks.

### D3 — The canvas is derived, never authored

The user authors a **stage layout** (physical placements, §4–5). From it the engine
derives:

- `px_per_mm` — canvas density. Default: the highest native density among placed
  displays (no display undersampled), clamped by a user density cap (§9).
- Canvas rect — the bounding box of all placement rects in mm, times `px_per_mm`,
  rounded up to even dimensions.
- Each output's source rect on the canvas — its placement rect in canvas pixels.

The user never sees px/m, crop rects, or canvas size as things to edit. Move a totem on
the stage plan → the mapping and canvas re-derive. Canvas re-derivation is per-action
(reallocates render targets), never per-frame.

**Legacy / single-display:** a project with no stage layout keeps today's behavior
byte-identically — canvas = `output_width × output_height`, one implicit full-canvas
mapping to the output window. `output_width/height` stay authoritative until the first
placement is added; after that they become derived values (kept in sync for export and
older code paths).

## 4. Stage view UX

The mental model is **macOS display arrangement, on a stage plan, in real units**.
Everyone who has plugged in a second monitor already knows this UI.

- A 2D top-of-stage view. Each output is a rectangle labeled with its name.
- Connected displays are detected and offered; physical size pre-filled from EDID
  (often right, sometimes garbage — always editable). LED processors that lie about
  size get corrected by typing the real panel dimensions.
- Drag to position. The gap you leave between two totems is the *real* gap — enter it
  numerically or drag until the readout matches the stage measurement.
- Rotation per placement: 0/90/180/270. Vertical totems are usually landscape panels
  rotated; the rotation applies in the output blit (§6), not in content.
- Live readout: derived canvas resolution + estimated cost tint (§9). The user sees
  "3200×1920" change as they drag, not a surprise at showtime.
- **Advanced flap, per output, closed by default:** 4-corner keystone (homography —
  covers projectors), RGB gain/lift color trim, EDR/tonemap override. Warp meshes are
  deferred (§12). Most users never open this.

The fiddly slicing layer still exists internally (§5) — it is **generated from the
stage view, never hand-edited**.

## 5. Data model

New module `manifold-core/src/stage.rs`. Serialized inside `ProjectSettings` (serde
conventions per the codebase; all fields `#[serde(default)]` so old projects load).

```rust
/// Physical arrangement of outputs on the stage plan. Millimetres.
pub struct StageLayout {
    pub placements: Vec<DisplayPlacement>,
    /// User density cap in px/mm; None = native density of the densest display.
    pub density_cap: Option<f32>,
}

pub struct DisplayPlacement {
    pub id: OutputId,                    // stable, project-scoped newtype (u64)
    pub name: String,                    // "Totem L"
    pub physical_size_mm: [f32; 2],      // pre-rotation panel size, EDID-prefilled
    pub native_resolution: [u32; 2],     // mode MANIFOLD drives, pre-rotation
    pub position_mm: [f32; 2],           // top-left on the stage plan, post-rotation
    pub rotation: Rotation,              // R0 | R90 | R180 | R270
    pub identity: Option<DisplayIdentity>, // which physical monitor
    pub enabled: bool,
    pub advanced: OutputAdvanced,        // keystone quad, color trim, tonemap override
}

/// Stable identity for re-matching a physical display across launches/reboots.
pub struct DisplayIdentity {
    /// CGDisplayCreateUUIDFromDisplayID — the stable key. Match on this first.
    pub uuid: Option<String>,
    /// NSScreen localizedName — fallback match + human label.
    pub name: String,
}
```

Derivation lives next to the model as pure functions with unit tests:
`derive_canvas(&StageLayout) -> DerivedCanvas { size_px, px_per_mm, source_rects }`.

**Display identity rules (gig-critical):** CGDirectDisplayIDs shuffle across reboots.
Match placements to live displays by UUID, then by name, else mark **unassigned** —
never silently guess. Unassigned placements render into the canvas as normal (content
is unaffected) but present nowhere, and the stage view shows a one-click "assign"
picker. Plugging in the totems at the venue must be: open project → two clicks → done.

**Mutations:** stage edits go through `EditingService` like everything else
(`ContentCommand::Execute` / commands with undo). Canvas re-derivation happens on
command apply.

## 6. Present architecture

Extends today's direct-present path from one surface to N. The content thread stays
the only clock (mach_wait_until pacing, SETTLED); **no display links are added** —
outputs vsync via their own `CAMetalLayer` at present time. This respects the
never-unify-CVDisplayLinks rule: no heavy GPU work ever runs in a vsync callback
because no vsync callback exists on this path.

- `ContentPipeline` replaces `output_surface: Option<GpuSurface>` with a small vec of
  `(OutputId, GpuSurface, InFlightCounter)`. Attach/detach flows mirror today's
  `AttachOutputSurface` command (windows are created on the UI thread as now; surfaces
  handed to the content thread).
- Per tick, per attached output: **non-blocking drawable acquire**. `next_drawable()`
  blocks when the layer's queue is full — with two displays at different refresh rates
  the slower one would stall the content tick and starve the faster one. Guard: an
  atomic in-flight counter per surface, incremented at present-schedule, decremented in
  the drawable's presented handler; skip this output this tick when the counter says
  the queue is full. A skipped output simply keeps showing its previous frame.
- The per-output present pass is one cheap draw appended to the compositor CB, exactly
  like today's single-output blit: sample the final canvas texture with the output's
  source rect UVs, apply rotation (UV swizzle), keystone homography if set, color trim,
  per-output tonemap/EDR mapping (headroom already tracked per window). Rgba16Float
  drawables as today.
- **Cadence decision:** outputs run at independent cadence. Two totems on different
  clocks may occasionally show frames one apart (~16ms at 60Hz). Across meters of
  physical separation this is imperceptible. Frame-locking displays in software is the
  never-unify trap and is rejected; true genlock is display-hardware territory.
- Workspace preview + perform HUD show the full canvas with placement outlines and
  ghosted gap regions (render as overlay in the existing preview pane; no new pipeline).

**What this means on stage:** the laptop drives UI + 2 totem windows. UI present
already contends with content GPU (`ui-present-content-gpu-contention`); the two output
presents are trivial blits but the *canvas* is bigger — see §9.

## 7. Display-aware content

The physical setup becomes **data flowing through the existing graph** — no new
modulation silo, no special runtime. Works with cards, MIDI, audio mod like everything
else.

### 7.1 The layout uniform

Extend the frame globals that already carry time/resolution into graph execution
(`FrameTime` in `node_graph/effect_node.rs` is the carrier; the uniform build follows
the existing pattern). Fixed-capacity array, `MAX_DISPLAYS = 8`, WGSL-aligned (vec4
fields only — respect the vec3-alignment rule):

```wgsl
struct DisplayLayout {
    count: u32,               // + padding to 16B
    displays: array<DisplayEntry, 8>,
}
struct DisplayEntry {
    rect_uv: vec4<f32>,       // canvas-UV rect: xy = min, zw = max
    center_size_uv: vec4<f32>,// xy = center, zw = size (canvas UV)
    physical_mm: vec4<f32>,   // xy = center, zw = size (stage plan mm)
}
```

Built per-frame from the cached derived layout — no allocation, no derivation on the
hot path. Also exposed to `wgsl_compute` (it's a live-show authoring surface).

### 7.2 Three primitives (atoms, not monoliths)

Named per the vocabulary conventions (`node.` + snake_case(label), outcome names).
Full descriptors required — label, summary, purpose, aliases, examples — the
completeness gate applies.

| type_id | label | one dispatch, one purpose |
|---|---|---|
| `node.display_mask` | Display Mask | White inside display N's canvas rect, black elsewhere (soft edge param). Per-totem strobe/isolate. |
| `node.display_uv` | Display UV | UV field remapping each display region to its own 0–1 space (mirror X/Y params). Same or mirrored pattern per totem. |
| `node.display_info` | Display Info | CPU/value node: count, plus display N's center/size in canvas UV and mm. Wire `displays[1].center` into an emitter → bounce between totems. |

Gap-spanning behavior needs none of these — it falls out of D2(a). The primitives are
only for *display-indexed* behavior. Later sugar like layer→display routing is a
transform preset over the same data, not a new mechanism.

**MCP note:** `get_project_overview` should include the stage layout summary so agents
can author display-aware content ("two portrait displays, 3.2m gap").

## 8. What it buys on stage

- A particle system flies off Totem L, crosses 3m of real air at real speed, lands on
  Totem R on beat — authored once, in physical space.
- Alternate-totem strobe = Display Mask × LFO. Chase = anything driven by
  Display Info centers.
- Rearrange the venue, drag the stage view to match, content adapts — no re-authoring.
- Plug-in at the venue is two clicks (identity re-matching, §5), not a mapping session.

## 9. Performance

The honest cost of D2(a): canvas pixels scale with **physical span × density**, and gap
pixels are rendered.

- Baseline content render today: 4.5–5.5ms at 1080p-class canvas; 4K margin is an OPEN
  campaign. A wide canvas spends from the same budget.
- Example: two portrait 1080×1920 displays 0.6m wide, 3m gap, at native density
  (1800px/m) → canvas ≈ 7560×1920 ≈ 14.5MP ≈ 7× a 1080p canvas. **Not viable.**
  Same layout with density cap at 500px/m → 2640×960 ≈ 2.5MP, totems upscaled ~3.6× at
  the blit. LED totems are physically low-res; upscale is usually invisible on them.
- Mitigations, in order: **density cap** (§5, the primary knob — stage view shows the
  derived resolution live and tints it red past a budget threshold), existing
  `render_scale` (uniform relief valve), and future dirty-region/scissor optimization
  (§12 — explicitly out of v1).
- Present cost: two extra fullscreen-triangle blits per tick — noise. The real watch
  items are canvas size (content render) and the known UI-present/content-GPU
  contention with three windows on one GPU. The perf campaign owns the budget; this
  design's job is to make cost visible before showtime, not to hide it.

## 10. Phasing (Sonnet-executable)

Each phase lands alone, is testable alone, and doesn't break single-display flow.

- **P1 — core model.** `stage.rs` (StageLayout, DisplayPlacement, identity, OutputId),
  `derive_canvas` + unit tests (density cap, rotation, bounding box, empty layout =
  legacy), serde defaults, EditingService commands. No behavior change with empty
  layout.
- **P2 — multi-output present.** Surface vec + in-flight counters + non-blocking
  acquire; per-output blit with rotation/trim/keystone; attach/detach commands; output
  window creation per placement ("Output" menu); identity matching + unassigned state.
  Test: two windows on one Mac (external monitor), skew accepted.
- **P3 — layout uniform + primitives.** DisplayLayout uniform into frame globals +
  `wgsl_compute`; the three atoms with descriptors; gpu_tests for mask/uv (value-level:
  exact rect edges), display_info is CPU (plain unit test).
- **P4 — stage view UI.** Arrangement panel (drag, numeric fields, EDID prefill,
  rotation, live canvas readout + cost tint, assign picker), advanced flap (keystone,
  trim). Uses existing panel/scroll infra; headless PNG verification applies.
- **P5 — later.** LED strips become placements (manifold-led samples via the same
  source-rect model), NDI/Syphon outputs, per-display export stems.

Full workspace test sweep gates P2 and P3 (present path + graph runtime = infra).

## 11. Decided — do not reopen

1. One virtual canvas; every output is a sampler of it. No per-display pipelines.
2. Pixel canvas with rendered gaps (Resolume model). Packed atlas rejected — breaks
   screen-space effects at seams.
3. Canvas is derived from the stage layout. Users never hand-edit slices, crops, px/m.
   Density cap is the one exposed cost knob.
4. Stage view = macOS-display-arrangement mental model, real units, EDID prefill,
   advanced flap closed by default.
5. Outputs present at independent cadence from the content thread's direct-present
   path. No new display links, no software frame-locking, no genlock.
6. Display awareness = one layout uniform + three atoms. No new runtime, no new
   modulation path, no special node kinds.
7. Display identity = CGDisplay UUID, name fallback, explicit unassigned state with
   one-click reassign. Never silently guess.
8. Master effects apply to the full canvas, before per-output sampling. Per-output
   stage is crop/rotate/keystone/trim/tonemap only — no content processing.
9. Non-blocking drawable acquire via in-flight counters; a full queue skips the output
   for that tick.
10. Export/recording = full canvas in v1.

## 12. Open (deferred, not blocking)

- Warp meshes beyond 4-corner keystone (projection-mapping territory).
- Dirty-region/scissor rendering to skip gap pixels when no spanning effect is active —
  an optimization with correctness traps (feedback reads gap pixels); only worth it if
  the perf campaign demands it.
- LED placement unification details (P5) — strip geometry on the stage plan.
- Per-display export stems; NDI/Syphon.
- >8 displays (bump MAX_DISPLAYS; uniform layout allows it trivially).
