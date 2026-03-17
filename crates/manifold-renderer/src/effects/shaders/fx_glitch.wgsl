// Glitch effect — block displacement, scanline jitter, RGB split.

struct Uniforms {
    amount: f32,
    block_size: f32,
    rgb_shift: f32,
    scanline: f32,      // GlitchEffect.shader:48 — _Scanline
    speed: f32,
    time: f32,
    resolution_x: f32,
    resolution_y: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

fn hash1(n: f32) -> f32 {
    return fract(sin(n) * 43758.5453123);
}

fn hash2(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var uv = in.uv;
    let res = vec2<f32>(uniforms.resolution_x, uniforms.resolution_y);
    let t = floor(uniforms.time * uniforms.speed * 12.0);

    // --- Block displacement ---
    let block_pixels = max(uniforms.block_size, 4.0);
    let block_uv = floor(uv * res / block_pixels);
    let block_hash = hash2(block_uv + t * 0.37);

    let displace_mask = step(1.0 - uniforms.amount * 0.6, block_hash);
    let displace_x = (hash2(block_uv + t * 1.13) * 2.0 - 1.0) * uniforms.amount * 0.15;
    let displace_y = (hash2(block_uv + t * 2.77) * 2.0 - 1.0) * uniforms.amount * 0.03;
    uv = uv + vec2<f32>(displace_x, displace_y) * displace_mask;

    // --- Scanline jitter ---
    let scanline_row = floor(uv.y * res.y);
    let scan_hash = hash1(scanline_row + t * 7.31);
    // GlitchEffect.shader:92 — step(1.0 - _Scanline * _Amount * 0.3, scanHash)
    let scan_mask = step(1.0 - uniforms.scanline * uniforms.amount * 0.3, scan_hash);
    let scan_shift = (hash1(scanline_row + t * 3.17) * 2.0 - 1.0) * uniforms.amount * 0.08;
    uv.x = uv.x + scan_shift * scan_mask;

    // --- RGB channel split ---
    let rgb_amount = uniforms.rgb_shift * uniforms.amount;
    let shift_angle = hash1(t * 5.0) * 6.283185;
    let rgb_dir = vec2<f32>(cos(shift_angle), sin(shift_angle));
    let rgb_offset = rgb_dir * rgb_amount;

    let r = textureSample(source_tex, tex_sampler, uv + rgb_offset).r;
    let g = textureSample(source_tex, tex_sampler, uv).g;
    let b = textureSample(source_tex, tex_sampler, uv - rgb_offset).b;
    let a = textureSample(source_tex, tex_sampler, in.uv).a;

    var effected = vec3<f32>(r, g, b);

    // Occasional color inversion on glitched blocks
    let invert_mask = step(0.92, block_hash * uniforms.amount);
    effected = mix(effected, 1.0 - clamp(effected, vec3<f32>(0.0), vec3<f32>(1.0)), invert_mask);

    let src = textureSample(source_tex, tex_sampler, in.uv).rgb;
    let result = mix(src, effected, uniforms.amount);
    return vec4<f32>(result, a);
}
