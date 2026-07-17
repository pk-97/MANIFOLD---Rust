// node.ssao_gtao — fusable body (freeze §12), Pointwise + GatherTexel.
//
// GTAO (docs/CINEMATIC_POST_DESIGN.md D9(a)) — REPLACES node.ssao_from_depth
// outright (D9(b): the old primitive is deleted, not paralleled). Exact
// committed algorithm, no substitution:
//   1. Reconstruct view-space center position P from raw depth (same
//      linearize_depth + inverse-perspective xy as D3/ssao_from_depth) and
//      the view-space normal via the SAME ±1-texel finite-difference method
//      as D3 (no normal G-buffer in v1).
//   2. V = normalize(-P) (view vector, surface point toward the eye).
//   3. `radius_px` = the world `radius` projected to a screen-space pixel
//      length AT the center pixel's depth — transcribed from
//      ssao_from_depth's existing `sample_ndc_y = sample_pos.y / (tan_half_
//      fov * view_z)` relation, inverted for a LENGTH rather than a
//      position: ndc_delta = radius / (tan_half_fov * view_z); px_delta =
//      ndc_delta * dims.y * 0.5 (NDC's y axis spans dims.y pixels over its
//      [-1,1] range).
//   4. 2 slices per pixel, slice angles phi_i = hash_angle(px)*0.5 +
//      i*(pi/2), i in {0,1} (D2's committed per-pixel rotation hash, halved
//      into [0,pi) so two slices spread the semicircle).
//   5. Per slice, per side (+/- the slice's 2D screen direction), 4 steps at
//      screen-space radii r_j = radius_px * (j+1)/4, j in 0..4. Each step
//      samples an integer texel offset from center, reconstructs its
//      view-space position S_j (same reconstruction as P). Range check:
//      reject a sample whose |S_j - P| exceeds the world `radius` (the D3
//      halo guard) — rejected samples don't participate in the per-side
//      max. Horizon cosine per side = max(-1.0, max over in-range samples of
//      dot(normalize(S_j - P), V)) — the -1.0 floor is the "no occluder
//      found on this side" default, which (after the h1/h2 clamp below)
//      integrates to the full unoccluded hemisphere contribution on that
//      side, not zero.
//   6. Signed horizon angles: h1 = -acos(hcos_minus), h2 = +acos(hcos_plus).
//      The surface normal projected into the slice's plane (spanned by V
//      and the slice's screen direction embedded as a view-space vector
//      with z=0 — the standard GTAO screen-space-direction-as-tangent
//      approximation) gives length ||N_p|| and a signed angle `n` from V
//      (sign by which side of the plane the slice tangent falls on).
//      Clamp: h1 = n + max(h1 - n, -pi/2); h2 = n + min(h2 - n, pi/2).
//   7. Per-side arc a(h) = 0.25*(-cos(2h-n) + cos(n) + 2h*sin(n)); slice
//      visibility = ||N_p|| * (a(h1) + a(h2)). Pixel visibility = mean of
//      the 2 slices.
//   8. out.r = clamp(1 - intensity*(1 - visibility), 0, 1), broadcast RGB,
//      alpha 1 — same output contract as D3 (ssao_from_depth), so the
//      preset's mix wiring and cards are untouched by the swap.
//
// Total taps: 2 slices * 2 sides * 4 steps = 16 depth taps + the same
// +/-1-texel normal reconstruction as D3 (4 more) — same sample class as
// D3's 16-tap SSAO, per Peter's "not hitting the performance harder" bar.
//
// No temporal accumulation, no thickness heuristic (D9(a) rejects both —
// deterministic single-frame budget only).
//
// `depth` is GatherTexel (integer textureLoad, manual ClampToEdge — no
// sampler), same convention as ssao_from_depth_body.wgsl.
//
// `camera` reads fov_y/near/far entirely via the three DERIVED_UNIFORMS
// below — never a GPU binding (P0/D7).
//
// PARAMS: [radius, intensity] — `bias` has no successor (D9(b): the range
// check subsumes it). DERIVED_UNIFORMS: [fov_y, near, far].
// Matches ssao_gtao.wgsl (the hand parity oracle) — kept independent (not
// sharing source) so the gpu_tests parity check is a real cross-check.
// Rounding uses an explicit round-half-away-from-zero helper rather than
// WGSL's `round()` builtin (round-half-to-even/banker's rounding per the
// WGSL spec) specifically so the CPU reference (plain Rust, which has no
// matching builtin either) can implement the IDENTICAL formula and agree
// bit-for-bit on tie cases instead of inheriting a language-level rounding
// mismatch on top of the already-accepted trig-rounding tolerance class
// (see ssao_from_depth.rs's `generated_ssao_matches_cpu_reference_on_
// synthetic_ramp` doc comment for the precedent this avoids compounding).

// Slice/step counts arrive as PARAMS since 2026-07-17 (quality knobs;
// defaults 2/4 keep the original 16-tap D9(a) budget bit-identical).
const GTAO_HALF_PI: f32 = 1.5707963267948966;

// D2's committed per-pixel rotation hash (docs/CINEMATIC_POST_DESIGN.md D2) —
// identical to ssao_from_depth_body.wgsl's ssao_hash_angle.
fn gtao_hash_angle(px: vec2<f32>) -> f32 {
    return fract(sin(dot(px, vec2<f32>(12.9898, 78.233))) * 43758.5453) * 6.283185307;
}

// Round half away from zero, expressed without WGSL's `round()` builtin
// (see file header) so the CPU-Rust twin agrees on every tie exactly.
fn gtao_round(x: f32) -> f32 {
    if x >= 0.0 {
        return floor(x + 0.5);
    }
    return -floor(-x + 0.5);
}

// Reconstruct a view-space position at integer texel `c` (clamped to the
// texture bounds) from `depth`'s raw [0,1] value — identical formula to
// ssao_from_depth_body.wgsl's `ssao_view_pos`.
fn gtao_view_pos(
    depth: texture_2d<f32>,
    c: vec2<i32>,
    dims_i: vec2<i32>,
    tan_half_fov: f32,
    aspect: f32,
    near: f32,
    far: f32,
) -> vec3<f32> {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    let raw = textureLoad(depth, cc, 0).r;
    let view_z = linearize_depth(raw, near, far);
    let uv = (vec2<f32>(cc) + vec2<f32>(0.5, 0.5)) / vec2<f32>(dims_i);
    let ndc_x = uv.x * 2.0 - 1.0;
    let ndc_y = 1.0 - uv.y * 2.0;
    let view_x = ndc_x * tan_half_fov * aspect * view_z;
    let view_y = ndc_y * tan_half_fov * view_z;
    return vec3<f32>(view_x, view_y, view_z);
}

// Committed a(h) integral (D9(a) step 7).
fn gtao_integrate_arc(h: f32, n: f32) -> f32 {
    return 0.25 * (-cos(2.0 * h - n) + cos(n) + 2.0 * h * sin(n));
}

fn body(
    depth: texture_2d<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    radius: f32,
    intensity: f32,
    slices: f32,
    steps: f32,
    fov_y: f32,
    near: f32,
    far: f32,
) -> vec4<f32> {
    let n_slices = max(1u, u32(gtao_round(slices)));
    let n_steps = max(1u, u32(gtao_round(steps)));
    let dims_i = vec2<i32>(dims);
    let c = vec2<i32>(uv * dims);
    let tan_half_fov = tan(fov_y * 0.5);
    let aspect = dims.x / dims.y;

    let p_c = gtao_view_pos(depth, c, dims_i, tan_half_fov, aspect, near, far);

    // Normal reconstruction — identical to D3 (ssao_from_depth).
    let p_xp = gtao_view_pos(depth, c + vec2<i32>(1, 0), dims_i, tan_half_fov, aspect, near, far);
    let p_xm = gtao_view_pos(depth, c - vec2<i32>(1, 0), dims_i, tan_half_fov, aspect, near, far);
    let p_yp = gtao_view_pos(depth, c + vec2<i32>(0, 1), dims_i, tan_half_fov, aspect, near, far);
    let p_ym = gtao_view_pos(depth, c - vec2<i32>(0, 1), dims_i, tan_half_fov, aspect, near, far);
    let ddx = p_xp - p_xm;
    let ddy = p_yp - p_ym;
    var normal = cross(ddx, ddy);
    let normal_len = length(normal);
    if normal_len > 1e-8 {
        normal = normal / normal_len;
    } else {
        normal = vec3<f32>(0.0, 0.0, -1.0);
    }

    let view_vec = normalize(-p_c);

    // radius_px — transcribed projection (see file header point 3).
    let center_vz = max(p_c.z, 1e-4);
    let radius_px = (radius / (tan_half_fov * center_vz)) * (dims.y * 0.5);

    let rot = gtao_hash_angle(vec2<f32>(c));

    var visibility_sum = 0.0;
    for (var s: u32 = 0u; s < n_slices; s = s + 1u) {
        // Spread N slices across the semicircle (pi/N spacing) — reduces to
        // the committed i*(pi/2) at the default N=2.
        let phi = rot * 0.5 + f32(s) * (2.0 * GTAO_HALF_PI / f32(n_slices));
        let dir2 = vec2<f32>(cos(phi), sin(phi));
        let dir3 = vec3<f32>(dir2, 0.0);

        // Slice-plane projection of the normal (standard GTAO
        // screen-direction-as-tangent construction, D9(a) step 6).
        let axis = cross(dir3, view_vec);
        let axis_len = length(axis);
        var proj_normal = normal;
        var proj_len = 0.0;
        var n_signed = 0.0;
        if axis_len > 1e-6 {
            let axis_n = axis / axis_len;
            proj_normal = normal - axis_n * dot(normal, axis_n);
            proj_len = length(proj_normal);
            if proj_len > 1e-6 {
                let cos_n = clamp(dot(proj_normal, view_vec) / proj_len, -1.0, 1.0);
                let ortho = dir3 - dot(dir3, view_vec) * view_vec;
                let sign_n = select(-1.0, 1.0, dot(ortho, proj_normal) >= 0.0);
                n_signed = sign_n * acos(cos_n);
            }
        }

        var hcos_minus = -1.0;
        var hcos_plus = -1.0;
        for (var j: u32 = 0u; j < n_steps; j = j + 1u) {
            let r_j = radius_px * (f32(j) + 1.0) / f32(n_steps);

            let off_plus = vec2<i32>(vec2<f32>(gtao_round(dir2.x * r_j), gtao_round(dir2.y * r_j)));
            let c_plus = clamp(c + off_plus, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
            let s_plus = gtao_view_pos(depth, c_plus, dims_i, tan_half_fov, aspect, near, far);
            let d_plus = s_plus - p_c;
            let len_plus = length(d_plus);
            if len_plus > 1e-5 && len_plus <= radius {
                hcos_plus = max(hcos_plus, dot(d_plus / len_plus, view_vec));
            }

            let off_minus = vec2<i32>(vec2<f32>(gtao_round(-dir2.x * r_j), gtao_round(-dir2.y * r_j)));
            let c_minus = clamp(c + off_minus, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
            let s_minus = gtao_view_pos(depth, c_minus, dims_i, tan_half_fov, aspect, near, far);
            let d_minus = s_minus - p_c;
            let len_minus = length(d_minus);
            if len_minus > 1e-5 && len_minus <= radius {
                hcos_minus = max(hcos_minus, dot(d_minus / len_minus, view_vec));
            }
        }

        var h1 = -acos(clamp(hcos_minus, -1.0, 1.0));
        var h2 = acos(clamp(hcos_plus, -1.0, 1.0));
        h1 = n_signed + max(h1 - n_signed, -GTAO_HALF_PI);
        h2 = n_signed + min(h2 - n_signed, GTAO_HALF_PI);

        let arc = gtao_integrate_arc(h1, n_signed) + gtao_integrate_arc(h2, n_signed);
        visibility_sum = visibility_sum + proj_len * arc;
    }

    let visibility = clamp(visibility_sum / f32(n_slices), 0.0, 1.0);
    let ao = clamp(1.0 - intensity * (1.0 - visibility), 0.0, 1.0);
    return vec4<f32>(ao, ao, ao, 1.0);
}
