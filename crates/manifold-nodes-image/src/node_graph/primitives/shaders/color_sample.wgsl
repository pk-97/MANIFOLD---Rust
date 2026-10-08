// node.color_sample — Texture→Scalar bridge.
//
// Reads a region around the configured normalised UV and averages it.
// `radius_px = 0` keeps the historical single-pixel behaviour;
// `radius_px = r` averages a (2r+1)² window in pixel space. Writes
// R, G, B, and a Rec.709 luma to a shared-mode storage buffer for
// one-frame-latency CPU readback. The averaging is what lets a
// "region brightness" wire reflect what the eye sees rather than
// whatever lone texel happens to land under the UV — single-pixel
// reads on high-frequency content (Oily Fluid, busy video sources)
// produce wildly varying readings and near-zero asymmetry across
// cardinal sample positions, which is the killer of effects like
// Color Compass.

struct UvParam {
    uv: vec2<f32>,
    radius_px: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uv_param: UvParam;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var<storage, read_write> result: array<f32>;

@compute @workgroup_size(1, 1)
fn cs_main() {
    let dims = textureDimensions(source_tex);
    let u = clamp(uv_param.uv.x, 0.0, 1.0);
    let v = clamp(uv_param.uv.y, 0.0, 1.0);
    // textureLoad takes integer pixel coords. Map [0, 1] UV to
    // [0, dims-1] pixel space so UV=1.0 lands on the edge pixel
    // rather than indexing out of bounds.
    let cx = i32(u * f32(dims.x - 1u));
    let cy = i32(v * f32(dims.y - 1u));
    let r = i32(round(uv_param.radius_px));
    let max_x = i32(dims.x) - 1;
    let max_y = i32(dims.y) - 1;

    var sum = vec3<f32>(0.0, 0.0, 0.0);
    var count: f32 = 0.0;
    // Walk the (2r+1)² window, clamping at the texture edges so
    // sample points near the boundary don't pull in wrap-around or
    // out-of-bounds reads. When r=0 the loop body runs exactly once
    // — preserves the single-pixel path bit-for-bit.
    for (var dy: i32 = -r; dy <= r; dy = dy + 1) {
        for (var dx: i32 = -r; dx <= r; dx = dx + 1) {
            let x = clamp(cx + dx, 0, max_x);
            let y = clamp(cy + dy, 0, max_y);
            let c = textureLoad(source_tex, vec2<i32>(x, y), 0);
            sum = sum + c.rgb;
            count = count + 1.0;
        }
    }
    let avg = sum / max(count, 1.0);

    // Rec.709 luma weights — matches the framewide `Luminance`
    // primitive so a region brightness reading and a frame-mean
    // brightness sample share the same definition of "brightness".
    let luma = dot(avg, vec3<f32>(0.2126, 0.7152, 0.0722));
    result[0] = avg.r;
    result[1] = avg.g;
    result[2] = avg.b;
    result[3] = luma;
}
