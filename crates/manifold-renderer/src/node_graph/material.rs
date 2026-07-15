//! `Material` — port-data type carried on [`PortType::Material`](crate::node_graph::ports::PortType::Material) wires.
//!
//! One material source primitive (`node.{unlit,phong,pbr,cel}_material`) emits
//! a fully-populated struct each frame; downstream consumers — the bundled 3D
//! mesh renderers ([`render_3d_mesh`](crate::node_graph::primitives::render_3d_mesh),
//! [`render_instanced_3d_mesh`](crate::node_graph::primitives::render_instanced_3d_mesh))
//! — take it as a single `material: Material` input and pick a per-kind
//! compiled shader pipeline instead of binding scattered surface scalars.
//!
//! Like [`Camera`](crate::node_graph::camera::Camera) and
//! [`Light`](crate::node_graph::light::Light), this is plain CPU data — no GPU
//! resource. Backends carry it through the same `(Slot → value)` map shape
//! that scalars / cameras / lights use; the executor drains
//! `pending_material_writes` after each node's `evaluate` returns, parallel
//! to the camera and light drains.
//!
//! The kind discriminator [`MaterialKind`] is the dispatch axis: the renderer
//! holds an `AHashMap<MaterialKind, GpuRenderPipeline>` and gets-or-compiles
//! the matching pipeline lazily. Fields not relevant to the wired kind are
//! inert (e.g. `metallic` is unread when `kind = Phong`); material atoms only
//! expose their kind's outer-card params, so users never see the superset.
//!
//! Emission is stored premultiplied with intensity (`rgb × intensity`) — same
//! convention as [`Light::color`](crate::node_graph::light::Light) — so the
//! consumer-side shading math is one multiply lighter. Atoms apply the
//! intensity at emission; downstream reads see the already-multiplied value
//! in `emission.rgb`. The alpha channel of `emission` is reserved (currently
//! `1.0`).

/// Discriminator for the material's shading model. Open enum — each added
/// kind ships with: (a) a new variant here, (b) a new material atom primitive
/// that emits it, (c) a new arm in each renderer's per-kind pipeline cache
/// and `conditional_requirements` list, (d) a new fragment shader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialKind {
    /// Flat colour passthrough. No lighting math. The renderer does NOT
    /// require a `light` input when this kind is wired.
    Unlit,
    /// Classic Lambert diffuse + Blinn-Phong specular. Cheap baseline.
    /// The renderer requires a `light` input.
    Phong,
    /// Cook-Torrance microfacet specular (D_GGX × G_Smith × F_Schlick) +
    /// IBL reflection. The workhorse for realistic surfaces. The renderer
    /// requires a `light` input AND an `envmap` texture.
    Pbr,
    /// Cel-shaded — Lambert N·L quantized into N discrete bands.
    /// Stylised look; the DigitalPlants aesthetic. The renderer requires a
    /// `light` input.
    Cel,
}

/// Alpha coverage model for the surface (glTF `alphaMode`).
/// [`Opaque`](AlphaMode::Opaque): alpha is ignored for coverage — every
/// rasterised fragment is written. [`Mask`](AlphaMode::Mask): a fragment
/// whose resolved alpha is below [`Material::alpha_cutoff`] is `discard`ed
/// (cutout), so foliage cards and decals punch holes instead of rendering
/// as opaque rectangles. [`Blend`](AlphaMode::Blend): the object draws in a
/// second, sorted, depth-write-off pass in `render_scene` — glTF `BLEND` and
/// `KHR_materials_transmission` materials import as this (IMPORT_FIDELITY_DESIGN.md
/// D8/F-P5, superseding MATERIAL M6-D3's "out of scope" call). `render_mesh`/
/// `render_copies` don't implement the sorted pass (D1 scope fence) — a
/// `Blend` material wired there renders as `Opaque` coverage (no cutout,
/// no sorting) until their own IBL-upgrade migration trigger fires
/// (IMPORT_FIDELITY_DESIGN.md §7 Deferred #3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlphaMode {
    /// Alpha ignored for coverage; all fragments written.
    Opaque,
    /// Cutout: fragments with `alpha < alpha_cutoff` are discarded.
    Mask,
    /// Sorted back-to-front blend pass, depth test on / write off
    /// (`render_scene` only — see the enum doc comment above).
    Blend,
}

/// Material struct flowing through [`PortType::Material`](crate::node_graph::ports::PortType::Material)
/// wires. Built once per frame in each material atom's `run()`; passed by
/// value to every downstream consumer.
///
/// ~80 bytes — trivially cheap to clone per wire per frame. The struct is
/// the superset of every kind's params; fields not relevant to the wired
/// `kind` are inert on the renderer side (e.g. the PBR pipeline ignores
/// `specular_color` / `specular_power`; the Phong pipeline ignores
/// `metallic` / `roughness`). Each atom only exposes its kind's outer-card
/// params, so the inert-field shape is implementation detail.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Material {
    /// Shading-model dispatch.
    pub kind: MaterialKind,

    // ---- Always-present surface scalars (every kind respects these). ----
    /// Linear-space surface colour. `rgb` is the diffuse / base colour; `a`
    /// is opacity (currently informational — opaque-only rendering for v1).
    pub base_color: [f32; 4],
    /// Linear-space emission, PREMULTIPLIED with intensity. A consumer
    /// reading `emission.rgb` gets the final emissive contribution directly
    /// — no second intensity multiply needed. `emission.a` is reserved
    /// (currently `1.0`).
    pub emission: [f32; 4],
    /// Lambert ambient floor in `[0, 1]`. Unread by `Unlit` (no lighting
    /// math). Unread by `Cel` (which uses `band_low` as its shadow band
    /// instead). For Phong / PBR, mixed in via
    /// `lit = lambert * (1 - ambient) + ambient`.
    pub ambient: f32,

    // ---- PBR-specific. Inert when `kind != Pbr`. ----
    /// `0` = dielectric (F0 ≈ 4%), `1` = metal (F0 = `base_color`).
    pub metallic: f32,
    /// Microfacet roughness `[0.01, 1.0]`. Lower = sharper highlight.
    pub roughness: f32,

    // ---- Phong-specific. Inert when `kind != Phong`. ----
    /// Linear-space specular tint. `a` reserved (currently `1.0`).
    pub specular_color: [f32; 4],
    /// Blinn-Phong exponent. `1` = very soft, `256` = pinpoint.
    pub specular_power: f32,

    // ---- Cel-specific. Inert when `kind != Cel`. ----
    /// Number of quantization bands `[2, 16]`.
    pub cel_bands: u32,
    /// Lowest band value — the "shadow side" colour multiplier.
    pub band_low: f32,
    /// Highest band value — the "lit side" colour multiplier.
    pub band_high: f32,

    // ---- Alpha coverage (all kinds respect these). ----
    /// Coverage model. [`AlphaMode::Opaque`] writes every fragment;
    /// [`AlphaMode::Mask`] discards fragments with resolved alpha below
    /// [`Self::alpha_cutoff`]. Set post-construction by the material atoms.
    pub alpha_mode: AlphaMode,
    /// Cutout threshold in `[0, 1]`, used only when `alpha_mode == Mask`.
    pub alpha_cutoff: f32,

    // ---- PBR dielectric F0 (GLB_CONFORMANCE_DESIGN.md G-P4/D5). Inert
    // when `kind != Pbr` — same "unread by other kinds" pattern as
    // `metallic`/`roughness` above. Defaults reproduce the pre-G-P4
    // hardcoded F0 = 0.04 exactly: `((1.5-1)/(1.5+1))^2 * 1.0 * [1,1,1] =
    // [0.04, 0.04, 0.04]`. ----
    /// glTF `KHR_materials_ior`'s `ior` (default `1.5`, glTF's implicit
    /// default when the extension is absent).
    pub ior: f32,
    /// glTF `KHR_materials_specular`'s `specularFactor` (default `1.0`) —
    /// scales the dielectric F0 term.
    pub specular_factor: f32,
    /// glTF `KHR_materials_specular`'s `specularColorFactor` (default
    /// `[1,1,1]`) — tints the dielectric F0 term.
    pub specular_tint: [f32; 3],

    // ---- Per-map UV transforms (GLB_CONFORMANCE_DESIGN.md G-P4/D5).
    // `KHR_texture_transform`, one folded 2×3 affine per map family —
    // `[m00, m01, m10, m11, tx, ty]` s.t. `uv' = (m00*u + m01*v + tx,
    // m10*u + m11*v + ty)`, folded ONCE at import (never per frame).
    // Default identity (`[1,0,0,1,0,0]`) is exactly inert. Per-map (not
    // one shared transform) because real assets differ per slot: the AMG
    // GT3 carries transforms on 9 normalTexture infos and only 1
    // baseColorTexture. base-color applies in every kind's
    // `resolve_albedo`; the other four apply in their dedicated resolve
    // fns (PBR-path maps). ----
    pub base_color_uv_transform: [f32; 6],
    pub normal_uv_transform: [f32; 6],
    pub mr_uv_transform: [f32; 6],
    pub occlusion_uv_transform: [f32; 6],
    pub emissive_uv_transform: [f32; 6],
}

impl Material {
    /// Identity-ish unlit-white default. Provided as a no-op fallback for
    /// test fixtures and for the very-narrow case where a backend exposes
    /// the slot before a producer has run. Production renderers MUST NOT
    /// silently substitute this — a missing `material` wire is a structured
    /// error, per the design doc's §2 "no silent fallbacks" rule.
    pub fn default_unlit_white() -> Self {
        Self {
            kind: MaterialKind::Unlit,
            base_color: [1.0, 1.0, 1.0, 1.0],
            emission: [0.0, 0.0, 0.0, 1.0],
            ambient: 0.0,
            metallic: 0.0,
            roughness: 0.5,
            specular_color: [1.0, 1.0, 1.0, 1.0],
            specular_power: 32.0,
            cel_bands: 4,
            band_low: 0.08,
            band_high: 1.0,
            alpha_mode: AlphaMode::Opaque,
            alpha_cutoff: 0.5,
            ior: 1.5,
            specular_factor: 1.0,
            specular_tint: [1.0, 1.0, 1.0],
            base_color_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            normal_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            mr_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            occlusion_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            emissive_uv_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        }
    }

    /// Build an Unlit material from the standard outer-card surface.
    /// `emission_rgb` is the un-multiplied colour; this function
    /// premultiplies `emission_intensity` into the stored `emission`.
    pub fn unlit(
        color_rgba: [f32; 4],
        emission_rgb: [f32; 3],
        emission_intensity: f32,
    ) -> Self {
        let mut m = Self::default_unlit_white();
        m.kind = MaterialKind::Unlit;
        m.base_color = color_rgba;
        m.emission = premultiply_emission(emission_rgb, emission_intensity);
        m
    }

    /// Build a Phong material from the standard outer-card surface.
    #[allow(clippy::too_many_arguments)]
    pub fn phong(
        color_rgba: [f32; 4],
        ambient: f32,
        specular_color_rgb: [f32; 3],
        specular_power: f32,
        emission_rgb: [f32; 3],
        emission_intensity: f32,
    ) -> Self {
        let mut m = Self::default_unlit_white();
        m.kind = MaterialKind::Phong;
        m.base_color = color_rgba;
        m.ambient = ambient;
        m.specular_color = [
            specular_color_rgb[0],
            specular_color_rgb[1],
            specular_color_rgb[2],
            1.0,
        ];
        m.specular_power = specular_power;
        m.emission = premultiply_emission(emission_rgb, emission_intensity);
        m
    }

    /// Build a PBR material from the standard outer-card surface.
    #[allow(clippy::too_many_arguments)]
    pub fn pbr(
        color_rgba: [f32; 4],
        ambient: f32,
        metallic: f32,
        roughness: f32,
        emission_rgb: [f32; 3],
        emission_intensity: f32,
    ) -> Self {
        let mut m = Self::default_unlit_white();
        m.kind = MaterialKind::Pbr;
        m.base_color = color_rgba;
        m.ambient = ambient;
        m.metallic = metallic;
        m.roughness = roughness.max(0.01);
        m.emission = premultiply_emission(emission_rgb, emission_intensity);
        m
    }

    /// Build a Cel material from the standard outer-card surface.
    /// `cel_bands` is clamped to `[2, 16]` — too few bands degenerates to a
    /// silhouette, too many degrades to smooth shading and defeats the
    /// stylised look.
    #[allow(clippy::too_many_arguments)]
    pub fn cel(
        color_rgba: [f32; 4],
        cel_bands: u32,
        band_low: f32,
        band_high: f32,
        emission_rgb: [f32; 3],
        emission_intensity: f32,
    ) -> Self {
        let mut m = Self::default_unlit_white();
        m.kind = MaterialKind::Cel;
        m.base_color = color_rgba;
        m.cel_bands = cel_bands.clamp(2, 16);
        m.band_low = band_low;
        m.band_high = band_high;
        m.emission = premultiply_emission(emission_rgb, emission_intensity);
        m
    }

    /// Whether the renderer needs a `light` input wired when this material
    /// is in use. Mirrors the per-kind requirement table in the design doc
    /// (§5 "Conditional requirements"). Renderer-side validation reads this
    /// at runtime; preset-load validation (when the material's source is
    /// statically resolvable) reads it via the same helper.
    pub fn requires_light(&self) -> bool {
        match self.kind {
            MaterialKind::Unlit => false,
            MaterialKind::Phong | MaterialKind::Pbr | MaterialKind::Cel => true,
        }
    }

    /// Whether the renderer needs an `envmap` texture wired when this
    /// material is in use. Only PBR currently requires one (IBL reflection
    /// is non-optional for the Cook-Torrance path; without it the lit term
    /// is just direct light, which looks degenerate for the workhorse PBR
    /// preset).
    pub fn requires_envmap(&self) -> bool {
        matches!(self.kind, MaterialKind::Pbr)
    }
}

fn premultiply_emission(rgb: [f32; 3], intensity: f32) -> [f32; 4] {
    [rgb[0] * intensity, rgb[1] * intensity, rgb[2] * intensity, 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn default_unlit_white_is_inert_passthrough() {
        let m = Material::default_unlit_white();
        assert_eq!(m.kind, MaterialKind::Unlit);
        assert_eq!(m.base_color, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(m.emission, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(m.ambient, 0.0);
    }

    #[test]
    fn unlit_constructor_premultiplies_emission() {
        let m = Material::unlit([0.5, 0.6, 0.7, 1.0], [0.5, 0.4, 0.3], 2.0);
        assert_eq!(m.kind, MaterialKind::Unlit);
        assert_eq!(m.base_color, [0.5, 0.6, 0.7, 1.0]);
        assert!(approx_eq(m.emission[0], 1.0, 1e-5));
        assert!(approx_eq(m.emission[1], 0.8, 1e-5));
        assert!(approx_eq(m.emission[2], 0.6, 1e-5));
        assert_eq!(m.emission[3], 1.0);
    }

    #[test]
    fn phong_constructor_populates_specular_fields() {
        let m = Material::phong(
            [0.8, 0.85, 0.9, 1.0],
            0.15,
            [1.0, 0.9, 0.8],
            64.0,
            [0.0, 0.0, 0.0],
            0.0,
        );
        assert_eq!(m.kind, MaterialKind::Phong);
        assert_eq!(m.ambient, 0.15);
        assert_eq!(m.specular_color, [1.0, 0.9, 0.8, 1.0]);
        assert_eq!(m.specular_power, 64.0);
    }

    #[test]
    fn pbr_constructor_clamps_roughness_floor() {
        // Roughness exactly zero is a numerical landmine in the GGX
        // denominator — clamp to a safe floor at constructor time so
        // downstream shaders never see it.
        let m = Material::pbr([0.8, 0.8, 0.82, 1.0], 0.05, 1.0, 0.0, [0.0; 3], 0.0);
        assert_eq!(m.kind, MaterialKind::Pbr);
        assert!(m.roughness >= 0.01);
        assert_eq!(m.metallic, 1.0);
    }

    #[test]
    fn cel_constructor_clamps_band_count() {
        // 2..=16 is the design's documented valid range; anything outside
        // wraps to that range so downstream shader assumptions hold.
        let too_few = Material::cel([0.4, 0.6, 0.3, 1.0], 1, 0.08, 1.0, [0.0; 3], 0.0);
        assert_eq!(too_few.cel_bands, 2);
        let too_many = Material::cel([0.4, 0.6, 0.3, 1.0], 999, 0.08, 1.0, [0.0; 3], 0.0);
        assert_eq!(too_many.cel_bands, 16);
        let just_right = Material::cel([0.4, 0.6, 0.3, 1.0], 4, 0.08, 1.0, [0.0; 3], 0.0);
        assert_eq!(just_right.cel_bands, 4);
        assert_eq!(just_right.kind, MaterialKind::Cel);
    }

    #[test]
    fn unlit_does_not_require_light_or_envmap() {
        let m = Material::default_unlit_white();
        assert!(!m.requires_light());
        assert!(!m.requires_envmap());
    }

    #[test]
    fn phong_requires_light_but_not_envmap() {
        let m = Material::phong(
            [0.5, 0.5, 0.5, 1.0],
            0.15,
            [1.0, 1.0, 1.0],
            32.0,
            [0.0; 3],
            0.0,
        );
        assert!(m.requires_light());
        assert!(!m.requires_envmap());
    }

    #[test]
    fn cel_requires_light_but_not_envmap() {
        let m = Material::cel([0.4, 0.6, 0.3, 1.0], 4, 0.08, 1.0, [0.0; 3], 0.0);
        assert!(m.requires_light());
        assert!(!m.requires_envmap());
    }

    #[test]
    fn pbr_requires_both_light_and_envmap() {
        let m = Material::pbr([0.8, 0.8, 0.82, 1.0], 0.05, 1.0, 0.05, [0.0; 3], 0.0);
        assert!(m.requires_light());
        assert!(m.requires_envmap());
    }

    #[test]
    fn emission_with_zero_intensity_is_black() {
        // Zero intensity should produce black emission regardless of the
        // un-multiplied colour — the multiply-at-emission convention means
        // the consumer never has to worry about an "off" knob being
        // overridden by a non-zero RGB.
        let m = Material::unlit([1.0; 4], [1.0, 0.5, 0.25], 0.0);
        assert_eq!(m.emission, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn material_is_copy_and_cheap_to_clone() {
        // Trip-wire on the size — the per-frame copy cost matters because
        // every wire carries one. Ceiling raised 128 → 256 for
        // GLB_CONFORMANCE_DESIGN.md G-P4's five per-map UV transforms
        // (5 × 24 B) — a ~224-byte Copy per wire per frame is still far
        // below anything measurable next to a single texture bind.
        let sz = std::mem::size_of::<Material>();
        assert!(sz <= 256, "Material grew unexpectedly large: {sz} bytes");
        let m = Material::default_unlit_white();
        let _copy = m;
        let _another = m;
    }
}
