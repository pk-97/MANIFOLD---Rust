// Oily Fluid — 4-pass reaction/advection fluid feedback.
//
// Faithful port of Bileam Tschepe's "red oily fluid" TouchDesigner tutorial.
// This shader is composed at runtime with particle_common.wgsl prepended,
// which provides wang_hash() and simplex_noise_3d().
//
// Entry points (dispatched in this order per frame):
//   cs_downsample : FBO_Velocity full-res → quarter-res (4x4 box filter)
//                   (two separable blur dispatches of gaussian_blur_compute.wgsl
//                    happen between downsample and cs_velocity)
//   cs_velocity   : Pass 2 — color gradient curl + self-advected blurred velocity
//   cs_color      : Pass 3 — advect color by velocity + inline noise injection
//   cs_render     : Pass 4 — heightmap → normal map → chromatic aberration → level
//
// All uniforms are 32 bytes at @binding(0) to satisfy Naga's multi-entry-point
// uniform-size rule.

// ─────────────────────────────────────────────────────────────────────
// Seed pass: cs_seed — fills color + velocity state with layered noise
// so the feedback simulation starts from a rich initial state instead
// of running warmup iterations from blank textures.
// ─────────────────────────────────────────────────────────────────────

struct SeedUniforms {
    width: f32,
    height: f32,
    aspect: f32,
    seed: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
    _pad5: f32,
    _pad6: f32,
    _pad7: f32,
};

@group(0) @binding(0) var<uniform> seed_u: SeedUniforms;
@group(0) @binding(1) var seed_color_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var seed_velocity_out: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16, 1)
fn cs_seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    let W = u32(seed_u.width);
    let H = u32(seed_u.height);
    if gid.x >= W || gid.y >= H {
        return;
    }

    let texel = vec2<f32>(1.0 / seed_u.width, 1.0 / seed_u.height);
    let uv = (vec2<f32>(gid.xy) + 0.5) * texel;
    let nuv = vec2<f32>(uv.x * seed_u.aspect, uv.y);
    let s = seed_u.seed;

    // Layered simplex noise for color (R and G channels, independent seeds).
    let cr = simplex_noise_3d(vec3<f32>(nuv * 3.0, s))
           + simplex_noise_3d(vec3<f32>(nuv * 7.0, s + 5.0)) * 0.5
           + simplex_noise_3d(vec3<f32>(nuv * 13.0, s + 11.0)) * 0.25;
    let cg = simplex_noise_3d(vec3<f32>(nuv * 3.0 + vec2<f32>(31.7, 17.3), s + 3.0))
           + simplex_noise_3d(vec3<f32>(nuv * 7.0 + vec2<f32>(31.7, 17.3), s + 8.0)) * 0.5
           + simplex_noise_3d(vec3<f32>(nuv * 13.0 + vec2<f32>(31.7, 17.3), s + 14.0)) * 0.25;

    // Strong initial color so gradient extraction in cs_velocity immediately
    // produces curl forcing — the feedback loop amplifies from here.
    let color = vec2<f32>(cr, cg) * 0.45;
    textureStore(seed_color_out, vec2<i32>(gid.xy), vec4<f32>(color, 0.0, 1.0));

    // Curl-like velocity from noise gradients — gives immediate flow structure.
    let eps = 0.01;
    let n0 = simplex_noise_3d(vec3<f32>(nuv * 4.0, s + 20.0));
    let nx = simplex_noise_3d(vec3<f32>(nuv * 4.0 + vec2<f32>(eps, 0.0), s + 20.0));
    let ny = simplex_noise_3d(vec3<f32>(nuv * 4.0 + vec2<f32>(0.0, eps), s + 20.0));
    // Rotate gradient 90° to get curl (divergence-free flow).
    let vel = vec2<f32>(-(ny - n0) / eps, (nx - n0) / eps) * 0.18;
    textureStore(seed_velocity_out, vec2<i32>(gid.xy), vec4<f32>(vel, 0.0, 1.0));
}

// ─────────────────────────────────────────────────────────────────────
// Pass A: cs_downsample — 4x4 box filter on velocity
// ─────────────────────────────────────────────────────────────────────

struct DownsampleUniforms {
    src_width: f32,
    src_height: f32,
    dst_width: f32,
    dst_height: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
    _pad5: f32,
    _pad6: f32,
    _pad7: f32,
};

@group(0) @binding(0) var<uniform> down_u: DownsampleUniforms;
@group(0) @binding(1) var down_src: texture_2d<f32>;
@group(0) @binding(2) var down_samp: sampler;
@group(0) @binding(3) var down_dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16, 1)
fn cs_downsample(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dw = u32(down_u.dst_width);
    let dh = u32(down_u.dst_height);
    if gid.x >= dw || gid.y >= dh {
        return;
    }

    // Sample center of dst pixel, mapped to src UV. Linear sampling gives a
    // 2x2 bilinear tap; we average 2x2 of those to get a 4x4 box filter.
    let inv_dst = vec2<f32>(1.0 / down_u.dst_width, 1.0 / down_u.dst_height);
    let uv = (vec2<f32>(gid.xy) + 0.5) * inv_dst;

    // Quarter-texel offset in DST space = one src texel pair offset.
    let off = 0.25 * inv_dst;

    let s0 = textureSampleLevel(down_src, down_samp, uv + vec2<f32>(-off.x, -off.y), 0.0);
    let s1 = textureSampleLevel(down_src, down_samp, uv + vec2<f32>( off.x, -off.y), 0.0);
    let s2 = textureSampleLevel(down_src, down_samp, uv + vec2<f32>(-off.x,  off.y), 0.0);
    let s3 = textureSampleLevel(down_src, down_samp, uv + vec2<f32>( off.x,  off.y), 0.0);

    let avg = (s0 + s1 + s2 + s3) * 0.25;
    textureStore(down_dst, vec2<i32>(gid.xy), avg);
}

// ─────────────────────────────────────────────────────────────────────
// Pass B: cs_velocity — Pass 2 of spec
//   1. Extract abs-color gradients (R & G channels independently)
//   2. Normalize each, sum, attenuate by 0.2
//   3. Rotate 90° to produce curl
//   4. Self-advect the blurred velocity by the unblurred velocity,
//      then dampen (× 0.98) and add the curl forcing.
// ─────────────────────────────────────────────────────────────────────

struct VelocityUniforms {
    width: f32,
    height: f32,
    grad_attenuation: f32,    // 0.2
    velocity_damping: f32,    // 0.98
    self_advect_scale: f32,   // 0.5 (base constant from spec)
    vel_disp: f32,            // VEL DISP multiplier (1.0 = spec default)
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
    _pad5: f32,
};

@group(0) @binding(0) var<uniform> vel_u: VelocityUniforms;
@group(0) @binding(1) var vel_color: texture_2d<f32>;            // FBO_Color (front)
@group(0) @binding(2) var vel_velocity: texture_2d<f32>;         // FBO_Velocity (front, unblurred)
@group(0) @binding(3) var vel_blurred: texture_2d<f32>;          // blurred velocity (quarter-res)
@group(0) @binding(4) var vel_samp: sampler;                     // linear repeat
@group(0) @binding(5) var vel_out: texture_storage_2d<rgba16float, write>;

fn safe_normalize2(v: vec2<f32>) -> vec2<f32> {
    let len = length(v);
    if len < 1e-6 {
        return vec2<f32>(0.0, 0.0);
    }
    return v / len;
}

@compute @workgroup_size(16, 16, 1)
fn cs_velocity(@builtin(global_invocation_id) gid: vec3<u32>) {
    let W = u32(vel_u.width);
    let H = u32(vel_u.height);
    if gid.x >= W || gid.y >= H {
        return;
    }

    let texel = vec2<f32>(1.0 / vel_u.width, 1.0 / vel_u.height);
    let uv = (vec2<f32>(gid.xy) + 0.5) * texel;

    // 1. Slope TOP — central difference of abs(color), R and G independently.
    let cL = abs(textureSampleLevel(vel_color, vel_samp, uv + vec2<f32>(-texel.x, 0.0), 0.0).rg);
    let cR = abs(textureSampleLevel(vel_color, vel_samp, uv + vec2<f32>( texel.x, 0.0), 0.0).rg);
    let cD = abs(textureSampleLevel(vel_color, vel_samp, uv + vec2<f32>(0.0, -texel.y), 0.0).rg);
    let cU = abs(textureSampleLevel(vel_color, vel_samp, uv + vec2<f32>(0.0,  texel.y), 0.0).rg);

    let grad_r = vec2<f32>((cR.r - cL.r) * 0.5, (cU.r - cD.r) * 0.5);
    let grad_g = vec2<f32>((cR.g - cL.g) * 0.5, (cU.g - cD.g) * 0.5);

    // 2. Normalize each independently (guarded).
    let grad_r_n = safe_normalize2(grad_r);
    let grad_g_n = safe_normalize2(grad_g);

    // 3. Sum and attenuate.
    let sum_grad = (grad_r_n + grad_g_n) * vel_u.grad_attenuation;

    // 4. Rotate 90°.
    let rot_grad = vec2<f32>(-sum_grad.y, sum_grad.x);

    // 5. Self-advect the BLURRED velocity by the UNBLURRED velocity.
    let v_unblurred = textureSampleLevel(vel_velocity, vel_samp, uv, 0.0).rg;
    let adv_uv = uv - v_unblurred * (vel_u.self_advect_scale * vel_u.vel_disp * texel);
    let v_blurred_advected = textureSampleLevel(vel_blurred, vel_samp, adv_uv, 0.0).rg;

    // 6. Dampen and add curl forcing.
    let final_v = v_blurred_advected * vel_u.velocity_damping + rot_grad;

    textureStore(vel_out, vec2<i32>(gid.xy), vec4<f32>(final_v, 0.0, 1.0));
}

// ─────────────────────────────────────────────────────────────────────
// Pass C: cs_color — Pass 3 of spec
//   1. Advect color field by the just-computed velocity, GL_REPEAT wrap.
//   2. Sample 3D Perlin noise inline (aspect-corrected, time-animated),
//      R and G channels seeded independently.
//   3. Feedback mix: advected * retention + noise * injection.
// ─────────────────────────────────────────────────────────────────────

struct ColorUniforms {
    width: f32,
    height: f32,
    feedback_retention: f32,  // 0.998
    noise_injection: f32,     // 0.002
    noise_time: f32,          // internal_frame * 0.01 * speed
    aspect: f32,
    col_disp: f32,            // COL DISP multiplier (1.0 = spec default)
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<uniform> col_u: ColorUniforms;
@group(0) @binding(1) var col_prev: texture_2d<f32>;             // FBO_Color (front)
@group(0) @binding(2) var col_velocity: texture_2d<f32>;         // FBO_Velocity (back)
@group(0) @binding(3) var col_samp: sampler;                     // linear repeat
@group(0) @binding(4) var col_out: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16, 1)
fn cs_color(@builtin(global_invocation_id) gid: vec3<u32>) {
    let W = u32(col_u.width);
    let H = u32(col_u.height);
    if gid.x >= W || gid.y >= H {
        return;
    }

    let texel = vec2<f32>(1.0 / col_u.width, 1.0 / col_u.height);
    let uv = (vec2<f32>(gid.xy) + 0.5) * texel;

    // Spec: advUV = uv - velocity / resolution, boundary = GL_REPEAT
    let velocity = textureSampleLevel(col_velocity, col_samp, uv, 0.0).rg;
    let adv_uv = uv - velocity * (col_u.col_disp * texel);
    let advected_color = textureSampleLevel(col_prev, col_samp, adv_uv, 0.0).rg;

    // Inline 3D noise — aspect-corrected so the pattern stays isotropic on
    // non-square canvases. R and G sampled at distinct seed offsets so each
    // channel carries an independent noise field (spec: "monochrome disabled").
    let nuv = vec2<f32>(uv.x * col_u.aspect, uv.y) * 3.0;
    let n_r = simplex_noise_3d(vec3<f32>(nuv,                         col_u.noise_time));
    let n_g = simplex_noise_3d(vec3<f32>(nuv + vec2<f32>(31.7, 17.3), col_u.noise_time));

    // Feedback mix: advected * 0.998 + noise * 0.002
    let new_color = advected_color * col_u.feedback_retention
                  + vec2<f32>(n_r, n_g) * col_u.noise_injection;

    textureStore(col_out, vec2<i32>(gid.xy), vec4<f32>(new_color, 0.0, 1.0));
}

// ─────────────────────────────────────────────────────────────────────
// Pass D: cs_render — Pass 4 of spec
//   1. Build heightmap from length(color.rg).
//   2. Generate tangent-space normal map via central difference + Z-scale.
//   3. Three chromatically-separated displaced normal-map lookups via the
//      velocity field (R/G/B at different displacement scales).
//   4. abs() of recombined RGB, then contrast/brightness grade.
// ─────────────────────────────────────────────────────────────────────

struct RenderUniforms {
    width: f32,
    height: f32,
    normal_z_scale: f32,      // 0.5
    chroma: f32,              // chromatic aberration master scale
    contrast: f32,            // 1.4
    hue_shift: f32,           // [0, 1] — hue rotation in turns
    saturation: f32,          // 0..2 (1 = neutral)
    brightness: f32,          // 0..2 (1 = neutral)
    mode: f32,                // 0=OilSlick, 1=FlowField, 2=HeightMap, 3=PBR, 4=Lines
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> rnd_u: RenderUniforms;
@group(0) @binding(1) var rnd_color: texture_2d<f32>;            // FBO_Color (back)
@group(0) @binding(2) var rnd_velocity: texture_2d<f32>;         // FBO_Velocity (back)
@group(0) @binding(3) var rnd_samp: sampler;                     // linear clamp
@group(0) @binding(4) var rnd_out: texture_storage_2d<rgba16float, write>;

fn render_height(uv: vec2<f32>) -> f32 {
    let c = textureSampleLevel(rnd_color, rnd_samp, uv, 0.0).rg;
    return length(c);
}

fn render_normal(uv: vec2<f32>, texel: vec2<f32>, z_scale: f32) -> vec3<f32> {
    let hL = render_height(uv - vec2<f32>(texel.x, 0.0));
    let hR = render_height(uv + vec2<f32>(texel.x, 0.0));
    let hD = render_height(uv - vec2<f32>(0.0, texel.y));
    let hU = render_height(uv + vec2<f32>(0.0, texel.y));
    let gx = (hR - hL) * 0.5;
    let gy = (hU - hD) * 0.5;
    // Signed tangent-space normal ([-1, 1] per component). The subsequent
    // abs() in cs_render operates on these signed values — packing to [0, 1]
    // here would neutralize abs() and leave the scene a flat purple.
    return normalize(vec3<f32>(-gx, -gy, max(z_scale, 1e-4)));
}

// ── Render mode helpers ────────────────────────────────────────────

// Oil Slick: signed-normal + chromatic aberration + abs + level grade.
fn mode_oil_slick(uv: vec2<f32>, texel: vec2<f32>) -> vec3<f32> {
    let v = textureSampleLevel(rnd_velocity, rnd_samp, uv, 0.0).rg;
    let ab = rnd_u.chroma;
    let disp_r = v * (-ab * texel);
    let disp_b = v * ( ab * texel);

    let n_r = render_normal(uv + disp_r, texel, rnd_u.normal_z_scale).r;
    let n_g = render_normal(uv,          texel, rnd_u.normal_z_scale).g;
    let n_b = render_normal(uv + disp_b, texel, rnd_u.normal_z_scale).b;

    var col = abs(vec3<f32>(n_r, n_g, n_b));
    col = (col - 0.5) * rnd_u.contrast + 0.5;
    return clamp(col, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Flow Field: N-step walk along velocity, accumulate heightmap (LIC-lite).
fn mode_flow_field(uv: vec2<f32>, texel: vec2<f32>) -> vec3<f32> {
    let steps: i32 = 16;
    let dt = 2.0 * texel;
    var sum: f32 = render_height(uv);
    var w_total: f32 = 1.0;

    var walker = uv;
    for (var i: i32 = 1; i <= steps; i = i + 1) {
        let v = textureSampleLevel(rnd_velocity, rnd_samp, walker, 0.0).rg;
        let vn = v / max(length(v), 1e-4);
        walker = walker + vn * dt;
        let w = 1.0 - f32(i) / f32(steps);
        sum = sum + render_height(walker) * w;
        w_total = w_total + w;
    }
    walker = uv;
    for (var i: i32 = 1; i <= steps; i = i + 1) {
        let v = textureSampleLevel(rnd_velocity, rnd_samp, walker, 0.0).rg;
        let vn = v / max(length(v), 1e-4);
        walker = walker - vn * dt;
        let w = 1.0 - f32(i) / f32(steps);
        sum = sum + render_height(walker) * w;
        w_total = w_total + w;
    }
    let acc = sum / w_total;
    let lum = clamp(acc * rnd_u.contrast * 3.0, 0.0, 1.0);
    // Warm sepia tint (sand / skin tone) with slight curve
    return vec3<f32>(lum, lum * 0.7, lum * 0.5);
}

// Height Map: dramatic z-scaled normal + lambert directional light.
fn mode_height_map(uv: vec2<f32>, texel: vec2<f32>) -> vec3<f32> {
    let n = render_normal(uv, texel, rnd_u.normal_z_scale * 0.2);
    let light_dir = normalize(vec3<f32>(0.4, 0.6, 0.7));
    let lambert = max(dot(n, light_dir), 0.0);
    let ambient = 0.1;
    let v = lambert * 0.9 + ambient;
    // Slight high-frequency detail from the raw fluid field
    let h = render_height(uv);
    let final_v = clamp((v + h * 0.05) * rnd_u.contrast, 0.0, 1.0);
    return vec3<f32>(final_v);
}

// PBR: analytical matcap + fresnel rim + blinn spec. No actual texture asset.
fn mode_pbr(uv: vec2<f32>, texel: vec2<f32>) -> vec3<f32> {
    let n = render_normal(uv, texel, rnd_u.normal_z_scale);
    let view = vec3<f32>(0.0, 0.0, 1.0);

    // Matcap-style two-tone gradient sampled by normal.xy (sphere in NDC)
    let mc_uv = n.xy * 0.5 + 0.5;
    let base = mix(
        vec3<f32>(0.08, 0.05, 0.22),   // deep purple shadow
        vec3<f32>(0.55, 0.75, 0.95),   // pale blue highlight
        clamp(mc_uv.y, 0.0, 1.0),
    );
    let side = mix(
        vec3<f32>(0.25, 0.10, 0.45),   // magenta
        vec3<f32>(0.15, 0.55, 0.60),   // teal
        clamp(mc_uv.x, 0.0, 1.0),
    );
    var col = (base + side) * 0.5;

    // Fresnel rim (iridescent edge highlight)
    let fresnel = pow(1.0 - max(dot(n, view), 0.0), 3.0);
    col = col + fresnel * vec3<f32>(0.55, 0.30, 0.85);

    // Blinn spec — light positioned over the shoulder
    let light = normalize(vec3<f32>(0.35, 0.55, 0.75));
    let h = normalize(light + view);
    let spec = pow(max(dot(n, h), 0.0), 48.0);
    col = col + spec * vec3<f32>(1.0, 0.95, 1.0);

    col = (col - 0.5) * rnd_u.contrast + 0.5;
    return clamp(col, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Lines (LAJNS): Line Integral Convolution streamlines along the velocity
// field, using a hash-noise texture as the ink source.
fn lines_ink(p: vec2<f32>) -> f32 {
    // Deterministic hash noise at high frequency. Anchored to UV so the
    // pattern stays stable across frames.
    let q = vec2<u32>(u32(p.x * 1024.0 + 4096.0), u32(p.y * 1024.0 + 4096.0));
    let h = wang_hash(q.x * 73856093u ^ q.y * 19349663u);
    return f32(h) / 4294967296.0;
}

fn mode_lines(uv: vec2<f32>, texel: vec2<f32>) -> vec3<f32> {
    let steps: i32 = 20;
    let dt = 1.5 * texel;
    var sum: f32 = lines_ink(uv);
    var w_total: f32 = 1.0;

    var walker = uv;
    for (var i: i32 = 1; i <= steps; i = i + 1) {
        let v = textureSampleLevel(rnd_velocity, rnd_samp, walker, 0.0).rg;
        let vn = v / max(length(v), 1e-4);
        walker = walker + vn * dt;
        let w = 1.0 - f32(i) / f32(steps);
        sum = sum + lines_ink(walker) * w;
        w_total = w_total + w;
    }
    walker = uv;
    for (var i: i32 = 1; i <= steps; i = i + 1) {
        let v = textureSampleLevel(rnd_velocity, rnd_samp, walker, 0.0).rg;
        let vn = v / max(length(v), 1e-4);
        walker = walker - vn * dt;
        let w = 1.0 - f32(i) / f32(steps);
        sum = sum + lines_ink(walker) * w;
        w_total = w_total + w;
    }
    let lic = sum / w_total;
    // Threshold + contrast so only aligned streaks survive.
    let line = smoothstep(0.45, 0.62, lic * rnd_u.contrast);
    return vec3<f32>(line);
}

@compute @workgroup_size(16, 16, 1)
fn cs_render(@builtin(global_invocation_id) gid: vec3<u32>) {
    let W = u32(rnd_u.width);
    let H = u32(rnd_u.height);
    if gid.x >= W || gid.y >= H {
        return;
    }

    let texel = vec2<f32>(1.0 / rnd_u.width, 1.0 / rnd_u.height);
    let uv = (vec2<f32>(gid.xy) + 0.5) * texel;

    let mode_i = i32(round(rnd_u.mode));
    var col: vec3<f32>;
    if mode_i == 1 {
        col = mode_flow_field(uv, texel);
    } else if mode_i == 2 {
        col = mode_height_map(uv, texel);
    } else if mode_i == 3 {
        col = mode_pbr(uv, texel);
    } else if mode_i == 4 {
        col = mode_lines(uv, texel);
    } else {
        col = mode_oil_slick(uv, texel);
    }

    // Shared color grading tail (hue / saturation / brightness). Applies to
    // every mode so users can re-tint any preset with the existing controls.
    var hsv = rgb_to_hsv(col);
    hsv.x = fract(hsv.x + rnd_u.hue_shift + 1.0);
    hsv.y = clamp(hsv.y * rnd_u.saturation, 0.0, 1.0);
    col = hsv_to_rgb(hsv.x, hsv.y, hsv.z);
    col = clamp(col * rnd_u.brightness, vec3<f32>(0.0), vec3<f32>(1.0));

    textureStore(rnd_out, vec2<i32>(gid.xy), vec4<f32>(col, 1.0));
}
