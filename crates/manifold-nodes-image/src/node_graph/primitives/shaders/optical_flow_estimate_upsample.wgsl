// node.optical_flow_estimate — bilinear-upsample the internal
// analysis-resolution flow staging texture into the runtime-
// allocated output Texture2D. Channel layout is preserved as-is:
//
//   R = flow_x (UV units; positive = right)
//   G = confidence (0..1)
//   B = flow_y (UV units; positive = down)
//   A = valid_mask (0 or 1)
//
// R/B convention matches node.flow_field_noise + node.uv_displace_by_flow
// (Watercolor), so flow output composes downstream without channel
// reshuffling.

@group(0) @binding(0) var flow_src: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let f = textureSampleLevel(flow_src, tex_sampler, uv, 0.0);
    textureStore(output_tex, vec2<i32>(id.xy), f);
}
