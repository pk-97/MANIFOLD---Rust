# Stored G-buffer — depth (and velocity) leave the tile

**Status:** SHIPPED · P1 (`560c59fd`) + P2 (`390d58dc`) landed 2026-07-12, main · designed 2026-07-12 · Fable 5
**Prerequisites:** none for P1 (render_scene P1–P3 shipped). P2 needs P1.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.
**Companions:** [RENDERING_INFRA_V2_DESIGN.md](RENDERING_INFRA_V2_DESIGN.md) §2 (the keystone framing this graduates) · [REALTIME_3D_DESIGN.md](REALTIME_3D_DESIGN.md) (§3 already commits lazy `depth`/`world_normal` outputs; §10 records the memoryless rationale) · [CAMERA_AND_LENS_DESIGN.md](CAMERA_AND_LENS_DESIGN.md) (the oracle + lens the consumers read) · [CINEMATIC_POST_DESIGN.md](CINEMATIC_POST_DESIGN.md) (the consumers) · [PERF_BUDGET_GATE_DESIGN.md](PERF_BUDGET_GATE_DESIGN.md) (owns the bandwidth measurement)

The keystone decision of the cinematic cluster (RENDERING_INFRA_V2 §2): scene
depth today is memoryless MSAA tile memory that never reaches RAM — one line,
`depth.setStoreAction(MTLStoreAction::DontCare)` at
`crates/manifold-gpu/src/metal/encoder.rs:764`. Storing it unlocks depth of
field, SSAO, motion blur, and depth-driven grading as ordinary 2D graph
work, where MANIFOLD's image pipeline is already strong. Peter, 2026-07-12:
*"stored depth G-buffer (keystone), physical camera with DoF first slice,
motion blur, and SSAO."* His scene profile shapes the priorities: *"pure
black backgrounds with the models I have as the main focus with some
lighting and effects on them"* — hero-object scenes where per-pixel depth
feeds focus pulls and contact shading.

**What this does NOT reopen:** the depth-aware two-pass compositor, rejected
by name in REALTIME_3D §10 and re-rejected in RENDERING_INFRA_V2. Storing
depth ≠ compositing renderers: the outputs designed here are post-process
inputs for ONE `render_scene`'s image, not a cross-renderer depth exchange.

Execution note (Peter, 2026-07-12, governs all phases): gates are numeric —
CPU oracle vs GPU readback — *"without Sonnet needing to make judgement
calls or by looking at images."* No PNG artifacts are produced by any phase
(his call, same day: *"No need to produce the PNGs if they're not going to
look at them"*).

## 1. Audit — what exists (verified 2026-07-12, tip `9e537b16`)

| Piece | Where | State |
|---|---|---|
| Memoryless MSAA color+depth, resolve-out color only | `render_scene.rs:384-408` (`ensure_msaa_targets`, `create_texture_msaa_memoryless`), `encoder.rs:733-804` (`draw_instanced_depth_msaa_batch`: color `MultisampleResolve`, depth `DontCare`) | The seam. Extend, don't redesign |
| Lazy G-buffer outputs precedent: `render_mesh` `world_pos`/`world_normal` — output rendered ONLY when wired (`ctx.outputs.texture_2d(..)` is `None` unwired) | `render_3d_mesh.rs:87,358-361,568-591` | The lazy rule, already named by REALTIME_3D §3 for render_scene |
| Per-output texture format override | `EffectNode::output_format`, `effect_node.rs:840`; pool keyed `(PortType, format)` `metal_backend.rs:78`; plan carries `resource_format` `execution_plan.rs:177` | Depth output declares `R32Float` with zero new infra |
| Per-object model matrices composed CPU-side each frame | `render_scene.rs:879-899` | Previous-frame matrices = a `Vec` kept on the node (velocity, P2) |
| Function constants in manifold-gpu | `docs/MANIFOLD_GPU_ARCHITECTURE.md` (function-constant pipeline specialization); `manifold-gpu-architecture` memory | Pipeline variant control for velocity (P2) |
| Ring-buffer discipline for CPU-written per-frame GPU data | `render_scene.rs:126` (`FRAMES_IN_FLIGHT = 3`) | Precedent if velocity needs per-frame CPU buffers (it doesn't — matrices ride uniforms) |
| MSAA depth resolve API | nowhere in manifold-gpu | Genuinely new: depth resolve attachment + filter (P1); MRT color attachment (P2) |

Binding constraints: hot path — this is per-frame GPU bandwidth, THE cost
(§2 honest-cost); thread — none (all content-thread GPU encoding);
persistence — none (outputs are wire textures; wiring serializes as ordinary
graph JSON); performance surface — indirect (the consumers are the
performable part, CINEMATIC_POST).

## 2. Decisions

**D1 — Lazy, not always-store.** `render_scene` grows output ports `depth`
(P1) and `velocity` (P2), *rendered only when wired* — the `render_mesh`
lazy rule, which REALTIME_3D §3 already commits for render_scene
(*"Outputs: color, plus lazy depth / world_normal … same lazy rule as
render_mesh"*). An unwired scene must not pay one byte of new bandwidth
(invariant I1). Rejected: always-store with a global toggle — pays the
full-res write on every scene that never wired a consumer, and a toggle is
config where wiring is already the graph's native intent signal.

**D2 — Depth output = raw device depth, `R32Float`, resolve filter
`Sample0`.** The pass gains a single-sample `R32Float` resolve target for
the depth attachment (`MTLStoreAction::MultisampleResolve` +
`MTLMultisampleDepthResolveFilter::Sample0`). Raw [0,1] clip depth, NOT
linearized in the pass: consumers linearize via ONE shared WGSL helper
(D4) — raw depth preserves full near-field precision and matches
`Camera::project_to_pixel().depth`, which is what makes the conformance gate
exact. `Sample0` because it is deterministic and equals what a
single-sample render would produce — `Min`/`Max` bias edges toward one
surface and can't be predicted by the CPU oracle without re-implementing
MSAA sample positions. Consumers sample with **nearest** filtering
(depth must never be bilinearly mixed across a silhouette; and R32Float
filterability is family-dependent — nearest sidesteps it entirely).
⚠ VERIFY-AT-IMPL (P1): confirm `objc2_metal` exposes
`setDepthResolveFilter` on the depth attachment descriptor — read
`MTLRenderPassDepthAttachmentDescriptor` bindings in the vendored crate; if
the enum name differs, transcribe, don't improvise.
Rejected: linear-depth output (loses precision, breaks oracle exactness);
f16 depth in an Rgba16Float wire (≈10-bit mantissa → visible CoC/SSAO
banding); a separate depth-only re-render pass (doubles geometry cost —
the resolve is free tile bandwidth by comparison).

**D3 — manifold-gpu API: one batch entry point grows optional G-buffer
attachments; the old signature stays as a thin forwarder.** Committed shape
(seam; interior free):

```rust
// crates/manifold-gpu/src/metal/encoder.rs
pub struct DepthMsaaPassDesc<'a> {
    pub msaa_color: &'a GpuTexture,
    pub resolve_target: &'a GpuTexture,
    pub msaa_depth: &'a GpuTexture,
    /// Some(tex) → depth attachment stores MultisampleResolve (Sample0)
    /// into this single-sample R32Float texture. None → DontCare (today).
    pub depth_resolve: Option<&'a GpuTexture>,
    /// Extra MRT color attachments (index 1..): (msaa_tex, resolve_tex).
    /// P2 uses one slot for velocity. Empty slice → exactly today's pass.
    pub aux_color: &'a [(&'a GpuTexture, &'a GpuTexture)],
    pub depth_stencil_state: &'a GpuDepthStencilState,
}
pub fn draw_instanced_depth_msaa_batch_desc(
    &mut self, desc: &DepthMsaaPassDesc, draws: &[DepthMsaaDraw], label: &str);
```

`draw_instanced_depth_msaa_batch` keeps its signature and forwards with
`depth_resolve: None, aux_color: &[]` — every other caller compiles
untouched, and the P1 negative gate proves no behavior change. Rejected: a
parallel second batch function (`_with_depth`) per attachment combination —
combinatorial API growth, and "parallel old path" is the forbidden move
this desc-struct shape exists to avoid.

**D4 — One WGSL linearize helper; near/far arrive via the Camera wire.**
`shared/depth.wgsl` (new, alongside the existing shared-header pattern):
`fn linearize_depth(raw: f32, near: f32, far: f32) -> f32` implementing the
exact inverse of `perspective_rh`'s depth mapping
(`mesh_pipeline.rs:171-180`: `range = far/(near−far)`, so
`view_z = (range·near)/(raw + range)` — the doc commits this formula; the
unit gate checks it against `Camera::project_to_pixel().view_z` at 5 depths).
Consumers (CINEMATIC_POST atoms) take a `camera: Camera` port and read
`near`/`far` from it CPU-side into their uniforms — no new convention, the
Camera wire is already how every 3D consumer gets camera facts.
**Forbidden by name:** re-deriving the linearization inline in any atom
(synthesis-drift; `feedback_synthesis_drift` is the memory, the shared
header is the fix).

**D5 — Velocity output (P2) = camera + rigid-object motion only, declared
honestly.** `velocity: Texture2D` (`Rg16Float`), NDC-space motion per pixel:
`vel = ndc_now.xy − ndc_prev.xy` computed in the vertex shader from
`prev_view_proj · prev_model` vs current, interpolated, written by fragment.
`render_scene` keeps per-object `prev_model` and scene `prev_view_proj` as
plain node state (first frame: prev = current → zero velocity, correct).
**Consequence, stated honestly:** GPU-deformed vertex buffers (deform/curve
atoms write positions per frame) contribute NO per-vertex motion — a
deforming surface blurs only by its rigid transform + camera. Fixing that
requires previous-frame vertex positions (deform runs twice or caches
output — RENDERING_INFRA_V2 §2 names the unmeasured cost); it is Deferred
with its trigger, not silently absorbed. Pipeline variants: velocity write
is a **function constant** (`EMIT_VELOCITY`), cache key grows to
`(MaterialKind, velocity_on)` — 8 entries max, no shader-file fork.
Rejected: a separate velocity geometry pass (re-runs all draws); computing
velocity in post from depth (screen-space reprojection is camera-only —
loses object motion entirely, worse than what D5 ships).

**D6 — `world_normal` output: ABI reserved, not built.** SSAO v1
reconstructs normals from depth (CINEMATIC_POST D3), so nothing in this
cluster consumes a normal attachment. The aux-MRT slot mechanism (D3) is
the reservation; the output ships when a consumer exists (SSR/true-normal
SSAO — trigger in Deferred). Rejected for now: building it "while we're in
there" — an output nothing reads is scope widening with a permanent
bandwidth invoice.

**Consequences of the whole design, stated honestly:** a wired depth output
adds a full-res R32Float resolve write (+ the consumer's read): ~33 MB/frame
at 4K ≈ 2 GB/s at 60 fps — affordable on Apple silicon per the direction
doc, but it is a permanent line item that PERF_BUDGET_GATE (approved, not
built) must measure; until the gate exists, the lazy rule (D1) is the cost
control. `Sample0` depth at MSAA-4 means post effects see one sample's
geometry at silhouettes — faint edge shimmer under motion is possible in
DoF/SSAO; accepted for v1, revisit trigger = Peter seeing it on a real
scene. Velocity's rigid-only honesty is D5's.

## 3. Invariants & enforcement

| Invariant | Machine check |
|---|---|
| I1 — Unwired G-buffer outputs cost zero: pass byte-identical to today | P1 gate: existing `gpu_proofs::render_scene_*` suites pass unmodified; plus new unit test asserting the pass desc chooses `DontCare`/no-aux when outputs unwired (descriptor-level, no GPU needed) |
| I2 — Stored depth equals the oracle | `gpu_proofs::gbuffer_depth_conformance`: known meshes at 5 depths → depth readback at their `project_to_pixel` pixels equals oracle `.depth` within 1e-5 |
| I3 — Linearize helper is the exact `perspective_rh` inverse | unit test (CPU): `linearize_depth(project_to_pixel(p).depth) == view_z` within 1e-4, 5 points — the WGSL and Rust implementations share the committed formula; the GPU side is covered by I2 + CINEMATIC_POST's CoC gate |
| I4 — Old batch signature behavior unchanged | negative gate: `rg -n 'draw_instanced_depth_msaa_batch\(' crates/` call-site count unchanged except render_scene; forwarder is `#[inline]`, one body |
| I5 — First-frame velocity is zero, motion is finite | P2 gate: two-frame readback, frame 0 velocity == 0 exactly; frame 1 velocity of a translated object equals CPU-computed NDC delta within 1e-4 |

## 4. Phasing

### P1 — depth resolve end-to-end (one session)

**Entry state:** tip carries CAMERA_AND_LENS P1 (`Camera::project_to_pixel`
exists — the gate needs it). Re-verify anchors: `encoder.rs:733` batch fn,
`render_scene.rs:384` msaa targets, `effect_node.rs:840` `output_format`.
**Read-back:** this doc §2 D1–D4; REALTIME_3D §10 (why memoryless — restate
why this design doesn't reopen the compositor); `render_3d_mesh.rs:358-361`
lazy-output shape; `encoder.rs:721-804` end-to-end.
**Deliverables:**
- `DepthMsaaPassDesc` + `draw_instanced_depth_msaa_batch_desc` in
  manifold-gpu; old fn forwards (D3). Depth resolve wired per D2
  (`Sample0`; the VERIFY-AT-IMPL objc2 check first).
- `depth` output port on `render_scene` (`output_format` → `R32Float`);
  single-sample resolve texture allocated alongside the msaa pair in
  `ensure_msaa_targets` ONLY when the output is wired this frame.
- `shared/depth.wgsl` linearize helper + Rust twin + I3 unit test.
- `gpu_proofs::gbuffer_depth_conformance` (I2) + I1 descriptor test.
**Gate:** `cargo test -p manifold-renderer --features gpu-proofs
gbuffer_depth` green; full render_scene gpu_proofs suite green UNMODIFIED;
`cargo nextest run -p manifold-renderer --lib` + `-p manifold-gpu`; clippy
`-p manifold-gpu -p manifold-renderer`. Negative: I4 rg gate.
**Demo:** none — L1 (Peter, 2026-07-12: no PNG artifacts anywhere in this
cluster; the I2 conformance readback is the acceptance).
**Performer gesture:** none this phase (infrastructure) — the gesture ships
with CINEMATIC_POST P1 (focus fader).
**Forbidden moves:** a second batch function per attachment combo · storing
linear depth · linearizing inline anywhere · rendering the depth output via
an extra geometry pass · touching `render_mesh`'s private G-buffer path.

### P2 — velocity output + MRT aux attachments (one session)

**Entry state:** P1 landed. Re-verify: pipeline cache keying in
`render_scene.rs` (`pipelines: AHashMap<MaterialKind, _>` at :206), function
constants exist in manifold-gpu (`MANIFOLD_GPU_ARCHITECTURE.md` §function
constants — re-read, don't recall).
**Read-back:** this doc D5; `render_scene.wgsl` `vs_main` whole;
FREEZE_COMPILER_MAP is NOT in scope (render nodes are compiler-exempt IO —
restate why).
**Deliverables:** aux MRT slot in the pass desc exercised; `velocity`
output (`Rg16Float`, `output_format` override); `prev_model` Vec +
`prev_view_proj` on the node; `EMIT_VELOCITY` function constant + cache key
`(MaterialKind, bool)`; I5 gpu_proof; I1 re-proof (velocity unwired =
byte-identical, suite unmodified).
**Gate:** as P1's shape; plus I5. Test scope: focused; the full-workspace
sweep runs at landing per the scope rule.
**Demo:** none — L1 (cluster no-PNG rule; the I5 two-frame velocity readback
is the acceptance).
**Performer gesture:** none (consumed by CINEMATIC_POST motion blur).
**Forbidden moves:** velocity via post reprojection · a velocity geometry
pass · shader-file fork per variant (function constant, not a second wgsl) ·
attempting deform-atom velocity (Deferred, trigger below).

## 5. Decided — do not reopen

1. Lazy per-wire storage; no global store toggle, no quality-tier gating
   (Peter dropped tiers 2026-07-12: *"Ignore the explicit per-scene export
   quality please"*).
2. Raw device depth, R32Float, Sample0 resolve, nearest sampling (D2).
3. One desc-struct batch API; old signature forwards (D3).
4. Near/far reach consumers on the Camera wire; one shared linearize
   helper (D4).
5. Velocity = camera + rigid motion v1, function-constant variant (D5).
6. Depth-aware cross-renderer compositing stays rejected (REALTIME_3D §10).

## 6. Deferred

- **Deform-atom velocity** (previous-frame vertex positions; deform-twice
  or cached-output cost) — trigger: a scene where rigid-only blur visibly
  breaks on a deforming hero mesh, AND perf-gate numbers exist to price it.
- **`world_normal` MRT output** — trigger: SSR design, or SSAO quality
  escalation after Peter sees reconstructed-normal AO on a real scene.
- **MetalFX temporal upscaling / RT denoise inputs** — post-release
  (RENDERING_INFRA_V2 §9); they consume exactly the depth+velocity ABI this
  doc ships, which is why the formats are committed now.
- **render_mesh depth output parity** — trigger: anyone wiring DoF onto the
  single-object renderer; same D2/D3 recipe, separate small phase.
