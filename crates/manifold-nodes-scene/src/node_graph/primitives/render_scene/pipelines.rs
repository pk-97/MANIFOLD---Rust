//! Render pipeline variants: aux-attachment shader specializations, get-or-compile pipeline lookup, and device-wide prewarm.

use super::*;

/// The shading every lit scene pass shares: `scene_shading.wgsl` (uniform,
/// lights, shadows, image-based lighting, fog, the PBR light loop) and the
/// look hooks, one file per look item. A pass prepends it to its own WGSL
/// with `concat!`, so the mesh renderer and a full-screen pass run the same
/// code.
macro_rules! scene_shading_wgsl {
    () => {
        concat!(
            include_str!("../shaders/scene_shading.wgsl"),
            include_str!("../shaders/look_ripple_detail.wgsl"),
            include_str!("../shaders/look_foam.wgsl"),
            include_str!("../shaders/look_mist.wgsl"),
            include_str!("../shaders/look_thin_water_light.wgsl"),
            include_str!("../shaders/look_wet_surface.wgsl"),
        )
    };
}

impl RenderScene {
    /// The mesh renderer's composed shader: the shared scene shading, then
    /// `render_scene.wgsl`.
    pub(super) const SHADER_WGSL: &'static str =
        concat!(scene_shading_wgsl!(), include_str!("../shaders/render_scene.wgsl"));

    /// GBUFFER_DESIGN.md section 2 D5 (P2) text-substitution patterns that turn
    /// the composed shader (unmodified — the velocity-off pipeline compiles
    /// it exactly as it ships) into the
    /// EMIT_VELOCITY variant: inject the `FsOut` struct and `VsOut`'s two
    /// extra clip-space fields, compute `clip_now`/`clip_prev` in
    /// `vs_main`, and rewrap every fragment entry's `-> @location(0)
    /// vec4<f32>` return into `-> FsOut` returning
    /// `(color, ndc_now.xy - ndc_prev.xy)`. All four fragment entry points
    /// share byte-identical signature/return text in the base file, so
    /// these two patterns apply once and cover all four — no per-material
    /// duplication, no second shader file (the whole point of the
    /// function-constant mechanism over a shader fork).
    pub(super) const VELOCITY_SPECIALIZATIONS: &'static [(&'static str, &'static str)] = &[
        (
            "// GBUFFER_FSOUT_VELOCITY_STRUCT",
            "struct FsOut {\n    @location(0) color: vec4<f32>,\n    @location(1) velocity: vec2<f32>,\n};",
        ),
        (
            "// GBUFFER_VSOUT_VELOCITY_FIELDS",
            "@location(3) clip_now: vec4<f32>,\n    @location(4) clip_prev: vec4<f32>,",
        ),
        (
            "// GBUFFER_VS_VELOCITY_BODY",
            "out.clip_now = out.clip_pos;\n    let prev_world = u.prev_model * vec4<f32>(prev_inst_pos, 1.0);\n    out.clip_prev = u.prev_view_proj * prev_world;",
        ),
        ("-> @location(0) vec4<f32> {", "-> FsOut {"),
        (
            "return apply_appearance(rgb, albedo.a, in.appearance_weight);",
            "return FsOut(apply_appearance(rgb, albedo.a, in.appearance_weight), (in.clip_now.xy / in.clip_now.w) - (in.clip_prev.xy / in.clip_prev.w) - (u.velocity_jitter.xy - u.velocity_jitter.zw));",
        ),
    ];

    /// RAYTRACING_DESIGN.md section 12 AM1: the EMIT_AO_MASK variant. Same
    /// substitution mechanism as velocity; the mask value is
    /// `u.fog_params.z` (`ao_mask_owed`, written CPU-side per object: 0 for
    /// unlit-kind materials and whenever RT is live, else 1 — AM2/AM6), so
    /// one shared replacement covers all four fragment entries with no
    /// per-kind shader text.
    pub(super) const AO_MASK_SPECIALIZATIONS: &'static [(&'static str, &'static str)] = &[
        (
            "// GBUFFER_FSOUT_VELOCITY_STRUCT",
            "struct FsOut {\n    @location(0) color: vec4<f32>,\n    @location(1) ao_mask: f32,\n};",
        ),
        ("-> @location(0) vec4<f32> {", "-> FsOut {"),
        (
            "return apply_appearance(rgb, albedo.a, in.appearance_weight);",
            "return FsOut(apply_appearance(rgb, albedo.a, in.appearance_weight), u.fog_params.z);",
        ),
    ];

    /// Velocity + ao_mask together: velocity keeps `@location(1)` (the pass
    /// encodes aux attachments in that order — velocity first, ao_mask
    /// second), ao_mask takes `@location(2)`.
    pub(super) const VELOCITY_AO_MASK_SPECIALIZATIONS: &'static [(&'static str, &'static str)] = &[
        (
            "// GBUFFER_FSOUT_VELOCITY_STRUCT",
            "struct FsOut {\n    @location(0) color: vec4<f32>,\n    @location(1) velocity: vec2<f32>,\n    @location(2) ao_mask: f32,\n};",
        ),
        (
            "// GBUFFER_VSOUT_VELOCITY_FIELDS",
            "@location(3) clip_now: vec4<f32>,\n    @location(4) clip_prev: vec4<f32>,",
        ),
        (
            "// GBUFFER_VS_VELOCITY_BODY",
            "out.clip_now = out.clip_pos;\n    let prev_world = u.prev_model * vec4<f32>(prev_inst_pos, 1.0);\n    out.clip_prev = u.prev_view_proj * prev_world;",
        ),
        ("-> @location(0) vec4<f32> {", "-> FsOut {"),
        (
            "return apply_appearance(rgb, albedo.a, in.appearance_weight);",
            "return FsOut(apply_appearance(rgb, albedo.a, in.appearance_weight), (in.clip_now.xy / in.clip_now.w) - (in.clip_prev.xy / in.clip_prev.w) - (u.velocity_jitter.xy - u.velocity_jitter.zw), u.fog_params.z);",
        ),
    ];

    /// RAYTRACING_DESIGN.md section 17.5 DN-E (DN4): denoise-only variant.
    /// Injects the FsOut struct with four denoiser G-buffer fields at
    /// locations 1–4 (color stays at 0), and rewrites the return statement
    /// to compute view-space normals, roughness, diffuse albedo, and
    /// specular albedo from the fragment's available values.
    pub(super) const DENOISE_ONLY_SPECIALIZATIONS: &'static [(&'static str, &'static str)] = &[
        (
            "// GBUFFER_FSOUT_VELOCITY_STRUCT",
            "struct FsOut {\n    @location(0) color: vec4<f32>,\n    @location(1) normals: vec4<f32>,\n    @location(2) roughness: f32,\n    @location(3) diffuse_albedo: vec4<f32>,\n    @location(4) specular_albedo: vec4<f32>,\n    @location(5) reactive: f32,\n};",
        ),
        ("-> @location(0) vec4<f32> {", "-> FsOut {"),
        (
            "return apply_appearance(rgb, albedo.a, in.appearance_weight);",
            "let vn = normalize((u.view_proj * vec4<f32>(in.world_normal, 0.0)).xyz);\n    let f0_dn = mix(vec3<f32>(0.04), albedo.rgb, u.pbr_metallic_roughness.x) * u.pbr_metallic_roughness.w;\n    let dn_reactive = select(0.0, 1.0, dot(resolve_emissive(in.uv), vec3<f32>(0.2126, 0.7152, 0.0722)) > 1e-3 || any(u.prev_model[0] != u.model[0]) || any(u.prev_model[1] != u.model[1]) || any(u.prev_model[2] != u.model[2]) || any(u.prev_model[3] != u.model[3]));\n    return FsOut(apply_appearance(rgb, albedo.a, in.appearance_weight), vec4<f32>(vn, 0.0), u.pbr_metallic_roughness.y, vec4<f32>(albedo.rgb, 1.0), vec4<f32>(f0_dn, 1.0), dn_reactive);",
        ),
    ];

    /// Denoise + velocity: velocity at location 1, denoise at 2–5.
    pub(super) const DENOISE_VELOCITY_SPECIALIZATIONS: &'static [(&'static str, &'static str)] = &[
        (
            "// GBUFFER_FSOUT_VELOCITY_STRUCT",
            "struct FsOut {\n    @location(0) color: vec4<f32>,\n    @location(1) velocity: vec2<f32>,\n    @location(2) normals: vec4<f32>,\n    @location(3) roughness: f32,\n    @location(4) diffuse_albedo: vec4<f32>,\n    @location(5) specular_albedo: vec4<f32>,\n    @location(6) reactive: f32,\n};",
        ),
        (
            "// GBUFFER_VSOUT_VELOCITY_FIELDS",
            "@location(3) clip_now: vec4<f32>,\n    @location(4) clip_prev: vec4<f32>,",
        ),
        (
            "// GBUFFER_VS_VELOCITY_BODY",
            "out.clip_now = out.clip_pos;\n    let prev_world = u.prev_model * vec4<f32>(prev_inst_pos, 1.0);\n    out.clip_prev = u.prev_view_proj * prev_world;",
        ),
        ("-> @location(0) vec4<f32> {", "-> FsOut {"),
        (
            "return apply_appearance(rgb, albedo.a, in.appearance_weight);",
            "let vn = normalize((u.view_proj * vec4<f32>(in.world_normal, 0.0)).xyz);\n    let f0_dn = mix(vec3<f32>(0.04), albedo.rgb, u.pbr_metallic_roughness.x) * u.pbr_metallic_roughness.w;\n    let dn_reactive = select(0.0, 1.0, dot(resolve_emissive(in.uv), vec3<f32>(0.2126, 0.7152, 0.0722)) > 1e-3 || any(u.prev_model[0] != u.model[0]) || any(u.prev_model[1] != u.model[1]) || any(u.prev_model[2] != u.model[2]) || any(u.prev_model[3] != u.model[3]));\n    return FsOut(apply_appearance(rgb, albedo.a, in.appearance_weight), (in.clip_now.xy / in.clip_now.w) - (in.clip_prev.xy / in.clip_prev.w) - (u.velocity_jitter.xy - u.velocity_jitter.zw), vec4<f32>(vn, 0.0), u.pbr_metallic_roughness.y, vec4<f32>(albedo.rgb, 1.0), vec4<f32>(f0_dn, 1.0), dn_reactive);",
        ),
    ];

    /// Denoise + ao_mask: ao_mask at location 1, denoise at 2–5.
    pub(super) const DENOISE_AO_MASK_SPECIALIZATIONS: &'static [(&'static str, &'static str)] = &[
        (
            "// GBUFFER_FSOUT_VELOCITY_STRUCT",
            "struct FsOut {\n    @location(0) color: vec4<f32>,\n    @location(1) ao_mask: f32,\n    @location(2) normals: vec4<f32>,\n    @location(3) roughness: f32,\n    @location(4) diffuse_albedo: vec4<f32>,\n    @location(5) specular_albedo: vec4<f32>,\n    @location(6) reactive: f32,\n};",
        ),
        ("-> @location(0) vec4<f32> {", "-> FsOut {"),
        (
            "return apply_appearance(rgb, albedo.a, in.appearance_weight);",
            "let vn = normalize((u.view_proj * vec4<f32>(in.world_normal, 0.0)).xyz);\n    let f0_dn = mix(vec3<f32>(0.04), albedo.rgb, u.pbr_metallic_roughness.x) * u.pbr_metallic_roughness.w;\n    let dn_reactive = select(0.0, 1.0, dot(resolve_emissive(in.uv), vec3<f32>(0.2126, 0.7152, 0.0722)) > 1e-3 || any(u.prev_model[0] != u.model[0]) || any(u.prev_model[1] != u.model[1]) || any(u.prev_model[2] != u.model[2]) || any(u.prev_model[3] != u.model[3]));\n    return FsOut(apply_appearance(rgb, albedo.a, in.appearance_weight), u.fog_params.z, vec4<f32>(vn, 0.0), u.pbr_metallic_roughness.y, vec4<f32>(albedo.rgb, 1.0), vec4<f32>(f0_dn, 1.0), dn_reactive);",
        ),
    ];

    /// Denoise + velocity + ao_mask: velocity at location 1, ao_mask at 2,
    /// denoise at 3–6.
    pub(super) const DENOISE_VELOCITY_AO_MASK_SPECIALIZATIONS: &'static [(&'static str, &'static str)] = &[
        (
            "// GBUFFER_FSOUT_VELOCITY_STRUCT",
            "struct FsOut {\n    @location(0) color: vec4<f32>,\n    @location(1) velocity: vec2<f32>,\n    @location(2) ao_mask: f32,\n    @location(3) normals: vec4<f32>,\n    @location(4) roughness: f32,\n    @location(5) diffuse_albedo: vec4<f32>,\n    @location(6) specular_albedo: vec4<f32>,\n    @location(7) reactive: f32,\n};",
        ),
        (
            "// GBUFFER_VSOUT_VELOCITY_FIELDS",
            "@location(3) clip_now: vec4<f32>,\n    @location(4) clip_prev: vec4<f32>,",
        ),
        (
            "// GBUFFER_VS_VELOCITY_BODY",
            "out.clip_now = out.clip_pos;\n    let prev_world = u.prev_model * vec4<f32>(prev_inst_pos, 1.0);\n    out.clip_prev = u.prev_view_proj * prev_world;",
        ),
        ("-> @location(0) vec4<f32> {", "-> FsOut {"),
        (
            "return apply_appearance(rgb, albedo.a, in.appearance_weight);",
            "let vn = normalize((u.view_proj * vec4<f32>(in.world_normal, 0.0)).xyz);\n    let f0_dn = mix(vec3<f32>(0.04), albedo.rgb, u.pbr_metallic_roughness.x) * u.pbr_metallic_roughness.w;\n    let dn_reactive = select(0.0, 1.0, dot(resolve_emissive(in.uv), vec3<f32>(0.2126, 0.7152, 0.0722)) > 1e-3 || any(u.prev_model[0] != u.model[0]) || any(u.prev_model[1] != u.model[1]) || any(u.prev_model[2] != u.model[2]) || any(u.prev_model[3] != u.model[3]));\n    return FsOut(apply_appearance(rgb, albedo.a, in.appearance_weight), (in.clip_now.xy / in.clip_now.w) - (in.clip_prev.xy / in.clip_prev.w) - (u.velocity_jitter.xy - u.velocity_jitter.zw), u.fog_params.z, vec4<f32>(vn, 0.0), u.pbr_metallic_roughness.y, vec4<f32>(albedo.rgb, 1.0), vec4<f32>(f0_dn, 1.0), dn_reactive);",
        ),
    ];

    /// The specialization list + aux attachment formats + label for an
    /// aux-emitting pipeline variant. One authority so `pipeline_for` and
    /// `prewarm_pipelines` can never disagree; `None` = no aux outputs (the
    /// plain non-specialized pipeline).
    #[allow(clippy::type_complexity)]
    pub(super) fn aux_variant(
        emit_velocity: bool,
        emit_ao_mask: bool,
        emit_denoise_feed: bool,
    ) -> Option<(
        &'static [(&'static str, &'static str)],
        &'static [manifold_gpu::GpuTextureFormat],
        &'static str,
    )> {
        use manifold_gpu::GpuTextureFormat::{R16Float, R8Unorm, Rg16Float, Rgba16Float};
        match (emit_velocity, emit_ao_mask, emit_denoise_feed) {
            (false, false, false) => None,
            (true, false, false) => Some((
                Self::VELOCITY_SPECIALIZATIONS,
                &[Rg16Float],
                "node.render_scene.velocity",
            )),
            (false, true, false) => Some((
                Self::AO_MASK_SPECIALIZATIONS,
                &[R8Unorm],
                "node.render_scene.ao_mask",
            )),
            (true, true, false) => Some((
                Self::VELOCITY_AO_MASK_SPECIALIZATIONS,
                &[Rg16Float, R8Unorm],
                "node.render_scene.velocity.ao_mask",
            )),
            // Denoise-only: 5 aux attachments after color (DN-L added the
            // reactive mask, R16Float).
            (false, false, true) => Some((
                Self::DENOISE_ONLY_SPECIALIZATIONS,
                &[Rgba16Float, R16Float, Rgba16Float, Rgba16Float, R16Float],
                "node.render_scene.denoise",
            )),
            // Denoise + velocity.
            (true, false, true) => Some((
                Self::DENOISE_VELOCITY_SPECIALIZATIONS,
                &[Rg16Float, Rgba16Float, R16Float, Rgba16Float, Rgba16Float, R16Float],
                "node.render_scene.velocity.denoise",
            )),
            // Denoise + ao_mask.
            (false, true, true) => Some((
                Self::DENOISE_AO_MASK_SPECIALIZATIONS,
                &[R8Unorm, Rgba16Float, R16Float, Rgba16Float, Rgba16Float, R16Float],
                "node.render_scene.ao_mask.denoise",
            )),
            // Denoise + velocity + ao_mask. 7 aux + color = 8 attachments,
            // exactly Metal's guaranteed limit — do not add another aux
            // output to this variant without dropping one.
            (true, true, true) => Some((
                Self::DENOISE_VELOCITY_AO_MASK_SPECIALIZATIONS,
                &[Rg16Float, R8Unorm, Rgba16Float, R16Float, Rgba16Float, Rgba16Float, R16Float],
                "node.render_scene.velocity.ao_mask.denoise",
            )),
        }
    }

    /// IMPORT_FIDELITY_DESIGN.md D8: classic straight-alpha "over" blend —
    /// `src_alpha / one_minus_src_alpha` on colour, and the matching
    /// straight-alpha formula on the output alpha channel (`src.a + dst.a *
    /// (1 - src.a)`). Never premultiplied (alpha-standardisation: this
    /// producer is straight-alpha, same convention as the P3 fog pass).
    pub(super) fn blend_state() -> manifold_gpu::GpuBlendState {
        manifold_gpu::GpuBlendState {
            src_factor: manifold_gpu::GpuBlendFactor::SrcAlpha,
            dst_factor: manifold_gpu::GpuBlendFactor::OneMinusSrcAlpha,
            operation: manifold_gpu::GpuBlendOp::Add,
            src_alpha_factor: manifold_gpu::GpuBlendFactor::One,
            dst_alpha_factor: manifold_gpu::GpuBlendFactor::OneMinusSrcAlpha,
            alpha_operation: manifold_gpu::GpuBlendOp::Add,
        }
    }

    /// IMPORT_FIDELITY_DESIGN.md D8/F-P5: `blend` selects the Blend-material
    /// pipeline variant — real alpha blending, no alpha-to-coverage (which
    /// would double up with the blend state). Same shader source and entry
    /// points as the opaque/mask variant; only the pipeline's fixed-function
    /// blend + alpha-to-coverage state differs (D8: "lighting, IBL, and fog
    /// run identically in both passes").
    ///
    /// RAYTRACING_DESIGN.md section 17.5 DN-E (DN4): `emit_denoise_feed`
    /// selects the denoiser G-buffer MRT pipeline variant — four additional
    /// fragment shader outputs (normals, roughness, diffuse albedo, specular
    /// albedo) at locations after the existing aux outputs.
    ///
    /// SCENE_RENDER_MODE_DESIGN.md D8: `points` selects the Points pipeline
    /// — `vs_points` (the `[[point_size]]` vertex entry, its `@location(7)`
    /// output rebound by `create_render_pipeline_depth_msaa_point_size`)
    /// with `fs_unlit` and NO aux outputs: `VsOutPoints` carries no velocity
    /// or ao_mask fields, and Points is a shading audit mode, not a
    /// temporal-upscale input — under Points the aux targets keep their
    /// clear values for the frame. The material substitution guarantees
    /// Unlit, so the key's `kind` is informational for points draws.
    pub(super) fn pipeline_for(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        kind: MaterialKind,
        emit_velocity: bool,
        emit_ao_mask: bool,
        emit_denoise_feed: bool,
        blend: bool,
        points: bool,
    ) -> &manifold_gpu::GpuRenderPipeline {
        if points {
            // D8: the points pipeline is the plain variant — no aux, no
            // blend (the substituted line material is opaque). Forcing the
            // flags here (not at every call site) keeps the no-aux contract
            // in one place.
            return self
                .pipelines
                .entry((MaterialKind::Unlit, false, false, false, false, true, MSAA_SAMPLES))
                .or_insert_with(|| {
                    device.create_render_pipeline_depth_msaa_point_size(
                        Self::SHADER_WGSL,
                        "vs_points",
                        "fs_unlit",
                        7,
                        manifold_gpu::GpuTextureFormat::Rgba16Float,
                        manifold_gpu::GpuTextureFormat::Depth32Float,
                        None,
                        MSAA_SAMPLES,
                        true,
                        "node.render_scene points",
                    )
                });
        }
        let fs_entry = match kind {
            MaterialKind::Unlit => "fs_unlit",
            MaterialKind::Pbr => "fs_pbr",
            MaterialKind::Cel => "fs_cel",
        };
        self.pipelines
            .entry((kind, emit_velocity, emit_ao_mask, emit_denoise_feed, blend, false, MSAA_SAMPLES))
            .or_insert_with(|| {
                let blend_state = if blend { Some(Self::blend_state()) } else { None };
                if let Some((specs, aux_formats, label)) =
                    Self::aux_variant(emit_velocity, emit_ao_mask, emit_denoise_feed)
                {
                    device.create_specialized_render_pipeline_depth_msaa(
                        Self::SHADER_WGSL,
                        "vs_main",
                        fs_entry,
                        specs,
                        manifold_gpu::GpuTextureFormat::Rgba16Float,
                        manifold_gpu::GpuTextureFormat::Depth32Float,
                        blend_state,
                        MSAA_SAMPLES,
                        !blend,
                        aux_formats,
                        label,
                    )
                } else {
                    device.create_render_pipeline_depth_msaa(
                        Self::SHADER_WGSL,
                        "vs_main",
                        fs_entry,
                        manifold_gpu::GpuTextureFormat::Rgba16Float,
                        manifold_gpu::GpuTextureFormat::Depth32Float,
                        blend_state,
                        MSAA_SAMPLES,
                        // alpha-to-coverage antialiases the cutout `discard`
                        // edge for Opaque/Mask; a Blend pipeline has no
                        // discard and already blends per-fragment alpha, so
                        // it stays off (the two mechanisms would double up).
                        !blend,
                        "node.render_scene",
                    )
                }
            })
    }

    /// The Blend pipeline for the transmissive Pass B: Rgba16Float color and
    /// Depth32Float depth, both 1x, no aux outputs, real blending, no
    /// alpha-to-coverage. Pass A's `MSAA_SAMPLES`x pipelines never bind there.
    pub(super) fn pass_b_pipeline_for(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        kind: MaterialKind,
    ) -> &manifold_gpu::GpuRenderPipeline {
        self.pipelines
            .entry((kind, false, false, false, true, false, 1))
            .or_insert_with(|| Self::create_pass_b_pipeline(device, kind))
    }

    pub(super) fn create_pass_b_pipeline(device: &manifold_gpu::GpuDevice, kind: MaterialKind) -> manifold_gpu::GpuRenderPipeline {
        let fs_entry = match kind {
            MaterialKind::Unlit => "fs_unlit",
            MaterialKind::Pbr => "fs_pbr",
            MaterialKind::Cel => "fs_cel",
        };
        device.create_render_pipeline_depth_msaa(
            Self::SHADER_WGSL,
            "vs_main",
            fs_entry,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            manifold_gpu::GpuTextureFormat::Depth32Float,
            Some(Self::blend_state()),
            1,
            false,
            "node.render_scene transmissive pass",
        )
    }

    /// BUG-037: compile every `pipeline_for` (`MaterialKind`, `emit_velocity`)
    /// variant into `device`'s shared render-pipeline cache ahead of time.
    /// `pipeline_for` compiles the requested variant lazily on the first draw
    /// that needs it — a real Metal render-pipeline compile (multisampled,
    /// alpha-to-coverage), tens of ms — which is what stalled the content
    /// thread ~37ms on a glTF scene layer's first rendered frame
    /// (`RENDER_TRACE` showed `generators=37.1ms`). The device's pipeline
    /// cache (`manifold-gpu`, keyed by shader hash) is shared across every
    /// `RenderScene` instance and every layer, so warming all 8 fixed
    /// variants once — independent of any specific project or asset — makes
    /// every later `pipeline_for` call, on any layer, a cache hit. Called
    /// from `GeneratorRegistry::prewarm_all` at app startup, the same
    /// sanctioned "compile pipelines ahead of the hot path" mechanism used
    /// for the bundled JSON generator presets.
    pub fn prewarm_pipelines(device: &manifold_gpu::GpuDevice) {
        for kind in [
            MaterialKind::Unlit,
            MaterialKind::Pbr,
            MaterialKind::Cel,
        ] {
            let fs_entry = match kind {
                MaterialKind::Unlit => "fs_unlit",
                MaterialKind::Pbr => "fs_pbr",
                MaterialKind::Cel => "fs_cel",
            };
            // IMPORT_FIDELITY_DESIGN.md D8/F-P5: the Blend-material pipeline
            // variants (BUG-037 discipline — a live project's first glass
            // draw must be a cache hit, not a first-use compile stall).
            // RAYTRACING_DESIGN.md section 12 AM1 adds the ao_mask aux
            // dimension — same lazy-compile-stall argument: every import
            // graph wires ao_mask, so its variants must be warm. All
            // compiles below are on-disk-cache hits after the first launch.
            Self::create_pass_b_pipeline(device, kind);
            for blend in [false, true] {
                let blend_state = blend.then(Self::blend_state);
                let a2c = !blend;
                for (emit_velocity, emit_ao_mask, emit_denoise_feed) in [
                    (false, false, false),
                    (true, false, false),
                    (false, true, false),
                    (true, true, false),
                    (false, false, true),
                    (true, false, true),
                    (false, true, true),
                    (true, true, true),
                ] {
                    if let Some((specs, aux_formats, label)) =
                        Self::aux_variant(emit_velocity, emit_ao_mask, emit_denoise_feed)
                    {
                        device.create_specialized_render_pipeline_depth_msaa(
                            Self::SHADER_WGSL,
                            "vs_main",
                            fs_entry,
                            specs,
                            manifold_gpu::GpuTextureFormat::Rgba16Float,
                            manifold_gpu::GpuTextureFormat::Depth32Float,
                            blend_state,
                            MSAA_SAMPLES,
                            a2c,
                            aux_formats,
                            label,
                        );
                    } else {
                        device.create_render_pipeline_depth_msaa(
                            Self::SHADER_WGSL,
                            "vs_main",
                            fs_entry,
                            manifold_gpu::GpuTextureFormat::Rgba16Float,
                            manifold_gpu::GpuTextureFormat::Depth32Float,
                            blend_state,
                            MSAA_SAMPLES,
                            a2c,
                            "node.render_scene",
                        );
                    }
                }
            }
        }

        // BUG-found-by-P3-perf-gate (VOLUMETRIC_LIGHT_DESIGN.md P3): the
        // shadow depth-only pipeline (`ensure_shadow_pass`) and the three
        // shaft pipelines (`ensure_shaft_pipelines`) are BOTH asset-
        // independent (fixed shader source, fixed entry points) — the exact
        // BUG-037 shape (a lazy `Option::get_or_insert_with` cache never
        // touched by `GeneratorRegistry::prewarm_all`'s bundled-preset loop,
        // because no bundled preset happens to hit a shadow-casting light or
        // a nonzero shaft_intensity). The P3 content-thread perf gate
        // (`MANIFOLD_RENDER_TRACE=1`, a 4-shadow-casting-light High-quality
        // scene) measured frame 0 at 79.9ms — entirely first-use pipeline
        // compiles, steady state (frames 1-119) stayed under the 20ms
        // budget with no RENDER_TRACE spike at all. Root fix, not a
        // per-scene workaround: warm all four pipelines here so a live
        // project's first shadow-casting/shaft-on frame is a cache hit.
        device.create_render_pipeline_depth_only(
            include_str!("../shaders/shadow_depth.wgsl"),
            "vs_main",
            "fs_shadow",
            manifold_gpu::GpuTextureFormat::Depth32Float,
            "node.render_scene shadow depth",
        );
        device.create_compute_pipeline(
            include_str!("../shaders/shaft_depth_downsample.wgsl"),
            "cs_main",
            "node.render_scene shaft downsample",
        );
        device.create_compute_pipeline(
            include_str!("../shaders/shaft_march.wgsl"),
            "cs_main",
            "node.render_scene shaft march",
        );
        let shaft_composite_blend = manifold_gpu::GpuBlendState {
            src_factor: manifold_gpu::GpuBlendFactor::One,
            dst_factor: manifold_gpu::GpuBlendFactor::One,
            operation: manifold_gpu::GpuBlendOp::Add,
            src_alpha_factor: manifold_gpu::GpuBlendFactor::Zero,
            dst_alpha_factor: manifold_gpu::GpuBlendFactor::One,
            alpha_operation: manifold_gpu::GpuBlendOp::Add,
        };
        device.create_render_pipeline(
            include_str!("../shaders/shaft_composite.wgsl"),
            "vs_main",
            "fs_main",
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            Some(shaft_composite_blend),
            "node.render_scene shaft composite",
        );

        // SCENE_RENDER_MODE_DESIGN.md D8 (P3), same BUG-037 discipline: the
        // Points pipeline (`pipeline_for`'s points slot — vs_points +
        // fs_unlit, the `[[point_size]]` rebind) is asset-independent and
        // lazily compiled on the first Points frame without this. Warm it so
        // flipping a scene to Points mid-show is a cache hit, not a stall.
        device.create_render_pipeline_depth_msaa_point_size(
            Self::SHADER_WGSL,
            "vs_points",
            "fs_unlit",
            7,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            manifold_gpu::GpuTextureFormat::Depth32Float,
            None,
            MSAA_SAMPLES,
            true,
            "node.render_scene points",
        );

        // COMPILE_CONTRACT_DESIGN P2: IBL pipelines are fixed-source,
        // asset-independent (shader source depends only on render_scene's
        // own WGSL files, not on any envmap content). Warm them here so
        // the first frame that enables IBL is a cache hit, not a mid-show
        // compile stall.
        device.create_compute_pipeline(
            concat!(
                include_str!("../shaders/pbr_brdf.wgsl"),
                include_str!("../shaders/ibl_prefilter_specular.wgsl"),
            ),
            "cs_main",
            "node.render_scene ibl prefilter",
        );
        device.create_compute_pipeline(
            concat!(
                include_str!("../shaders/pbr_brdf.wgsl"),
                include_str!("../shaders/ibl_irradiance.wgsl"),
            ),
            "cs_main",
            "node.render_scene ibl irradiance",
        );
        device.create_compute_pipeline(
            concat!(
                include_str!("../shaders/pbr_brdf.wgsl"),
                include_str!("../shaders/ibl_brdf_lut.wgsl"),
            ),
            "cs_main",
            "node.render_scene ibl brdf lut",
        );

        // COMPILE_CONTRACT_DESIGN P2: hit_dist_extract and upscale_alpha_combine
        // are both fixed-source compute pipelines, independent of scene content.
        // Warm them here so their first use is a cache hit.
        device.create_compute_pipeline(
            include_str!("../shaders/hit_dist_extract.wgsl"),
            "cs_main",
            "node.render_scene hit_dist_extract",
        );
        device.create_compute_pipeline(
            include_str!("../shaders/upscale_alpha_combine.wgsl"),
            "cs_main",
            "node.render_scene upscale_alpha_combine",
        );
    }
}
