// node.affine_transform — fusable body (freeze §12). `in` is a GATHER input:
// the body samples it at the affine-transformed coordinate it computes, so the
// codegen passes texture + sampler args rather than a pre-read register.
// Verbatim port of affine_transform.wgsl with two relocations: aspect ratio is
// derived from `dims` (the hand run() computed width/height CPU-side) and the
// user-facing degrees → math radians conversion happens here (the hand run()
// converted before packing the uniform) — so the fused uniform carries the
// param's raw degree value. Same f32 ops in the same order, bit-identical.
// PARAMS: [translate_x, translate_y, scale, rotation].
fn body(tex_in: texture_2d<f32>, s_in: sampler, uv: vec2<f32>, dims: vec2<f32>, translate_x: f32, translate_y: f32, scale: f32, rotation: f32) -> vec4<f32> {
    let aspect_ratio = dims.x / dims.y;
    let rot = -(rotation * 3.14159265358979323846 / 180.0);

    var p = uv - vec2<f32>(0.5, 0.5);

    p.x = p.x * aspect_ratio;

    let cos_r = cos(rot);
    let sin_r = sin(rot);
    p = vec2<f32>(
        p.x * cos_r - p.y * sin_r,
        p.x * sin_r + p.y * cos_r,
    );

    p.x = p.x / aspect_ratio;

    p = p / max(scale, 0.01);
    p = p - vec2<f32>(translate_x, translate_y);
    p = p + vec2<f32>(0.5, 0.5);

    if (p.x < 0.0 || p.x > 1.0 || p.y < 0.0 || p.y > 1.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    return textureSampleLevel(tex_in, s_in, p, 0.0);
}
