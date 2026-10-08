// node.uv_displace_by_flow — displace a source texture's UV by a
// flow vector field, then sample. Companion to node.flow_field_noise
// (which writes flow.x to R and flow.y to B), but accepts any flow
// texture as long as the offset packing matches.
//
// offset = (flow.rb - bias) * weight
// sampled_uv = original_uv + offset
//
// Bias defaults to 0.5 (matches Watercolor — maps [0,1] color to
// [-0.5, +0.5]); set to 0 for already-signed flow textures.

struct DisplaceUniforms {
    weight: f32,
    bias:   f32,
    _pad0:  f32,
    _pad1:  f32,
};

@group(0) @binding(0) var<uniform> u: DisplaceUniforms;
@group(0) @binding(1) var t_source: texture_2d<f32>;
@group(0) @binding(2) var t_flow: texture_2d<f32>;
@group(0) @binding(3) var s_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    let flow = textureSampleLevel(t_flow, s_sampler, uv, 0.0);
    let offset = (vec2<f32>(flow.r, flow.b) - vec2<f32>(u.bias)) * u.weight;
    let sampled_uv = uv + offset;

    let result = textureSampleLevel(t_source, s_sampler, sampled_uv, 0.0);
    textureStore(output_tex, vec2<i32>(gid.xy), result);
}
