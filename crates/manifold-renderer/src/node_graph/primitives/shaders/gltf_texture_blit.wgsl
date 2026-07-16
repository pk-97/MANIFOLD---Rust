// Stretch-blit the decoded glTF source texture into the chain-allocated
// output texture. Unlike image_folder.wgsl this is a plain resample —
// no aspect-fit, no uv_scale — the source's full 0..1 UV range is
// stretched across the entire output, since the output resolution is
// author-controlled via the primitive's width/height params rather than
// derived from the canvas.

struct Uniforms {
    out_width: f32,
    out_height: f32,
    // GLB_XFAIL_BURNDOWN_DESIGN.md D2 (BUG-167): 0 = passthrough (default,
    // byte-identical to before this field existed); 1 = gloss_to_roughness
    // — repack a KHR_materials_pbrSpecularGlossiness
    // `specularGlossinessTexture` (RGB = specular tint, A = glossiness)
    // into `render_scene`'s existing glTF metal-rough packing convention
    // (G = roughness, B = metallic) so `resolve_mr` in render_scene.wgsl
    // needs no spec-gloss-aware branch at all — the conversion lives here,
    // in this generic texture-repack primitive, not in the PBR shader.
    // metallic is written 0.0 (spec-gloss's dielectric default, matching
    // `gltf_load::convert_spec_gloss`'s factor-level conversion); the RGB
    // specular tint is Deferred (§8) and is NOT read here.
    mode: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = vec2<u32>(u32(u.out_width), u32(u.out_height));
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    var c = textureSampleLevel(src_tex, src_sampler, uv, 0.0);
    if (u.mode > 0.5) {
        let roughness = 1.0 - c.a;
        c = vec4<f32>(0.0, roughness, 0.0, 1.0);
    }
    textureStore(output_tex, vec2<i32>(gid.xy), c);
}
