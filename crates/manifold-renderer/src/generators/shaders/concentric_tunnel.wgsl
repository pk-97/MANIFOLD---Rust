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

const PI: f32 = 3.14159265;
const TWO_PI: f32 = 6.28318530;

// Concentric regular polygon distance metric.
// Level sets of this function form concentric N-gons.
// Matches Unity: atan2 + PI before modular sector computation.
fn poly_dist(p: vec2<f32>, sides: f32) -> f32 {
    let r = length(p);
    if r < 0.0001 {
        return 0.0;
    }
    let a = atan2(p.y, p.x) + PI;
    let seg = TWO_PI / sides;
    let a_mod = a - floor(a / seg) * seg - seg * 0.5;
    return r * cos(a_mod);
}

// Concentric star distance metric.
// Level sets form concentric 5-pointed stars.
// Uses Unity's piecewise-linear formula: lerp(1.0, 0.42, t)
fn star_dist(p: vec2<f32>) -> f32 {
    let r = length(p);
    if r < 0.0001 {
        return 0.0;
    }
    let a = atan2(p.y, p.x) + PI;
    let seg = TWO_PI / 5.0;
    let half_seg = seg * 0.5;
    let sa = a - floor(a / seg) * seg;
    let t = abs(sa - half_seg) / half_seg;
    let star_r = mix(1.0, 0.42, t);
    return r / star_r;
}

// Select distance metric by shape index
fn shape_dist(p: vec2<f32>, shape: i32) -> f32 {
    if shape <= 0 { return length(p); }                    // Circle
    if shape == 1 { return poly_dist(p, 3.0); }           // Triangle
    if shape == 2 { return max(abs(p.x), abs(p.y)); }     // Square
    if shape == 3 { return poly_dist(p, 5.0); }           // Pentagon
    if shape == 4 { return poly_dist(p, 6.0); }           // Hexagon
    return star_dist(p);                                    // Star
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var uv = in.uv - vec2<f32>(0.5);
    uv.x *= u.aspect_ratio;
    uv *= u.uv_scale;

    let shape = clamp(i32(floor(u.shape_type)), 0, 5);
    let r = shape_dist(uv, shape);

    // Ring spacing from beats-per-ring (anim_speed = beatsPerRing)
    let beats_per_ring = max(u.anim_speed, 0.01);
    let ring_freq = 1.0 / beats_per_ring;
    var expansion = u.beat * ring_freq;

    // Spawn mode: when snap_mode > 0.5, add trigger_count to expansion
    if u.snap_mode > 0.5 {
        expansion += u.trigger_count;
    }

    // Concentric rings expanding outward from center
    let pattern = r * ring_freq - expansion;
    let ring_dist = abs(fract(pattern) - 0.5) / ring_freq;

    // Anti-aliased ring edges (single-sided smoothstep, matching Unity)
    let pw = fwidth(ring_dist);
    let half_thick = u.line_thickness * 0.5;
    let ring = 1.0 - smoothstep(half_thick - pw, half_thick + pw, ring_dist);

    let lum = clamp(ring, 0.0, 1.0);
    return vec4<f32>(lum, lum, lum, lum);
}
