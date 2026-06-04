// node.sample_volume_2d — fusable body (freeze §12), MIXED-DIM GATHER. Sample a
// Texture3D `volume` at a fixed Z slice (with a UV re-frame) to produce a
// Texture2D. The output is 2D (so the wrapper is 2D: frag_uv/dims are vec2), but
// the input is a 3D volume gathered at a body-computed vec3 coord. Matches
// sample_volume_2d.wgsl. PARAMS: [slice_z, uv_scale, center_x, center_y].
fn body(volume: texture_3d<f32>, samp: sampler, frag_uv: vec2<f32>, dims: vec2<f32>, slice_z: f32, uv_scale: f32, center_x: f32, center_y: f32) -> vec4<f32> {
    let centered = frag_uv - vec2<f32>(0.5);
    let uv = centered / max(uv_scale, 0.001)
           + vec2<f32>(center_x + 0.5, center_y + 0.5);

    return textureSampleLevel(
        volume,
        samp,
        vec3<f32>(uv.x, uv.y, clamp(slice_z, 0.0, 1.0)),
        0.0,
    );
}
