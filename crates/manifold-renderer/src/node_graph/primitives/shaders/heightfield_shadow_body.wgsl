// node.heightfield_shadow — fusable body (freeze §12), Pointwise + GatherTexel.
//
// Screen-space heightfield shadow raymarch (docs/DEPTH_RELIGHT_DESIGN.md D5).
// Ortho heightfield frame IDENTICAL to node.ssao_gtao's Height Field mode
// (shaders/ssao_gtao_body.wgsl's `gtao_height_pos`): position =
// (uv.x*aspect, 1.0-uv.y, (1.0-raw)*relief) — x is aspect-corrected, y is
// FLIPPED (world-up = decreasing v/pixel-row) so the frame is right-handed
// with z toward the viewer. NOT shared source with ssao_gtao_body.wgsl (D5
// requires the CPU reference + hand oracle stay independent), but the
// geometric convention matches bit-for-bit so a `height` texture composed
// alongside a GTAO Height Field pass reads consistently.
//
// Algorithm:
//   1. `light_dir3 = normalize(light_x, light_y, light_z)` — same
//      scene-toward-light convention as node.basic_light's light_x/y/z,
//      expressed in the SAME y-up ortho frame as gtao_height_pos (positive
//      light_y = light toward the top of the screen).
//   2. `xy_len = length(light_dir3.xy)`. If ~0 (light straight up/down —
//      no horizontal component to march along), the ray never leaves the
//      pixel's column: fully lit, out = 1.0.
//   3. `dir2 = light_dir3.xy / xy_len` — the unit horizontal direction
//      toward the light in the ortho frame's (x, y-up) plane.
//      `slope = light_dir3.z / xy_len` — height gained per uv unit of
//      horizontal travel toward the light.
//   4. March `steps` samples at uniform spacing out to `max_dist =
//      relief*2.0` (uv units, isotropic — same radius-to-pixel conversion
//      as ssao_gtao's heightfield mode: `* dims.y` for both axes, since x
//      is already aspect-corrected to the same per-unit pixel density as
//      y). At step i (1-indexed), `t = max_dist * i / steps` (uv units);
//      the sampled PIXEL offset from the center texel is `(round(dir2.x *
//      t*dims.y), round(-dir2.y * t*dims.y))` — x has no sign flip (an
//      aspect-space +x step is a +1 pixel-column step), y IS flipped
//      (an ortho-frame +y step, i.e. toward the top of the screen, is a
//      DECREASING pixel row) — same relationship `gtao_height_pos`'s own
//      +/-1-texel normal-reconstruction offsets satisfy. `textureLoad` the
//      terrain height there (integer texel, clamp-to-edge — no sampler,
//      same GatherTexel convention as ssao_gtao). Ray height at this step:
//      `start_height + t * slope`. Track `max_penetration = max(
//      max_penetration, terrain - ray_height)` across all steps.
//   5. Shadow term: `max_penetration <= 0` → fully lit (`out = 1.0`).
//      Otherwise `occlusion = smoothstep(0, softness*relief + 1e-4,
//      max_penetration) * strength`; `out = clamp(1.0 - occlusion, 0, 1)`
//      (clamped so `strength` in (1,2] can't push the output negative —
//      same defensive clamp as GTAO's visibility term).
//
// Rounding uses the shared explicit round-half-away-from-zero helper (see
// ssao_gtao_body.wgsl's header comment for why WGSL's `round()` builtin —
// round-half-to-even — is avoided: the CPU reference has no matching
// builtin either, so an explicit formula lets both sides agree bit-for-bit
// on tie cases).
//
// PARAMS (declaration order): [light_x, light_y, light_z, steps, strength,
// softness, relief]. No DERIVED_UNIFORMS — this atom needs no Camera.

// Round half away from zero — identical formula to ssao_gtao_body.wgsl's
// `gtao_round`, kept as an independent copy per this file's no-shared-
// source convention (see file header).
fn hfshadow_round(x: f32) -> f32 {
    if x >= 0.0 {
        return floor(x + 0.5);
    }
    return -floor(-x + 0.5);
}

// Terrain height at an integer texel (clamp-to-edge, GatherTexel
// convention) — same `(1.0 - raw) * relief` mapping as
// ssao_gtao_body.wgsl's `gtao_height_pos`.
fn hfshadow_height(height: texture_2d<f32>, c: vec2<i32>, dims_i: vec2<i32>, relief: f32) -> f32 {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    let raw = textureLoad(height, cc, 0).r;
    return (1.0 - raw) * relief;
}

fn body(
    height: texture_2d<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    light_x: f32,
    light_y: f32,
    light_z: f32,
    steps: f32,
    strength: f32,
    softness: f32,
    relief: f32,
) -> vec4<f32> {
    let dims_i = vec2<i32>(dims);
    let n_steps = max(1u, u32(hfshadow_round(steps)));

    let light_len = length(vec3<f32>(light_x, light_y, light_z));
    var light_dir3 = vec3<f32>(0.0, 0.0, 1.0);
    if light_len > 1e-8 {
        light_dir3 = vec3<f32>(light_x, light_y, light_z) / light_len;
    }
    let xy_len = length(light_dir3.xy);

    // Fully lit if the light has no horizontal component to march along.
    if xy_len < 1e-6 {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }

    let dir2 = light_dir3.xy / xy_len;
    let slope = light_dir3.z / xy_len;

    let c = vec2<i32>(uv * dims);
    let start_height = hfshadow_height(height, c, dims_i, relief);

    let max_dist = relief * 2.0;
    let max_dist_px = max_dist * dims.y;

    var max_penetration = 0.0;
    for (var i: u32 = 1u; i <= n_steps; i = i + 1u) {
        let t = max_dist * f32(i) / f32(n_steps);
        let t_px = max_dist_px * f32(i) / f32(n_steps);
        let offset = vec2<i32>(vec2<f32>(hfshadow_round(dir2.x * t_px), hfshadow_round(-dir2.y * t_px)));
        let cs = clamp(c + offset, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
        let terrain = hfshadow_height(height, cs, dims_i, relief);
        let ray_height = start_height + t * slope;
        let penetration = terrain - ray_height;
        max_penetration = max(max_penetration, penetration);
    }

    if max_penetration <= 0.0 {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }

    let occlusion = smoothstep(0.0, softness * relief + 1e-4, max_penetration) * strength;
    let lit = clamp(1.0 - occlusion, 0.0, 1.0);
    return vec4<f32>(lit, lit, lit, 1.0);
}
