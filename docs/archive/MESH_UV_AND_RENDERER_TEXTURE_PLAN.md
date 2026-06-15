# Mesh UV + Renderer Texture Sampling — Plan

**Status:** Draft, awaiting commit. Written after the MetallicGlass Material-system migration shipped with screen-space normal sampling, which produced a screen-locked texture pattern on the rasterized mesh (the texture didn't move with the surface as the camera orbited). The renderer is supposed to be a generic 3D camera; silently accepting broken texture inputs is a footgun for every future mesh-based scene. This plan replaces the broken approach with the industry-standard "sample at mesh UV in tangent / world space" pattern that TouchDesigner, Unreal, Unity, and Blender all use.

**Companion docs:** [`MATERIAL_SYSTEM_DESIGN.md`](MATERIAL_SYSTEM_DESIGN.md), [`MANIFOLD_GPU_ARCHITECTURE.md`](MANIFOLD_GPU_ARCHITECTURE.md), [`ADDING_PRIMITIVES.md`](ADDING_PRIMITIVES.md).

---

## 1. Goal

Make `node.render_3d_mesh` and `node.render_instanced_3d_mesh` a correct general 3D camera that supports per-pixel surface textures (normal map, roughness map) using **mesh UV** (the surface's own parameterization), the way every production rendering engine does it. Two consequences:

1. The renderer's existing `material` / `light` / `envmap` contract continues to work for ANY mesh that provides per-vertex position + normal — vertex-normal-driven forward shading, no surprises.
2. When a preset wants per-pixel detail (e.g. a procedural height-field driving MetallicGlass), it wires a normal-or-height map into the renderer and the renderer samples it at each fragment's interpolated mesh UV — texture sticks to the surface as the camera moves.

**Out of scope for this plan:** tangent-space normal mapping with per-vertex tangents. That's a future extension — call it the "authored normal map" tranche. The MetallicGlass case (and any procedural height-field) needs only world-space normals, which work without tangents on flat or axis-aligned-base surfaces.

---

## 2. Why screen-UV sampling failed

The legacy MetallicGlass uses **deferred shading**: `render_3d_mesh` produced a screen-space `world_pos` G-buffer; downstream `cook_torrance_specular` + `equirect_envmap_sample` sampled BOTH `world_pos` AND the screen-space normal map (from `heightmap_to_normal`) at the SAME screen UV. They agreed because both producers were screen-space-aligned. This is the classic GPU deferred-shading pipeline.

The Material-system bundled renderer is a **forward shader**. Each fragment represents real world-space geometry. Sampling a texture at screen UV gives a value that has nothing to do with where the fragment lies on the actual surface — so the texture appears locked to the screen rather than the geometry. As the camera orbits, the rasterized mesh shape rotates but the lit pattern stays still. Conceptually wrong.

The correct fix is to sample surface-bound textures at **mesh UV**, an interpolated per-fragment value that tells the fragment "where am I on the parametric surface". Each fragment of the same surface texel maps to the same texel regardless of camera position.

---

## 3. Core decisions

1. **Add a `uv: [f32; 2]` channel to `MeshVertex`.** Position + normal + UV is the standard mesh vertex shape — every pro engine has at least this much per vertex. Total grows from 32 → 32 bytes (with packing) or 48 bytes (with padding); see §4 for the packing decision.

2. **All current mesh producers populate UV.** Each producer's UV mapping is intrinsic to the geometry it generates — grid uses (col/cols, row/rows); cube uses per-face UV unwrap; polytope/wireframe uses a parametric (s, t). No new authoring burden on preset authors.

3. **All current mesh consumers preserve UV.** `displace_mesh`, `triangulate_grid`, `rotate_3d`, `project_3d` copy UV unchanged when they pass MeshVertex through. `render_3d_mesh` / `render_instanced_3d_mesh` use UV for surface texture sampling.

4. **No tangent yet.** Tangent-space normal mapping (`baked Substance Painter normal map on a cube`) is a follow-up. For procedural height fields (MetallicGlass), world-space normal maps work without tangent because the surface basis is implicit in the world coordinate frame.

5. **Texture inputs declared at the renderer level, not on Material.** Same shape as the broken migration: `render_3d_mesh.normal_map`, `render_3d_mesh.roughness_map` (optional Texture2D inputs). The Material wire carries CPU-only surface params (kind + base colour + metallic/roughness scalars); textures stay alongside the geometry inputs on the renderer.

6. **Normal-map convention: world-space SIGNED RGB.** Matches what `heightmap_to_normal` already produces in `WorldYUp` mode. `texture.rgb` is the normal vector; renderer normalizes after sampling. Tangent-space normal maps (which would need `(N * 2 - 1)` decode + TBN transform) ship in the future tranche.

7. **Validator changes: none.** The new optional inputs are declared as `Texture2D optional` — same shape as `envmap`. The existing required-input + conditional-requirement infrastructure handles them.

---

## 4. `MeshVertex` layout

**Chosen layout (48 bytes, padded for std430 alignment):**

```rust
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub _pad0: f32,
    pub normal: [f32; 3],
    pub _pad1: f32,
    pub uv: [f32; 2],
    pub _pad2: [f32; 2],   // 16-byte tail for std430 / Metal compatibility
}
const _: () = assert!(std::mem::size_of::<MeshVertex>() == 48);
```

Matching WGSL:

```wgsl
struct Vertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
};
```

**Why 48-bytes-with-pad rather than packing tighter:**
- Keeps the same vec4-aligned access pattern as today's 32-byte layout — predictable for both Rust and WGSL.
- 16-byte tail (`_pad2`) is the standard slot for adding `tangent: [f32; 4]` later without another layout change.
- Cost: ~50% more memory per vertex (~12MB for a 500×500 grid instead of 8MB). Fine at MANIFOLD's scale.

**`ItemKind::MeshVertex` stays.** The kind tag is semantic ("this is a 3D mesh vertex") and unchanged. Only the size changes. Every `Array(MeshVertex)` declaration via the `primitive!` macro automatically picks up the new size from `KnownItem::ITEM_KIND` + `size_of::<MeshVertex>()`. Wire validation matches on `(item_size, item_align, item_kind)` triples — both producer and consumer regenerate from the same Rust source-of-truth, so they agree.

**Backward-compatibility note:** every saved preset that mentions `Array<MeshVertex>` survives the change because validation reads the live struct size, not a JSON-baked value. The first time the validator runs after this change, every MeshVertex wire is re-validated against the new 48-byte size; consumer and producer agree because both come from the same Rust type. **No JSON edits required for the layout change itself.**

---

## 5. UV mapping per producer

Every existing MeshVertex producer needs to write UV. Conventions:

### `generate_grid_mesh`
Flat grid of `(cols × rows)` vertices spanning XZ. Natural UV is the grid index normalized to `[0, 1]`:

```
uv = (col / max(cols - 1, 1), row / max(rows - 1, 1))
```

This is the load-bearing case: `displace_mesh` reads the height texture at THIS UV per-vertex to derive the position offset, and the renderer samples normal_map / roughness_map at the SAME UV per-fragment. Perfect 1:1 alignment between displacement source, normal map, and roughness map.

### `generate_cube_mesh`
6 faces, each its own (s, t) ∈ [0, 1]. Conventional cube unwrap:

```
+X face: uv = (1 - z_norm, 1 - y_norm)
-X face: uv = (z_norm,     1 - y_norm)
+Y face: uv = (x_norm,     z_norm)
-Y face: uv = (x_norm,     1 - z_norm)
+Z face: uv = (x_norm,     1 - y_norm)
-Z face: uv = (1 - x_norm, 1 - y_norm)
```

(Where `*_norm = coord * 0.5 + 0.5`.) Matches standard cubemap unfold; textures sample without seams on each face. Sphere / capsule / other authored shapes can ship their own UV mappings when the corresponding generators are added.

### `polytope_vertices`
Output is a flat vertex buffer for line rendering, not lit surfaces. UV can be:
- (vertex_index / total, 0) — minimal placeholder; lit polytope rendering isn't a thing today.

This is the only producer where UV isn't load-bearing. Stub with placeholder; revisit if a lit-polytope use case arises.

### Future producers
Any new MeshVertex producer declares its UV mapping in its primitive `purpose` field. The macro pattern `Array(MeshVertex)` requires the producer to write a full vertex; UV is part of "full".

---

## 6. Transform primitives — pass UV through unchanged

These read a MeshVertex array, modify position/normal/orientation, and write a MeshVertex array. UV gets copied verbatim:

- **`displace_mesh`** — reads height per-vertex, offsets `position` along normal direction. UV unchanged. The existing implementation reconstructs UV from `vertex_index + cols/rows` to sample the height texture; can SIMPLIFY by reading `vertex.uv` directly (post-change).
- **`triangulate_grid`** — rearranges quads into triangle list; positions/normals/UVs copy through.
- **`rotate_3d`** — rotates positions and normals by Euler angles; UV is a parametric value, doesn't rotate.

Each WGSL shader's `Vertex` struct grows by 16 bytes; the copy paths add `out.uv = v.uv;`.

---

## 7. Renderer integration

### `render_3d_mesh` (and `render_instanced_3d_mesh`)

**Re-introduce the optional texture inputs:**

```rust
inputs: {
    vertices: Array(MeshVertex) required,
    camera:   Camera            required,
    material: Material          required,
    light:    Light             optional,
    envmap:   Texture2D         optional,
    normal_map:    Texture2D    optional,
    roughness_map: Texture2D    optional,
}
```

**WGSL `VsOut` gains `uv`:**

```wgsl
struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos:    vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv:           vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let v = verts[vid];
    var out: VsOut;
    out.clip_pos     = u.view_proj * vec4<f32>(v.position, 1.0);
    out.world_pos    = v.position;
    out.world_normal = v.normal;
    out.uv           = v.uv;
    return out;
}
```

**`fs_pbr` (and analogously `fs_phong`, `fs_cel`) sample at `in.uv` not screen UV:**

```wgsl
var N: vec3<f32>;
if u.texture_flags.x > 0.5 {
    // World-space signed normal; renormalise post-sample.
    N = normalize(textureSampleLevel(normal_map, sampler, in.uv, 0.0).rgb + vec3<f32>(1e-8));
} else {
    N = normalize(in.world_normal);
}

var roughness: f32;
if u.texture_flags.y > 0.5 {
    roughness = max(textureSampleLevel(roughness_map, sampler, in.uv, 0.0).r, 0.01);
} else {
    roughness = max(u.pbr_metallic_roughness.y, 0.01);
}
```

**Uniform shape:** drop `viewport_flags.xy` (no longer needed — screen UV derivation is gone). Replace with `texture_flags: vec4<f32>` carrying just the `(use_normal_map, use_roughness_map, _, _)` flags. Net uniform size: ~208 bytes → 192 bytes (back to the pre-broken-migration shape, with `texture_flags` replacing the old reserved field).

**Pipeline cache + bindings:** unchanged — per-MaterialKind pipeline already in place; the new texture bindings reuse the existing `envmap_sampler`. Naga's per-entry-point reflection still drops unused texture args for `fs_unlit` (which references neither normal_map nor roughness_map).

**G-buffer outputs:** unchanged. `world_pos` and `world_normal` still ship as a side channel for legacy deferred-shading consumers.

---

## 8. MetallicGlass migration (post-fix)

The actual full migration the screen-UV attempt was reaching for. Once the infrastructure above is in place, the same JSON rewrite plays out cleanly — just at the right surface:

- Delete `cook_torrance_specular` (id 53), `equirect_envmap_sample` (id 54), `mix Add` (id 55), `reinhard_tone_map` (id 56). Drop their wires.
- Add `node.pbr_material` (id 62) with `metallic=1.0`, `roughness=0.05`, `base_color=(0.8, 0.8, 0.82, 1.0)`, `ambient=0`.
- Add `node.light` (id 63, Sun mode, `pos=(-2, 2, 5)`, `aim=(0, 0, 0)`, `intensity=3.5`).
- Wire: `pbr.out → 50.material`, `key_light.out → 50.light`, `bake_equirect_envmap.envmap → 50.envmap`, `heightmap_to_normal.out → 50.normal_map`, `roughness_from_edge.out → 50.roughness_map`. Then `50.color → final_output.in`.
- Rebind outer-card sliders: `light_int → key_light.intensity`, `roughness → pbr.roughness + roughness_from_edge.offset`.

Expected visual deltas vs the legacy deferred-shading version (all minor):
- No reinhard tone-map post-pass — `pbr_material` writes linear; canvas composite handles SDR. If colors feel hot, add a `tone_map` enum to `pbr_material` as a one-line follow-up.
- No per-pixel inverse-square attenuation — Sun light has uniform intensity across the surface; legacy applied `1 / (1 + d²/25)` ≈ 0.43× at this distance scale. Default `light_int` value updates to compensate (~3.5 → ~1.5 in the slider default).
- Cook-Torrance math bit-identical modulo reassociation (same D_GGX × G_Smith × F_Schlick × IBL split that already shipped in the renderer's fs_pbr).

**Per-pixel detail now correct.** The normal_map is sampled at the mesh UV, which corresponds 1:1 to the height texture's UV (because the grid mesh's UV is the grid index normalized — same coordinate space). Texture detail follows the surface as the camera orbits.

---

## 9. Implementation tranches

### Tranche U1 — `MeshVertex` shape change + producers populate UV

**Goal:** every mesh in the system carries valid UV; nothing samples it yet.

- `mesh_common.rs`: extend struct, bump assertion to 48 bytes.
- `generate_grid_mesh.rs` + `.wgsl`: write `uv = (col/cols, row/rows)`.
- `generate_cube_mesh.rs` + `.wgsl`: write per-face UV per §5.
- `polytope_vertices.rs` + `.wgsl`: write placeholder `uv = (i/total, 0)`.
- WGSL `Vertex` struct updates in `generate_grid_mesh.wgsl`, `generate_cube_mesh.wgsl`, `polytope_vertices.wgsl`.
- `cast_array.rs` (cast_as_mesh_vertex): the cast input declares byte buffer + size; size assertion now matches the new layout.
- Verify: `cargo test -p manifold-renderer --lib node_graph::primitives::generate_grid_mesh::`, same for cube + polytope. `cargo run -p manifold-renderer --bin check-presets` — every preset that uses a MeshVertex wire re-validates against the new size.

**~1-1.5 hours.** Mechanical but every shader file is hand-touched.

### Tranche U2 — Transform primitives pass UV through

**Goal:** UV survives every existing MeshVertex transform path.

- `displace_mesh.rs` + `.wgsl`: copy `v.uv` into output; simplify the height-texture lookup to use `v.uv` directly (drops the `cols/rows`-based UV reconstruction).
- `triangulate_grid.rs` + `.wgsl`: copy UV.
- `rotate_3d.rs` + `.wgsl`: copy UV (parametric, doesn't rotate with position).
- WGSL `Vertex` struct updates in the three shaders.
- Verify: existing tests for these primitives + visual A/B on MetallicGlass (still using legacy deferred-shading; this tranche is invisible to the rendered output).

**~1 hour.**

### Tranche U3 — `render_3d_mesh` per-pixel texture sampling at mesh UV

**Goal:** the renderer correctly samples normal_map / roughness_map at the surface's UV.

- `render_3d_mesh.rs`: re-add `normal_map` + `roughness_map` optional Texture2D inputs. Update `MaterialRenderUniforms` — drop the viewport-derived `viewport_flags`, add a 16-byte `texture_flags` (use_normal_map, use_roughness_map, 0, 0). Bind the new textures (or `dummy_envmap` for unwired).
- `render_3d_mesh.wgsl`: VsOut gains `uv: vec2<f32>` at `@location(2)`; vs_main passes it through. fs_pbr / fs_phong / fs_cel sample normal_map and roughness_map at `in.uv` using the same `envmap_sampler`. Drop the screen-UV math.
- `project_3d.rs` + `.wgsl`: `Vertex` struct must match — even though it doesn't sample UV, naga rejects mismatched layouts.
- Tests: existing `render_3d_mesh` surface tests stay; add a gpu_test that renders a known mesh with a known normal_map and verifies the lit pixel uses the texture's normal at the right UV.

**~1.5 hours.**

### Tranche U4 — `render_instanced_3d_mesh` same treatment

**Goal:** instanced renderer matches.

- Mirror U3 changes in the instanced renderer + WGSL (the `Vertex` struct is the same; `Instance` is its own thing).
- Tests: same surface + parity strategy.

**~0.5 hours.**

### Tranche U5 — Full MetallicGlass migration

**Goal:** the visible MetallicGlass image comes from the bundled PBR pipeline; legacy deferred-shading nodes deleted.

- Edit `MetallicGlass.json` per §8.
- Default slider values may need a tweak to match the legacy look (light_int ~1.5 to compensate for no attenuation).
- Verify: `check-presets`, visual A/B against legacy, smoke test the outer-card sliders.

**~0.5 hours.**

**Total: ~5 hours of focused work.**

---

## 10. File-by-file inventory

Each tranche's file list, ready for the edit pass:

### Tranche U1 (struct + producers)
- `crates/manifold-renderer/src/generators/mesh_common.rs`
- `crates/manifold-renderer/src/node_graph/primitives/generate_grid_mesh.rs`
- `crates/manifold-renderer/src/node_graph/primitives/generate_cube_mesh.rs`
- `crates/manifold-renderer/src/node_graph/primitives/polytope_vertices.rs`
- `crates/manifold-renderer/src/node_graph/primitives/cast_array.rs` (size assertion only)
- `crates/manifold-renderer/src/node_graph/primitives/shaders/generate_grid_mesh.wgsl`
- `crates/manifold-renderer/src/node_graph/primitives/shaders/generate_cube_mesh.wgsl`
- `crates/manifold-renderer/src/node_graph/primitives/shaders/polytope_vertices.wgsl`

### Tranche U2 (transforms)
- `crates/manifold-renderer/src/node_graph/primitives/displace_mesh.rs`
- `crates/manifold-renderer/src/node_graph/primitives/triangulate_grid.rs`
- `crates/manifold-renderer/src/node_graph/primitives/rotate_3d.rs`
- `crates/manifold-renderer/src/node_graph/primitives/shaders/displace_mesh.wgsl`
- `crates/manifold-renderer/src/node_graph/primitives/shaders/triangulate_grid.wgsl`
- `crates/manifold-renderer/src/node_graph/primitives/shaders/rotate_3d.wgsl`

### Tranche U3 (render_3d_mesh)
- `crates/manifold-renderer/src/node_graph/primitives/render_3d_mesh.rs`
- `crates/manifold-renderer/src/node_graph/primitives/shaders/render_3d_mesh.wgsl`
- `crates/manifold-renderer/src/node_graph/primitives/project_3d.rs` (layout match only)
- `crates/manifold-renderer/src/node_graph/primitives/shaders/project_3d.wgsl`
- `crates/manifold-renderer/src/node_graph/primitives/shaders/project_4d.wgsl` (if it reads MeshVertex — check)

### Tranche U4 (render_instanced_3d_mesh)
- `crates/manifold-renderer/src/node_graph/primitives/render_instanced_3d_mesh.rs`
- `crates/manifold-renderer/src/node_graph/primitives/shaders/render_instanced_3d_mesh.wgsl`

### Tranche U5 (MetallicGlass)
- `crates/manifold-renderer/assets/generator-presets/MetallicGlass.json`

---

## 11. Test discipline

**Per tranche:**
- `cargo check -p manifold-renderer --lib --tests`
- `cargo test -p manifold-renderer --lib <module_path>` for the primitives touched
- `cargo clippy -p manifold-renderer --lib -- -D warnings`
- `cargo run -p manifold-renderer --bin check-presets` after the U5 JSON edit

**Per-primitive parity tests** (when behaviour changes shape, not just layout):
- `displace_mesh` — gpu_test verifies UV-driven height lookup matches the legacy `cols/rows`-reconstructed one to ≤1 ULP per vertex.
- `render_3d_mesh` — gpu_test renders a small mesh with a known checkerboard normal_map; verifies the lit pixel's RGB matches a CPU-derived expectation.

**No workspace runs per tranche.** Batch `cargo test --workspace` at the end of U5.

**Visual A/B for MetallicGlass:** required before merging U5 — load the preset, orbit the camera, confirm the texture pattern sticks to the surface (not the screen) and the overall look reads as a metallic surface in a 3D scene.

---

## 12. Extension points (forward-looking)

Each piece of the v1 design has a deliberate seam for future work:

### Tangent-space normal mapping
The `_pad2: [f32; 2]` tail on `MeshVertex` is the slot for `tangent: [f32; 3] + _pad: f32`. When a real use case arrives (authored normal maps on cube / sphere geometry), extend:
1. Replace the pad with `tangent`. Layout stays 48 bytes.
2. WGSL `Vertex` struct grows correspondingly.
3. Producers compute tangents from UV gradients per face.
4. `fs_pbr` etc. add a path: when normal_map is tangent-space (a new flag or a new MaterialKind extension), unpack `(N * 2 - 1)`, transform via `TBN = mat3(T, cross(N, T), N)`, use the result.

### Additional surface texture inputs
- `base_color_map` (Texture2D) — per-pixel albedo; multiplied into `material.base_color`.
- `metallic_map` (Texture2D) — `.r` overrides `material.metallic` per pixel.
- `ao_map` (Texture2D) — ambient occlusion; multiplied into the diffuse + ambient term.
- `emission_map` (Texture2D) — per-pixel emission; added to `material.emission`.

Each is the same shape as `normal_map` / `roughness_map`: optional Texture2D input on the renderer, sampled at mesh UV in the fragment shader, conditioned on a presence flag. No new infrastructure.

### Multi-UV (UV0 / UV1)
For lightmap workflows or detail texturing, MeshVertex grows a second UV channel. Could fit into 64 bytes (`position + normal + uv0 + uv1 + tangent`). Future expansion.

### Procedural height-field path (alternative to normal_map)
For procedural surfaces, sampling the HEIGHT texture and computing the normal per-fragment from finite differences is more correct (no pre-baked normal map, normal stays consistent with the displacement at sub-pixel resolution). Could add an optional `height_map` input to `render_3d_mesh` that, when wired, computes N per-fragment in `fs_pbr` via three height samples + cross product. Doubles the texture sampling cost but trivial GPU work.

### Tessellation-shader-based displacement
Long-term: instead of CPU-side `displace_mesh` writing to a buffer, use Metal's tessellation pipeline to displace at draw time. Removes the buffer write + lets the displacement adapt to camera distance. Material wire stays the same; the renderer's vertex stage gains optional tessellation eval.

---

## Appendix A: pro-engine reference points

For each industry engine, the equivalent pattern this plan mirrors:

- **Blender Cycles / Eevee:** `MeshVertex` carries position + normal + UV (multiple UV layers in `bm.loops.layers.uv`). The Principled BSDF samples its base color / normal / roughness from image texture nodes that default to `UVMap` (the mesh's UV0). Identical shape to this plan's v1.
- **Unreal Engine:** `FStaticMeshVertex` carries position + normal + tangent + colour + UV (up to 4 UV channels). Material expressions sample with `TexCoord[0]` by default. This plan's v1 ships position + normal + UV; tangent is the next slot.
- **Unity Shader Graph:** Vertex inputs are position / normal / tangent / UV0..UV3. Sample Texture 2D nodes default to UV0 input. Same shape.
- **TouchDesigner:** GLSL TOPs feed mesh attributes via `vP` (position), `vN` (normal), `vT` (tangent), `vUV[0]` (UV). MAT operators sample textures with `vUV[0]` by default. Same shape.

The plan's v1 = the foundational subset every engine has. Future tangent + multi-UV expansions match the standard order in which each engine added them.

---

## Appendix B: implementation order rationale

U1 (`MeshVertex` shape change) MUST land first. Every downstream tranche depends on the new layout being stable across producer / consumer / WGSL. Once U1 ships, U2 / U3 / U4 are independent — they could even ship in parallel if a single commit-per-tranche cadence is wanted.

U5 (MetallicGlass JSON) depends on U3 (renderer reads normal_map at mesh UV). Lands last.

Per-tranche commit hygiene: each tranche compiles + tests + clippy clean before the next tranche starts. No partial-state commits.
