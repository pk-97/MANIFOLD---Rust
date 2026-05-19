// node.render_lines — instanced capsule line renderer over an
// Array<LinePoint>. Phase C of BUFFER_PORT_PLAN.
//
// Each segment connects point[i] to point[(i+1) % N] for closed
// loops, or point[i] to point[i+1] (0 <= i < N-1) for open
// strokes. One instance per segment; six vertices per instance
// form a capsule quad with round caps. Fragment shader evaluates
// a capsule SDF with fwidth() anti-aliasing.

struct LineUniforms {
    rt_width: f32,
    rt_height: f32,
    half_thickness: f32,
    closed_loop: u32,
    color: vec4<f32>,
    num_points: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

struct LinePoint {
    xy: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: LineUniforms;
@group(0) @binding(1) var<storage, read> points: array<LinePoint>;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) line_len: f32,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> VsOut {
    let n = u.num_points;
    let ia = iid;
    var ib: u32 = ia + 1u;
    if u.closed_loop != 0u {
        ib = (ia + 1u) % n;
    }
    let a = points[ia].xy;
    let b = points[ib].xy;

    let dx = (b.x - a.x) * u.rt_width;
    let dy = (b.y - a.y) * u.rt_height;
    let len = sqrt(dx * dx + dy * dy);

    var perp: vec2<f32>;
    var dir: vec2<f32>;
    var line_len: f32;
    if len < 0.001 {
        perp = vec2<f32>(u.half_thickness / u.rt_width, 0.0);
        dir = vec2<f32>(0.0, u.half_thickness / u.rt_height);
        line_len = 0.0;
    } else {
        let inv = 1.0 / len;
        perp = vec2<f32>(
            -dy * inv * u.half_thickness / u.rt_width,
             dx * inv * u.half_thickness / u.rt_height,
        );
        dir = vec2<f32>(
            dx * inv * u.half_thickness / u.rt_width,
            dy * inv * u.half_thickness / u.rt_height,
        );
        line_len = len / u.half_thickness;
    }

    var pos: vec2<f32>;
    var uv_out: vec2<f32>;
    switch vid {
        case 0u: {
            pos = a - perp - dir;
            uv_out = vec2<f32>(-1.0, -1.0);
        }
        case 1u: {
            pos = a + perp - dir;
            uv_out = vec2<f32>(1.0, -1.0);
        }
        case 2u: {
            pos = b + perp + dir;
            uv_out = vec2<f32>(1.0, line_len + 1.0);
        }
        case 3u: {
            pos = a - perp - dir;
            uv_out = vec2<f32>(-1.0, -1.0);
        }
        case 4u: {
            pos = b + perp + dir;
            uv_out = vec2<f32>(1.0, line_len + 1.0);
        }
        case 5u: {
            pos = b - perp + dir;
            uv_out = vec2<f32>(-1.0, line_len + 1.0);
        }
        default: {
            pos = a;
            uv_out = vec2<f32>(0.0, 0.0);
        }
    }

    let ndc = pos * 2.0 - 1.0;

    var out: VsOut;
    out.position = vec4<f32>(ndc.x, ndc.y, 0.0, 1.0);
    out.uv = uv_out;
    out.line_len = line_len;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let t = clamp(in.uv.y, 0.0, in.line_len);
    let d = length(vec2<f32>(in.uv.x, in.uv.y - t));
    let fw = fwidth(d);
    let aa = 1.0 - smoothstep(1.0 - fw, 1.0, d);
    return vec4<f32>(u.color.rgb * aa, u.color.a * aa);
}
