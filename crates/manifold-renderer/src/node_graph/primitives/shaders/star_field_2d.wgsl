// node.star_field_2d — cinematic 3D parallax star field, with a
// secondary compute pass that materialises per-star screen positions
// into an Array<CurvePoint> for downstream composition (constellation
// lines, particle seeding, audio-reactive modulation, etc.).
//
// Entry points:
//   cs_render     — bit-exact port of the legacy star_field.wgsl
//                   cs_main: NDC → view-ray → 4 parallax layers
//                   summed per pixel. Produces the cinematic texture.
//   cs_materialize — fixed-slot fill of `stars: array<vec2<f32>>` with
//                   each layer × cell either (a) the star's NDC position
//                   when the cell passes the density threshold, or (b)
//                   a sentinel (-99.0, -99.0). Same hash, same cell
//                   layout, same camera drift as cs_render — the two
//                   passes agree on which cells contain stars and where
//                   they appear on screen.
//
// Layer config is hardcoded to match the legacy generator's 4-layer
// scheme. Scale, threshold, intensity_mult, seed, and depth multiplier
// are baked in.

struct Uniforms {
    time_val: f32,
    aspect_ratio: f32,
    density: f32,
    brightness: f32,
    depth: f32,
    drift_speed: f32,
    drift_x: f32,
    drift_y: f32,
    twinkle: f32,
    warmth: f32,
    glow: f32,
    _pad: vec3<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<storage, read_write> stars: array<vec2<f32>>;

// ── Hash function (shared) ──

fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

// ── Camera rotations (shared) ──

fn rotation_y(angle: f32) -> mat3x3<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return mat3x3<f32>(
        vec3<f32>(c, 0.0, s),
        vec3<f32>(0.0, 1.0, 0.0),
        vec3<f32>(-s, 0.0, c),
    );
}

fn rotation_x(angle: f32) -> mat3x3<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return mat3x3<f32>(
        vec3<f32>(1.0, 0.0, 0.0),
        vec3<f32>(0.0, c, -s),
        vec3<f32>(0.0, s, c),
    );
}

// ── Procedural star layer (render path) ──
//
// Bit-exact port of legacy star_field.wgsl::star_layer.

fn star_layer(
    dir: vec3<f32>, scale: f32, threshold: f32,
    intensity_mult: f32, seed: f32,
) -> vec3<f32> {
    let theta = acos(clamp(dir.y, -1.0, 1.0));
    let phi = atan2(dir.z, dir.x) + 3.14159265;
    let uv = vec2<f32>(phi * scale * 0.15915, theta * scale * 0.31831);
    let cell = floor(uv);
    let f = fract(uv);

    let adj_threshold = threshold + 0.12 - u.density * 0.27;

    var light = vec3<f32>(0.0);

    for (var j = -1; j <= 1; j++) {
        for (var i = -1; i <= 1; i++) {
            let neighbor = cell + vec2<f32>(f32(i), f32(j));
            let h = hash21(neighbor + seed);
            if h > adj_threshold {
                let sx = hash21(neighbor * 1.273 + seed + 7.0);
                let sy = hash21(neighbor * 2.178 + seed + 13.0);
                let d_raw = f - vec2<f32>(f32(i), f32(j)) - vec2<f32>(sx, sy);
                let d = vec2<f32>(d_raw.x * 2.0, d_raw.y);
                let dist2 = dot(d, d);

                let norm_bright = (h - adj_threshold) / (1.0 - adj_threshold);
                let star_intensity = pow(norm_bright, 2.5) * intensity_mult;

                let s2 = scale * scale;
                let core = exp(-dist2 * 8.0 * s2);
                let halo_falloff = 2.0 * s2 / max(0.3 + u.glow * 2.0, 0.3);
                let halo = exp(-dist2 * halo_falloff)
                    * norm_bright * norm_bright * 0.04 * (0.3 + u.glow);

                let temp = hash21(neighbor * 3.46 + seed + 27.0) + u.warmth * 0.2;
                var star_col: vec3<f32>;
                if temp > 0.82 {
                    star_col = vec3<f32>(0.88, 0.92, 1.15);
                } else if temp > 0.55 {
                    star_col = vec3<f32>(0.97, 0.98, 1.05);
                } else if temp > 0.25 {
                    star_col = vec3<f32>(1.0, 0.97, 0.93);
                } else {
                    star_col = vec3<f32>(1.05, 0.92, 0.82);
                }

                let phase = hash21(neighbor * 5.13 + seed + 41.0) * 6.28318;
                let freq = 1.5 + hash21(neighbor * 7.91 + seed + 53.0) * 3.5;
                let phase2 = hash21(neighbor * 9.37 + seed + 67.0) * 6.28318;
                let flicker = 0.5 + 0.3 * sin(u.time_val * freq + phase)
                    + 0.2 * sin(u.time_val * freq * 2.7 + phase2);
                let twinkle_val = mix(1.0, flicker, u.twinkle);

                light += star_col * star_intensity * (core + halo) * twinkle_val;
            }
        }
    }
    return light;
}

// ── Render entry point ──

@compute @workgroup_size(16, 16)
fn cs_render(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if gid.x >= dims.x || gid.y >= dims.y { return; }

    let uv = vec2<f32>(
        (f32(gid.x) + 0.5) / f32(dims.x) * 2.0 - 1.0,
        (1.0 - (f32(gid.y) + 0.5) / f32(dims.y)) * 2.0 - 1.0,
    );

    let raw_dir = normalize(vec3<f32>(uv.x * u.aspect_ratio * 0.577, uv.y * 0.577, 1.0));

    let drift_t = u.time_val * u.drift_speed * 0.1;
    let cam_rot = rotation_y(drift_t * u.drift_x) * rotation_x(drift_t * u.drift_y);

    var color = vec3<f32>(0.0);
    let depth_scale = u.depth * 0.15;

    let dir1 = normalize(cam_rot * rotation_y(drift_t * 0.4 * depth_scale) * raw_dir);
    color += star_layer(dir1, 40.0, 0.82, 1.5, 0.0);

    let dir2 = normalize(cam_rot * rotation_y(drift_t * 0.2 * depth_scale) * raw_dir);
    color += star_layer(dir2, 50.0, 0.80, 1.2, 100.0);

    let dir3 = normalize(cam_rot * rotation_y(drift_t * 0.08 * depth_scale) * raw_dir);
    color += star_layer(dir3, 100.0, 0.84, 0.5, 200.0);

    let dir4 = normalize(cam_rot * raw_dir);
    color += star_layer(dir4, 180.0, 0.88, 0.15, 300.0);

    color *= u.brightness;

    textureStore(output, vec2<i32>(gid.xy), vec4<f32>(color, 1.0));
}

// ── Materialise entry point ──
//
// Iterates every (layer, cell) combination across the four layers and
// emits the corresponding star's screen NDC into the `stars` buffer.
// Cells that fail the density threshold write a sentinel position.
//
// Slot layout (tight per-layer):
//   layer 0 (scale 40):  slots [0, 1600)
//   layer 1 (scale 50):  slots [1600, 4100)
//   layer 2 (scale 100): slots [4100, 14100)
//   layer 3 (scale 180): slots [14100, 46500)
// Total = 46500 slots. Dispatch is 4 × max_scale² = 4 × 180² threads,
// with out-of-range cells exiting early.
//
// Sentinel = (-99.0, -99.0). Downstream consumers filter on x < -1.5
// (well outside the [-1, 1] NDC range a real star can occupy).

const STAR_SENTINEL: vec2<f32> = vec2<f32>(-99.0, -99.0);
const LAYER_COUNT: u32 = 4u;
const TOTAL_STARS: u32 = 46500u;

// Per-layer configs. Index by layer_idx ∈ [0, 4).
fn layer_scale(idx: u32) -> f32 {
    if idx == 0u { return 40.0; }
    if idx == 1u { return 50.0; }
    if idx == 2u { return 100.0; }
    return 180.0;
}

fn layer_threshold(idx: u32) -> f32 {
    if idx == 0u { return 0.82; }
    if idx == 1u { return 0.80; }
    if idx == 2u { return 0.84; }
    return 0.88;
}

fn layer_seed(idx: u32) -> f32 {
    if idx == 0u { return 0.0; }
    if idx == 1u { return 100.0; }
    if idx == 2u { return 200.0; }
    return 300.0;
}

fn layer_drift_mult(idx: u32) -> f32 {
    // Layer-specific extra parallax rotation around Y. Layer 0 is the
    // foreground (largest mult); layer 3 is the most distant (zero).
    if idx == 0u { return 0.4; }
    if idx == 1u { return 0.2; }
    if idx == 2u { return 0.08; }
    return 0.0;
}

fn layer_offset(idx: u32) -> u32 {
    if idx == 0u { return 0u; }
    if idx == 1u { return 1600u; }
    if idx == 2u { return 4100u; }
    return 14100u;
}

// Transpose of rotation_y — same as rotation_y(-angle).
fn rotation_y_t(angle: f32) -> mat3x3<f32> {
    return rotation_y(-angle);
}

fn rotation_x_t(angle: f32) -> mat3x3<f32> {
    return rotation_x(-angle);
}

@compute @workgroup_size(8, 8, 1)
fn cs_materialize(@builtin(global_invocation_id) gid: vec3<u32>) {
    let layer_idx = gid.z;
    if layer_idx >= LAYER_COUNT { return; }

    let scale = layer_scale(layer_idx);
    let scale_u = u32(scale);
    if gid.x >= scale_u || gid.y >= scale_u { return; }

    let slot = layer_offset(layer_idx) + gid.y * scale_u + gid.x;
    if slot >= TOTAL_STARS { return; }

    let cell = vec2<f32>(f32(gid.x), f32(gid.y));
    let seed = layer_seed(layer_idx);
    let h = hash21(cell + seed);
    let adj_threshold = layer_threshold(layer_idx) + 0.12 - u.density * 0.27;

    if h <= adj_threshold {
        stars[slot] = STAR_SENTINEL;
        return;
    }

    // Sub-cell jitter — same hashes as the render path.
    let sx = hash21(cell * 1.273 + seed + 7.0);
    let sy = hash21(cell * 2.178 + seed + 13.0);
    let cell_uv = cell + vec2<f32>(sx, sy);

    // Inverse of the render's uv mapping:
    //   uv.x = phi   * scale * 0.15915  →  phi   = uv.x / (scale * 0.15915)
    //   uv.y = theta * scale * 0.31831  →  theta = uv.y / (scale * 0.31831)
    let phi = cell_uv.x / (scale * 0.15915);
    let theta = cell_uv.y / (scale * 0.31831);

    // Spherical → world direction. The render maps with
    //   phi = atan2(dir.z, dir.x) + π
    //   theta = acos(dir.y)
    // so the inverse is:
    let world_dir = vec3<f32>(
        sin(theta) * cos(phi - 3.14159265),
        cos(theta),
        sin(theta) * sin(phi - 3.14159265),
    );

    // Inverse of the render's rotation chain:
    //   world_dir = cam_rot * layer_rot * raw_dir
    //   →  raw_dir = layer_rot^T * cam_rot^T * world_dir
    let drift_t = u.time_val * u.drift_speed * 0.1;
    let depth_scale = u.depth * 0.15;
    let layer_rot_t = rotation_y_t(drift_t * layer_drift_mult(layer_idx) * depth_scale);
    let cam_rot_t = rotation_x_t(drift_t * u.drift_y) * rotation_y_t(drift_t * u.drift_x);

    let raw_dir = layer_rot_t * cam_rot_t * world_dir;

    // Inverse FOV projection. The render constructs raw_dir from
    //   raw_dir = normalize(vec3(uv.x * aspect * 0.577, uv.y * 0.577, 1.0))
    // so for a unit raw_dir with z > 0:
    //   uv.x = (raw_dir.x / raw_dir.z) / (aspect * 0.577)
    //   uv.y = (raw_dir.y / raw_dir.z) / 0.577
    // Stars behind the camera (raw_dir.z <= 0) write sentinel.
    if raw_dir.z <= 0.001 {
        stars[slot] = STAR_SENTINEL;
        return;
    }

    let ndc_x = (raw_dir.x / raw_dir.z) / (u.aspect_ratio * 0.577);
    let ndc_y = (raw_dir.y / raw_dir.z) / 0.577;

    // Clip to a reasonable bound so degenerate near-pole projections
    // don't escape into infinity. Anything outside this would be off
    // screen anyway.
    if abs(ndc_x) > 5.0 || abs(ndc_y) > 5.0 {
        stars[slot] = STAR_SENTINEL;
        return;
    }

    stars[slot] = vec2<f32>(ndc_x, ndc_y);
}
