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
@group(0) @binding(1) var output: texture_storage_2d<rgba16float, write>;

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

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output);
    if id.x >= dims.x || id.y >= dims.y { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    var p_uv = uv - vec2<f32>(0.5);
    p_uv.x *= u.aspect_ratio;
    p_uv *= u.uv_scale;

    let shape = clamp(i32(floor(u.shape_type)), 0, 5);
    let r = shape_dist(p_uv, shape);

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

    // Approximate fwidth(ring_dist) via finite differences.
    let step_x = vec2<f32>(u.aspect_ratio * u.uv_scale / f32(dims.x), 0.0);
    let step_y = vec2<f32>(0.0, u.uv_scale / f32(dims.y));
    let r_dx = shape_dist(p_uv + step_x, shape);
    let r_dy = shape_dist(p_uv + step_y, shape);
    let rd_dx = abs(fract((r_dx * ring_freq - expansion)) - 0.5) / ring_freq;
    let rd_dy = abs(fract((r_dy * ring_freq - expansion)) - 0.5) / ring_freq;
    let fw = abs(rd_dx - ring_dist) + abs(rd_dy - ring_dist);
    let half_fw = fw * 0.5;

    let half_thick = u.line_thickness * 0.5;
    let ring = 1.0 - smoothstep(half_thick - half_fw, half_thick + half_fw, ring_dist);

    let lum = clamp(ring, 0.0, 1.0);
    textureStore(output, vec2<i32>(id.xy), vec4<f32>(lum, lum, lum, lum));
}
