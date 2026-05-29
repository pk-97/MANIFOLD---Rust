// node.hdr_retention_mix — preserve a `reference` texture's
// above-1.0 highlight energy through a `compressed` texture's gain
// adjustment. SDR body (per-pixel ≤ 1.0) comes from `compressed`;
// HDR portion (per-pixel > 1.0) lerps between compressed's HDR and
// reference's HDR by `retention`.
//
// retention = 1 → highlights stay at the reference's original level
//                  regardless of gain (HDR ceiling preserved).
// retention = 0 → highlights ride the gain like everything else.
//
// Alpha is passed through from `compressed` so downstream layers see
// the gain branch's alpha (typically the same as reference's).

struct Uniforms {
    retention: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_compressed: texture_2d<f32>;
@group(0) @binding(2) var tex_reference: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let c = textureSampleLevel(tex_compressed, tex_sampler, uv, 0.0);
    let r = textureSampleLevel(tex_reference,  tex_sampler, uv, 0.0);

    let sdr = min(c.rgb, vec3<f32>(1.0));
    let compressed_hdr = max(c.rgb - vec3<f32>(1.0), vec3<f32>(0.0));
    let reference_hdr  = max(r.rgb - vec3<f32>(1.0), vec3<f32>(0.0));
    let retained_hdr = mix(compressed_hdr, reference_hdr, uniforms.retention);
    let result = sdr + retained_hdr;

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, c.a));
}
