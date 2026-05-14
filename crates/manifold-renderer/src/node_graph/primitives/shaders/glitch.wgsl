// node.glitch — pixel-exact replacement for legacy
// `effects/shaders/fx_glitch.wgsl`. Block-displacement + scanline-
// jitter + RGB-shift + per-block invert in a single compute pass.
// Fused composite — atomic Hash + BlockDisplace + Scanline +
// ChromaticOffset would round through fp16 between every pass and
// break bit-exact parity.

struct Uniforms {
    amount: f32,
    block_size: f32,
    rgb_shift: f32,
    scanline: f32,
    speed: f32,
    time: f32,
    resolution_x: f32,
    resolution_y: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

fn hash1(n: f32) -> f32 {
    return fract(sin(n) * 43758.5453123);
}

fn hash2(p: vec2<f32>) -> f32 {
    let h = dot(p, vec2<f32>(127.1, 311.7));
    return fract(sin(h) * 43758.5453123);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv_orig = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    var uv = uv_orig;
    let res = vec2<f32>(uniforms.resolution_x, uniforms.resolution_y);
    let t = floor(uniforms.time * uniforms.speed * 12.0);

    let block_pixels = max(uniforms.block_size, 4.0);
    let block_uv = floor(uv * res / block_pixels);
    let block_hash = hash2(block_uv + t * 0.37);

    let displace_mask = step(1.0 - uniforms.amount * 0.6, block_hash);
    let displace_x = (hash2(block_uv + t * 1.13) * 2.0 - 1.0) * uniforms.amount * 0.15;
    let displace_y = (hash2(block_uv + t * 2.77) * 2.0 - 1.0) * uniforms.amount * 0.03;
    uv = uv + vec2<f32>(displace_x, displace_y) * displace_mask;

    let scanline_row = floor(uv.y * res.y);
    let scan_hash = hash1(scanline_row + t * 7.31);
    let scan_mask = step(1.0 - uniforms.scanline * uniforms.amount * 0.3, scan_hash);
    let scan_shift = (hash1(scanline_row + t * 3.17) * 2.0 - 1.0) * uniforms.amount * 0.08;
    uv.x = uv.x + scan_shift * scan_mask;

    let rgb_amount = uniforms.rgb_shift * uniforms.amount;
    let shift_angle = hash1(t * 5.0) * 6.283185;
    let rgb_dir = vec2<f32>(cos(shift_angle), sin(shift_angle));
    let rgb_offset = rgb_dir * rgb_amount;

    let r = textureSampleLevel(source_tex, tex_sampler, uv + rgb_offset, 0.0).r;
    let g = textureSampleLevel(source_tex, tex_sampler, uv, 0.0).g;
    let b = textureSampleLevel(source_tex, tex_sampler, uv - rgb_offset, 0.0).b;
    let a = textureSampleLevel(source_tex, tex_sampler, uv_orig, 0.0).a;

    var effected = vec3<f32>(r, g, b);

    let invert_mask = step(0.92, block_hash * uniforms.amount);
    effected = mix(effected, 1.0 - clamp(effected, vec3<f32>(0.0), vec3<f32>(1.0)), invert_mask);

    let src = textureSampleLevel(source_tex, tex_sampler, uv_orig, 0.0).rgb;
    let result = mix(src, effected, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, a));
}
