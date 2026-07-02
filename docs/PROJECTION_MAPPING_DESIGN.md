# Projection Mapping — Warp, Masks, Slices, Edge Blend

**Status: APPROVED design, not built · 2026-07-02 · Fable queue #11**
**Prerequisites: MULTI_DISPLAY_DESIGN P1–P3 (stage model, island rendering, multi-output present).**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any
phase. Conformance-hardened: this executes AFTER multi-display reshapes the present path —
every claim about the per-output blit and `content_pipeline.rs` is `⚠ VERIFY-AT-IMPL`
against the as-built multi-display code. Run the §8.3 pre-flight first.**

Expands the "first post-v1 item" from `docs/MULTI_DISPLAY_DESIGN.md` §12. Mapping is an
**output transform in the per-output present pass** (§6.2 there) — invisible to content,
zero impact on islands, domains, or shader semantics. That architectural slot was decided
in multi-display (#10: per-output stage is region/rotate/keystone/trim/tonemap only);
this doc fills it in.

First customer: Peter's rig — 2× Optoma GT2000HDR portrait (fixed 0.496:1 lens, no zoom,
no lens shift, 360° mount). The projector has zero optical adjustments: **MANIFOLD's
warp is the lens.**

---

## 1. Decisions

- **D1 — Mapping is a present-pass output transform.** Never a content pass, never an
  island change. Content renders rectilinear into islands exactly as designed; each
  output's present draw does the geometry. Blend included: a blended two-projector wall
  is **one island** — the two outputs sample *overlapping source regions* of it.
  Overlapping **islands** never exist; the overlap is rendered once and sampled twice.
- **D2 — `Vec<Slice>` from day one; single-slice UI in v1.** (Peter: "sure.") The data
  model carries multi-slice (one projector onto three set-piece boxes); the v1 panel
  exposes exactly one identity slice per output. No schema migration later.
- **D3 — Corner-pin is a true homography with projective interpolation.** Not bilinear,
  not two affine triangles (both produce the classic diagonal-seam distortion). Grid
  warp is Catmull-Rom — *interpolating*, because calibration nudges a point and the
  surface must pass through it (Bézier control points float off-surface; wrong tool).
- **D4 — Blend is fully specced now, implemented last.** (Peter: "sure.") Manual ramps
  first; auto-derivation from same-island source overlap after.
- **D5 — Mapping data lives in the venue profile**, keyed by display identity
  (multi-display #13: the composition never contains venue data it can't shed). Plug in
  at the venue, placements re-match, the warp comes back.
- **D6 — Calibration input: mouse handles + arrow-key nudge in P1; MIDI encoder nudge
  later.** Peter: "mapping with a controller would probably be super useful and allow
  you to fine tune it a bit better than by mouse but +- nudge buttons also work.
  Doesn't need to be high priority." Encoder nudge ships as bindable actions in P4.
- **D7 — All mapping edits are EditingService commands** (undoable; drags coalesce
  start→end into one entry). Mesh/mask caches rebuild **per-action, never per-frame**;
  buffers pre-allocated at max grid size — no per-frame allocation on the present path.
- **D8 — Test patterns render in the present pass**, replacing the content sample — they
  work with no project loaded, which is exactly the venue-morning situation.

---

## 2. Data model

Lives in the venue profile beside `DisplayPlacement`; all fields serde with defaults
(house convention).

```rust
/// Per output. Default = one identity slice → byte-identical to the plain blit.
pub struct OutputMapping {
    pub slices: Vec<Slice>,
}

pub struct Slice {
    /// Island-space source rect. Defaults to the output display's own region.
    /// Must lie within one island (validated; UI clamps). A blended wall is one
    /// island, so blend slices satisfy this trivially.
    pub source_rect_px: RectF,
    pub warp: WarpMesh,
    pub masks: Vec<MaskPoly>,
    pub blend: EdgeBlend,
    pub opacity: f32,        // default 1.0
    pub enabled: bool,       // default true
}

pub enum WarpMesh {
    /// Output-space positions of the source rect's 4 corners. Homography.
    CornerPin { corners: [Vec2; 4] },
    /// K×L control points in output space, UV = uniform parameter grid over
    /// source_rect. Catmull-Rom surface through the points.
    Grid { cols: u8, rows: u8, points: Vec<Vec2> },   // 2..=17 per axis, default 5×5
}

pub struct MaskPoly {
    pub points: Vec<Vec2>,   // output space, ≥3
    pub feather_px: f32,     // default 0
    pub invert: bool,        // default false = mask hides inside
}

pub struct EdgeBlend {
    pub edges: [BlendEdge; 4],   // left, right, top, bottom
}

pub struct BlendEdge {
    pub width_px: f32,           // 0 = off (default)
    pub gamma: f32,              // ramp curve, default 1.0 (see §5)
}
```

`CornerPin` stays a distinct variant (not a 2×2 grid): it needs projective
interpolation, which the tessellated grid path deliberately doesn't do.

---

## 3. Warp math

**Corner-pin (homography).** Solve the 8-DOF projective transform mapping the unit
square to the four output corners (standard quad-to-quad DLT / adjugate form; pure CPU,
per-action). Render one quad whose vertices carry homogeneous texture coordinates
`float3 uvq = (u·w, v·w, w)`; the fragment does `uv = uvq.xy / uvq.z`. Hardware
perspective-correct interpolation then yields the exact projective mapping — no
diagonal seam, no tessellation needed.

**Grid warp (Catmull-Rom).** Control points are positions the surface passes through.
Per edit action (CPU): evaluate the Catmull-Rom patch on an 8×8 tessellation per cell →
vertex buffer of (output_pos, uv), uv affine over the source rect. Per frame: one
indexed mesh draw. Worst case 16×16 cells × 64 verts ≈ 16k vertices — trivial.

**Density change never loses calibration:** switching K×L resamples the *current
surface* at the new parameter grid (evaluate old patch at new knots), so the picture
doesn't move — the handles do. Same rule for CornerPin → Grid promotion (grid initialized
from the homography image of the parameter grid).

**Resolution honesty:** warping resamples; heavy warps eat pixels at 1080p. Sampling is
bilinear from the island atlas (already filterable); nothing else to do — physics.

---

## 4. Present-pass integration

Multi-display §6.2 defines the per-output present draw (sample atlas region → rotate →
keystone → trim → tonemap). Mapping **replaces the keystone hook** with the general
path:

```
for slice in mapping.slices.iter().filter(enabled):
    draw slice mesh (fullscreen quad for CornerPin, cached mesh for Grid)
      fragment: sample island atlas within source_rect (clamped)
                × mask texture
                × blend ramps
                × opacity
                → existing color trim / tonemap chain
```

- **Fast path preserved:** empty mapping or single identity slice (no warp, no masks,
  no blend, opacity 1) takes today's plain blit — byte-identical, verified by PNG diff
  (mirrors the multi-display P2 acceptance style).
- Multiple slices = a few draws appended to the same CB. Present stays one cheap pass.
- **Calibration drag:** rebuilds happen per mouse-move *action* on the UI/edit side
  (CPU tessellation into a pre-allocated staging vec, upload). The content tick never
  waits; a mid-rebuild frame just presents the previous mesh.
- Rotation (portrait outputs) composes before warp exactly as §6.2 orders it — Peter's
  portrait rig is rotation + corner-pin, both in the one draw.

**Masks** rasterize per-action into a cached R8 half-res mask texture per output
(polygon fill + separable blur for feather); the fragment multiplies. Rect masks are
4-point polygons — one code path.

---

## 5. Edge blend

Applied as a multiplier in the present pass's **linear-light domain** (the pipeline is
Rgba16Float linear through present; the OS applies the display transfer):

```
factor(t) = t ^ gamma        // t: 0 at the blended edge, 1 at width_px inward
```

With two projectors adding light linearly, `gamma = 1.0` sums to uniform luminance in
theory; real projectors deviate (lens falloff, non-ideal EOTF), so gamma is exposed
per edge (0.5–3.0) and you tune it looking at the wall. This is the honest version of
every mapping tool's "blend curve" knob.

- **v1 (P4): manual.** Set `width_px` + gamma per edge, eyeball a gray test pattern.
- **Auto-derive (post-P4):** when two outputs' slices sample overlapping source regions
  of the same island, the overlap extent — mapped through each slice's warp — gives the
  ramp widths. Writes the same `EdgeBlend` fields; manual remains the override.
- **Black-level compensation deferred:** projectors can't show true black, so the
  overlap zone glows in dark scenes; compensation lifts non-overlap regions to match.
  Needs measurement to do honestly — documented, not built (§8).

---

## 6. Calibration UX

New **Mapping** dock panel (editor surface; usable while content runs — that's the
point). Per output:

- **Canvas** shows the output's frame (what the projector shows) with handles drawn on
  top. Tabs: **Warp / Masks / Blend**.
- **Warp:** 4 handles (corner-pin) or K×L handles (grid). Click to select; drag with
  mouse; **arrow keys nudge 1px, Shift+arrows 10px, Option+arrows 0.1px**. Grid density
  stepper (resample rule §3). Tab cycles selected point.
- **Masks:** click-to-place polygon points, drag to adjust, feather slider, invert.
- **Blend:** per-edge width + gamma sliders.
- **Test patterns** per output: alignment grid (100px lines, border, center cross,
  output name + resolution), solid white, color bars. Plus Identify (multi-display P3).
- **MIDI encoder nudge (P4):** two bindable relative actions — *nudge selected point
  X/Y* — through the existing binding/mapping system. An encoder detent = one fine
  step. Low priority per D6; the binding layer already exists, so it's small.

Venue save/restore is implicit: mapping edits mutate the venue profile through
commands; the venue file round-trips it (multi-display #13).

---

## 7. Phasing (Sonnet-executable)

Each phase lands alone. Present path = infrastructure → **full workspace test sweep
gates every phase that touches it (P1, P2, P4).**

- **P1 — Corner-pin end to end.** Data model (full `Vec<Slice>` serde + defaults),
  homography solve + projective sampling in the present pass, fast-path preservation
  (PNG diff), test patterns, Mapping panel v1 (single slice, 4 handles, nudge keys),
  venue persistence + undo commands.
  *Acceptance: align a 2-projector portrait rig, relaunch, mapping restores; unmapped
  projects byte-identical.*
- **P2 — Grid warp + masks.** Catmull-Rom tessellation cache, density resample,
  CornerPin→Grid promotion, polygon masks + feather + mask cache.
- **P3 — Multi-slice.** Slice list UI, per-slice source-rect editor over the island
  region, per-slice opacity/enable. (Set pieces: one projector, N boxes.)
- **P4 — Blend + encoder nudge.** Manual ramps + gamma, gray test pattern, bindable
  nudge actions. Auto-derive from same-island overlap as the follow-up inside P4.

---

## 8. Decided — do not reopen

1. Mapping is a per-output present transform. No content processing, no island or
   domain changes (multi-display #10 upheld).
2. Blended wall = one island; outputs overlap in *source regions*, never islands.
3. `Vec<Slice>` schema from day one; v1 UI is single-slice.
4. Corner-pin = homography with projective interpolation. Grid = interpolating
   Catmull-Rom, tessellated per-action, cached.
5. Density/mode changes resample the surface — calibration never visibly moves.
6. Mapping data lives in the venue profile, undoable via EditingService commands.
7. Per-frame present cost = mesh draw(s) + texture multiplies; all rebuilds per-action;
   no per-frame allocation.
8. Blend ramps multiply in linear light, per-edge width + gamma; manual before auto.
9. Calibration: mouse + keyboard nudge first; MIDI encoder nudge is P4, bindable
   actions through the existing binding system (Peter: useful, not high priority).

Deferred (not blocking): camera-assisted auto-calibration (multi-display §12 — it
*writes* these structures; natural MCP flow), black-level compensation, per-slice color
trim, bilinear-warp toggle for stretched-fabric surfaces.
