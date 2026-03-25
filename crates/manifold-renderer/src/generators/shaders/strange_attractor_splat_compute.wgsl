// Compute variant of strange_attractor.wgsl — same decay + Gaussian splat,
// reads previous state via textureSampleLevel, reads positions via textureLoad,
// writes target via textureStore.

struct Uniforms {
    decay: f32,
    brightness: f32,
    particle_size: f32,
    particle_count: f32,
    texel_x: f32,
    texel_y: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var state_tex: texture_2d<f32>;
@group(0) @binding(2) var state_sampler: sampler;
@group(0) @binding(3) var position_tex: texture_2d<f32>;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    // Decay previous state
    let prev = textureSampleLevel(state_tex, state_sampler, uv, 0.0);
    var accum = prev.r * u.decay;

    let count = i32(u.particle_count);
    let r = u.particle_size * u.texel_x; // Particle radius in UV space

    // Gaussian splat accumulation over all particles
    for (var i = 0; i < count; i++) {
        // Read projected 2D position from position texture (384x1, RG channels)
        let pos = textureLoad(position_tex, vec2<i32>(i, 0), 0);
        let px = pos.r; // x in [0,1]
        let py = pos.g; // y in [0,1]

        let dx = (uv.x - px) / r;
        let dy = (uv.y - py) / r;
        let d2 = dx * dx + dy * dy;

        if d2 < 1.0 {
            // Quadratic falloff Gaussian splat: (1 - d²)² * 0.15
            let falloff = (1.0 - d2);
            accum += falloff * falloff * 0.15;
        }
    }

    let lum = clamp(accum * u.brightness, 0.0, 1.0);
    textureStore(output_tex, gid.xy, vec4<f32>(lum, lum, lum, lum));
}
