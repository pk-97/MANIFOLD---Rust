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
    self_advect_scale: f32,   // 0.5
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
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
    let adv_uv = uv - v_unblurred * (vel_u.self_advect_scale * texel);
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
    _pad0: f32,
    _pad1: f32,
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
    let adv_uv = uv - velocity * texel;
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
    let n = normalize(vec3<f32>(-gx, -gy, max(z_scale, 1e-4)));
    return n * 0.5 + 0.5; // pack to [0, 1]
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

    // Velocity drives chromatic aberration displacement.
    let v = textureSampleLevel(rnd_velocity, rnd_samp, uv, 0.0).rg;

    // Per-channel displacement scales (symmetric split around green).
    let ab = rnd_u.chroma;
    let scale_r = -ab;
    let scale_g =  0.0;
    let scale_b =  ab;

    // Displacement in UV space = velocity * scale * texel (keeps effect
    // resolution-invariant: spec's "three separate displacement lookups").
    let disp_r = v * (scale_r * texel);
    let disp_g = v * (scale_g * texel);
    let disp_b = v * (scale_b * texel);

    let n_r = render_normal(uv + disp_r, texel, rnd_u.normal_z_scale).r;
    let n_g = render_normal(uv + disp_g, texel, rnd_u.normal_z_scale).g;
    let n_b = render_normal(uv + disp_b, texel, rnd_u.normal_z_scale).b;

    let recombined = vec3<f32>(n_r, n_g, n_b);

    // Spec: abs() of recombined RGB → contrast grade (lower exposure, red-oily).
    var col = abs(recombined);
    col = (col - 0.5) * rnd_u.contrast + 0.5;
    col = clamp(col, vec3<f32>(0.0), vec3<f32>(1.0));

    textureStore(rnd_out, vec2<i32>(gid.xy), vec4<f32>(col, 1.0));
}
