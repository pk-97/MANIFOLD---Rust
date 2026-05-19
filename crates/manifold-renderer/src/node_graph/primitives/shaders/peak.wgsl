// node.peak — Texture→Scalar bridge.
//
// Same sparse-256-grid + workgroup reduction as luminance.wgsl, but
// emits the *maximum* Rec. 709 luminance across the grid samples
// rather than the average. Use to drive "respond to the brightest
// spot" behaviors (highlight-keyed effects, transient-driven knobs).

@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> result: array<f32>;

var<workgroup> shared_lum: array<f32, 256>;

@compute @workgroup_size(16, 16)
fn cs_main(
    @builtin(local_invocation_id) lid: vec3<u32>,
    @builtin(local_invocation_index) idx: u32,
) {
    let dims = textureDimensions(source_tex);
    let coord = vec2<i32>(
        i32((f32(lid.x) + 0.5) / 16.0 * f32(dims.x)),
        i32((f32(lid.y) + 0.5) / 16.0 * f32(dims.y)),
    );
    let color = textureLoad(source_tex, coord, 0);
    let lum = max(0.0, dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722)));

    shared_lum[idx] = lum;
    workgroupBarrier();

    // Tree reduction taking max, mirror of luminance.wgsl's sum.
    for (var s = 128u; s > 0u; s >>= 1u) {
        if idx < s {
            shared_lum[idx] = max(shared_lum[idx], shared_lum[idx + s]);
        }
        workgroupBarrier();
    }

    if idx == 0u {
        result[0] = shared_lum[0];
    }
}
