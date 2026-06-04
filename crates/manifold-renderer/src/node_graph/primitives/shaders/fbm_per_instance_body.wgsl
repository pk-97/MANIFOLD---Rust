// node.fbm_per_instance — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT. Sample multi-octave 3D-simplex fBM at each UV; emit one f32 per
// slot. Matches fbm_per_instance.wgsl bit-for-bit (same simplex3d + fbm loop;
// simplex3d prepended via wgsl_includes from noise_common.wgsl).
//
// ABI (buffer standalone codegen): `uv` ([f32;2]) is coincident, so the wrapper
// pre-reads `e_uv = buf_uv[idx]` and passes it; the body returns the f32 written
// to buf_out[idx]. The OUTPUT port is Array(f32) — a single channel — so the
// codegen emits a bare `array<f32>` (no struct) and the body returns `f32`. The
// uv input is the 2-channel `struct Element { x: f32, y: f32 }`. `octaves`
// arrives as i32 (Int param), cast to u32 for the loop. `fbm_param` is self-
// contained (takes octaves/lacunarity/gain as args); simplex3d is the include.
fn fbm_param(p: vec3<f32>, octaves: u32, lacunarity: f32, gain: f32) -> f32 {
    var val = 0.0;
    var amp = 1.0;
    var freq = 1.0;
    var total_amp = 0.0;
    for (var i = 0u; i < octaves; i++) {
        val += simplex3d(p * freq) * amp;
        total_amp += amp;
        freq *= lacunarity;
        amp *= gain;
    }
    return val / total_amp;
}

fn body(
    idx: u32,
    count: u32,
    e_uv: Element,
    scale: f32,
    z: f32,
    offset_x: f32,
    offset_y: f32,
    octaves: i32,
    lacunarity: f32,
    gain: f32,
) -> f32 {
    let p = vec3<f32>(
        e_uv.x * scale + offset_x,
        e_uv.y * scale + offset_y,
        z,
    );
    return fbm_param(p, u32(octaves), lacunarity, gain);
}
