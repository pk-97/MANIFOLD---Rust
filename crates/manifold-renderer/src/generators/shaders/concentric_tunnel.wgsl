struct Uniforms {
    time_val: f32,
    beat: f32,
    aspect_ratio: f32,
    line_thickness: f32,
    anim_speed: f32,
    uv_scale: f32,
    shape_type: f32,
    snap_mode: f32,
    trigger_count: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Fullscreen triangle — 3 vertices, no vertex buffer
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// ── Distance metrics for different shapes ──

const PI: f32 = 3.14159265;
const TWO_PI: f32 = 6.28318530;

// Circle: Euclidean distance
fn dist_circle(p: vec2<f32>) -> f32 {
    return length(p);
}

// Regular polygon distance (N sides)
fn poly_dist(p: vec2<f32>, n: f32) -> f32 {
    let angle = atan2(p.y, p.x);
    let sector = TWO_PI / n;
    // Distance to nearest polygon edge
    let r = length(p);
    let theta = ((angle % sector) + sector) % sector;  // mod into [0, sector)
    let half_sector = sector * 0.5;
    return r * cos(theta - half_sector);
}

// Square: Chebyshev distance
fn dist_square(p: vec2<f32>) -> f32 {
    return max(abs(p.x), abs(p.y));
}

// Star distance
fn star_dist(p: vec2<f32>) -> f32 {
    let r = length(p);
    let angle = atan2(p.y, p.x);
    // 5-pointed star modulation
    let star_mod = cos(5.0 * angle) * 0.3 + 0.7;
    return r / star_mod;
}

// Get distance based on shape type
fn shape_distance(p: vec2<f32>, shape: i32) -> f32 {
    switch shape {
        case 1: { return poly_dist(p, 3.0); }    // Triangle
        case 2: { return dist_square(p); }         // Square
        case 3: { return poly_dist(p, 5.0); }     // Pentagon
        case 4: { return poly_dist(p, 6.0); }     // Hexagon
        case 5: { return star_dist(p); }           // Star
        default: { return dist_circle(p); }        // Circle
    }
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Center and aspect-correct UV
    var uv = in.uv - vec2<f32>(0.5);
    uv.x *= u.aspect_ratio;
    uv *= u.uv_scale;

    let shape = i32(u.shape_type);

    // Beat-driven expansion: rings expand from center at beat rate
    // anim_speed is the beat fraction (0.25, 0.5, 1.0, 2.0, 4.0)
    let beat_phase = u.beat * u.anim_speed;
    let expansion = fract(beat_phase);

    // Get distance from center using the selected shape metric
    let d = shape_distance(uv, shape);

    // Create concentric rings expanding outward
    // Ring spacing is proportional to the beat rate
    let ring_spacing = 0.15;
    let shifted_d = d + expansion * ring_spacing;
    let ring_pattern = fract(shifted_d / ring_spacing);

    // Anti-aliased ring edges
    let aa = fwidth(shifted_d / ring_spacing);
    let edge = smoothstep(0.5 - u.line_thickness / ring_spacing - aa,
                          0.5 - u.line_thickness / ring_spacing,
                          ring_pattern)
             - smoothstep(0.5 + u.line_thickness / ring_spacing,
                          0.5 + u.line_thickness / ring_spacing + aa,
                          ring_pattern);

    let lum = edge;
    return vec4<f32>(lum, lum, lum, lum);
}
