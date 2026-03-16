// Display shader for ComputeStrangeAttractor: decay + splat + tone mapping.
// Reuses CPU trajectory integration; adds contrast, invert, tilt, splat size.

struct Uniforms {
    decay: f32,
    brightness: f32,
    particle_size: f32,
    particle_count: f32,
    texel_x: f32,
    texel_y: f32,
    contrast: f32,
    invert: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var state_tex: texture_2d<f32>;
@group(0) @binding(2) var state_sampler: sampler;
@group(0) @binding(3) var position_tex: texture_2d<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;

    // Decay previous state
    let prev = textureSample(state_tex, state_sampler, uv);
    var accum = prev.r * u.decay;

    let count = i32(u.particle_count);
    let r = u.particle_size * u.texel_x;

    // Gaussian splat accumulation
    for (var i = 0; i < count; i++) {
        let pos = textureLoad(position_tex, vec2<i32>(i, 0), 0);
        let px = pos.r;
        let py = pos.g;

        let dx = (uv.x - px) / r;
        let dy = (uv.y - py) / r;
        let d2 = dx * dx + dy * dy;

        if d2 < 1.0 {
            let falloff = (1.0 - d2);
            accum += falloff * falloff * 0.12;
        }
    }

    // Extended Reinhard tone mapping with contrast
    let x_val = accum * u.brightness * u.contrast;
    var lum = x_val * (1.0 + x_val / 9.0) / (1.0 + x_val);

    // Invert
    if u.invert > 0.5 {
        lum = 1.0 - lum;
    }

    lum = clamp(lum, 0.0, 1.0);
    return vec4<f32>(lum, lum, lum, lum);
}
