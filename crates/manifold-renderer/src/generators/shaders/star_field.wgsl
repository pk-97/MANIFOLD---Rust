// Star Field — Cinematic 3D parallax star field with procedural nebulosity
//
// Adapted from BlackHole's star_layer() with:
//   - Virtual camera (pan/tilt via drift)
//   - 4 depth layers on separate spheres for parallax
//   - Per-star twinkle via hash-based phase offset
//   - Configurable nebula backdrop

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
    nebula_amt: f32,
    glow: f32,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output: texture_storage_2d<rgba16float, write>;

// ── Hash functions ──

fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn noise2d(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u2 = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(hash21(i), hash21(i + vec2<f32>(1.0, 0.0)), u2.x),
        mix(hash21(i + vec2<f32>(0.0, 1.0)), hash21(i + vec2<f32>(1.0, 1.0)), u2.x),
        u2.y,
    );
}

fn fbm(p: vec2<f32>) -> f32 {
    var val = noise2d(p) * 0.5;
    val += noise2d(p * 2.03 + vec2<f32>(1.7, -1.3)) * 0.25;
    val += noise2d(p * 4.07 + vec2<f32>(3.4, -2.6)) * 0.125;
    return val;
}

// ── Camera ──

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

// ── Procedural star layer ──
//
// Each layer places stars on a celestial sphere at a different scale.
// The `sphere_offset` parameter shifts the camera origin per layer,
// creating parallax when the camera drifts.

fn star_layer(
    dir: vec3<f32>, scale: f32, threshold: f32,
    intensity_mult: f32, seed: f32,
) -> vec3<f32> {
    let theta = acos(clamp(dir.y, -1.0, 1.0));
    let phi = atan2(dir.z, dir.x) + 3.14159265;
    let uv = vec2<f32>(phi * scale * 0.15915, theta * scale * 0.31831);
    let cell = floor(uv);
    let f = fract(uv);

    // Density adjustment — shift threshold based on user param
    // density=0 -> threshold+0.12 (sparser), density=1 -> threshold-0.06 (denser)
    let adj_threshold = threshold + 0.12 - u.density * 0.18;

    var light = vec3<f32>(0.0);

    for (var j = -1; j <= 1; j++) {
        for (var i = -1; i <= 1; i++) {
            let neighbor = cell + vec2<f32>(f32(i), f32(j));
            let h = hash21(neighbor + seed);
            if h > adj_threshold {
                let sx = hash21(neighbor * 1.273 + seed + 7.0);
                let sy = hash21(neighbor * 2.178 + seed + 13.0);
                let d = f - vec2<f32>(f32(i), f32(j)) - vec2<f32>(sx, sy);
                let dist2 = dot(d, d);

                // Power-law brightness — many faint, few bright
                let norm_bright = (h - adj_threshold) / (1.0 - adj_threshold);
                let star_intensity = pow(norm_bright, 1.5) * intensity_mult;

                // Core + halo (glow param scales halo width)
                let core = exp(-dist2 * 6000.0);
                let halo_width = 800.0 / max(0.3 + u.glow * 2.0, 0.3);
                let halo = exp(-dist2 * halo_width)
                    * norm_bright * norm_bright * 0.06 * (0.5 + u.glow * 1.5);

                // Spectral color with warmth bias
                let temp = hash21(neighbor * 3.46 + seed + 27.0) + u.warmth * 0.2;
                var star_col: vec3<f32>;
                if temp > 0.82 {
                    star_col = vec3<f32>(0.88, 0.92, 1.15); // O/B blue
                } else if temp > 0.55 {
                    star_col = vec3<f32>(0.97, 0.98, 1.05); // A/F white
                } else if temp > 0.25 {
                    star_col = vec3<f32>(1.0, 0.97, 0.93);  // G solar
                } else {
                    star_col = vec3<f32>(1.05, 0.92, 0.82); // K/M warm
                }

                // Per-star twinkle — slow sinusoidal flicker
                let phase = hash21(neighbor * 5.13 + seed + 41.0) * 6.28318;
                let freq = 0.4 + hash21(neighbor * 7.91 + seed + 53.0) * 0.6;
                let twinkle_val = 1.0 - u.twinkle * 0.4
                    * (0.5 + 0.5 * sin(u.time_val * freq + phase));

                light += star_col * star_intensity * (core + halo) * twinkle_val;
            }
        }
    }
    return light;
}

// ── Nebulosity ──

fn nebula(dir: vec3<f32>) -> vec3<f32> {
    let n1 = fbm(dir.xz * 1.5 + dir.y * 0.5);
    let n2 = noise2d(dir.xz * 3.0 + vec2<f32>(10.0, 20.0));
    let density = max(n1 * 0.7 + n2 * 0.3 - 0.35, 0.0);

    let tint = noise2d(dir.xz * 0.8 + vec2<f32>(50.0, 60.0));
    let warm = vec3<f32>(0.15, 0.06, 0.03);
    let cool = vec3<f32>(0.04, 0.06, 0.12);
    let base = mix(cool, warm, tint + u.warmth * 0.3);
    return base * density;
}

// ── Main ──

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if gid.x >= dims.x || gid.y >= dims.y { return; }

    // NDC → view ray
    let uv = vec2<f32>(
        (f32(gid.x) + 0.5) / f32(dims.x) * 2.0 - 1.0,
        (1.0 - (f32(gid.y) + 0.5) / f32(dims.y)) * 2.0 - 1.0,
    );

    // FOV ~60° (tan(30°) ≈ 0.577)
    let raw_dir = normalize(vec3<f32>(uv.x * u.aspect_ratio * 0.577, uv.y * 0.577, 1.0));

    // Camera drift — slow rotation over time
    let drift_t = u.time_val * u.drift_speed * 0.1;
    let cam_rot = rotation_y(drift_t * u.drift_x) * rotation_x(drift_t * u.drift_y);

    // Accumulate star layers with parallax offsets
    var color = vec3<f32>(0.0);

    // Each layer gets a slightly different camera rotation (parallax)
    // Near layers (bright, sparse) shift more than far layers (faint, dense)
    let depth_scale = u.depth * 0.15;

    // Layer 1: bright foreground stars — most parallax
    let dir1 = normalize(cam_rot * rotation_y(drift_t * 0.4 * depth_scale) * raw_dir);
    color += star_layer(dir1, 20.0, 0.82, 3.0, 0.0);

    // Layer 2: medium stars
    let dir2 = normalize(cam_rot * rotation_y(drift_t * 0.2 * depth_scale) * raw_dir);
    color += star_layer(dir2, 50.0, 0.80, 1.2, 100.0);

    // Layer 3: dense background
    let dir3 = normalize(cam_rot * rotation_y(drift_t * 0.08 * depth_scale) * raw_dir);
    color += star_layer(dir3, 100.0, 0.84, 0.5, 200.0);

    // Layer 4: faint dust — minimal parallax (most distant)
    let dir4 = normalize(cam_rot * raw_dir);
    color += star_layer(dir4, 180.0, 0.88, 0.15, 300.0);

    // Nebulosity behind everything — uses base camera direction
    let neb_dir = normalize(cam_rot * raw_dir);
    color += nebula(neb_dir) * u.nebula_amt;

    color *= u.brightness;

    textureStore(output, vec2<i32>(gid.xy), vec4<f32>(color, 1.0));
}
