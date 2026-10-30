//! Render pipeline variants: aux-attachment shader specializations, get-or-compile pipeline lookup, and device-wide prewarm.

use super::*;

impl RenderScene {
    /// Text-substitution patterns that turn
    /// the base `render_scene.wgsl` (unmodified — the velocity-off
    /// pipeline compiles the file exactly as it ships) into the
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
    pub(super) const DENOISE_VELOCITY_AO_MASK_SPECIALIZATIONS: &'static [(
        &'static str,
        &'static str,
    )] = &[
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

    /// The subsurface light pass (`subsurface.rs`): `fs_pbr` returns the
    /// scattering surface's diffuse light before its colour, with light
    /// from behind through its thin parts, and its slot in alpha. The main
    /// pass keeps the marker comment, so it never computes this.
    pub(super) const SUBSURFACE_LIGHT_SPECIALIZATIONS: &'static [(&'static str, &'static str)] = &[
        (
            "        // subsurface light pass (only in its pipeline, `SUBSURFACE_LIGHT_SPECIALIZATIONS`)\n",
            "        return vec4<f32>(\n            direct.diffuse_light + kd_ibl * ibl.irradiance * occlusion\n                + subsurface_through(in.world_pos, N, subsurface_look.distance.rgb),\n            subsurface_look.weight_slot.y,\n        );\n",
        ),
    ];

    /// The solid-surface variant: drops every `discard` from the three
    /// fragment entries (the `fs_pbr` volume-nearest test, and the
    /// appearance-fade and alpha-cutout tests all three share). A fragment
    /// shader that can discard defeats hidden-surface removal on Apple GPUs, so
    /// occluded surfaces behind an opaque panel still get shaded. Only valid
    /// for a draw where each removed test is provably false (see
    /// `solid_surface`), or for a cut-out's shading under depth Equal after
    /// its depth draw ran the tests.
    pub(super) const OPAQUE_FAST_SPECIALIZATIONS: &'static [(&'static str, &'static str)] = &[
        (
            "    if u.volume_optics.x > 0.5 {\n        let nearest = textureLoad(volume_nearest, vec2<i32>(in.clip_pos.xy), 0);\n        if in.clip_pos.z < nearest - max(nearest * 0.00001, 0.0000001) { discard; }\n    }\n",
            "",
        ),
        (
            "    if appearance_discard(in.appearance_weight) {\n        discard;\n    }\n    if u.alpha_params.x == 1.0 && albedo.a < u.alpha_params.y {\n        discard;\n    }\n",
            "",
        ),
    ];

    /// The parallax variant: traces the height field in `begin_surface` for
    /// a material with Depth, box mapped or on the mesh's UVs, and writes the
    /// carved depth so other objects meet the relief. Every other draw
    /// compiles without the trace, because its registers alone slow every
    /// PBR pixel even when the trace never runs. Only valid for a draw where
    /// `parallax_surface` holds. Applied after the aux edits: the depth
    /// output joins their `FsOut`, or makes one for the plain variant.
    pub(super) const PARALLAX_SPECIALIZATIONS: &'static [(&'static str, &'static str)] = &[
        (
            "    // parallax trace (only in the parallax pipeline, `PARALLAX_SPECIALIZATIONS`)\n",
            "    p = box_parallax(p, px, py, n, s, mat3x3<f32>(ax, ay, az), world_pos, pixel);\n",
        ),
        (
            "        // uv parallax (only in the parallax pipeline, `PARALLAX_SPECIALIZATIONS`)\n",
            "        uv_parallax(uv, world_normal, world_pos, pixel);\n",
        ),
        // The UV path's reads keep the unshifted gradients, so a cliff in
        // the height never jumps the mip or bends the tangent frame.
        (
            "    return textureSampleBias(tex, samp, material_uv(uv, index), u.render_mode.y);",
            "    return textureSampleGrad(tex, samp, material_uv(uv, index), flat_uv_dx(uv, index), flat_uv_dy(uv, index));",
        ),
        (
            "textureSampleBias(normal_map, normal_sampler, uv_t, u.render_mode.y)",
            "textureSampleGrad(normal_map, normal_sampler, uv_t, flat_uv_dx(uv, 1u), flat_uv_dy(uv, 1u))",
        ),
        (
            "let tbn = tbn_for(n, world_pos, uv_t, tangent);",
            "let tbn = tbn_for(n, world_pos, material_uv_flat(uv, 1u), tangent);",
        ),
        (
            "let footprint = max(length(dpdx(coord) * dim), length(dpdy(coord) * dim));",
            "let footprint = max(length(dpdx(material_uv_flat(uv, index)) * dim), length(dpdy(material_uv_flat(uv, index)) * dim));",
        ),
        // Carving only pushes depth away from the eye, so the early depth
        // test stays valid.
        (
            "@fragment\nfn fs_pbr(",
            "@fragment\n@early_depth_test(greater_equal)\nfn fs_pbr(",
        ),
        (
            "struct FsOut {\n    @location(0) color: vec4<f32>,",
            "struct FsOut {\n    @builtin(frag_depth) depth: f32,\n    @location(0) color: vec4<f32>,",
        ),
        (
            "return FsOut(",
            "return FsOut(carved_depth(in.clip_pos.z, in.world_pos), ",
        ),
        (
            "// GBUFFER_FSOUT_VELOCITY_STRUCT",
            "struct FsOut {\n    @builtin(frag_depth) depth: f32,\n    @location(0) color: vec4<f32>,\n};",
        ),
        ("-> @location(0) vec4<f32> {", "-> FsOut {"),
        (
            "return apply_appearance(rgb, albedo.a, in.appearance_weight);",
            "return FsOut(carved_depth(in.clip_pos.z, in.world_pos), apply_appearance(rgb, albedo.a, in.appearance_weight));",
        ),
    ];

    /// The solid depth-only variant of `shaders/shadow_depth.wgsl`: drops the
    /// cutout and appearance-fade `discard` block that all three fragment
    /// entries share. A fragment shader that can discard defeats Apple's
    /// hidden-surface removal, so the shadow maps and the camera depth
    /// prepass would shade occluded solid geometry. Only valid for a draw
    /// where `ShadowUniforms::discard_free` holds.
    pub(super) const SOLID_DEPTH_SPECIALIZATIONS: &'static [(&'static str, &'static str)] = &[(
        "    if su.alpha.x == 1.0 {\n        var alpha = su.alpha.z * in.vertex_alpha;\n        if su.alpha.w > 0.5 { alpha *= textureSample(base_color_map, base_color_sampler, in.uv).a; }\n        if alpha < su.alpha.y { discard; }\n    }\n    if su.appearance.x * in.appearance_weight <= 0.0 {\n        discard;\n    }\n",
        "",
    )];

    /// The solid depth-only shader source: `shadow_depth.wgsl` with
    /// `SOLID_DEPTH_SPECIALIZATIONS` applied.
    pub(super) fn solid_depth_source() -> String {
        let mut source = include_str!("../shaders/shadow_depth.wgsl").to_string();
        for &(pattern, replacement) in Self::SOLID_DEPTH_SPECIALIZATIONS {
            source = source.replace(pattern, replacement);
        }
        source
    }

    /// Compile the depth-only pipeline for the shadow maps and the camera
    /// depth prepass: the plain one, or the solid variant. The one
    /// constructor behind `ensure_shadow_pass` and `prewarm_pipelines`, so
    /// they can never disagree on a variant's source.
    pub(super) fn create_depth_only_pipeline(
        device: &manifold_gpu::GpuDevice,
        solid: bool,
    ) -> manifold_gpu::GpuRenderPipeline {
        if solid {
            device.create_render_pipeline(
                &manifold_gpu::RenderPipelineDesc::new(
                    &Self::solid_depth_source(),
                    "vs_main",
                    "fs_shadow",
                    "node.render_scene shadow depth solid",
                )
                .depth(manifold_gpu::GpuTextureFormat::Depth32Float),
            )
        } else {
            device.create_render_pipeline(
                &manifold_gpu::RenderPipelineDesc::new(
                    include_str!("../shaders/shadow_depth.wgsl"),
                    "vs_main",
                    "fs_shadow",
                    "node.render_scene shadow depth",
                )
                .depth(manifold_gpu::GpuTextureFormat::Depth32Float),
            )
        }
    }

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
        use manifold_gpu::GpuTextureFormat::{R8Unorm, R16Float, Rg16Float, Rgba16Float};
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
                &[
                    Rg16Float,
                    Rgba16Float,
                    R16Float,
                    Rgba16Float,
                    Rgba16Float,
                    R16Float,
                ],
                "node.render_scene.velocity.denoise",
            )),
            // Denoise + ao_mask.
            (false, true, true) => Some((
                Self::DENOISE_AO_MASK_SPECIALIZATIONS,
                &[
                    R8Unorm,
                    Rgba16Float,
                    R16Float,
                    Rgba16Float,
                    Rgba16Float,
                    R16Float,
                ],
                "node.render_scene.ao_mask.denoise",
            )),
            // Denoise + velocity + ao_mask. 7 aux + color = 8 attachments,
            // exactly Metal's guaranteed limit — do not add another aux
            // output to this variant without dropping one.
            (true, true, true) => Some((
                Self::DENOISE_VELOCITY_AO_MASK_SPECIALIZATIONS,
                &[
                    Rg16Float,
                    R8Unorm,
                    Rgba16Float,
                    R16Float,
                    Rgba16Float,
                    Rgba16Float,
                    R16Float,
                ],
                "node.render_scene.velocity.ao_mask.denoise",
            )),
        }
    }

    /// Classic straight-alpha "over" blend —
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

    /// `blend` selects the Blend-material
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
    ///
    /// `opaque_fast` selects the solid-surface variant (see
    /// `OPAQUE_FAST_SPECIALIZATIONS`): no `discard` and no alpha-to-coverage.
    /// The caller must only set it for a non-blend draw where `solid_surface`
    /// holds. `parallax` selects the height-tracing variant (see
    /// `PARALLAX_SPECIALIZATIONS`), set where `parallax_surface` holds.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn pipeline_for(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        kind: MaterialKind,
        emit_velocity: bool,
        emit_ao_mask: bool,
        emit_denoise_feed: bool,
        blend: bool,
        points: bool,
        opaque_fast: bool,
        parallax: bool,
    ) -> &manifold_gpu::GpuRenderPipeline {
        if points {
            // D8: the points pipeline is the plain variant — no aux, no
            // blend (the substituted line material is opaque). Forcing the
            // flags here (not at every call site) keeps the no-aux contract
            // in one place.
            return self
                .pipelines
                .entry((
                    MaterialKind::Unlit,
                    false,
                    false,
                    false,
                    false,
                    true,
                    false,
                    false,
                    MSAA_SAMPLES,
                ))
                .or_insert_with(|| {
                    device.create_render_pipeline(
                        &manifold_gpu::RenderPipelineDesc::new(
                            RENDER_SCENE_WGSL.as_str(),
                            "vs_points",
                            "fs_unlit",
                            "node.render_scene points",
                        )
                        .color(manifold_gpu::GpuTextureFormat::Rgba16Float, None)
                        .depth(manifold_gpu::GpuTextureFormat::Depth32Float)
                        .samples(MSAA_SAMPLES)
                        .alpha_to_coverage(true)
                        .point_size(7),
                    )
                });
        }
        debug_assert!(!(blend && opaque_fast), "a blend draw is never solid");
        self.pipelines
            .entry((
                kind,
                emit_velocity,
                emit_ao_mask,
                emit_denoise_feed,
                blend,
                false,
                opaque_fast,
                parallax,
                MSAA_SAMPLES,
            ))
            .or_insert_with(|| {
                Self::create_variant(
                    device,
                    kind,
                    emit_velocity,
                    emit_ao_mask,
                    emit_denoise_feed,
                    blend,
                    opaque_fast,
                    parallax,
                )
            })
    }

    /// Compile one Pass A pipeline variant. The one constructor behind both
    /// `pipeline_for` and `prewarm_pipelines`, so they can never disagree on
    /// a variant's source, blend state or alpha-to-coverage.
    #[allow(clippy::too_many_arguments)]
    fn create_variant(
        device: &manifold_gpu::GpuDevice,
        kind: MaterialKind,
        emit_velocity: bool,
        emit_ao_mask: bool,
        emit_denoise_feed: bool,
        blend: bool,
        opaque_fast: bool,
        parallax: bool,
    ) -> manifold_gpu::GpuRenderPipeline {
        let fs_entry = match kind {
            MaterialKind::Unlit => "fs_unlit",
            MaterialKind::Pbr => "fs_pbr",
            MaterialKind::Cel => "fs_cel",
        };
        let blend_state = blend.then(Self::blend_state);
        // Alpha-to-coverage antialiases the cutout `discard` edge for
        // Opaque/Mask; a Blend pipeline has no discard and already blends
        // per-fragment alpha, so it stays off (the two mechanisms would
        // double up). The solid-surface variant has no discard and writes
        // alpha exactly 1, so its coverage is every sample and it stays off
        // too.
        let alpha_to_coverage = !blend && !opaque_fast;
        let (aux_specs, aux_formats, label) = Self::aux_variant(
            emit_velocity,
            emit_ao_mask,
            emit_denoise_feed,
        )
        .unwrap_or((&[][..], &[][..], "node.render_scene"));
        let mut specs = aux_specs.to_vec();
        if opaque_fast {
            specs.extend_from_slice(Self::OPAQUE_FAST_SPECIALIZATIONS);
        }
        if parallax {
            specs.extend_from_slice(Self::PARALLAX_SPECIALIZATIONS);
        }
        device.create_render_pipeline(
            &manifold_gpu::RenderPipelineDesc::new(
                RENDER_SCENE_WGSL.as_str(),
                "vs_main",
                fs_entry,
                label,
            )
            .color(manifold_gpu::GpuTextureFormat::Rgba16Float, blend_state)
            .depth(manifold_gpu::GpuTextureFormat::Depth32Float)
            .samples(MSAA_SAMPLES)
            .alpha_to_coverage(alpha_to_coverage)
            .aux_colors(aux_formats)
            .specialize(&specs),
        )
    }

    /// A cut-out's depth draw in Pass A (`shaders/cutout_depth.wgsl`), for
    /// the frame's aux outputs.
    pub(super) fn cutout_depth_pipeline_for(
        &mut self,
        device: &manifold_gpu::GpuDevice,
        emit_velocity: bool,
        emit_ao_mask: bool,
        emit_denoise_feed: bool,
    ) -> &manifold_gpu::GpuRenderPipeline {
        self.cutout_depth_pipelines
            .entry((emit_velocity, emit_ao_mask, emit_denoise_feed))
            .or_insert_with(|| {
                Self::create_cutout_depth_pipeline(
                    device,
                    emit_velocity,
                    emit_ao_mask,
                    emit_denoise_feed,
                )
            })
    }

    /// Compiled from the solid-surface variant's exact source, so its vertex
    /// stage, and the depth it writes, match the shading draw's under depth
    /// Equal. Alpha-to-coverage dithers an appearance fade as the cut-out
    /// pipeline does.
    fn create_cutout_depth_pipeline(
        device: &manifold_gpu::GpuDevice,
        emit_velocity: bool,
        emit_ao_mask: bool,
        emit_denoise_feed: bool,
    ) -> manifold_gpu::GpuRenderPipeline {
        let (aux_specs, aux_formats, _) = Self::aux_variant(
            emit_velocity,
            emit_ao_mask,
            emit_denoise_feed,
        )
        .unwrap_or((&[][..], &[][..], ""));
        let mut specs = aux_specs.to_vec();
        specs.extend_from_slice(Self::OPAQUE_FAST_SPECIALIZATIONS);
        device.create_render_pipeline(
            &manifold_gpu::RenderPipelineDesc::new(
                RENDER_SCENE_WGSL.as_str(),
                "vs_main",
                "fs_cutout_depth",
                "node.render_scene cutout depth",
            )
            .color(manifold_gpu::GpuTextureFormat::Rgba16Float, None)
            .depth(manifold_gpu::GpuTextureFormat::Depth32Float)
            .samples(MSAA_SAMPLES)
            .alpha_to_coverage(true)
            .aux_colors(aux_formats)
            .specialize(&specs),
        )
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
            .entry((kind, false, false, false, true, false, false, false, 1))
            .or_insert_with(|| Self::create_pass_b_pipeline(device, kind))
    }

    pub(super) fn create_pass_b_pipeline(
        device: &manifold_gpu::GpuDevice,
        kind: MaterialKind,
    ) -> manifold_gpu::GpuRenderPipeline {
        let fs_entry = match kind {
            MaterialKind::Unlit => "fs_unlit",
            MaterialKind::Pbr => "fs_pbr",
            MaterialKind::Cel => "fs_cel",
        };
        device.create_render_pipeline(
            &manifold_gpu::RenderPipelineDesc::new(
                RENDER_SCENE_WGSL.as_str(),
                "vs_main",
                fs_entry,
                "node.render_scene transmissive pass",
            )
            .color(
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                Some(Self::blend_state()),
            )
            .depth(manifold_gpu::GpuTextureFormat::Depth32Float),
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
        // The Blend-material pipeline variants (BUG-037 discipline — a live
        // project's first glass draw must be a cache hit, not a first-use
        // compile stall). RAYTRACING_DESIGN.md section 12 AM1 adds the
        // ao_mask aux dimension — same lazy-compile-stall argument: every
        // import graph wires ao_mask, so its variants must be warm. The
        // solid-surface variants (never blend) are the common case for
        // opaque geometry. All compiles below are on-disk-cache hits after
        // the first launch.
        //
        // After an edit to the scene shader every variant is a cold compile
        // costing seconds of spirv-opt, so they compile in parallel. One job
        // per distinct shader source: the blend and Pass B pipelines share
        // their source with the plain variant, so they run after it in the
        // same job and reuse its cached MSL instead of compiling it again.
        // PBR jobs go first because they are the slow ones, and PBR alone
        // has the parallax variants. Each cold PBR
        // compile holds over 2 GB inside spirv-opt, so the worker count is
        // capped to keep a cold start near 10 GB.
        let mut jobs = Vec::new();
        for (kind, parallax) in [
            (MaterialKind::Pbr, false),
            (MaterialKind::Pbr, true),
            (MaterialKind::Cel, false),
            (MaterialKind::Unlit, false),
        ] {
            for opaque_fast in [false, true] {
                for aux in [
                    (false, false, false),
                    (true, false, false),
                    (false, true, false),
                    (true, true, false),
                    (false, false, true),
                    (true, false, true),
                    (false, true, true),
                    (true, true, true),
                ] {
                    jobs.push((kind, aux, opaque_fast, parallax));
                }
            }
        }
        let next_job = std::sync::atomic::AtomicUsize::new(0);
        const WORKERS: usize = 4;
        std::thread::scope(|scope| {
            for _ in 0..WORKERS {
                scope.spawn(|| {
                    while let Some(&(kind, (velocity, ao_mask, denoise), opaque_fast, parallax)) =
                        jobs.get(next_job.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
                    {
                        Self::create_variant(
                            device,
                            kind,
                            velocity,
                            ao_mask,
                            denoise,
                            false,
                            opaque_fast,
                            parallax,
                        );
                        if opaque_fast && kind == MaterialKind::Pbr && !parallax {
                            Self::create_cutout_depth_pipeline(device, velocity, ao_mask, denoise);
                        }
                        if !opaque_fast {
                            Self::create_variant(
                                device, kind, velocity, ao_mask, denoise, true, false, parallax,
                            );
                            if !(velocity || ao_mask || denoise || parallax) {
                                Self::create_pass_b_pipeline(device, kind);
                            }
                        }
                    }
                });
            }
            scope.spawn(|| {
                for solid in [false, true] {
                    subsurface::light_pipeline(device, solid);
                }
            });
        });

        // The
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
        Self::create_depth_only_pipeline(device, false);
        Self::create_depth_only_pipeline(device, true);
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
            &manifold_gpu::RenderPipelineDesc::new(
                include_str!("../shaders/shaft_composite.wgsl"),
                "vs_main",
                "fs_main",
                "node.render_scene shaft composite",
            )
            .color(
                manifold_gpu::GpuTextureFormat::Rgba16Float,
                Some(shaft_composite_blend),
            ),
        );

        // SCENE_RENDER_MODE_DESIGN.md D8 (P3), same BUG-037 discipline: the
        // Points pipeline (`pipeline_for`'s points slot — vs_points +
        // fs_unlit, the `[[point_size]]` rebind) is asset-independent and
        // lazily compiled on the first Points frame without this. Warm it so
        // flipping a scene to Points mid-show is a cache hit, not a stall.
        device.create_render_pipeline(
            &manifold_gpu::RenderPipelineDesc::new(
                RENDER_SCENE_WGSL.as_str(),
                "vs_points",
                "fs_unlit",
                "node.render_scene points",
            )
            .color(manifold_gpu::GpuTextureFormat::Rgba16Float, None)
            .depth(manifold_gpu::GpuTextureFormat::Depth32Float)
            .samples(MSAA_SAMPLES)
            .alpha_to_coverage(true)
            .point_size(7),
        );

        // The water march and its shading pass, which draws in Pass B with
        // Pass B's colour format; the first water frame is a cache hit.
        super::water_march::WaterMarch::prewarm(device);
        super::water_shade::WaterShade::prewarm(
            device,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
        );
        // The gas march draws in Pass B too; the first smoke is a cache hit.
        super::gas_volume::GasPass::prewarm(device, manifold_gpu::GpuTextureFormat::Rgba16Float);

        // IBL pipelines are fixed-source,
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

        // Hit_dist_extract and upscale_alpha_combine
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
        prev_positions::create_pack_pipeline(device);
    }
}
