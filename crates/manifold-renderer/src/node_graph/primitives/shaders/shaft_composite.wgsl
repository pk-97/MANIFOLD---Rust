// node.render_scene internal pass (VOLUMETRIC_LIGHT_DESIGN.md D3, P2) —
// full-res depth-aware bilateral upsample of the half-res light-shaft
// inscatter, composited additively into the resolved scene color. The
// composite itself is hardware additive blending (src=One/dst=One on rgb,
// src_alpha=Zero/dst_alpha=One), NOT a manual alpha write — the blend
// hardware enforces "alpha UNTOUCHED" (D3) by construction, same pattern as
// node.value_overlay's additive overlay pass. Full-screen triangle, no
// vertex buffer (compositor_blend.wgsl's vs_main pattern).
//
// `linearize_depth` is forked (copied, not shared/concatenated) from
// `shared/depth_common.wgsl` — same convention as ssao_from_depth.wgsl /
// coc_from_depth.wgsl / shaft_march.wgsl, so this file validates standalone
// under `tests/wgsl_validation.rs`'s auto-discovery.

struct Uniforms {
    near_far: vec4<f32>, // x: near, y: far, z/w: reserved
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var inscatter: texture_2d<f32>;
@group(0) @binding(2) var half_depth: texture_2d<f32>;
@group(0) @binding(3) var full_depth: texture_2d<f32>;

fn linearize_depth(raw: f32, near: f32, far: f32) -> f32 {
    let range = far / (near - far);
    return (range * near) / (raw + range);
}

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

// One committed-weight tap: `bilinear_w * exp(-(Δz/z_full)^2 * 400)` (D3).
// Returns (color*weight, weight) packed in a vec4 (xyz = weighted color, w =
// weight) so the caller can also recover the plain bilinear term (color *
// bilinear_w, derivable as the same call with depth_w forced to 1 — done
// inline below instead, to keep this fork a pure function of its inputs).
fn tap(
    tc: vec2<i32>,
    max_xy: vec2<i32>,
    near: f32,
    far: f32,
    z_full: f32,
    bilinear_w: f32,
) -> vec4<f32> {
    let c = clamp(tc, vec2<i32>(0, 0), max_xy);
    let color = textureLoad(inscatter, c, 0).rgb;
    let raw = textureLoad(half_depth, c, 0).r;
    let z_tap = linearize_depth(raw, near, far);
    let dz = (z_full - z_tap) / z_full;
    let depth_w = exp(-(dz * dz) * 400.0);
    let w = bilinear_w * depth_w;
    return vec4<f32>(color * w, w);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let near = u.near_far.x;
    let far = u.near_far.y;

    let full_dims = textureDimensions(full_depth);
    let half_dims = textureDimensions(half_depth);
    let full_c = clamp(
        vec2<i32>(in.uv * vec2<f32>(full_dims)),
        vec2<i32>(0, 0),
        vec2<i32>(full_dims) - vec2<i32>(1, 1),
    );

    let full_raw = textureLoad(full_depth, full_c, 0).r;
    let z_full = max(linearize_depth(full_raw, near, far), 1e-4);

    // Half-res sample position: full-res UV mapped into half-res texel
    // space, offset -0.5 so the 4 taps below are the standard bilinear
    // neighbourhood of that position.
    let half_coord = in.uv * vec2<f32>(half_dims) - vec2<f32>(0.5, 0.5);
    let base = floor(half_coord);
    let frac_xy = half_coord - base;
    let x0 = i32(base.x);
    let y0 = i32(base.y);
    let max_xy = vec2<i32>(half_dims) - vec2<i32>(1, 1);

    let w00 = (1.0 - frac_xy.x) * (1.0 - frac_xy.y);
    let w10 = frac_xy.x * (1.0 - frac_xy.y);
    let w01 = (1.0 - frac_xy.x) * frac_xy.y;
    let w11 = frac_xy.x * frac_xy.y;

    let t00 = tap(vec2<i32>(x0, y0), max_xy, near, far, z_full, w00);
    let t10 = tap(vec2<i32>(x0 + 1, y0), max_xy, near, far, z_full, w10);
    let t01 = tap(vec2<i32>(x0, y0 + 1), max_xy, near, far, z_full, w01);
    let t11 = tap(vec2<i32>(x0 + 1, y0 + 1), max_xy, near, far, z_full, w11);

    let weighted_sum = t00.xyz + t10.xyz + t01.xyz + t11.xyz;
    let weight_sum = t00.w + t10.w + t01.w + t11.w;

    var result: vec3<f32>;
    if weight_sum < 1e-4 {
        // Fallback: plain bilinear (no depth term) — the committed
        // renormalize-fails-to-plain-bilinear case (D3). Bilinear weights
        // already sum to 1 by construction, so a fresh (undepth-weighted)
        // sum is the plain bilinear result directly.
        let c00 = clamp(vec2<i32>(x0, y0), vec2<i32>(0, 0), max_xy);
        let c10 = clamp(vec2<i32>(x0 + 1, y0), vec2<i32>(0, 0), max_xy);
        let c01 = clamp(vec2<i32>(x0, y0 + 1), vec2<i32>(0, 0), max_xy);
        let c11 = clamp(vec2<i32>(x0 + 1, y0 + 1), vec2<i32>(0, 0), max_xy);
        result = textureLoad(inscatter, c00, 0).rgb * w00
            + textureLoad(inscatter, c10, 0).rgb * w10
            + textureLoad(inscatter, c01, 0).rgb * w01
            + textureLoad(inscatter, c11, 0).rgb * w11;
    } else {
        result = weighted_sum / weight_sum;
    }

    // Alpha is irrelevant here: the blend state's src_alpha_factor=Zero
    // discards whatever this shader writes to .w, enforcing "alpha
    // untouched" (D3) via the ROP, not this shader's return value.
    return vec4<f32>(result, 0.0);
}
