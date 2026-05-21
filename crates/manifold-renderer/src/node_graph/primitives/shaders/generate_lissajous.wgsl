// node.generate_lissajous — emit an Array<LinePoint> sampled from
// the Lissajous curve `x(t) = sin(freq_x * t + phase)`, `y(t) =
// sin(freq_y * t)` at `vertex_count` points evenly spaced over
// `t ∈ [0, 2π]`. Output is in pre-aspect curve space centered at
// origin — `node.render_lines` applies the aspect correction +
// center offset on its way to the framebuffer.
//
// Always-interpolate: when `freq_x` or `freq_y` is non-integer the
// curve doesn't close (start ≠ end), producing a scribble that
// fills the box. We blend between the floor and ceil integer
// Lissajous curves so non-integer parameters produce a smoothly-
// morphing sequence of clean closed shapes. Matches the legacy
// LissajousGenerator's per-vertex math bit-for-bit.

struct LissajousUniforms {
    active_count: u32,
    capacity: u32,
    _pad0: u32,
    _pad1: u32,
    freq_x: f32,
    freq_y: f32,
    phase: f32,
    scale: f32,
};

struct LinePoint {
    xy: vec2<f32>,
};

const TWO_PI: f32 = 6.28318530717958647692;

// Matches LissajousGenerator's use of generator_math::PROJ_SCALE.
// Bakes the same 0.25 multiplier into curve output so the visible
// curve fills the inner 50% of the screen at `scale = 1.0`.
const PROJ_SCALE: f32 = 0.25;

@group(0) @binding(0) var<uniform> params: LissajousUniforms;
@group(0) @binding(1) var<storage, read_write> points: array<LinePoint>;

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.capacity {
        return;
    }
    if i >= params.active_count {
        // Buffer slot past the active range. Park at origin so the
        // line renderer's closed-loop wrap doesn't draw a stray
        // segment back to a random previous-frame value.
        points[i].xy = vec2<f32>(0.0, 0.0);
        return;
    }

    let t = f32(i) / f32(max(params.active_count, 1u)) * TWO_PI;

    // Floor/ceil interpolation: blend between the curves at the
    // bracketing integer ratios so non-integer freq_x/freq_y don't
    // produce non-closing scribbles. Matches the legacy generator
    // line-for-line (Unity LissajousGenerator lines 84-110).
    let a_lo = floor(params.freq_x);
    let a_hi = ceil(params.freq_x);
    let a_lerp = params.freq_x - a_lo;

    let b_lo = floor(params.freq_y);
    let b_hi = ceil(params.freq_y);
    let b_lerp = params.freq_y - b_lo;

    let x_lo = sin(a_lo * t + params.phase);
    let x_hi = sin(a_hi * t + params.phase);
    let x = x_lo + (x_hi - x_lo) * a_lerp;

    let y_lo = sin(b_lo * t);
    let y_hi = sin(b_hi * t);
    let y = y_lo + (y_hi - y_lo) * b_lerp;

    points[i].xy = vec2<f32>(
        x * params.scale * PROJ_SCALE,
        y * params.scale * PROJ_SCALE,
    );
}
