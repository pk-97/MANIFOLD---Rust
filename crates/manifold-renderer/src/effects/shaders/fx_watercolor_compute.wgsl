// Watercolor — multi-pass feedback effect simulating watercolor paint flow.
// Ported from TouchDesigner watercolor tutorial signal chain.
//
// 8-pass pipeline per frame:
//   Mode 0: Grain overlay (source + white noise)
//   Mode 1: Maximum composite with decay (grain_base max feedback*decay)
//   Mode 2: Flow map generation (domain-warped fBM → RB channels)
//   Mode 3: Flow displacement (displace max result by flow map)
//   Mode 4: Edge diffusion blur (Gaussian, radius 2)
//   Mode 5: Slope displacement (soft light → Sobel → UV displace)
//   Mode 6: Luma blur (variable blur with binary noise mask)
//   Mode 7: Emboss post-process (emboss + overlay composite + amount blend)
//
// Binding layout: ComputeDualBlitHelper (5 bindings).
// Single-input modes bind source_tex_a == source_tex_b.

struct Uniforms {
    mode:             u32,  // 0–7 (specialized via function constants)
    time:             f32,  // seconds — drives noise animation
    width:            f32,  // render width in pixels
    height:           f32,  // render height in pixels
    displace_weight:  f32,  // UV displacement strength (default 0.001)
    blur_radius:      f32,  // edge diffusion blur radius in pixels (default 2.0)
    emboss_strength:  f32,  // emboss filter strength (default 12.0)
    amount:           f32,  // overall wet/dry mix (0 = bypass, 1 = full)
    slope_strength:   f32,  // Sobel gradient multiplier (default 5.0)
    slope_step:       f32,  // Sobel sample offset in pixels (default 5.0)
    luma_blur_radius: f32,  // heavy blur radius for dilution (default 10.0)
    grain_amount:     f32,  // noise grain strength (default 0.15)
    decay:            f32,  // feedback energy dissipation (0.9–1.0, default 0.99)
    _pad0:            f32,
    _pad1:            f32,
    _pad2:            f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex_a: texture_2d<f32>;
@group(0) @binding(2) var source_tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

// ═══════════════════════════════════════════════════════════════════
// Hashing — Wang hash (deterministic, no sin())
// ═══════════════════════════════════════════════════════════════════

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

// ═══════════════════════════════════════════════════════════════════
// 3D Perlin noise — identical to particle_common.wgsl simplex_noise_3d
// Returns approximately [-1, 1]. Quintic fade, 16 edge-centered gradients.
// ═══════════════════════════════════════════════════════════════════

fn perlin3_hash(ix: i32, iy: i32, iz: i32) -> u32 {
    let x = u32(ix + 10000) * 73856093u;
    let y = u32(iy + 10000) * 19349663u;
    let z = u32(iz + 10000) * 83492791u;
    return wang_hash(x ^ y ^ z);
}

fn perlin3_grad(h: u32) -> vec3<f32> {
    let sel = h & 15u;
    switch sel {
        case 0u:  { return vec3<f32>( 1.0,  1.0,  0.0); }
        case 1u:  { return vec3<f32>(-1.0,  1.0,  0.0); }
        case 2u:  { return vec3<f32>( 1.0, -1.0,  0.0); }
        case 3u:  { return vec3<f32>(-1.0, -1.0,  0.0); }
        case 4u:  { return vec3<f32>( 1.0,  0.0,  1.0); }
        case 5u:  { return vec3<f32>(-1.0,  0.0,  1.0); }
        case 6u:  { return vec3<f32>( 1.0,  0.0, -1.0); }
        case 7u:  { return vec3<f32>(-1.0,  0.0, -1.0); }
        case 8u:  { return vec3<f32>( 0.0,  1.0,  1.0); }
        case 9u:  { return vec3<f32>( 0.0, -1.0,  1.0); }
        case 10u: { return vec3<f32>( 0.0,  1.0, -1.0); }
        case 11u: { return vec3<f32>( 0.0, -1.0, -1.0); }
        case 12u: { return vec3<f32>( 1.0,  1.0,  0.0); }
        case 13u: { return vec3<f32>(-1.0,  1.0,  0.0); }
        case 14u: { return vec3<f32>( 0.0, -1.0,  1.0); }
        default:  { return vec3<f32>( 0.0, -1.0, -1.0); }
    }
}

fn noise3d(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    // Perlin quintic fade: 6t^5 - 15t^4 + 10t^3
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);

    let ix = i32(i.x);
    let iy = i32(i.y);
    let iz = i32(i.z);

    let h000 = perlin3_hash(ix,     iy,     iz);
    let h100 = perlin3_hash(ix + 1, iy,     iz);
    let h010 = perlin3_hash(ix,     iy + 1, iz);
    let h110 = perlin3_hash(ix + 1, iy + 1, iz);
    let h001 = perlin3_hash(ix,     iy,     iz + 1);
    let h101 = perlin3_hash(ix + 1, iy,     iz + 1);
    let h011 = perlin3_hash(ix,     iy + 1, iz + 1);
    let h111 = perlin3_hash(ix + 1, iy + 1, iz + 1);

    let g000 = perlin3_grad(h000);
    let g100 = perlin3_grad(h100);
    let g010 = perlin3_grad(h010);
    let g110 = perlin3_grad(h110);
    let g001 = perlin3_grad(h001);
    let g101 = perlin3_grad(h101);
    let g011 = perlin3_grad(h011);
    let g111 = perlin3_grad(h111);

    let n000 = dot(g000, vec3<f32>(f.x,       f.y,       f.z));
    let n100 = dot(g100, vec3<f32>(f.x - 1.0, f.y,       f.z));
    let n010 = dot(g010, vec3<f32>(f.x,       f.y - 1.0, f.z));
    let n110 = dot(g110, vec3<f32>(f.x - 1.0, f.y - 1.0, f.z));
    let n001 = dot(g001, vec3<f32>(f.x,       f.y,       f.z - 1.0));
    let n101 = dot(g101, vec3<f32>(f.x - 1.0, f.y,       f.z - 1.0));
    let n011 = dot(g011, vec3<f32>(f.x,       f.y - 1.0, f.z - 1.0));
    let n111 = dot(g111, vec3<f32>(f.x - 1.0, f.y - 1.0, f.z - 1.0));

    let nx00 = mix(n000, n100, u.x);
    let nx10 = mix(n010, n110, u.x);
    let nx01 = mix(n001, n101, u.x);
    let nx11 = mix(n011, n111, u.x);
    let nxy0 = mix(nx00, nx10, u.y);
    let nxy1 = mix(nx01, nx11, u.y);
    return mix(nxy0, nxy1, u.z);
}

// ═══════════════════════════════════════════════════════════════════
// Fractal Brownian Motion — 6-octave 3D fBM
// Returns approximately [-1, 1].
// 6 octaves: octaves 7–10 contribute <1% each (amp < 0.008).
// ═══════════════════════════════════════════════════════════════════

const FBM_OCTAVES: i32 = 5;

fn fbm3d(p_in: vec3<f32>) -> f32 {
    var val: f32 = 0.0;
    var amp: f32 = 0.5;
    var p = p_in;
    for (var i: i32 = 0; i < FBM_OCTAVES; i = i + 1) {
        val += amp * noise3d(p);
        p = p * 2.0;
        amp = amp * 0.5;
    }
    return val;
}

// ═══════════════════════════════════════════════════════════════════
// Domain-warped flow noise — two channels of noise-of-noise
// Returns vec2 in [0, 1] for displacement lookup.
// ═══════════════════════════════════════════════════════════════════

const FLOW_SCALE: f32 = 4.0;

fn flow_noise(uv: vec2<f32>, z: f32) -> vec2<f32> {
    // Two independent fBM channels with offset seeds.
    // Direct evaluation (no domain warping) for performance.
    let red = fbm3d(vec3<f32>(uv * FLOW_SCALE, z));
    let blue = fbm3d(vec3<f32>(uv * FLOW_SCALE + vec2<f32>(5.2, 1.3), z));

    // Map [-1,1] → [0,1]
    return vec2<f32>(red * 0.5 + 0.5, blue * 0.5 + 0.5);
}

// ═══════════════════════════════════════════════════════════════════
// White noise — high-frequency hash for grain texture
// ═══════════════════════════════════════════════════════════════════

fn white_noise(coord: vec2<f32>) -> f32 {
    return fract(sin(dot(coord, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

// ═══════════════════════════════════════════════════════════════════
// Blend modes
// ═══════════════════════════════════════════════════════════════════

// W3C standard Soft Light (CSS Compositing Level 1 spec)
fn soft_light_ch(base: f32, blend: f32) -> f32 {
    if blend <= 0.5 {
        return base - (1.0 - 2.0 * blend) * base * (1.0 - base);
    } else {
        var d: f32;
        if base <= 0.25 {
            d = ((16.0 * base - 12.0) * base + 4.0) * base;
        } else {
            d = sqrt(base);
        }
        return base + (2.0 * blend - 1.0) * (d - base);
    }
}

fn soft_light(base: vec3<f32>, blend: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        soft_light_ch(base.r, blend.r),
        soft_light_ch(base.g, blend.g),
        soft_light_ch(base.b, blend.b),
    );
}

// Standard Overlay blend
fn overlay_ch(base: f32, blend: f32) -> f32 {
    if base < 0.5 {
        return 2.0 * base * blend;
    } else {
        return 1.0 - 2.0 * (1.0 - base) * (1.0 - blend);
    }
}

fn overlay(base: vec3<f32>, blend: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        overlay_ch(base.r, blend.r),
        overlay_ch(base.g, blend.g),
        overlay_ch(base.b, blend.b),
    );
}

// ═══════════════════════════════════════════════════════════════════
// NaN/Inf guard for feedback stability
// ═══════════════════════════════════════════════════════════════════

fn safe_clamp(v: vec4<f32>) -> vec4<f32> {
    var s = clamp(v, vec4<f32>(-100.0), vec4<f32>(100.0));
    if any(s != s) {
        s = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    return s;
}

// ═══════════════════════════════════════════════════════════════════
// Main entry point — mode branching (dead-code eliminated per pipeline)
// ═══════════════════════════════════════════════════════════════════

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let texel = vec2<f32>(1.0 / uniforms.width, 1.0 / uniforms.height);

    if uniforms.mode == 0u {
        // ─── Mode 0: Grain ─────────────────────────────────────────
        // Add white noise texture to source image.
        // source_a = input source
        let source = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);
        let pixel_coord = uv * vec2<f32>(uniforms.width, uniforms.height);
        let noise = white_noise(pixel_coord);
        // Multiplicative grain: black stays black, bright areas get texture.
        // Simulates paper absorbing paint unevenly.
        let color = vec4<f32>(
            source.rgb * (1.0 - uniforms.grain_amount * (1.0 - noise)),
            source.a,
        );
        textureStore(output_tex, vec2<i32>(gid.xy), color);

    } else if uniforms.mode == 1u {
        // ─── Mode 1: Grain + Maximum Composite with Decay ──────────
        // Inline grain: multiplicative noise on source, then max with
        // decayed feedback. Eliminates separate grain pass + texture.
        // source_a = original source, source_b = feedback
        let source = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);
        let pixel_coord = uv * vec2<f32>(uniforms.width, uniforms.height);
        let noise = white_noise(pixel_coord);
        let grain_rgb = source.rgb * (1.0 - uniforms.grain_amount * (1.0 - noise));

        let feedback = textureSampleLevel(source_tex_b, tex_sampler, uv, 0.0) * uniforms.decay;
        let color = vec4<f32>(
            max(grain_rgb, feedback.rgb),
            max(source.a, feedback.a),
        );
        textureStore(output_tex, vec2<i32>(gid.xy), color);

    } else if uniforms.mode == 2u {
        // ─── Mode 2: Flow Map Generation ───────────────────────────
        // Domain-warped fBM noise → RB channels for displacement.
        // Purely procedural — no input textures read.
        let z = uniforms.time * 0.01; // very slow noise evolution
        let flow = flow_noise(uv, z);
        textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(flow.x, 0.0, flow.y, 1.0));

    } else if uniforms.mode == 3u {
        // ─── Mode 3: Flow Displacement ─────────────────────────────
        // Displace max composite using flow map.
        // source_a = max composite (temp_a), source_b = flow map
        let flow = textureSampleLevel(source_tex_b, tex_sampler, uv, 0.0);

        // TD maps [0,1] color to [-weight, +weight] displacement
        let offset = (flow.rb - 0.5) * uniforms.displace_weight;
        let displaced_uv = uv + offset;

        let color = textureSampleLevel(source_tex_a, tex_sampler, displaced_uv, 0.0);
        textureStore(output_tex, vec2<i32>(gid.xy), color);

    } else if uniforms.mode == 4u {
        // ─── Mode 4: Edge Diffusion Blur ───────────────────────────
        // 9-tap weighted Gaussian approximation.
        // source_a = flow-displaced result
        let r = texel * uniforms.blur_radius;

        // Center
        var acc = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0) * 0.25;
        // Cardinal at full radius
        acc += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( r.x, 0.0), 0.0) * 0.125;
        acc += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-r.x, 0.0), 0.0) * 0.125;
        acc += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(0.0,  r.y), 0.0) * 0.125;
        acc += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(0.0, -r.y), 0.0) * 0.125;
        // Diagonal at ~0.707× radius
        let d = r * 0.707;
        acc += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( d.x,  d.y), 0.0) * 0.0625;
        acc += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-d.x,  d.y), 0.0) * 0.0625;
        acc += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( d.x, -d.y), 0.0) * 0.0625;
        acc += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-d.x, -d.y), 0.0) * 0.0625;

        textureStore(output_tex, vec2<i32>(gid.xy), acc);

    } else if uniforms.mode == 5u {
        // ─── Mode 5: Slope Displacement ────────────────────────────
        // Soft light blend → Sobel gradient → displace blurred result.
        // source_a = original source, source_b = blurred (temp_a)
        let step_uv = vec2<f32>(uniforms.slope_step * texel.x, uniforms.slope_step * texel.y);

        // Sample grain_base and blurred at 5 positions, compute soft light
        let ga_r = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(step_uv.x, 0.0), 0.0).rgb;
        let bl_r = textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>(step_uv.x, 0.0), 0.0).rgb;

        let ga_l = textureSampleLevel(source_tex_a, tex_sampler, uv - vec2<f32>(step_uv.x, 0.0), 0.0).rgb;
        let bl_l = textureSampleLevel(source_tex_b, tex_sampler, uv - vec2<f32>(step_uv.x, 0.0), 0.0).rgb;

        let ga_u = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(0.0, step_uv.y), 0.0).rgb;
        let bl_u = textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>(0.0, step_uv.y), 0.0).rgb;

        let ga_d = textureSampleLevel(source_tex_a, tex_sampler, uv - vec2<f32>(0.0, step_uv.y), 0.0).rgb;
        let bl_d = textureSampleLevel(source_tex_b, tex_sampler, uv - vec2<f32>(0.0, step_uv.y), 0.0).rgb;

        // Soft light at each position
        let sl_r = soft_light(ga_r, bl_r);
        let sl_l = soft_light(ga_l, bl_l);
        let sl_u = soft_light(ga_u, bl_u);
        let sl_d = soft_light(ga_d, bl_d);

        // Sobel gradient (luminance-weighted)
        let luma = vec3<f32>(0.2126, 0.7152, 0.0722);
        let dx = dot(sl_r - sl_l, luma) * uniforms.slope_strength;
        let dy = dot(sl_u - sl_d, luma) * uniforms.slope_strength;

        // Displace the blurred image by the slope gradient
        // TD Slope output is 0-centered float — no -0.5 subtraction needed
        let slope_offset = vec2<f32>(dx, dy) * uniforms.displace_weight;
        let displaced_uv = uv + slope_offset;

        let color = textureSampleLevel(source_tex_b, tex_sampler, displaced_uv, 0.0);
        textureStore(output_tex, vec2<i32>(gid.xy), color);

    } else if uniforms.mode == 6u {
        // ─── Mode 6: Luma Blur (Dilution) ──────────────────────────
        // Heavy blur masked by binary noise threshold.
        // source_a = slope-displaced result (temp_b)

        // Binary noise mask — controls where "more water" dilutes the paint
        let mask_noise = noise3d(vec3<f32>(uv * 3.0, uniforms.time * 0.005));
        let mask = step(0.0, mask_noise); // ~50% coverage, organic shapes

        // Unblurred sample
        let sharp = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);

        // Heavy blur: 3-ring multi-tap approximation
        let r1 = texel * uniforms.luma_blur_radius * 0.33;
        let r2 = texel * uniforms.luma_blur_radius * 0.67;
        let r3 = texel * uniforms.luma_blur_radius;

        // Center
        var blurred = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0) * 0.1;

        // Inner ring — cardinal + diagonal (8 taps)
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( r1.x, 0.0),  0.0) * 0.07;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-r1.x, 0.0),  0.0) * 0.07;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(0.0,  r1.y),  0.0) * 0.07;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(0.0, -r1.y),  0.0) * 0.07;
        let d1 = r1 * 0.707;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( d1.x,  d1.y), 0.0) * 0.04;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-d1.x,  d1.y), 0.0) * 0.04;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( d1.x, -d1.y), 0.0) * 0.04;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-d1.x, -d1.y), 0.0) * 0.04;

        // Middle ring — cardinal + diagonal (8 taps)
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( r2.x, 0.0),  0.0) * 0.05;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-r2.x, 0.0),  0.0) * 0.05;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(0.0,  r2.y),  0.0) * 0.05;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(0.0, -r2.y),  0.0) * 0.05;
        let d2 = r2 * 0.707;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( d2.x,  d2.y), 0.0) * 0.025;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-d2.x,  d2.y), 0.0) * 0.025;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( d2.x, -d2.y), 0.0) * 0.025;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-d2.x, -d2.y), 0.0) * 0.025;

        // Outer ring — cardinal only (4 taps)
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( r3.x, 0.0),  0.0) * 0.02;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-r3.x, 0.0),  0.0) * 0.02;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(0.0,  r3.y),  0.0) * 0.02;
        blurred += textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(0.0, -r3.y),  0.0) * 0.02;
        // Weights: 0.1 + 4×0.07 + 4×0.04 + 4×0.05 + 4×0.025 + 4×0.02 = 0.92
        blurred = blurred / 0.92;

        // Mix sharp and blurred based on binary mask
        let color = mix(sharp, blurred, mask);

        // NaN/Inf guard — this writes to the persistent feedback buffer
        textureStore(output_tex, vec2<i32>(gid.xy), safe_clamp(color));

    } else {
        // ─── Mode 7: Emboss Post-Process ───────────────────────────
        // Sobel gradient → directional light projection → overlay composite.
        // source_a = feedback (watercolor result), source_b = original source

        let wc = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);

        // 3×3 neighborhood for Sobel gradients
        let tl = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-texel.x, -texel.y), 0.0).rgb;
        let tc = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(     0.0, -texel.y), 0.0).rgb;
        let tr = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( texel.x, -texel.y), 0.0).rgb;
        let ml = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-texel.x,      0.0), 0.0).rgb;
        let mr = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( texel.x,      0.0), 0.0).rgb;
        let bl = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-texel.x,  texel.y), 0.0).rgb;
        let bc = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(     0.0,  texel.y), 0.0).rgb;
        let br = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( texel.x,  texel.y), 0.0).rgb;

        // Sobel X/Y gradients (luminance-weighted)
        let luma = vec3<f32>(0.2126, 0.7152, 0.0722);
        let gx = dot(-tl + tr - 2.0 * ml + 2.0 * mr - bl + br, luma) / 4.0;
        let gy = dot(-tl - 2.0 * tc - tr + bl + 2.0 * bc + br, luma) / 4.0;

        // Project gradient onto 45° light direction for emboss look
        let light_dir = vec2<f32>(0.7071, 0.7071); // normalize(1, 1)
        let emboss_val = clamp(
            dot(vec2<f32>(gx, gy), light_dir) * uniforms.emboss_strength + 0.5,
            0.0,
            1.0,
        );
        let emboss = vec3<f32>(emboss_val, emboss_val, emboss_val);

        // Overlay composite: paint texture over watercolor result
        let composited = overlay(wc.rgb, emboss);

        // Blend with original source by amount parameter
        let original = textureSampleLevel(source_tex_b, tex_sampler, uv, 0.0);
        let final_rgb = mix(original.rgb, composited, uniforms.amount);
        let final_a = mix(original.a, wc.a, uniforms.amount);

        textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(final_rgb, final_a));
    }
}
