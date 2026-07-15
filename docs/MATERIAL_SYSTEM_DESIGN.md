# Material System — Design Doc

**Status:** Tranches **M1–M6 ALL SHIPPED** (M1–M5 verified in-repo 2026-07-04: `material.rs` + all four atoms + renderer integration + MetallicGlass/NestedCubes migrated — see §11 for the as-built record and where it deviates from §5; **M6 verified in-repo 2026-07-05 in the baseline review**: `AlphaMode`/`alpha_cutoff` + `base_color_map`/`metallic_map` present across all four material atoms — the status previously still read "APPROVED, not built"). Design accepted 2026-05-27; un-held 2026-07-03 by `docs/REALTIME_3D_DESIGN.md`, which consumes this contract unchanged.

**Companion docs:** [`NODE_CATALOG.md`](NODE_CATALOG.md), [`DECOMPOSING_GENERATORS.md`](DECOMPOSING_GENERATORS.md), [`ADDING_PRIMITIVES.md`](ADDING_PRIMITIVES.md).
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any phase. Conformance-hardened: written 2026-05-27, the oldest active design — `render_3d_mesh`/`render_instanced_3d_mesh` are renamed to `node.render_mesh`/`node.render_copies` by the vocab-audit apply; run the §8.3 pre-flight and read the migration table before touching any node id in this doc.

---

## 1. Goal

Introduce a `Material` port type and a v1 set of material atoms that 3D mesh renderers consume to drive surface shading. Aligns MANIFOLD's 3D-rendering shape with TouchDesigner / Blender / Unreal / Unity: bundled renderer + first-class Light + first-class Material. The current pattern of "compose shading via atoms downstream of a G-buffer" (MetallicGlass's cook_torrance + envmap chain) collapses into a single Material wire.

**Why now (before shadow infrastructure + DigitalPlants migration):** the Material system simplifies the renderer's surface enough that downstream work (shadow infrastructure, DigitalPlants migration, future 3D effects and generators) lands on the clean shape rather than building atop scattered scalar params slated for deletion.

---

## 2. Core decisions (settled)

1. **Shape A — Material atoms are registered Rust primitives.** Each material is one registered atom with a bespoke WGSL shader. Future Shape B (user-authored material sub-graphs via WGSL inliner) lands as a new `MaterialKind::Authored` variant when the `graph_compiler` initiative ships — additive, not a rewrite.
2. **No silent fallbacks.** Missing required inputs produce structured errors at preset-load time (when statically detectable) or first-frame runtime (when the wire's source is dynamic). Black output never happens because a fallback path dropped to zero — if you see black, it's a real "this pixel is black" answer.
3. **`material` wire is REQUIRED on every 3D mesh renderer.** No backwards-compatible "scattered scalar fallback." The renderer's existing `color_r/g/b`, `ambient`, `light_intensity` scalar params and bindings are **deleted entirely** — they were the source of every dead-state-trap we hit during the Light wire migration.
4. **`light` wire is conditionally required per material kind.** Unlit materials don't need a light; Phong / PBR / Cel do. The renderer's `evaluate` errors if material is non-Unlit and `light` is unwired.
5. **Texture inputs wire directly to the renderer, NOT through the material wire.** Material wire is CPU-only (parallel to Light + Camera). Texture inputs (`normal_map`, `base_color_map`, `roughness_map`, `metallic_map`, `envmap`) sit alongside the material wire on the renderer. Conditionally required per material kind.
6. **Industry-standard TD/Blender shape:** the renderer is one bundled node that takes geometry + camera + lights + material + textures and emits a shaded image. Materials describe surfaces; lights describe illumination; renderers combine them. No deferred-shading-via-atoms in user graphs (the OilyFluid screen-space pattern keeps using lambert / matcap / fresnel / blinn atoms — those operate on Texture2D, not surfaces; they are NOT in scope for this migration).

---

## 3. The `Material` port type

CPU-only struct, parallel to [`Camera`](../crates/manifold-renderer/src/node_graph/camera.rs) and [`Light`](../crates/manifold-renderer/src/node_graph/light.rs). One source primitive emits a Material per frame; downstream renderers read it via `ctx.inputs.material("material")`.

```rust
// crates/manifold-renderer/src/node_graph/material.rs

/// Discriminator for the material's shading model. Open enum — each
/// added kind ships with: (a) a new variant here, (b) a new material
/// atom primitive that emits it, (c) a new arm in each renderer's
/// per-kind shader dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialKind {
    /// Flat colour passthrough. No lighting math. No light wire required.
    Unlit,
    /// Classic Lambert diffuse + Blinn-Phong specular. Cheap.
    Phong,
    /// Cook-Torrance microfacet specular (D_GGX × G_Smith × F_Schlick) + IBL
    /// reflection. The workhorse for realistic surfaces.
    Pbr,
    /// Cel-shaded — Lambert N·L quantized into N bands. Stylised look.
    Cel,
}

/// Surface description carried on [`PortType::Material`] wires. CPU
/// struct, ~96 bytes. The renderer reads the kind, selects its matching
/// compiled pipeline, and binds the kind-relevant fields as uniforms.
///
/// Fields not relevant to the wired kind are inert (e.g. `metallic` is
/// unread when `kind = Phong`). Material atoms expose only their kind's
/// outer-card params; the struct's superset is implementation detail
/// — users never see the inert fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Material {
    pub kind: MaterialKind,

    // Always-present surface scalars (every kind respects these).
    pub base_color: [f32; 4],   // rgba, alpha = opacity
    pub emission: [f32; 4],     // rgb = emissive colour, a = intensity
    pub ambient: f32,           // unread by Unlit; lambert ambient floor for others

    // PBR-specific. Inert when kind != Pbr.
    pub metallic: f32,
    pub roughness: f32,

    // Phong-specific. Inert when kind != Phong.
    pub specular_color: [f32; 4],
    pub specular_power: f32,

    // Cel-specific. Inert when kind != Cel.
    pub cel_bands: u32,
    pub band_low: f32,
    pub band_high: f32,
}

impl Material {
    /// Sensible Phong default for renderer's `default-when-unwired`...
    /// actually no — there is no default. The renderer errors if material
    /// is unwired. This function is provided only for test fixtures.
    #[cfg(test)]
    pub fn test_phong_white() -> Self { ... }
}
```

**`PortType::Material` variant** added to [`ports.rs`](../crates/manifold-renderer/src/node_graph/ports.rs). **`(Material) => $crate::node_graph::ports::PortType::Material`** macro arm added to the `primitive!` declarative macro in [`primitive.rs`](../crates/manifold-renderer/src/node_graph/primitive.rs).

**`PortKindSnapshot::Material` variant** added to [`snapshot.rs`](../crates/manifold-renderer/src/node_graph/snapshot.rs) for the graph editor's wire colour. New colour constant `PORT_MATERIAL_COLOR` in [`graph_canvas.rs`](../crates/manifold-app/src/graph_canvas.rs) — recommend a desaturated copper / orange (`[0.95, 0.65, 0.40, 1.0]`) so it's distinct from Light's yellow and Camera's red.

**Backend trait extension** (mirror of [`Camera`](../crates/manifold-renderer/src/node_graph/backend.rs)):

```rust
fn material(&self, _slot: Slot) -> Option<Material> { None }
fn set_material(&mut self, _slot: Slot, _value: Material) {}
```

`MockBackend` + `MetalBackend` get a `materials: AHashMap<Slot, Material>` field; same shape as the existing `cameras` / `lights` maps.

**Bindings extension** ([`bindings.rs`](../crates/manifold-renderer/src/node_graph/bindings.rs)): `NodeInputs::material(port)` + `NodeOutputs::set_material(port, value)`, with the executor draining a `pending_material_writes` scratch.

---

## 4. v1 Material atoms

Four atoms ship in v1. Each is a registered primitive (`crate::primitive!` macro), CPU-only (no GPU dispatch), one output `out: Material`. All scalar params are port-shadowed for performance-time modulation.

### `node.unlit_material`

Flat colour passthrough. UI overlays, debug, neon, anything that shouldn't react to lights.

| Param | Type | Default | Notes |
|---|---|---|---|
| `color_r/g/b/a` | Float ×4 | (1,1,1,1) | Output colour, alpha = opacity |
| `emission_r/g/b` | Float ×3 | (0,0,0) | Added to base_color before output |
| `emission_intensity` | Float | 0.0 | Multiplied into emission RGB |

No `light` requirement on the renderer when this material is wired.

### `node.phong_material`

Classic Lambert + Blinn-Phong specular. Cheap baseline.

| Param | Type | Default | Notes |
|---|---|---|---|
| `color_r/g/b/a` | Float ×4 | (0.85, 0.88, 0.92, 1.0) | Diffuse colour |
| `ambient` | Float | 0.15 | Lambert ambient floor [0, 1] |
| `specular_color_r/g/b` | Float ×3 | (1, 1, 1) | Highlight tint |
| `specular_power` | Float | 32.0 | Phong exponent (1 = soft, 256 = pinpoint) |
| `emission_r/g/b` | Float ×3 | (0, 0, 0) | |
| `emission_intensity` | Float | 0.0 | |

Light required. Optional texture: `normal_map` (tangent-space normal perturbation, sampled per fragment).

### `node.pbr_material`

Cook-Torrance microfacet specular (D_GGX × G_Smith × F_Schlick) + IBL reflection. The workhorse.

| Param | Type | Default | Notes |
|---|---|---|---|
| `color_r/g/b/a` | Float ×4 | (0.8, 0.8, 0.82, 1.0) | Base colour |
| `ambient` | Float | 0.05 | Slight floor; PBR mostly lit by direct + IBL |
| `metallic` | Float | 0.0 | 0 = dielectric (F0=4%), 1 = metal (F0=base_color) |
| `roughness` | Float | 0.5 | Microfacet roughness [0.01, 1.0] |
| `emission_r/g/b` | Float ×3 | (0, 0, 0) | |
| `emission_intensity` | Float | 0.0 | |

Light required. Required textures: **`envmap`** (equirectangular HDR for IBL reflection — REQUIRED, no fallback; PBR without IBL is degenerate). Optional textures: `normal_map`, `base_color_map`, `roughness_map`, `metallic_map`.

### `node.cel_material`

Cel-shaded — Lambert N·L quantized into N discrete bands. Stylised; the DigitalPlants look.

| Param | Type | Default | Notes |
|---|---|---|---|
| `color_r/g/b/a` | Float ×4 | (0.36, 0.56, 0.24, 1.0) | Plant-green default; matches legacy DigitalPlants |
| `ambient` | Float | 0.0 | Cel uses band_low as its "shadow band" instead |
| `cel_bands` | Int (Enum-ish) | 4 | Number of quantization bands [2, 16] |
| `band_low` | Float | 0.08 | Lowest band value (matches legacy DigitalPlants) |
| `band_high` | Float | 1.0 | Highest band value |
| `emission_r/g/b` | Float ×3 | (0, 0, 0) | |
| `emission_intensity` | Float | 0.0 | |

Light required. No required textures. Optional: `normal_map`.

### Atom file structure

Each atom ships in its own file: `crates/manifold-renderer/src/node_graph/primitives/{unlit,phong,pbr,cel}_material.rs`. Each is registered automatically via `inventory::submit!` from the `primitive!` macro — no manual registration.

---

## 5. Renderer integration

### Affected renderers (v1 scope)

- [`render_3d_mesh`](../crates/manifold-renderer/src/node_graph/primitives/render_3d_mesh.rs)
- [`render_instanced_3d_mesh`](../crates/manifold-renderer/src/node_graph/primitives/render_instanced_3d_mesh.rs)

`render_lines` is out of scope — it's pure colour-along-curve rendering with no surface to shade.

### Input changes

Both renderers' input declarations become:

```rust
inputs: {
    vertices:  Array(MeshVertex)   required,  // (or InstanceTransform for instanced)
    camera:    Camera              required,
    material:  Material            required,  // NEW — required, no fallback
    light:     Light               optional,  // conditional per material kind
    normal_map:        Texture2D   optional,
    base_color_map:    Texture2D   optional,
    roughness_map:     Texture2D   optional,
    metallic_map:      Texture2D   optional,
    envmap:            Texture2D   optional,
}
```

**Removed inputs/params:** `light_x/y/z`, `light_intensity`, `ambient`, `color_r/g/b`. These are deleted entirely. Existing presets that use them must migrate (see §6).

### Conditional requirements (validated at preset-load + runtime)

Encoded as a new `EffectNode::conditional_requirements()` method returning a list of per-kind requirement rules:

```rust
fn conditional_requirements(&self) -> &'static [ConditionalRequirement] {
    &[
        ConditionalRequirement {
            on_material_kind: MaterialKind::Phong,
            required_inputs: &["light"],
        },
        ConditionalRequirement {
            on_material_kind: MaterialKind::Pbr,
            required_inputs: &["light", "envmap"],
        },
        ConditionalRequirement {
            on_material_kind: MaterialKind::Cel,
            required_inputs: &["light"],
        },
        // Unlit has no conditional requirements.
    ]
}
```

This is small new infrastructure on the `EffectNode` trait — default implementation returns `&[]` (no conditional requirements). Validator hooks in at preset-load via [`validation.rs`](../crates/manifold-renderer/src/node_graph/validation.rs).

**Preset-load validation:** when a renderer's `material` input is wired to a statically-resolvable Material atom (no upstream mux on the wire), the validator reads the atom's `kind` param, looks up the renderer's `conditional_requirements`, and checks each required input is wired. Failure → `LoadError::ConditionalRequirementUnmet { node_id, material_kind, missing_input }` (new variant on the existing `LoadError` enum in [`persistence.rs`](../crates/manifold-renderer/src/node_graph/persistence.rs)).

**Runtime fallback (when wire is dynamic — e.g., material flows through a mux):** the renderer's `evaluate` checks at first frame. New `ctx.error(message)` API on `EffectNodeContext` surfaces a structured error to the executor, which logs once + skips the dispatch + emits a fallback fill (deterministic magenta `[1.0, 0.0, 1.0, 1.0]` so missing-input errors are visually obvious without breaking the frame).

### Pipeline-per-kind compilation

Each renderer holds an `AHashMap<MaterialKind, GpuRenderPipeline>` in its `extra_fields`. On `evaluate`:

1. Read material from `ctx.inputs.material("material")`.
2. Check `conditional_requirements` are met (runtime check for dynamic case).
3. Get-or-compile pipeline for `material.kind`.
4. Bind: uniform block from material + relevant textures + camera VP + light (if required) + instance buffer (instanced renderer only).
5. Dispatch.

Pipelines compile lazily on first use — a preset that only uses PBR never compiles the Phong / Cel / Unlit pipelines.

### Shader file layout

Each material kind has its own fragment shader file:

- `crates/manifold-renderer/src/node_graph/primitives/shaders/material_unlit.wgsl`
- `crates/manifold-renderer/src/node_graph/primitives/shaders/material_phong.wgsl`
- `crates/manifold-renderer/src/node_graph/primitives/shaders/material_pbr.wgsl`
- `crates/manifold-renderer/src/node_graph/primitives/shaders/material_cel.wgsl`

All four share the same vertex shader (the existing one in `render_3d_mesh.wgsl` / `render_instanced_3d_mesh.wgsl`). The renderer's `create_render_pipeline_depth` call picks the right fragment entry point per kind.

PBR's shader includes the existing [`pbr_brdf.wgsl`](../crates/manifold-renderer/src/node_graph/primitives/shaders/pbr_brdf.wgsl) helper that cook_torrance + envmap_sample atoms already share — that code stays reusable; it now lives inside the material's fragment shader instead of being composed via separate atoms.

---

## 6. Migration plan

### MetallicGlass (the centrepiece migration)

Current shape: ~50 nodes, with ~30 nodes in the PBR shading pipeline (G-buffer outputs from `render_3d_mesh` → `heightmap_to_normal` → `cook_torrance_specular` + `equirect_envmap_sample` → `mix` → `reinhard_tone_map` → output).

After migration:

```
upstream height-field generation (unchanged: ~20 nodes)
                ↓
         normal_map ──┐
         roughness_map ┤
                       │      camera ──┐
                       │      light  ──┤
   pbr_material ──────►├─── render_3d_mesh ──► color ──► final_output
                       │       (vertices, camera, material,
   envmap ────────────►│        light, normal_map, roughness_map,
                       │        envmap)
                  ────►│
   geometry (mesh)
```

Node count: ~50 → ~25. The G-buffer-then-shade chain (`world_pos`, `world_normal`, `heightmap_to_normal` (in WorldYUp mode), `cook_torrance_specular`, `equirect_envmap_sample`, `mix Add`, `reinhard_tone_map`, `roughness_from_edge` `scale_offset_texture`) collapses into `node.pbr_material` + the renderer's bundled shader.

Outer-card sliders preserved: `feedback`, `noise_scale`, `noise_speed`, `edge_str`, `mirror`, `displace`, `roughness` (binds to `pbr_material.roughness`), `light_int` (binds to `light.intensity`), `cam_dist`, `cam_orbit`, `cam_tilt`, `cam_fov`, `look_y`. New sliders for direct light control come from the existing `node.light` atom.

The reinhard tone map is deleted from the preset — PBR's bundled shader handles tone mapping internally (writing in linear; the canvas's HDR-to-SDR pass at composite time handles final mapping). If this looks different, add a `tone_map` enum to PBR material itself.

### NestedCubes (small migration)

Currently uses `render_instanced_3d_mesh` with scattered scalar params. Migration: add a `node.phong_material` with the current scattered values (color → base_color, ambient → ambient). One-line change of bindings.

### DigitalPlants (tranche 5, deferred)

Currently a fused `node.digital_plants_render` primitive. With Material system + Light + Shadow infrastructure all landed: `instance suite → render_instanced_3d_mesh + node.cel_material + node.light(cast_shadows=true)`. Tranche 5 absorbs the cel material wiring as part of its already-planned decomposition.

### Other 3D presets

Audit (run before implementation): `rg "node\.render_3d_mesh\b|node\.render_instanced_3d_mesh\b" crates/manifold-renderer/assets/generator-presets/ -l`. Every preset listed must be migrated (add a material wire). Expected to be: MetallicGlass, NestedCubes, and possibly nothing else. `check-presets` will refuse to build the renderer otherwise (missing required material wire).

### Out of scope

- **OilyFluid** and any other screen-space shading preset. These use lambert / matcap / fresnel / blinn atoms operating on Texture2D inputs (height-field shading, not 3D geometry shading). The standalone shading atoms stay registered as image-domain primitives. Material system is for 3D mesh rendering.
- **render_lines** consumers (Lissajous, Tesseract, Duocylinder, WireframeZoo, ConcentricTunnel). Line rendering doesn't have a surface concept. If we later add 3D-lit line rendering it gets its own design decision.

---

## 7. Extension points (forward-looking)

Each piece of the v1 design has a deliberate seam for future work. None of the extensions below require v1 design changes — all are additive.

### Adding new material kinds

Future kinds (Glass, Hair, Skin, Toon, Water, Fabric, …) ship as:

1. New `MaterialKind` variant in [`material.rs`](../crates/manifold-renderer/src/node_graph/material.rs).
2. New fields on the `Material` struct if the kind needs them (defaulted on existing materials — no version-break). Example: Glass needs `ior: f32` + `transmission: f32` — both ship defaulted to sensible inert values (1.0 / 0.0) on existing materials.
3. New atom file (`crates/manifold-renderer/src/node_graph/primitives/{kind}_material.rs`) exposing only the kind's params on the outer card.
4. New fragment shader (`shaders/material_{kind}.wgsl`).
5. New arm in each renderer's pipeline cache and conditional_requirements list.

No existing kind's behaviour changes.

### Shape B — user-authored material sub-graphs

When the [`graph_compiler` initiative](GRAPH_COMPILER.md) ships the WGSL inliner, authored materials become a new `MaterialKind::Authored { graph_ref: SubGraphRef }` variant. The renderer's match arm for `Authored` dispatches to the inliner, which compiles the sub-graph into a fragment shader at pipeline-creation time. Registered kinds (Pbr / Phong / Cel / Unlit) coexist as permanent vocabulary; authored materials are user-customisable on top.

### Multi-light support

The `Light` port type was designed to extend to `Array<Light>` (per the Light design audit). When multi-light lands, the renderer's per-material fragment shader iterates lights and accumulates lit contributions. No material-side change required — the material describes the surface BRDF; the renderer + lights handle the per-light evaluation.

### Shadow infrastructure (next tranche)

Materials don't carry shadow params. Shadow casting is a `Light` property (`cast_shadows`, `shadow_softness`, `shadow_bias`, `shadow_resolution` — already on the Light struct from Tranche 1). When the shadow-map cache infrastructure lands on the Metal backend, the renderer generates the shadow map when a shadow-casting light is wired and multiplies the per-fragment shadow factor into each material's lit term. Affects all lit material kinds uniformly. No per-material shadow handling.

### Per-pixel attenuation for point lights

The renderer's fragment shaders for lit material kinds compute per-pixel L from `world_pos` (interpolated by the vertex shader). Point lights compute `attenuation = 1 / (1 + d²/range²)` per-pixel; sun lights skip the attenuation. Same code path across material kinds.

### Compositor / multi-pass outputs

If we ever need a deferred-shading-style workflow (post-process effects sampling a depth or normal buffer from the 3D pass), `render_3d_mesh` retains its `world_pos` / `world_normal` G-buffer outputs alongside `color`. They're optional and lazy: pipelines only compile when the output is consumed. The Material system doesn't remove these — it just makes the bundled `color` output the primary path.

### Texture-on-material-wire (Path B from the design audit)

If the "textures wire to the renderer, not through the material" UX wart becomes painful in practice, we can later add a `MaterialTextureBundle` carried on the Material wire. Existing material atoms gain optional texture inputs; renderer reads texture references from either the material wire OR its own direct inputs. Backwards-compatible — no preset migration needed.

---

## 8. Implementation tranches

### Tranche M1 — Material port type + plumbing

**Goal:** the wire exists end-to-end with no consumers yet.

- `PortType::Material` variant + `(Material)` macro arm + `PortKindSnapshot::Material` + graph_canvas wire colour.
- `crates/manifold-renderer/src/node_graph/material.rs` — `Material` struct, `MaterialKind` enum, helpers. Mirror the shape of [`light.rs`](../crates/manifold-renderer/src/node_graph/light.rs).
- Backend trait: `material(slot)` + `set_material(slot, value)`. MockBackend + MetalBackend storage.
- Bindings: `NodeInputs::material(port)` + `NodeOutputs::set_material(port, value)`. Executor drains `pending_material_writes` scratch.
- Unit tests on Material helpers (default values, premultiplied emission, kind-dispatch helpers).

**~1 day.** Modeled on Tranche-1 of the Light work.

### Tranche M2 — Material atoms

**Goal:** four registered material atoms emit Materials of each kind.

- `node.unlit_material` + WGSL stub (no shader yet — materials don't dispatch).
- `node.phong_material`.
- `node.pbr_material`.
- `node.cel_material`.
- Each ~150-200 lines + atomic `inventory::submit!` registration.
- Unit tests on `Primitive::run` for each atom (constructs the Material struct from params, writes to `ctx.outputs.set_material("out", ...)`, output validates).

**~2 days.**

### Tranche M3 — Structured-error infrastructure

**Goal:** the executor surfaces "missing required input" errors to the user instead of black output.

- New `ctx.error(message)` API on `EffectNodeContext` — pushes an entry into a per-frame error scratch buffer the executor drains and logs.
- `EffectNode::conditional_requirements()` method on the trait, default `&[]`.
- Preset-load validator extension in [`validation.rs`](../crates/manifold-renderer/src/node_graph/validation.rs): for each renderer with `conditional_requirements`, if the material wire's source is statically resolvable, check the wired material's kind against the requirements list. Emit `LoadError::ConditionalRequirementUnmet { node_id, material_kind, missing_input }` (new variant on `LoadError`).
- New variant in [`persistence.rs`](../crates/manifold-renderer/src/node_graph/persistence.rs) for the `LoadError` enum.
- Fallback magenta fill: a `gpu.native_enc.clear_texture(target, 1.0, 0.0, 1.0, 1.0)` call when a runtime conditional check fails. New helper on the renderer's path.

**~1 day.**

### Tranche M4 — Renderer integration

**Goal:** render_3d_mesh + render_instanced_3d_mesh consume the Material wire and dispatch the right per-kind shader.

- Add `material: Material required` + texture inputs (`normal_map`, `base_color_map`, `roughness_map`, `metallic_map`, `envmap`) to both renderers. **Remove** `light_x/y/z/intensity`, `ambient`, `color_r/g/b` params and their related code paths.
- Each renderer holds `pipelines: AHashMap<MaterialKind, GpuRenderPipeline>` in `extra_fields`.
- `evaluate` reads material → checks runtime conditional requirements → get-or-compile pipeline for kind → builds uniform block → binds textures + camera + light → dispatch.
- Four new WGSL shaders in `crates/manifold-renderer/src/node_graph/primitives/shaders/material_{unlit,phong,pbr,cel}.wgsl`. PBR includes `pbr_brdf.wgsl`.
- Implement `conditional_requirements()` on both renderers.

**~1.5 days.**

### Tranche M5 — Preset migrations

**Goal:** every existing 3D-mesh preset wires a material.

- `MetallicGlass.json`: delete the cook_torrance + envmap + heightmap_to_normal + mix + tone_map nodes; add `node.pbr_material` + rewire. Verify visually against the legacy preset.
- `NestedCubes.json`: add `node.phong_material` with the current scattered values.
- Audit pass with `rg` to confirm no other 3D-mesh consumers; `check-presets` enforces the required-material rule at build.
- Delete `MetallicGlassLit.json` (the test variant from the Light audit; no longer needed).

**~half-day.**

### Total: ~5 days of focused work for the Material system + first migrations.

Subsequent tranches (shadow infrastructure, DigitalPlants migration) build on this base.

---

## 9. Test discipline

Per [`feedback_prefer_focused_tests`](../.claude/projects/-Users-peterkiemann-MANIFOLD---Rust/memory/feedback_prefer_focused_tests.md) and [`DECOMPOSING_GENERATORS.md`](DECOMPOSING_GENERATORS.md) §4.1:

**Per tranche:**
- `cargo check -p manifold-renderer --lib --tests`
- `cargo test -p manifold-renderer --lib node_graph::material::` (Tranche M1) / `node_graph::primitives::{unlit,phong,pbr,cel}_material::` (Tranche M2) / etc.
- `cargo run -p manifold-renderer --bin check-presets` after any JSON edit (Tranche M5).
- `cargo clippy -p manifold-renderer --lib -- -D warnings`.

**Per atom (Tranche M2):**
- Unit test that `Primitive::run` constructs a Material with the right `kind` and propagates outer-card params correctly.
- Surface test that the atom's INPUTS / OUTPUTS / PARAMS declare what they should.

**Per renderer extension (Tranche M4):**
- Surface test that the renderer declares `material: required` and the texture inputs.
- gpu_tests parity for each material kind: dispatch a small mesh, read back the colour, assert expected RGB for a known-good camera + light + material setup. Bit-exact where possible, bounded tolerance for PBR's transcendental functions.

**Per preset migration (Tranche M5):**
- `check-presets` validates loadability.
- Manual visual A/B against the legacy preset on the canonical fixture (`Liveschool Live Show V6 LEDS.manifold`).
- Documented parity delta if any (PBR with bundled tone-mapping vs the decomposed atom-chain version may show ≤1 ULP per-pixel deltas from reassociation; document and accept).

**No workspace runs per tranche.** Batch `cargo test --workspace` + workspace clippy at the end of the Material initiative, after Tranche M5 lands.

---

## 10. Naming convention

Atoms ship as `node.{kind}_material` — `node.unlit_material`, `node.phong_material`, `node.pbr_material`, `node.cel_material`. Suffix grouping sorts them adjacent in any reasonable alphabetical palette ordering, and "what it produces" matches §6.6 of [`DECOMPOSING_GENERATORS.md`](DECOMPOSING_GENERATORS.md).

The `Material` port type is `node_graph::Material` (struct) + `PortType::Material` (variant). Material kinds are `MaterialKind::Unlit / Phong / Pbr / Cel`.

WGSL shaders are `shaders/material_{kind}.wgsl` (prefix grouping in the shaders directory).

---

## 11. Addendum 2026-07-04 — as-built record + Tranche M6 (surface completion for imported assets)

Written for the glTF import wave (`docs/IMPORT_DESIGN.md` P1 hard-depends on M6 —
a glTF material without its albedo texture is not that material, and foliage
without alpha cutout renders as opaque rectangles).

### 11.1 As-built record (verified 2026-07-04)

Where the shipped implementation deviates from §5, the repo is authoritative:

| §5 said | As-built | Anchor |
|---|---|---|
| Four shader files `material_{kind}.wgsl` | ONE file with per-kind fragment entry points `fs_unlit` / `fs_phong` / `fs_pbr` / `fs_cel` (+ `fs_world_pos` / `fs_world_normal`), pipeline-per-kind selects the entry point | `primitives/shaders/render_3d_mesh.wgsl:150,157,174,232,250,256` |
| Renderer texture inputs incl. `base_color_map`, `metallic_map` | Shipped WITHOUT them. Inputs are: `envmap`, `normal_map`, `roughness_map` only | `primitives/render_3d_mesh.rs:67–75` |
| `normal_map` = tangent-space perturbation | As-built contract is a **world-space signed normal** (fits the procedural heightfield chain it was built for; does NOT fit glTF's tangent-space maps) | `render_3d_mesh.rs:66` (purpose), `render_3d_mesh.wgsl:124` |
| — | Optional texture sampling is gated by a `texture_flags` vec4 uniform (x = normal_map, roughness gate in `resolve_normal`/`resolve_roughness`); unwired maps bind a 1×1 dummy | `render_3d_mesh.wgsl:75–100,125,136` |
| — | **No cull-mode configuration exists anywhere in `manifold-gpu`** (`rg -i cull crates/manifold-gpu/src` → zero hits). Metal's default is `MTLCullModeNone`, so all meshes already rasterize both faces — de-facto double-sided, but back faces are lit with the front normal (wrong) | `manifold-gpu` (negative search, 2026-07-04) |

Post-vocab ids in play: `node.render_mesh`, `node.render_copies`,
`node.bake_environment` (`bake_equirect_envmap.rs:30`), `node.orbit_camera`,
`node.spawn_from_image` (`seed_particles_from_texture.rs:55`).

### 11.2 M6 decisions

- **M6-D1 — `base_color_map` + `metallic_map` land on both renderers.** Optional
  `Texture2D` inputs on `node.render_mesh` and `node.render_copies`, gated through
  the existing `texture_flags` pattern (`.z` = base_color_map, `.w` = metallic_map).
  `base_color_map.rgb × material.base_color.rgb` = surface albedo;
  `base_color_map.a × material.base_color.a` = surface opacity (the cutout source);
  `metallic_map.r` replaces scalar `metallic` (same shape as `roughness_map.r`).
  Sampled in ALL lit entry points and `fs_unlit` (albedo + opacity apply to unlit
  too — foliage cards are often unlit). ⚠ VERIFY-AT-IMPL: colour space — whether
  image decode delivers sRGB-decoded or raw values; read the texture-import path in
  `manifold-media` (`image_renderer.rs`) before choosing sample-time conversion.
- **M6-D2 — Alpha cutout is a Material property, not a renderer param.** The
  `Material` struct (`node_graph/material.rs`) gains `alpha_mode: AlphaMode`
  (`Opaque | Mask`) + `alpha_cutoff: f32` (default 0.5) — the §7 "new fields,
  defaulted, no version-break" seam, exercised as designed. All four material atoms
  gain the two outer-card params (enum + float, port-shadowed). Every fragment entry
  point applies: `if alpha_mode == Mask && resolved_alpha < alpha_cutoff { discard; }`.
  Rejected: a renderer-side cutout param — glTF puts `alphaMode`/`alphaCutoff` on the
  material, and so do we; one import mapping, no impedance.
- **M6-D3 — Blend (smooth transparency) stays DEFERRED.** Real transparency needs
  draw-order/OIT design; Mask covers foliage, decals, and cutout UI. glTF `BLEND`
  materials import as Mask (cutoff 0.5) with an import-report warning
  (IMPORT_DESIGN D9). Trigger to revive: a hero asset that genuinely reads wrong as
  cutout. **[TRIGGER FIRED 2026-07-15 (car windows) → `docs/IMPORT_FIDELITY_DESIGN.md`
  D8/F-P5 (SHIPPED 2026-07-15, `61400029`) added `AlphaMode::Blend` + a sorted
  per-object blend pass in `render_scene` and flipped the import mapping there.
  `render_mesh`/`render_copies` keep Mask-only — this deferral stays live for
  them; OIT stays deferred everywhere.]**
- **M6-D4 — Double-sided stays the only mode; back-face lighting gets fixed.** No
  cull-mode API is added (nothing needs single-sided today; revisit only if the perf
  HUD ever shows overdraw pain). The lit entry points take `@builtin(front_facing)`
  and negate the resolved normal on back faces, so a leaf's underside shades
  correctly instead of going black. Rejected: adding a `double_sided` material flag
  now — it would be a no-op flag on top of an engine that can't cull; flags that
  defer decisions are a named anti-pattern.
- **M6-D5 — Tangent-space normal maps stay out.** `MeshVertex` carries no tangents
  and the as-built `normal_map` contract is world-space (§11.1). glTF import P1
  skips tangent-space normal maps and lists each skip in the import report.
  Trigger: a hero import that visibly needs them → tangent generation at import
  time + a `normal_space` mode on the renderer, as its own designed slice.
  **[TRIGGER FIRED 2026-07-15 → `docs/IMPORT_FIDELITY_DESIGN.md` D4 is the designed
  slice. It lands tangent-space maps on `render_scene` only, via a fragment-shader
  cotangent frame — NOT import-time tangent generation, and NOT a `normal_space`
  mode on `render_mesh`, whose world-space contract stays untouched.]**

### 11.3 Tranche M6 brief (one session)

- **Entry state:** M1–M5 anchors above re-verified (`rg -n 'type_id' primitives/pbr_material.rs`
  → `node.pbr_material`; `rg -n 'fn fs_' primitives/shaders/render_3d_mesh.wgsl` → six
  entry points; `rg -in cull crates/manifold-gpu/src` → zero hits).
- **Read-back:** this addendum whole, §5 + §7 of this doc,
  `docs/MANIFOLD_GPU_ARCHITECTURE.md` uniform-alignment rules (the uniform block
  grows: `alpha_cutoff: f32` + an `alpha_mode: u32` — mind 16-byte alignment),
  `render_3d_mesh.rs` + its wgsl end-to-end, the alpha-standardisation memory
  (compositor expects premultiplied; producers aren't — cutout discards instead of
  blending, so no premultiply question arises; do NOT premultiply in the shader).
- **Deliverables:** `AlphaMode` enum + 2 struct fields + defaults (`material.rs`);
  2 params × 4 atoms; 2 inputs × 2 renderers + `texture_flags.zw` + uniform fields;
  discard + front-facing-flip in `render_3d_mesh.wgsl` and
  `render_instanced_3d_mesh.wgsl`; gpu_tests.
- **Gate (positive):** gpu_test — checkerboard-alpha `base_color_map` on a quad,
  Mask mode: transparent texels leave clear-colour, opaque texels shade (value-level
  readback both sides of the cutoff). gpu_test — `base_color_map` modulation: known
  texel × known base_color = expected RGB. gpu_test — camera behind a single
  triangle: back face is LIT (front-facing flip proof), not silhouette-black.
  Existing bundled 3D presets: zero PNG diffs (all new inputs optional, all new
  params defaulted to Opaque).
- **Gate (negative):** `rg -i cull crates/manifold-gpu/src` still zero hits;
  `rg 'premultipl' primitives/shaders/render_3d_mesh.wgsl` zero hits;
  `cargo run -p manifold-renderer --bin check-presets` clean.
- **Forbidden moves:** adding a blend pipeline "while at it" (M6-D3) · premultiplying
  alpha in the mesh shader · a `double_sided`/cull param (M6-D4) · synthesizing the
  uniform layout from memory instead of reading the existing block · touching
  `render_lines`.
- **Test scope:** full workspace sweep at end of tranche — the `Material` struct
  rides a port type; struct-on-the-wire changes are infrastructure per the scope
  rule.

---

## Appendix A: MetallicGlass.json migration diff (sketch)

**Removed nodes:** 51 (heightmap_to_normal), 52 (roughness_from_edge), 53 (cook_torrance_specular), 54 (equirect_envmap_sample), 55 (direct_plus_ibl mix), 56 (reinhard_tone_map). 6 nodes deleted, plus their wires.

**Added nodes:** 1 — `node.pbr_material` (id 63) with `metallic=1.0`, `roughness=0.05`, `base_color=(0.8, 0.8, 0.82, 1.0)`. The existing `node.light` (id 62 from MetallicGlassLit) carries over if Light has shipped by then; otherwise add it as part of this migration.

**Rewires:**
- `13 → 51 (in)` deleted.
- `25 → 52 (in)` deleted.
- Several `pos_x/y/z` → `53/54` wires deleted.
- `35 (triangulated.out) → 50 (render_3d_mesh.vertices)` stays.
- `43 (cam_orbit_node.out) → 50 (render_3d_mesh.camera)` stays.
- New: `63 (pbr_material.out) → 50 (render_3d_mesh.material)`.
- New: `62 (light_node.out) → 50 (render_3d_mesh.light)`.
- New: heightmap normal source (from upstream chain) → `50 (render_3d_mesh.normal_map)`.
- New: roughness source (from upstream chain) → `50 (render_3d_mesh.roughness_map)`.
- New: `32 (bake_equirect_envmap.envmap) → 50 (render_3d_mesh.envmap)`.
- `50 (render_3d_mesh.color) → 40 (final_output.in)`.

**Outer-card bindings rebound:**
- `roughness → 63 (pbr_material.roughness)`.
- `light_int → 62 (light_node.intensity)`.

**Result:** ~25 nodes total, down from ~50. Visually equivalent (same Cook-Torrance + IBL math), structurally simpler.

---

## Appendix B: structured-error API shape

```rust
// crates/manifold-renderer/src/node_graph/effect_node.rs

impl<'ctx, 'gpu> EffectNodeContext<'ctx, 'gpu> {
    /// Report a structured error for the current node. The executor
    /// drains errors after `evaluate` returns, logs each one once per
    /// graph rebuild (de-duplicated by `(node_id, message)`), and
    /// surfaces them to the editor's error toast.
    ///
    /// Use when an input is missing OR has a value the node can't
    /// process (e.g., conditional requirement unmet). Does NOT halt
    /// the frame — the node should also emit a deterministic fallback
    /// (magenta clear on a Texture2D output, zero values on scalar
    /// outputs) so the rest of the graph isn't poisoned by garbage.
    pub fn error(&mut self, message: impl Into<String>) {
        self.errors.push((self.node_id, message.into()));
    }
}
```

Executor drains `errors` after each step alongside the existing scalar / camera / light scratch drains. Editor's existing error panel consumes them.
