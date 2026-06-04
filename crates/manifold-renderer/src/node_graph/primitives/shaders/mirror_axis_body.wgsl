// node.mirror_axis — fusable body (freeze §12), GATHER. Sample the input at UVs
// mirrored across a line through the centre at `angle` radians (rotate -angle →
// fold Y → rotate +angle → fract(+0.5)). The hand shader computes cos/sin from the
// angle uniform on the GPU, so the body does the same (bit-exact). Matches
// mirror_axis.wgsl. PARAMS: [angle].
fn body(tex_source: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, angle: f32) -> vec4<f32> {
    let centered = uv - vec2<f32>(0.5);
    let ca = cos(angle);
    let sa = sin(angle);

    let rotated = vec2<f32>(
        centered.x * ca - centered.y * sa,
        centered.x * sa + centered.y * ca,
    );
    let folded = vec2<f32>(rotated.x, abs(rotated.y));
    let unrotated = vec2<f32>(
        folded.x * ca + folded.y * sa,
        -folded.x * sa + folded.y * ca,
    );

    let mirrored_uv = fract(unrotated + vec2<f32>(0.5));
    return textureSampleLevel(tex_source, samp, mirrored_uv, 0.0);
}
