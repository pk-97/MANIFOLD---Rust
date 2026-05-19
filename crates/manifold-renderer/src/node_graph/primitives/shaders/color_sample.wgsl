// node.color_sample — Texture→Scalar bridge.
//
// Single-thread textureLoad at a configurable normalised UV; writes
// R, G, B, and a Rec.709-weighted luma into a shared-mode storage
// buffer for one-frame-latency CPU readback. Trivially small
// dispatch (workgroup [1,1]) since there's no reduction — exactly
// one pixel sampled.

struct UvParam {
    uv: vec2<f32>,
    _pad0: f32,
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
    let coord = vec2<i32>(
        i32(u * f32(dims.x - 1u)),
        i32(v * f32(dims.y - 1u)),
    );
    let color = textureLoad(source_tex, coord, 0);
    // Rec.709 luma weights — matches the framewide `Luminance`
    // primitive so a `luma`-driven region reading and a frame-mean
    // brightness sample share the same definition of "brightness".
    let luma = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    result[0] = color.r;
    result[1] = color.g;
    result[2] = color.b;
    result[3] = luma;
}
