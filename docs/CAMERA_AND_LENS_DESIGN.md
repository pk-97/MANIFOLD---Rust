# Camera & Lens — one projection convention, one performable lens

**Status:** SHIPPED · P1 (`22530ac1`) + P2 (`de193e01`) landed 2026-07-12, main · designed 2026-07-12 · Fable 5
**Prerequisites:** none (REALTIME_3D P1–P3 shipped; camera atoms shipped)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.
**Companions:** [GBUFFER_DESIGN.md](GBUFFER_DESIGN.md) (stored depth this doc's lens consumers read) · [CINEMATIC_POST_DESIGN.md](CINEMATIC_POST_DESIGN.md) (DoF/motion-blur atoms that consume `LensParams`) · [RENDERING_INFRA_V2_DESIGN.md](RENDERING_INFRA_V2_DESIGN.md) (direction doc this graduates from, §6) · [REALTIME_3D_DESIGN.md](REALTIME_3D_DESIGN.md) (the scene pass; its Camera port is the convention's anchor)

The governing insight: MANIFOLD already has a canonical camera — the `Camera`
struct (`node_graph/camera.rs:42`) with right-handed `look_at_rh` view and
Metal-range `perspective_rh` projection — but only the *scene-renderer family*
speaks it. The wireframe path (`node.flatten_3d`) projects through bespoke
scale math with the opposite handedness, which is why the Scene 3
wireframe↔photoreal morph failed on five serially-patched convention
mismatches and was reverted (scene-ladder record, 2026-07-11). Peter,
2026-07-12: *"our projections and cameras use different conventions and
coordinate frames which makes aligning things and keeping everything
consistent nearly impossible between different rendering types."* This design
makes the existing convention **the law**, gives the wireframe path a Camera
port so both render families agree pixel-for-pixel, and adds the physical
lens (`focus`, `f-stop`, `shutter`, `exposure`) as a composable atom every
camera source can feed — Peter: *"all of the physical camera stuff."*

**What this explicitly does not reopen:** the two-renderer overlay morph
(rejected, scene-ladder 2026-07-11 — "overlaying two renderers = wrong
architecture"). Sharing a camera makes two renders *alignable and
consistent*; compositing them into one morph remains rejected.

Execution note (Peter's directive for this whole cluster, 2026-07-12): *"written
to a standard that a cheap Sonnet → Sonnet orchestration session can get them
all implemented in full without Sonnet needing to make judgement calls or by
looking at images."* Every gate in this doc is a numeric assertion (CPU oracle
vs GPU readback). **No PNG artifacts are produced by any phase** (Peter,
2026-07-12: *"No need to produce the PNGs if they're not going to look at
them"*) — acceptance is the numeric gates; Peter looks in-app when he
chooses. This overrides DESIGN_DOC_STANDARD §5's L2-demo minimum for this
cluster, by his explicit call.

## 1. Audit — what exists (verified 2026-07-12, tip `9e537b16`)

| Piece | Where | State |
|---|---|---|
| `Camera` struct: pos/fwd/right/up, near/far, `CameraMode` (persp fov_y / ortho half_height), cached RH view; `proj(aspect)`, `view_proj(aspect)` | `crates/manifold-renderer/src/node_graph/camera.rs:42-253` | Canonical. Aspect lives on the consumer (doc comment :15-19) |
| `look_at_rh` / `perspective_rh` (depth [0,1] Metal) / `mat4_mul` | `crates/manifold-renderer/src/generators/mesh_pipeline.rs:171-196` | The convention's math. `ortho_rh` lives in `camera.rs:279` |
| Camera emitters: `node.orbit_camera`, `node.free_camera`, `node.look_at_camera` | `primitives/camera_orbit.rs`, `free_camera.rs`, `look_at_camera.rs` | All conform (all build via the `Camera` builders) |
| Camera consumers, conforming: `node.render_scene` (required port, `cam.view_proj(aspect)` at `render_scene.rs:813`), `node.render_mesh`, `node.render_copies`, `node.flatten_to_camera_plane` (reads `cam.fwd` only) | respective primitives | Extend, don't redesign |
| Camera consumer, fused own math: `node.scatter_particles_camera` ("projects each particle through orthographic (with toroidal wrap) or perspective camera math", `scatter_particles_camera.rs:71`) | `primitives/scatter_particles_camera.rs` | VERIFIED-DIVERGENT (P1, 2026-07-12): its perspective branch (`scp_project` in `shaders/scatter_particles_camera_body.wgsl`) computes `dot(rel,right)/(view_z·aspect)` / `dot(rel,up)/view_z` — a tangent-plane projection via the camera's raw basis vectors, with NO `f = 1/tan(fov_y/2)` scale term (the comment says so explicitly: "ignores cam.fov_y ... implicit-FOV — basis vectors set the projection scale"). `Camera::proj`'s `perspective_rh` applies that `f` factor to both axes. The two agree only at the one `fov_y` where `f = 1` (90°); at any other `fov_y` this primitive's output is off by a constant multiplicative factor of `f` in both NDC axes vs. what `Camera::project_to_pixel` would compute for the same camera. Per this design: NOT changed — fluid-scatter's shipped presets tune their look against this exact math, and D3's precedent (Rejected: rewriting legacy modes in terms of `Camera`) applies here too. Revival trigger unchanged in §6. |
| NON-conforming projector: `node.flatten_3d` — no Camera port; `mode` ortho (`xy·proj_scale`) / perspective (`s = proj_dist/(proj_dist+z)`, **+z away = opposite handedness**), origin-centred pre-aspect output | `primitives/project_3d.rs:43-95`, body shader `shaders/project_3d_body.wgsl` | The gap this design closes |
| `node.draw_lines` screen mapping: `curve_to_screen(p) = (p.x/aspect + 0.5, p.y + 0.5)`, aspect = `rt_width/rt_height` | `primitives/shaders/render_lines.wgsl:63-66` | Fixed contract — flatten_3d's camera mode must target it |
| `node.project_4d` (4D→2D, own projection) | `primitives/project_4d.rs` | Out of scope: no 3D camera semantics for a 4D→2D map. Listed so nobody "fixes" it |
| Lens params | nowhere | Genuinely new: `LensParams` + `node.camera_lens` |
| Exposure application | nowhere (tone_map/reinhard_tone_map exist as 2D atoms but nothing camera-driven) | Genuinely new: EV term in `render_scene` |

Binding constraints checked (DESIGN_AUTHORING §1): hot path — camera math is
per-frame CPU, ~µs, no allocation (extends existing per-frame struct build);
thread — all content-thread, no new state; time model — untouched;
persistence — `Camera` is wire data, never serialized; new *params* serialize
on their atoms exactly like every existing param (no format change, no
migration); performance surface — focus/f-stop/shutter/EV are THE point:
port-shadowed scalars, bindable to MIDI/macros like any card param.

## 2. Decisions

**D1 — The MANIFOLD camera convention is the existing scene-family math,
now stated as law.** World space right-handed, +Y up. View = `look_at_rh`
(camera looks down −Z in view space; `fwd = target − eye`, normalized).
Projection = `perspective_rh` / `ortho_rh`, clip depth [0,1] (Metal), NDC x,y
∈ [−1,1] with +y up. One struct (`Camera`), one wire type
(`PortType::Camera`), aspect supplied by the consumer. Every node that maps
3D → screen consumes `PortType::Camera` and derives its projection from
`Camera::view` / `Camera::proj` — **bespoke per-node projection math is the
forbidden move of this design.** Rejected: introducing a new "Camera v2"
type or a second convention for 2D-flavored projectors — the struct already
does everything; the failure was consumers not using it.

**D2 — `Camera::project_to_pixel` is the committed CPU oracle.** One
canonical helper on the struct:

```rust
// camera.rs — THE reference for every conformance gate in this cluster.
// Returns None when the point is behind the near plane (clip.w <= 0).
pub struct PixelProjection {
    pub px: f32,          // pixel x in [0, width)
    pub py: f32,          // pixel y in [0, height), y-down (Metal viewport)
    pub ndc: [f32; 2],    // pre-viewport NDC, +y up
    pub depth: f32,       // clip-space depth in [0,1] (raw, non-linear)
    pub view_z: f32,      // linear view-space distance along -fwd (for CoC/SSAO)
}
impl Camera {
    pub fn project_to_pixel(&self, world: [f32; 3], width: u32, height: u32)
        -> Option<PixelProjection>;
}
```

`px = (ndc.x·0.5 + 0.5)·width`, `py = (1 − (ndc.y·0.5 + 0.5))·height` (Metal
rasterizes y-down). GPU paths are verified against this function, never
against each other or against a screenshot — this is what makes every gate
in the cluster executor-runnable without judgment. Precedent for the shape:
the value-level parity oracles of the `gpu_tests` suite (e.g.
`project_3d.rs::gpu_tests`).

**D3 — `node.flatten_3d` grows an optional `camera: Camera` port
(port-shadows-param, the house pattern already on its own `proj_scale`).**
Wired: project `view_proj(aspect_of?)` — no; flatten_3d outputs *pre-aspect*
curve space and doesn't know the target, so the camera mode emits:

```text
clip   = cam.proj(1.0) · cam.view · vec4(pos, 1)      // aspect 1: pre-aspect by construction
ndc    = clip.xy / clip.w                              // cull: clip.w <= near_eps → collapse to origin (matches inactive-slot convention)
out.xy = vec2(ndc.x · 0.5, S · ndc.y · 0.5)            // curve space: draw_lines does p.x/aspect + 0.5, p.y + 0.5
```

`S ∈ {+1, −1}` is the y-sign that makes the P1 pixel-parity gate pass — both
candidates are named here so the executor flips one constant, reruns the
numeric gate, and commits the passing value with a comment; no judgment, one
bit, decided by the test. (Derivation: draw_lines' `curve_to_screen` divides
x by aspect and offsets both by 0.5 into [0,1] screen space; feeding it
aspect-1 half-NDC lands the point at the same pixel `project_to_pixel`
computes, up to the y-flip the two paths' NDC→screen conventions may or may
not share. The gate, not prose, pins it.) Unwired: the existing `mode`/
`proj_scale`/`proj_dist` math **bit-identical** — the P1 negative gate is the
existing `gpu_tests` parity suite unchanged, so every shipped preset
(BlossomWire and family) renders byte-identically without migration.
Rejected: a new `node.project_camera` atom (splits the wireframe vocabulary
in two for no reason — flatten_3d IS the projection atom); rewriting the
legacy modes in terms of `Camera` (changes shipped-preset pixels; forbidden).

**D4 — `LensParams` rides the `Camera` struct; `node.camera_lens` is the
one writer.** The struct gains a lens block with a neutral default:

```rust
// camera.rs
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LensParams {
    pub focus_distance: f32,   // world units along fwd; <= 0 → hyperfocal/neutral
    pub f_stop: f32,           // aperture N; f32::INFINITY = pinhole (DoF off)
    pub shutter_angle: f32,    // degrees, 0..=360; 0 = no motion blur
    pub exposure_ev: f32,      // stops; 0 = neutral (scene rgb × 2^ev)
}
impl LensParams { pub const PINHOLE: Self = /* inf f_stop, 0 shutter, 0 ev, 0 focus */; }
pub struct Camera { /* existing fields */ pub lens: LensParams, }
```

Every existing builder sets `lens: LensParams::PINHOLE`, so all shipped
graphs behave identically (invariant I2). `node.camera_lens` is a pure CPU
atom, `camera: Camera in → out: Camera`, params `focus_distance` / `f_stop` /
`shutter_angle` / `exposure_ev`, **all four port-shadowed scalars** — that is
the performable lens: rack focus on a fader, shutter smear bound to a drop
macro. Consumers read the lens from the Camera wire (`coc_from_depth`,
`motion_blur` in CINEMATIC_POST; `render_scene` for EV). Rejected: lens
params as individual ports on every camera emitter (N×4 param duplication,
and a lens is conceptually one thing you insert in front of any camera);
lens params as params on the consuming post atoms only (breaks "one lens" —
focus would live in three places and drift; the atoms still port-shadow for
override, D5 in CINEMATIC_POST). Note `fov_y` stays owned by the camera
atoms — the lens does not zoom; DoF math derives focal length from the
camera's fov (CINEMATIC_POST §3), sensor model fixed at 24 mm vertical.

**D5 — Exposure applies in `render_scene`, scene-referred.** Each material
fragment entry multiplies its final STRAIGHT rgb (post-fog, post-emission,
pre-output) by `exp2(exposure_ev)`; alpha untouched (alpha contract). One new
uniform float in the spare `scene_params.z` slot (`render_scene.rs:174` —
currently zero-padded, so the uniform stays 320 B and the naga 16-byte rule
holds untouched). `ev = 0` must be byte-identical to today (gate). Rejected:
a separate 2D exposure atom as the primary path (the lens must act through
the camera wire to be "one lens"; a graph author can still use tone_map
math freely).

**Consequences, stated honestly:** flatten_3d's camera mode culls behind-camera
points by collapsing to origin, which draws a visible dot at screen centre for
lines with one culled endpoint if edges aren't filtered — accepted for v1
exactly as the legacy inactive-slot convention already does this; `Camera`
grows 16 bytes (~112 B total, still trivially Copy per wire per frame);
`exposure_ev` in `scene_params.z` burns the last spare scalar in that vec4 —
the next scene-wide scalar pays a 16 B uniform growth.

## 3. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| I1 — Every GPU projection path agrees with `Camera::project_to_pixel` within 1.0 px | `gpu_proofs::camera_conformance` (P1): render_scene rasterized probe + flatten_3d camera-mode readback vs oracle, asserted per-vertex |
| I2 — `LensParams::PINHOLE` cameras render byte-identically to pre-lens builds | P2 gate: existing `render_scene` gpu_tests + fog/shadow proofs pass unmodified (they construct cameras via builders, which default to PINHOLE) |
| I3 — flatten_3d unwired-camera output is bit-identical to today | existing `project_3d.rs::gpu_tests::generated_project3d_matches_hand_kernel_both_modes` passes with `shaders/project_3d.wgsl` and all its assertions unmodified; the test's `gen_bytes` packing updates mechanically to the new `Params` layout (PARAMS words → one zero word per derived word, `use_camera` inert → `dispatch_count` → pad) — amended 2026-07-12 (Fable review, P1 escalation) after the packing/derived-uniforms conflict below was found |
| I4 — No new bespoke projection math in any primitive | negative gate, landing: `rg -n 'proj_dist|proj_scale' crates/manifold-renderer/src/node_graph/primitives/ -g '*.rs'` hit-count == pre-phase count (re-derive at execution; new hits must be in flatten_3d only) |
| I5 — ev=0 byte-identity | P2 gate: `gpu_proofs::render_scene_fog` density-0 byte-identity test extended with `ev=0` assertion |

## 4. Phasing

### P1 — flatten_3d camera mode + the conformance oracle (one session)

**Entry state:** tip ≥ `9e537b16`; `cargo nextest run -p manifold-renderer --lib` green.
Re-verify anchors: `camera.rs:42` struct shape; `render_lines.wgsl:63` `curve_to_screen`
unchanged; `project_3d.rs:43` port list.
**Read-back:** this doc §2 D1–D3 whole; `docs/ADDING_PRIMITIVES.md` §codegen-path;
`project_3d.rs` + `project_3d_body.wgsl` end-to-end; `camera.rs` end-to-end.
Restate: the forbidden moves, the S-sign procedure, why unwired must be bit-identical.
**Deliverables:**
- `Camera::project_to_pixel` + `PixelProjection` in `camera.rs`, unit-tested
  against hand-computed values (center point → center pixel; known 45° fov
  point → hand-derived pixel; behind-camera → None).
- `camera: Camera optional` port on `node.flatten_3d` + camera branch in
  `project_3d_body.wgsl` (mode stays the enum; camera wire overrides both
  modes — port-shadows-param). Derived uniforms: the camera's view matrix
  rows + proj params resolved CPU-side, same derived-field pattern as
  `flatten_to_camera_plane.rs:84` (`derived_uniforms`). Codegen path stays —
  the branch is pure per-element math (`fusion_kind: Pointwise` unchanged).
- `gpu_proofs::camera_conformance`: (a) 5 known world points →
  flatten_3d(camera) → draw_lines as dots → readback → each dot's measured
  centroid within 1.0 px of `project_to_pixel`; (b) the same 5 points as
  tiny triangles through `render_scene` (unlit, white) → readback centroids
  within 1.0 px of the oracle; (c) therefore both paths agree with each
  other. Centroid = intensity-weighted mean over the readback buffer —
  arithmetic, not eyeballing.
- `scatter_particles_camera` projection-math conformance note (VERIFY-AT-IMPL
  from §1) appended to this doc's audit table.
**Gate:** `cargo test -p manifold-renderer --features gpu-proofs camera_conformance`
green; I3's existing parity test green — `git diff` on `shaders/project_3d.wgsl`
= 0 lines and the test's assertion block (lines ~284-327 pre-phase) unchanged;
only the `gen_bytes` packing (the hand-constructed generated-layout bytes) may
differ, per the amended I3 machine-check above. Focused
`cargo nextest run -p manifold-renderer --lib`; clippy `-p manifold-renderer`.

**Amendment 2026-07-12 (Fable review, P1 escalation):** D3's derived-uniforms
extension and I3's original "0-line diff on the test file" gate are mutually
exclusive — the freeze codegen (`codegen.rs:834-880`) always places
`derived_uniforms` fields immediately before the injected `dispatch_count`
word, so any new derived field moves `dispatch_count`'s byte offset and the
test's hardcoded `gen_bytes` buffer misfeeds it. Ruling: the "0-line diff" was
a proxy for "legacy math untouched," not a literal freeze — the test's
byte-packing is scaffolding, not the oracle. Pin `Project3D::DERIVED_UNIFORMS`
as, in order: `"cam_right:vec3", "cam_up:vec3", "cam_fwd:vec3", "cam_pos:vec3",
"proj_f", "cam_near", "use_camera:u32"` (15 words: vec3 expands to 3 scalars
per codegen.rs:852-862; basis+pos = the view rows, `proj_f` = 1/tan(fov_y/2)
at aspect 1, `cam_near` = the cull epsilon). Rebuild `gen_bytes` as: (1) `mode`
u32, `proj_scale` f32, `proj_dist` f32, `active as f32` — unchanged, 4 words;
(2) 15 zero words (60 zero bytes) — the derived camera block, correctly zero
since `use_camera = 0` makes it dead; (3) `CAPACITY` u32 — `dispatch_count`;
(4) no pad needed, 4+15+1 = 20 words ≡ 0 mod 4 per codegen.rs:877 (80 bytes
total). If the derived-field list drifts during implementation, recompute
packing as: PARAMS words in order → one zero word per derived word →
`dispatch_count` → pad to a 4-word multiple — nothing else changes. The hand
layout, `shaders/project_3d.wgsl`, and every assertion in the test stay
byte-for-byte unchanged. Rejected: reordering freeze codegen so
`dispatch_count`'s offset never moves — a cross-cutting change touching every
other primitive's `derived_uniforms` usage and every hand-packed gpu_test,
far outside a one-primitive phase, for a stability guarantee nothing needs
yet.
**Demo:** none — L1 (cluster-wide no-PNG rule, header note).
**Performer gesture:** orbit-camera `orbit` param on an LFO — wireframe and
solid views rotate in lockstep.
**Forbidden moves:** changing legacy-mode math or its shader lines · a new
projection atom · touching draw_lines' `curve_to_screen` · adapters that
post-scale one path to match the other (if the parity gate fails, the
mapping constant is wrong — fix the mapping, don't calibrate it away).

### P2 — LensParams + node.camera_lens + EV in render_scene (one session)

**Entry state:** P1 landed. Re-verify: `render_scene.rs` uniform struct still
320 B with `scene_params.zw` spare; the three camera builders' call sites.
**Read-back:** this doc D4–D5; `render_scene.wgsl` fs entries (all four);
`docs/ADDING_EFFECTS_AND_GENERATORS.md` atom-registration checklist.
**Deliverables:** `LensParams` + `PINHOLE` + field on `Camera` (all builders);
`node.camera_lens` atom (CPU pass-through, four port-shadowed params, full
descriptor + picker + summary); EV multiply in all four fs entries reading
`scene_params.z`; unit tests (lens defaults neutral; camera_lens writes lens
fields; port-shadow precedence); gpu_proofs: unlit white quad at `ev = 1.0`
reads back 2.0× the `ev = 0.0` value within f16 tolerance, and `ev = 0`
byte-identical to a build-of-record readback (I5).
**Gate:** focused nextest + the render_scene gpu_proofs modules green;
clippy; negative: `rg 'exposure' crates/manifold-renderer/src -g '*.wgsl'`
hits only `render_scene.wgsl`.
**Demo:** none — L1 (cluster-wide no-PNG rule, header note).
**Performer gesture:** `exposure_ev` on a fader — whole scene dips to black
and blooms back without touching any light.
**Forbidden moves:** applying EV in tone-map atoms "as well" (parallel path) ·
lens fields on emitter atoms · a second Camera-like struct · growing the
uniform beyond 320 B.

## 5. Decided — do not reopen

1. One `Camera` struct, one convention (D1); no Camera2D, no per-node math.
2. flatten_3d keeps legacy modes bit-exact forever unwired; camera wire is
   the only new behavior (D3).
3. Lens = struct on Camera + one writer atom (D4); fov stays on the camera.
4. EV applies in render_scene, scene-referred, alpha untouched (D5).
5. The y-sign S is decided by the P1 gate, then frozen as a named constant.
6. Two-renderer overlay morphs stay rejected (scene-ladder 2026-07-11).

## 6. Deferred

- **project_4d camera semantics** — revive only if a 4D piece needs to sit
  in a 3D scene (no known want).
- **scatter_particles_camera conformance rewrite** — revive when fluid
  content needs to composite against render_scene geometry; requires its own
  parity plan (shipped-content pixels).
- **Sensor model as a param** (sensor height fixed 24 mm) — revive if Peter
  asks for anamorphic/crop looks; one constant → param, no structural change.
- **Lens breathing / distortion** (focus changing fov, barrel distortion) —
  post-release candy; a `wgsl_compute` user effect can prototype it.
