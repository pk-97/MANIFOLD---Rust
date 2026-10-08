// node.luminance — Texture→Scalar bridge.
//
// Single workgroup of 16×16 threads sparse-samples the input texture
// at 256 grid positions, parallel-reduces to average Rec. 709
// luminance, writes a single f32 to a storage buffer. The host reads
// the buffer back on next frame's evaluate (one-frame latency to
// avoid pipeline stalls).
//
// Dispatched as [1, 1, 1] — tiny compute cost regardless of input
// resolution. Sparse sampling is "good enough" for control-rate
// signals; users wanting pixel-exact reduction can chain through
// MipChain first.

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

    // Tree reduction across the 256 grid samples.
    for (var s = 128u; s > 0u; s >>= 1u) {
        if idx < s {
            shared_lum[idx] += shared_lum[idx + s];
        }
        workgroupBarrier();
    }

    if idx == 0u {
        result[0] = shared_lum[0] / 256.0;
    }
}
