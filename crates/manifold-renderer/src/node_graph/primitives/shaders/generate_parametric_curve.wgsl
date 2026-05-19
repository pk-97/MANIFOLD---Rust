// node.generate_parametric_curve — emit an Array<LinePoint>
// sampled from a parametric curve. Phase C of BUFFER_PORT_PLAN.
//
// Curves supported (param `curve_type`):
//   0 = Lissajous:     x = sin(freq_x * t + phase), y = sin(freq_y * t)
//   1 = Hypocycloid:   classic gear-trace
//   2 = Rose:          r = cos(k * t)
//   3 = Circle:        x = cos(t), y = sin(t)
//
// Output is in screen space [0, 1], centered at (0.5, 0.5) and
// scaled by `scale`.

const CURVE_LISSAJOUS: u32 = 0u;
const CURVE_HYPOCYCLOID: u32 = 1u;
const CURVE_ROSE: u32 = 2u;
const CURVE_CIRCLE: u32 = 3u;

struct CurveUniforms {
    active_count: u32,
    capacity: u32,
    curve_type: u32,
    _pad0: u32,
    freq_x: f32,
    freq_y: f32,
    phase: f32,
    scale: f32,
};

struct LinePoint {
    xy: vec2<f32>,
};

@group(0) @binding(0) var<uniform> params: CurveUniforms;
@group(0) @binding(1) var<storage, read_write> points: array<LinePoint>;

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.capacity {
        return;
    }
    if i >= params.active_count {
        points[i].xy = vec2<f32>(0.5, 0.5);
        return;
    }

    let two_pi = 6.28318530718;
    let t = f32(i) / f32(max(params.active_count, 1u)) * two_pi;

    var x: f32;
    var y: f32;
    if params.curve_type == CURVE_LISSAJOUS {
        x = sin(params.freq_x * t + params.phase);
        y = sin(params.freq_y * t);
    } else if params.curve_type == CURVE_HYPOCYCLOID {
        let k = max(params.freq_x, 1.0);
        let big_r = 1.0;
        let little_r = big_r / k;
        let diff = big_r - little_r;
        x = diff * cos(t) + little_r * cos(diff / little_r * t);
        y = diff * sin(t) - little_r * sin(diff / little_r * t);
    } else if params.curve_type == CURVE_ROSE {
        let k = max(params.freq_x, 1.0);
        let r = cos(k * t);
        x = r * cos(t);
        y = r * sin(t);
    } else {
        x = cos(t);
        y = sin(t);
    }

    points[i].xy = vec2<f32>(
        0.5 + x * params.scale * 0.5,
        0.5 + y * params.scale * 0.5,
    );
}
