// node.simplex_per_instance — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT. Sample 3D simplex noise at each UV; emit one f32 per slot. Matches
// simplex_per_instance.wgsl bit-for-bit (simplex3d prepended via wgsl_includes
// from noise_common.wgsl).
//
// ABI (buffer standalone codegen): `uv` ([f32;2]) is coincident, pre-read into
// `e_uv` (the 2-channel `struct Element { x, y }`); the OUTPUT is Array(f32), a
// single channel, so the codegen emits a bare `array<f32>` and the body returns
// `f32`. simplex3d comes from the prepended noise_common include.
fn body(
    idx: u32,
    count: u32,
    e_uv: Element,
    scale: f32,
    z: f32,
    offset_x: f32,
    offset_y: f32,
) -> f32 {
    let p = vec3<f32>(
        e_uv.x * scale + offset_x,
        e_uv.y * scale + offset_y,
        z,
    );
    return simplex3d(p);
}
