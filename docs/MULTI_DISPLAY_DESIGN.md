# Multi-Display / Totem Canvas Model

**Status: APPROVED design, not implemented.** Written 2026-07-02 (Fable). **v2, same
day:** the v1 "render the gaps" pixel canvas was rejected by Peter — a super-wide stage
must not spend its frame budget on invisible air. v2 replaces it with the island atlas
model. Execution is a Sonnet apply pass — every decision needed is in here; don't
reopen §11.

The driving use case: two vertical LED totems on stage, meters apart. Content chases
between them, bounces off them, crosses the gap — the physical setup is part of the
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
4. **Zero dead pixels** — render cost scales with the displays you own, never with the
   air between them. A 50m-wide stage with two totems costs the same as the two totems.

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
| Canvas dimensions | `ProjectSettings::output_width/output_height` + `render_scale` | Legacy single-canvas path; becomes the one-island case |
| EDR per-display headroom | `edr_surface.rs` (event-driven headroom re-query on screen change) | Already per-window |
| LED output samples the final texture | `manifold-led` (blit + readback → Art-Net) | Already "a sampler of the canvas" |
| Display ID lookup | `display_link.rs::display_id_for_window` (CGDirectDisplayID via NSScreen) | Runtime only — IDs are NOT stable across reboots |

The missing piece is the **mapping model**: one composition → N physical outputs, plus
the physical layout as data the content can read.

## 3. Core model

### D1 — One composition; outputs are samplers of it

The content thread renders once per tick, exactly as today. Every output — display
window, LED strip, future NDI/recording — owns a mapping: *region of the render
target → transform → device*. There are no per-display content pipelines: they would
duplicate state and make cross-display behavior unauthorable.

### D2 — Islands: contiguous pixels only where continuity is visible

The key observation (Peter's catch, v2): pixel continuity between displays only
matters where displays **abut** — a panel wall, where a seam-crossing blur or feedback
trail is actually visible. Across a real physical gap, neighborhood continuity is
invisible; rendering the gap preserves nothing and costs real GPU time.

So:

- **An island is a cluster of abutting displays**, derived automatically from the stage
  layout: placements whose rects touch (within a small snap tolerance) merge into one
  contiguous pixel region. A 3×2 panel wall = one island. Two totems meters apart =
  two islands. A single display = one island (the legacy case, byte-identical
  behavior).
- **The render target is a packed atlas of islands.** One texture, one frame graph
  pass structure, containing only display pixels plus small gutters (16px) between
  regions. Gaps between islands are never allocated and never rendered.
- **The stage is a coordinate frame, not pixels.** Physical positions exist as data
  (mm, in the layout uniform §7) — content computes with them; nothing rasterizes air.

Rejected alternatives, for the record:

- **Rendered-gap pixel canvas (v1 / Resolume model):** canvas = bounding box of the
  stage, gap pixels rendered and discarded. Rejected — cost scales with stage width,
  not owned hardware; a wide stage drowns the frame budget in black pixels. Its one
  advantage (seam-free neighborhood ops across displays) only matters for abutting
  displays, which island merging preserves anyway.
- **One canvas texture per island:** same semantics as the atlas, but multiplies
  render-target allocations and breaks the one-texture assumptions in preview, LED
  sampling, and export paths. The atlas keeps one texture; regions do the same job.

### D3 — Everything is derived, never authored

From the stage layout (§5) the engine derives: island clustering, per-island pixel
density (native density of its densest display, optionally capped per island §9),
atlas packing (region rects + gutters), each output's source region, and the layout
uniform. The user never sees the atlas, px/mm, crops, or packing. Move a totem →
everything re-derives. Re-derivation is per-action (may reallocate render targets),
never per-frame.

**Legacy / single-display:** a project with no stage layout = one island of
`output_width × output_height`, one implicit full-region mapping to the output window.
Today's behavior, byte-identical. `render_scale` keeps working (it scales island
render resolution).

## 4. Per-layer spatial domain — the one new performer-facing concept

With multiple islands, "where does this layer's content live?" needs an answer. It is
a per-layer toggle with two values:

- **Stage** (default): the layer's content is mapped across physical space. Each
  island rasterizes its window of the stage. A generator sweep travels the real stage;
  a particle crossing the gap exists in stage coordinates and simply isn't rasterized
  while it's in the air — correct, and free. A video clip spanning both totems shows
  its left part on Totem L and right part on Totem R, gap omitted.
- **Every display**: the layer's content repeats on each island (each island is its
  own 0–1 canvas). Mirrored totems, same pattern everywhere — the other most common
  live look.

On a single island the two are identical, so single-display users never see the
concept. Effects on a layer follow the layer's domain trivially because effects always
run island-locally (§6.1).

This is a `Layer` field + UI toggle, mutated through `EditingService` like everything
else.

## 5. Stage view UX + data model

### UX

The mental model is **macOS display arrangement, on a stage plan, in real units**.

- A 2D top-of-stage view. Each output is a rectangle labeled with its name.
- Connected displays detected and offered; physical size pre-filled from EDID (often
  right, sometimes garbage — always editable). LED processors that lie get corrected
  by typing real panel dimensions.
- Drag to position. Displays that touch snap together and visibly merge into an island
  (subtle shared outline). The gap you leave between islands is the *real* gap — enter
  it numerically or drag until the readout matches the stage measurement.
- Rotation per placement: 0/90/180/270. Vertical totems are usually landscape panels
  rotated; rotation applies in the output blit (§6.2), not in content.
- Live readout: total rendered pixels (sum of islands — dragging displays *apart*
  never changes it) + per-island resolutions.
- **Test patterns + Identify (v1, non-negotiable):** per-output grid, focus chart,
  white field, and an Identify button that flashes the output's number/name on the
  physical device. The first five minutes at every venue.
- **Advanced flap, per output, closed by default:** 4-corner keystone (homography —
  covers projectors), RGB gain/lift color trim, EDR/tonemap override, per-island
  density cap. Most users never open this.

The slicing/packing layer exists internally — **generated from the stage view, never
hand-edited**.

### Data model

New module `manifold-core/src/stage.rs`. Serialized inside `ProjectSettings` (serde
conventions; all fields `#[serde(default)]` so old projects load).

```rust
/// Physical arrangement of outputs on the stage plan. Millimetres.
pub struct StageLayout {
    pub placements: Vec<DisplayPlacement>,
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
    pub advanced: OutputAdvanced,        // keystone quad, color trim, tonemap override,
                                         // density cap (px/mm, None = native)
}

/// Stable identity for re-matching a physical display across launches/reboots.
pub struct DisplayIdentity {
    /// CGDisplayCreateUUIDFromDisplayID — the stable key. Match on this first.
    pub uuid: Option<String>,
    /// NSScreen localizedName — fallback match + human label.
    pub name: String,
}
```

Islands are **not stored** — they are derived (pure function, unit-tested):
`derive_stage(&StageLayout) -> DerivedStage { islands: Vec<Island>, atlas_size }`,
where `Island { display_ids, stage_rect_mm, px_per_mm, atlas_region_px }`. Clustering =
union of placements whose post-rotation rects touch within snap tolerance (5mm).

**Display identity rules (gig-critical):** CGDirectDisplayIDs shuffle across reboots.
Match placements to live displays by UUID, then by name, else mark **unassigned** —
never silently guess. Unassigned placements still render (content unaffected) but
present nowhere; the stage view shows a one-click "assign" picker. Plugging in at the
venue must be: open project → two clicks → done. **Hot-replug mid-show:** a display
that disappears (kicked cable) degrades gracefully — content keeps rendering, output
marked unassigned; when a display with the *same UUID* reappears, reattach silently
with zero clicks. The two-click flow is for new hardware only.

**Venue profiles:** the show and the venue are different lifetimes. The composition is
per-show; stage layout + assignments + advanced-flap data (keystone, warp, trim,
calibration) are per-venue. `StageLayout` stays serialized in the project (single
source at runtime), but is **exportable/importable as a standalone venue file**
(JSON) — "load `corner-hotel.venue`, play the same set." Decided now because
retrofitting separability after projects bake layouts in is painful.

**Mutations:** stage edits go through `EditingService` (commands with undo). Derivation
runs on command apply.

## 6. Rendering + present architecture

### 6.1 Per-island execution — today's shader semantics, unchanged

Effects and compositing execute **once per island**, as a scissored viewport pass into
that island's atlas region. Inside the viewport, a shader sees a contiguous,
aspect-correct local canvas: local UV, local resolution — exactly today's semantics.
**Zero shader changes.** This is what makes neighborhood ops (blur, convolution,
feedback advection) well-defined: they operate within an island, clamped by the
viewport, and never bleed across a gap that doesn't physically exist. For abutting
panels — where bleed *is* visible — they're one island, so continuity is preserved
where it matters.

Consequences, stated honestly:

- **Encoder/dispatch work multiplies by island count.** Total pixel work does not (sum
  of island pixels = the content you own). Two islands ≈ 2× dispatches — fine. Many
  islands (6+ totem stages) will want the pointwise-fusion path to collapse per-island
  loops into single atlas-wide dispatches for pointwise nodes; that's the existing
  fusion-compiler direction, noted as the optimization escape hatch, not v1.
- **Stateful effect state keys extend to (node, island)** — the state store and chain
  caches key per island. Per-island feedback/trails are semantically correct
  (physically separate surfaces). Reset paths must walk both caches per the existing
  two-cache rule.
- Layer domain (§4) enters as the content coordinate mapping per island: Stage domain
  = island's stage window drives generator coordinates / clip placement; Every-display
  domain = island-local 0–1. Pointwise/compositing behavior is identical either way.

### 6.2 Present

Extends today's direct-present path from one surface to N. The content thread stays
the only clock (mach_wait_until pacing, SETTLED); **no display links are added** —
outputs vsync via their own `CAMetalLayer` at present time. This respects the
never-unify-CVDisplayLinks rule: no vsync callback exists on this path at all.

- `ContentPipeline` replaces `output_surface: Option<GpuSurface>` with a small vec of
  `(OutputId, GpuSurface, InFlightCounter)`. Attach/detach mirrors today's flow
  (windows created on the UI thread as now; surfaces handed to the content thread).
- Per tick, per attached output: **non-blocking drawable acquire**. `next_drawable()`
  blocks when the layer's queue is full — with displays at different refresh rates the
  slower one would stall the content tick. Guard: an atomic in-flight counter per
  surface (incremented at present-schedule, decremented in the presented handler);
  skip the output this tick when full. A skipped output keeps showing its last frame.
- The per-output present pass is one cheap draw appended to the compositor CB, like
  today's single-output blit: sample the output's atlas region, apply rotation (UV
  swizzle), keystone homography if set, color trim, per-output tonemap/EDR mapping
  (headroom already tracked per window). Rgba16Float drawables as today.
- **Cadence decision:** outputs run at independent cadence. Two totems on different
  clocks may occasionally show frames one apart (~16ms at 60Hz). Across meters of
  physical separation this is imperceptible. Software frame-locking is the never-unify
  trap and is rejected; true genlock is display-hardware territory.
- **Preview:** the workspace preview composites island textures at their stage-plan
  positions over the panel background — the gap is literal empty UI, costing nothing.
  Placement outlines drawn on top. Perform HUD same. **Rehearsal view** (deferred,
  §12) elevates this into an audience-eye mode — islands glowing in a dark stage,
  fixtures rendered as points — so a show is authorable at home with zero hardware.

## 7. Display-aware content

The physical setup becomes **data flowing through the existing graph** — no new
modulation silo, no special runtime. Works with cards, MIDI, audio mod like everything
else.

### 7.1 The layout uniform

Extend the frame globals that already carry time/resolution into graph execution
(`FrameTime` in `node_graph/effect_node.rs` is the carrier; the uniform build follows
the existing pattern). Fixed capacity `MAX_DISPLAYS = 8`, `MAX_ISLANDS = 8`,
WGSL-aligned (vec4 fields only — respect the vec3-alignment rule):

```wgsl
struct StageUniform {
    counts: vec4<u32>,           // x = displays, y = islands, z = current island id
    stage_rect_mm: vec4<f32>,    // whole-stage bounding box (for normalization)
    displays: array<DisplayEntry, 8>,
    islands: array<IslandEntry, 8>,
}
struct DisplayEntry {
    physical_mm: vec4<f32>,      // xy = center, zw = size (stage plan mm)
    island_local: vec4<f32>,     // rect within its island's local 0–1 space
    meta: vec4<u32>,             // x = island id
}
struct IslandEntry {
    physical_mm: vec4<f32>,      // xy = center, zw = size
    atlas_region: vec4<f32>,     // atlas UV rect (for advanced/wgsl_compute use)
}
```

`current island id` is what makes per-island execution composable: a node knows which
island it's rendering and can look up its stage window. Built per-frame from the
cached derived stage — no allocation, no derivation on the hot path. Also exposed to
`wgsl_compute` (it's a live-show authoring surface).

### 7.2 Three primitives (atoms, not monoliths)

Named per the vocabulary conventions (`node.` + snake_case(label), outcome names).
Full descriptors required — label, summary, purpose, aliases, examples — the
completeness gate applies.

| type_id | label | one dispatch, one purpose |
|---|---|---|
| `node.display_mask` | Display Mask | White where display N intersects the current island, black elsewhere (soft edge param). Per-totem strobe/isolate within a wall or across the stage. |
| `node.stage_uv` | Stage UV | UV field in normalized stage space for the current island's window — the coordinate handle for Stage-domain looks in graphs that want it explicitly. |
| `node.display_info` | Display Info | CPU/value node: counts, display or island N's center/size in mm and normalized stage space. Wire `islands[1].center` into an emitter → bounce between totems. |

Cross-gap behavior needs no gap pixels — Stage domain (§4) plus these coordinates
cover it. Later sugar like layer→display routing is a preset over the same data, not a
new mechanism.

**MCP note:** `get_project_overview` should include the stage layout summary so agents
can author display-aware content ("two portrait islands, 3.2m apart").

### 7.3 Lighting: fixtures as placements, consoles as peers

The stage plan doesn't stop at screens. `manifold-led` already outputs Art-Net;
extending the placement model to **DMX fixtures** (pars, strips, moving-head colors)
puts lights on the same map: a fixture placement samples the Stage-domain composition
at its physical position. A sweep crosses Totem L → the fixtures in the air between →
Totem R. Visuals and lighting from one composition. For a small artist this replaces a
lighting operator; lands with the LED unification phase (§10).

**Who each tier is for (Peter, 2026-07-02):** most clubs are console-owned DMX cables
with no network — the house rig is physically unreachable (and club desks often can't
even receive triggers). So the *primary* lighting story for the target user is the
**artist-owned package**: your fixtures + one Art-Net/sACN node in your case, MANIFOLD
as the entire lighting brain, no console anywhere. Console cooperation below is the
growth path (theatres, festivals, real LDs) — not the entry point.

At venues with a house rig and console (grandMA is the industry standard), MANIFOLD is
the **media server / position-aware color engine — never the console.** Cue-based
lighting craft stays with the LD. Three integration levels:

1. **Sync** — timecode/MIDI/OSC cue exchange with the console. Rides existing
   infrastructure (OSC, MIDI, Ableton sync, timecode).
   *Console-owned cables are not triggers-only:* grandMA-class desks accept network
   DMX as **input** — the LD can map it to remote faders/executors (continuous control
   over their looks) or patch fixture color attributes to MANIFOLD's input universes
   (full per-fixture color, console acting as the network→cable node, LD priority on
   top). All grades are the same sACN output on MANIFOLD's side — the bridging is
   console-side patching. Verify MA2/MA3 input-merge specifics when this phase is
   specced.
2. **Cooperative pixel control — sACN with priority.** sACN's per-source priority
   field lets the house rig merge MANIFOLD's color with the LD's control, LD holding
   override. Art-Net has no priority; **sACN output in manifold-led is the ticket into
   pro rooms.** Direct-to-fixture output (bypassing the console) is standard media-
   server practice, not exotic — the transport is a stateless ~44Hz value refresh, and
   MANIFOLD already ships it for LED strips. **Failure semantics (gig-critical,
   decided):** MANIFOLD always transmits below the console's priority; on exit or
   crash it must *stop transmitting cleanly* (socket teardown, no frozen last frame)
   so fixtures revert to the console or their own dark state — never freeze
   mid-strobe. Best practice split: washes/chases on the direct path (a blip is
   invisible), make-or-break cues on the console via triggers.
3. **Pre-mapping — MVR/GDTF import.** The lighting industry's open exchange formats:
   GDTF describes a fixture, MVR describes a whole rig (positions + patch). grandMA3
   exports MVR. Import → the stage plan auto-populates with the venue's fixtures at
   real positions/addresses → Stage-domain content maps onto the house rig before
   arriving, rehearsed in rehearsal view. The industry's own format feeding the
   "everything derives from the stage plan" doctrine.

**Routing is a bus, never a graph node.** Fixture placements select a source: **Master**
(default — lights follow the composition) or a **specific layer/group** (the "lighting
bus"). One dropdown on the perform surface, like the layer domain toggle. Device
output never lives inside a content graph — graph effects are per-clip instanced,
undoable, state-cached authoring objects, and hardware ownership inside one is
ambiguous by construction (same doctrine that keeps audio I/O off the graph). The
creative half stays fully graph-shaped: a lighting-bus layer is a normal layer with
full effect graphs — author what the lights see with any graph; route it with a
switch. Ableton's division: racks author, the mixer routes.

**Clip → console triggers.** Clip start/stop events map to outbound OSC/MIDI messages
("Go Executor 3") — MANIFOLD's timeline fires the console's pre-programmed looks. A
mapping table on existing OSC/MIDI infra, not a new system. Also the universal
fallback at venues that won't expose their rig: sync via triggers with zero fixture
mapping. Three details, decided: **(a)** clips reference stable friendly names
("Strobe", "Blackout"); the **name → cue-ID table lives in the venue profile**, so a
touring show re-points to each desk by loading the venue file, never by editing the
composition. **(b)** clip start sends the go, **clip end sends the release** — a
strobe's duration is the clip's length, like a note. **(c)** an unmapped name is a
visible warning in the stage view, not a silent no-op at showtime. **(d)** filling the
table: no universal cue-list exchange format exists (MVR carries the rig, not the
programming; grandMA3 exports sequence XML, theater desks speak USITT ASCII, others
are proprietary). The table is ~a dozen rows — manual entry is fine, and the MCP
agent path ("paste the LD's email, fill the venue profile") covers every console
brand with zero parsers. Do NOT build per-console importers unless demand proves
them — with one exception worth building when this phase lands: grandMA3's XMLs.
Sequence XML (cue numbers/names/notes/fade times) auto-fills the trigger table; and
**timecode XML is an *export* target**: MANIFOLD renders the arrangement's trigger
clips as a grandMA3 timecode file, the LD imports it before doors, and the desk
chases timecode during the show — every hit pre-loaded console-side, network reduced
to a clock signal. Third-party tools already generate these files in the field
(Myelin Director, GMA Toolbox, MATools), so the format is import-proven. The most
gig-proof trigger path of the three.

**Timecode OUT — required for the pre-loaded chase workflows, does not exist yet.**
MANIFOLD today only *receives* sync (Ableton, MIDI clock, OSC). The console/BEYOND
timecode-chase stories need MANIFOLD to *broadcast* timecode derived from its
transport. Decided master chain: **Ableton → MANIFOLD → timecode out → console +
lasers** — one clock for music, video, lights, lasers, MANIFOLD as relay. Start with
MTC (cheap, MIDI infra exists); SMPTE/LTC (an audio-channel signal) only if a venue
demands it.

**Remote control IN — Art-Net/DMX input.** The reverse direction: at venues where the
LD runs the show, they ride MANIFOLD parameters (master intensity, clip fire) from
their desk. One more remote-control input beside the existing MIDI/OSC-in — same
mapping surface, no new concept. Makes MANIFOLD a well-behaved media server when it
isn't the boss.

**Hazers/foggers are fixtures.** Intensity-only placements — zero new design. Haze
level as a clip: fog builds four bars before the drop, on the grid.

**Tech rider / advance email as an MCP flow.** Power, universes, IPs, MVR request,
cue-list request — all derivable from stage plan + venue profile. An agent drafts the
venue advance from it. Near-zero cost once MCP lands.

**Lasers (Pangolin): trigger tier ONLY — decided.** BEYOND/FB4 natively accept OSC,
MIDI, timecode chase, and DMX/Art-Net cue triggers, so laser hits are ordinary trigger
clips with venue-profile mappings ("Laser Burst" → BEYOND cue 12), including
continuous parameter rides (size/rotation/color over OSC). MANIFOLD never generates
laser frames directly: vector/galvo rendering is a separate art, and **audience-scan
safety zones live in Pangolin's software — MANIFOLD must never sit between the safety
layer and the hardware.** Never the console; never the safety authority.

**Lighting looks are clips, not cues.** Because fixtures sample the composition,
strobes/chases/washes are authored as ordinary visual content: a strobe = a
white-flash clip over the fixture positions; a chase = a bright bar sweeping in Stage
domain (fixtures fire in physical order); a wash = a slow gradient clip. All existing
machinery applies for free — beat quantization, timeline, session cells, MIDI/phantom
triggers, Ableton sync. **No lighting-cue system is ever built** (no 5th modulation
silo). v1 boundary: color/intensity sampling only — covers pars, washes, strips,
matrices. Moving-head pan/tilt, gobos, hardware shutter channels are channel-level
control, explicitly deferred (console/LD territory until a dedicated design says
otherwise).

**Implementation notes for P6:** both formats are zip+XML (`manifold-io` already ships
zip infra). Specs are open (GDTF = DIN SPEC 15800, MVR = DIN SPEC 15801;
github.com/mvrdevelopment/spec). Fixture files: gdtf-share.com (~12k files, free
account, REST API). Realistic test venues: grandMA3 onPC (free) builds a rig and
exports MVR.

### 7.4 Projectors

A projector placement is the **projected image** on the stage plan, not the projector:
`physical_size_mm` = measured throw size, typed by the user (EDID prefill is
meaningless for projectors and is skipped). Everything else is identical — a projected
wall is an island like any panel. Consequence for priorities (Peter, 2026-07-02:
projectors are the likely primary rig — cheap large surfaces for small-scale artists):
**warp meshes and edge blending move to the front of the post-v1 queue** (§12).
4-corner keystone in the v1 advanced flap covers a flat, square-ish throw only; real
mapping (uneven surfaces, set pieces, overlapping projectors) needs warp + blend. The
per-output stage in §6.2 is where both slot in — they are output transforms, invisible
to content, requiring no change to islands or domains.

## 8. What it buys on stage

- A particle system flies off Totem L, crosses 3m of real air at real speed, lands on
  Totem R on beat — authored once, in physical space, with zero pixels spent on the
  air.
- Alternate-totem strobe = Display Mask × LFO. Chase = anything driven by Display Info
  centers. Mirrored totems = flip the layer to Every-display.
- Rearrange the venue, drag the stage view to match, content adapts — no re-authoring.
- Plug-in at the venue is two clicks (identity re-matching, §5), not a mapping session.
- A super-wide stage is free: cost follows the hardware you own, not the meters
  between it.

## 9. Performance

- **Pixel work = sum of display pixels.** Two portrait 1080×1920 totems ≈ 4.1MP ≈ 2×
  1080p — regardless of whether they're 1m or 40m apart. (The rejected v1 model hit
  14.5MP at native density for a 3m gap.)
- **Dispatch overhead × island count** (§6.1). Two islands is noise; the fusion
  compiler is the escape hatch for many-island stages. Baseline content render today
  is 4.5–5.5ms; the 4K-margin campaign owns the budget — this design's job is to keep
  cost proportional to owned hardware and visible in the stage view readout.
- **Density knobs:** per-island density defaults to the densest member display's
  native; the per-output advanced flap can cap it (LED walls with absurd processor
  modes), and `render_scale` still applies globally. These are quality knobs now, not
  survival knobs — gaps no longer create cost.
- **Present cost:** N fullscreen-triangle blits per tick — noise. Watch item: the
  known UI-present/content-GPU contention with three windows on one GPU.
- **Hot-path discipline:** stage derivation is per-action; the per-frame uniform build
  copies from a cached struct. No per-frame allocation.

## 10. Phasing (Sonnet-executable)

Each phase lands alone, is testable alone, and doesn't break single-display flow.

- **P1 — core model.** `stage.rs` (StageLayout, DisplayPlacement, identity, OutputId),
  `derive_stage` + unit tests (clustering/snap, rotation, packing, density, empty
  layout = legacy single island), serde defaults, EditingService commands. No behavior
  change with empty layout.
- **P2 — island rendering.** Atlas render target, per-island scissored execution loop,
  (node, island) state keying, layer `spatial_domain` field (Stage | EveryDisplay) +
  coordinate mapping. Single-island path must be provably identical to today
  (headless PNG diff on existing presets).
- **P3 — multi-output present.** Surface vec + in-flight counters + non-blocking
  acquire; per-output blit with region/rotation/trim/keystone; attach/detach; output
  window creation per placement ("Output" menu); identity matching + unassigned state.
  Test: two windows on one Mac (external monitor), skew accepted.
- **P4 — stage uniform + primitives.** StageUniform into frame globals +
  `wgsl_compute`; the three atoms with descriptors; gpu_tests for mask/stage_uv
  (value-level: exact rect edges), display_info is CPU (plain unit test).
- **P5 — stage view UI.** Arrangement panel (drag, snap-to-island with visible merge,
  numeric fields, EDID prefill, rotation, live pixel readout, assign picker), advanced
  flap (keystone, trim, density cap). Uses existing panel/scroll infra; headless PNG
  verification applies.
- **P6 — later.** LED strips *and DMX fixtures* become placements (manifold-led
  samples atlas regions / stage positions via the same model; add sACN alongside
  Art-Net), MVR/GDTF rig import, NDI/Syphon outputs, per-island export stems,
  rehearsal view.

Venue-profile export/import lands in P1 (it's serialization); test patterns +
Identify land in P3 (output windows exist there).

Full workspace test sweep gates P2 and P3 (graph runtime + present path = infra).

## 11. Decided — do not reopen

1. One composition; every output is a sampler of it. No per-display content pipelines.
2. **Islands, not a stage-sized canvas.** Contiguous pixels only where displays abut;
   packed atlas; gaps are never allocated or rendered. The v1 rendered-gap model is
   rejected (Peter, 2026-07-02): render cost must scale with owned hardware, never
   with stage width.
3. Everything derived from the stage layout — islands, packing, density, mappings.
   Users never hand-edit slices, crops, or px/mm.
4. Per-layer spatial domain: **Stage** (default) | **Every display**. The only new
   performer-facing concept; invisible on single-display projects.
5. Effects execute per-island with viewport scissor — today's shader semantics, zero
   shader changes; state keyed by (node, island). Cross-gap neighborhood bleed is
   intentionally impossible; abutting continuity is preserved by island merging.
6. Stage view = macOS-display-arrangement mental model, real units, EDID prefill,
   snap-to-merge islands, advanced flap closed by default.
7. Outputs present at independent cadence from the content thread's direct-present
   path. No new display links, no software frame-locking, no genlock.
8. Display awareness = one StageUniform + three atoms (`display_mask`, `stage_uv`,
   `display_info`). No new runtime, no new modulation path.
9. Display identity = CGDisplay UUID, name fallback, explicit unassigned state with
   one-click reassign. Never silently guess.
10. Master effects apply per-island like all effects, before per-output sampling.
    Per-output stage is region/rotate/keystone/trim/tonemap only — no content
    processing.
11. Non-blocking drawable acquire via in-flight counters; a full queue skips the
    output for that tick.
12. Export/recording = per-island in v1 (single-island projects: unchanged full-frame
    export).
13. Stage layout + assignments + calibration are exportable/importable as a standalone
    **venue file**; the composition never contains venue-specific data it can't shed.
14. Hot-replug with a matching UUID reattaches silently mid-show; manual assignment is
    for new hardware only.
15. Consoles: MANIFOLD is media server / position-aware color engine, never the
    lighting console. Cooperation = sync (timecode/OSC), sACN priority merge, MVR/GDTF
    import — in that order.

## 12. Open (deferred, not blocking)

- **Warp meshes + edge blending — FIRST post-v1 item** (raised from the bottom of this
  list per §7.4: projectors are the likely primary rig). Output-stage transforms in
  §6.2; no impact on islands/domains.
- **Camera-assisted auto-calibration** (after warp/blend exist as data): structured-
  light scan — project gray-code patterns, any camera watches, solve projector↔surface
  correspondence → warp mesh + edge blend + surface mask filled automatically. Proven
  tech (MadMapper spatial scanner, TD CamSchnappr), no ML required. Optional tiers on
  top: depth/segmentation models for scene understanding (shares ML-nodes infra +
  existing `DepthEstimator`), phone LiDAR venue scan to auto-populate the stage plan.
  Key property: calibration is a setup-time tool that *writes* the §6.2 data
  structures — never a runtime path. Natural MCP-driven flow.
- Pointwise-fusion of per-island loops into atlas-wide dispatches (many-island
  stages) — rides the existing fusion-compiler direction.
- Bezel compensation for abutting panels (island merge with dead-zone offsets).
- LED placement unification details (P6) — strip geometry on the stage plan.
- Multi-island export composites (stage-plan-arranged proxy video for offline review).
- **Rehearsal view** — audience-eye preview mode (islands + fixture points in a dark
  stage); author a show with zero hardware. Builds on the §6.2 preview compositing.
- **Per-output frame delay** — hybrid rigs mix device latencies (projector ≈ 1–3
  frames, LED processors vary); a delay offset per output re-syncs them. Costs
  buffered frames per output, so post-v1 — but the advanced flap reserves the slot.
- >8 displays/islands (bump the uniform capacities; layout allows it trivially).
