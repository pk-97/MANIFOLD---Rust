# render_scene Unbounded Lights — lights move from a fixed uniform array to a storage buffer

**Status:** SHIPPED 2026-07-10 (Opus) — P1 complete, merged to main @ `310800ed`
(ancestry verified against origin/main 2026-07-16). Lights now ride `@binding(8) var<storage, read>`; `MAX_LIGHTS` deleted;
`LIGHT_SLIDER_MAX = 64` soft bound; uniform 400→272. Proven: 12/12 render_scene unit tests
(incl. `lights_generalize_well_past_the_old_cap_of_4`), naga validates the shader across all three
lit entry points, gpu-proofs `fragment_storage` (D7) green, and a NEW gpu-proofs
`render_scene_lights` binary — an 8-light plane (lights 0–3 red, 4–7 green) renders green-dominant
(read as a PNG: lights past index 3 reach binding 8 in the real depth-MSAA batch path), and a
zero-light plane renders finite with no validation error (D4). Original design · 2026-07-06 · Fable 5
**Prerequisites:** none hard, but same-file co-claimants (coherence audit F3, 2026-07-10):
SCENE_BUILD_AND_GROUP_PARAMS P2 and GAUSSIAN_SPLATS P4 also edit `render_scene.rs`'s
`rebuild`/`evaluate`. Independent of everything in flight in the sense that no other
design's *output* gates this one — but the three must be **sequenced, never concurrent**
(`docs/DESIGN_BUILD_ORDER.md` §2 recommends this phase first, smallest). Whichever of the
three lands later re-derives this doc's `render_scene.rs`/`.wgsl` line anchors before editing.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting the phase.

`render_scene`'s object count is already uncapped (the old cap was a naming artifact);
light count is still hard-capped at 4 because the light data is baked into a fixed-size
uniform array on both sides of the ABI. This design removes the last structural cap in
the primitive: lights become a runtime-sized storage buffer, the slider cap becomes a
soft editor bound exactly like `objects`. On stage: a scene can carry a venue's worth
of light sources — one per stab, one per performer, one per LED zone — and the number
is a performance decision, not an engine constant.

Scope is deliberately one primitive + its shader. Everything else (Light port type,
sun/point derivation, material response) is untouched.

## 1. Audit — what exists (verified 2026-07-06)

| Piece | Where | State |
|---|---|---|
| `MAX_LIGHTS = 4` + `LIGHT_NAMES` static table | [render_scene.rs:68](../crates/manifold-renderer/src/node_graph/primitives/render_scene.rs#L68), :76 | delete both |
| `lights: [[f32;4]; MAX_LIGHTS*2]` in `RenderSceneUniforms` | render_scene.rs:112 | remove field |
| `size_of == 400` static assert | render_scene.rs:115 | becomes 272 |
| `rebuild()` port list: `LIGHT_NAMES[..n]`, `PortType::Light`, optional | render_scene.rs:176-183 | switch to `Cow::Owned(format!("light_{i}"))` — the `mesh_{i}` pattern at :187 |
| `lights` ParamDef, range `(0.0, MAX_LIGHTS)` | render_scene.rs:216-220 | range becomes `(0.0, LIGHT_SLIDER_MAX)` |
| `reconfigure()` clamp to `MAX_LIGHTS` | render_scene.rs:538-542 | clamp to `LIGHT_SLIDER_MAX` (objects precedent :536) |
| `evaluate()` light collection into fixed array; packing `[−dir, 1.0]` / `[color, 0.0]` | render_scene.rs:563-569 | becomes `Vec<[f32;4]>` push, packing IDENTICAL |
| `build_uniforms(..., lights)` | render_scene.rs:473-513 | drop the `lights` param + field |
| Draw-loop bindings: `Bytes{binding:0}` uniform, `Buffer{binding:1}` verts, textures 2–7 | render_scene.rs:732-761 | add `Bytes{binding:8, data:lights}` |
| Shader `Uniforms.lights: array<vec4<f32>, 8>`; three fragment loops `u.lights[i*2u]`, count from `u.scene_params.x` | [render_scene.wgsl:86](../crates/manifold-renderer/src/node_graph/primitives/shaders/render_scene.wgsl#L86), :183, :229, :285 | array moves to `@binding(8) var<storage, read>`; loops repoint; count source unchanged |
| Vertex-stage storage buffer already shipping in this pipeline | render_scene.wgsl:87 (`@binding(1) var<storage, read> verts`) | proof the render pipeline handles `var<storage>` in the VERTEX stage |
| `GpuBinding::Bytes` on render pipelines → `setVertexBytes` + `setFragmentBytes`, slot-map indexed, missing slot skipped (`continue`) | [encoder.rs:1223-1240](../crates/manifold-gpu/src/metal/encoder.rs#L1223-L1240) | the binding mechanism; stripped-binding safe |
| Fragment-stage `var<storage, read>` precedent | [tests/gpu_proofs/fragment_storage.rs](../crates/manifold-renderer/tests/gpu_proofs/fragment_storage.rs) | **PROVEN 2026-07-06** (was: none shipped — `blob_overlay_render.wgsl`'s storage array is a `@compute` kernel). Isolated proof: uniform@0 + `var<storage>`@8, both `Bytes`-backed, read per-pixel from a fragment entry point, byte-correct through SPIRV-Cross → MSL. See D7. |
| Objects generalization (the sibling change) | shipped on main (feat/render-scene-generalize) | the Rust-side template for every naming/cap change here |

Extend, don't redesign: every change above is the objects change replayed on the
lights axis, plus one new GPU binding.

## 2. Decisions

- **D1 — Lights live in `@group(0) @binding(8) var<storage, read> lights: array<vec4<f32>>`.**
  Two `vec4`s per light, packing byte-identical to today (`[−dir, 1.0]`,
  `[color, 0.0]` — render_scene.rs:568-569). `light_count` stays in
  `scene_params.x`. **Rejected: `arrayLength(&lights)`** — the SPIRV-Cross
  buffer-size-index class of bug (see `project_compute_arraylength_buffer_size_index`
  memory: silently returns 0 when the size table misaligns) buys nothing here; the
  count already flows through the uniform.
- **D2 — Bind with `GpuBinding::Bytes`, not a `GpuBuffer`.** The house pattern for
  small per-draw data (the uniform itself is `Bytes` at binding 0). Metal `setBytes`
  caps at 4KB → 127 lights; D3's slider cap keeps us at half that. **Rejected:
  per-frame `GpuBuffer`** — allocation/pooling machinery for ≤2KB of data; revive
  only via the Deferred item.
- **D3 — `LIGHT_SLIDER_MAX = 64`** (soft editor bound, mirrors
  `OBJECT_SLIDER_MAX = 64` at render_scene.rs:64). 64 × 32B = 2KB, half the
  `setBytes` ceiling. The comment on the const must state the 4KB/127 hard ceiling
  so nobody raises it past 127 without switching to a buffer.
- **D4 — Zero lights still binds one zeroed `[f32;4; 2]` entry.** The loop never
  reads it (`light_count == 0`), but the fragment functions declare the buffer and
  Metal validation must always see it bound. **Forbidden: skipping the binding when
  the count is 0** — that is the silent-fallback shape of this design.
- **D5 — Uniform shrinks 400 → 272 bytes.** Assert updated; WGSL `Uniforms` drops the
  array. 272 = 17 × 16, so the naga 16-byte uniform rule holds.
- **D6 — Port names go dynamic**: delete `LIGHT_NAMES`, emit
  `Cow::Owned(format!("light_{i}"))` in `rebuild()` — exactly the `mesh_{i}` shape
  one loop below it. Serialized wire names are unchanged (`light_0`… already), so
  **no project migration**: old projects load as-is, their `lights` value (≤4) is
  inside the new range.
- **D7 — The once-unproven mechanic is now PROVEN in isolation (2026-07-06).**
  At design time no fragment entry point in the repo had ever read a `var<storage>`
  buffer through the SPIRV-Cross → MSL render path. The isolated proof now ships:
  `gpu_proofs::fragment_storage::fragment_reads_storage_buffer_via_bytes_binding` —
  uniform@0 + storage@8, both `GpuBinding::Bytes`, fragment-read, byte-correct
  (run: `cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs
  fragment_storage`). The phase still runs the render_scene ≤4-light pixel-parity
  gate FIRST — no longer as a mechanic probe, but as the refactor-correctness proof.
  A parity failure now indicts the render_scene change itself, not the platform:
  debug the change; the uniform-array-64 fallback is dead and must not be reached for.

## 3. Design body

Rust (`render_scene.rs`), all inside the existing shapes:

```rust
const LIGHT_SLIDER_MAX: u32 = 64; // soft UI bound; setBytes hard ceiling = 127 (4KB / 32B)

// evaluate(): replaces the fixed lights_uniform array
let mut light_data: Vec<[f32; 4]> = Vec::with_capacity(lights_n * 2);
// … push [−dir, 1.0] / [color, 0.0] per wired light_{i}, count as today …
if light_data.is_empty() {
    light_data.extend([[0.0; 4]; 2]); // D4: validation-safe zero entry
}
```

The draw loop's `bindings` array (render_scene.rs:732-761) gains, unconditionally:

```rust
GpuBinding::Bytes { binding: 8, data: bytemuck::cast_slice(&light_data) },
```

`RenderSceneUniforms` loses `lights`; `build_uniforms` loses the parameter; the
`const _` assert becomes 272.

WGSL (`render_scene.wgsl`): remove `lights` from `Uniforms`; add

```wgsl
@group(0) @binding(8) var<storage, read> lights: array<vec4<f32>>;
```

and in `fs_phong` / `fs_pbr` / `fs_cel`, replace `u.lights[i*2u]` / `u.lights[i*2u+1u]`
with `lights[i*2u]` / `lights[i*2u+1u]`. Loop bounds and everything else untouched.
`fs_unlit` has no light loop; if the compiler strips binding 8 from that pipeline the
encoder's missing-slot `continue` makes the unconditional binding a no-op (verified,
encoder.rs:1224-1226).

**The plausible-wrong turns, by name:** you will want `arrayLength(&lights)` instead
of `scene_params.x` — no (D1). You will want to skip binding 8 when there are no
lights — no (D4). You will want to keep `MAX_LIGHTS` "for safety" alongside the new
path — no; the cap is deleted, the slider bound is the only limit (D3).

**Consequences, stated honestly:** light data moves from the uniform (constant
address space) to device storage — for ≤64 lights × three lit entry points this is
noise against the fragment work, but it has never been measured in this repo because
the mechanic has never shipped (D7's probe is also the perf sanity check: the parity
gate would catch a pathological regression as a timing anomaly in the GPU test run).
And binding 8 becomes load-bearing for three of four pipelines — any future binding
renumbering in this shader must keep the slot map and the `bindings` array in step.

## 4. Phasing — one phase

**P1 (one session): the whole change.**
- **Entry state:** clean main; re-verify the audit anchors (`rg -n "MAX_LIGHTS" render_scene.rs`,
  the :115 assert, wgsl :86/:183/:229/:285). A moved anchor = re-audit, not guess.
- **Read-back:** this doc §2–§3; restate D1, D4, D7 and the three forbidden turns
  before writing code.
- **Order:** shader + binding change FIRST, run the D7 probe (below), then the Rust
  cap/naming deletions, then tests.
- **Gate (mechanical):**
  1. `cargo test -p manifold-renderer --features gpu-proofs render_scene` —
     the existing parity/gpu tests must pass with **pixel-identical output for the
     previously-representable range (≤4 lights)**. This is the D7 probe and the
     refactor-correctness proof in one.
  2. A new port-rebuild unit test asserting `light_{LIGHT_SLIDER_MAX-1}` exists and
     `light_{LIGHT_SLIDER_MAX}` doesn't (mirror of the objects test at
     render_scene.rs:858-864).
  3. A zero-light GPU frame (unwire all lights) renders without validation errors —
     the D4 proof. And one frame at >4 lights (e.g. 8 wired) renders non-black —
     the actual feature proof.
  4. `cargo test -p manifold-renderer --features gpu-proofs --test gpu_proofs`
     (alpha sweep + generator smoke) and `cargo clippy --workspace -- -D warnings`.
- **Exit:** committed on a `feat/` branch, landed per GIT_TREE_DISCIPLINE §2;
  NODE_CATALOG regenerated if it records the lights range.

## 5. Decided — do not reopen

1. Storage buffer at binding 8, count in `scene_params.x` (D1).
2. `Bytes` binding, 64 soft cap, 127 hard ceiling documented (D2/D3).
3. Zero lights binds a zero entry — never skip (D4).
4. No project migration; wire names unchanged (D6).
5. Fragment-storage mechanic is probe-first, escalate-on-failure (D7).

## 6. Deferred

- **>64 lights** — swap the `Bytes` arm for a pooled `GpuBuffer` (no shader change;
  `array<vec4<f32>>` is already runtime-sized). Trigger: a real patch wants more
  than 64, or light data outgrows 4KB.
- **Per-light falloff/type (point/spot)** — different design (Light port type
  change, material response); this doc only moves storage.
