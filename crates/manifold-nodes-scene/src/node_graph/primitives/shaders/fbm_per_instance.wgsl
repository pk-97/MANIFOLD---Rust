// node.fbm_per_instance — sample fractal-Brownian-motion (multi-
// octave simplex) at each UV in an Array<vec2<f32>>; emit Array<f32>.
//
// noise_common.wgsl is prepended at pipeline creation time; this
// shader uses simplex3d() from there.
//
// The fbm_param loop below matches noise_common.wgsl's `fbm()`
// implementation byte-for-byte (same accumulation order, same
// total_amp normalisation, same freq/amp update order). With
// octaves=5, lacunarity=1.5, gain=0.8 it produces output bit-
// identical to the legacy fbm — DigitalPlants's petal-noise pass
// relies on this.

struct Uniforms {
    count:       u32,
    scale:       f32,
    z:           f32,
    offset_x:    f32,
    offset_y:    f32,
    octaves:     u32,
    lacunarity:  f32,
    gain:        f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read>       uv_in: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> out:    array<f32>;

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

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.count { return; }
    let uv = uv_in[idx];
    let p = vec3<f32>(
        uv.x * u.scale + u.offset_x,
        uv.y * u.scale + u.offset_y,
        u.z,
    );
    out[idx] = fbm_param(p, u.octaves, u.lacunarity, u.gain);
}
