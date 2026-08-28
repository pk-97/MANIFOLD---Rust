// node.bokeh_gather — internal near/far composite (BOKEH_LAYERED_DOF_DESIGN.md P3).
//
// `out = mix(far_result.rgb, near_result.rgb, near_result.a)` — the near
// field is additive light gathered from out-of-focus foreground, composited
// over the far result using the near field's accumulated coverage as alpha.
// Output alpha is carried from the far result.
//
// Bindings: far(0), near(1), samp(2), dst(3, rgba16float write).

@group(0) @binding(0) var far: texture_2d<f32>;
@group(0) @binding(1) var near: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims_i = textureDimensions(dst);
    if id.x >= dims_i.x || id.y >= dims_i.y {
        return;
    }

    let dims = vec2<f32>(f32(dims_i.x), f32(dims_i.y));
    let uv = (vec2<f32>(f32(id.x), f32(id.y)) + vec2<f32>(0.5)) / dims;

    let far_sample = textureSampleLevel(far, samp, uv, 0.0);
    let near_sample = textureSampleLevel(near, samp, uv, 0.0);
    let rgb = mix(far_sample.rgb, near_sample.rgb, clamp(near_sample.a, 0.0, 1.0));
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(rgb, far_sample.a));
}
