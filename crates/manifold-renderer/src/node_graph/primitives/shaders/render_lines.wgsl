// node.render_lines — instanced capsule renderer for an
// Array<CurvePoint>. One instance per edge or dot; the vertex
// shader expands 6 vertices into a screen-space quad. The fragment
// shader evaluates a capsule SDF with fwidth() AA.
//
// Input positions are in pre-aspect curve space centred at the
// origin (the natural output of `node.generate_lissajous` and the
// other curve generators); this shader applies the aspect
// correction + centre offset before line-thickness math. That
// matches the legacy `LineGeneratorHelper::prepare_instances` path
// where the generator's projected_x/y values are post-PROJ_SCALE
// but pre-aspect.
//
// Per-instance `EdgeInstance` carries the two endpoint indices
// `a, b` and an `alpha` (encoded as f32 bits in a u32). When
// `a == b` the capsule degenerates to a dot using `dot_thickness`
// rather than `edge_thickness`. Dot instances are appended after
// edge instances; `num_edges` tells the vertex shader which
// thickness to use.
//
// `beat_flash_amount` adds a brief luminance boost at each beat,
// matching the legacy `generator_lines.wgsl` flash for bit-perfect
// parity with the pre-graph line generators. Set to 0 to disable.

struct LineUniforms {
    rt_width: f32,
    rt_height: f32,
    edge_half_thickness: f32,
    dot_half_thickness: f32,
    color: vec4<f32>,
    num_edges: u32,
    beat: f32,
    beat_flash_amount: f32,
    _pad: f32,
};

struct CurvePoint {
    xy: vec2<f32>,
};

struct EdgeInstance {
    a: u32,
    b: u32,
    alpha_bits: u32,
    _pad: u32,
};

@group(0) @binding(0) var<uniform> u: LineUniforms;
@group(0) @binding(1) var<storage, read> points: array<CurvePoint>;
@group(0) @binding(2) var<storage, read> edges: array<EdgeInstance>;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) line_len: f32,
    @location(2) alpha: f32,
};

// Pre-aspect curve coord → screen-space [0, 1]. Aspect is
// `rt_width / rt_height`. The curve is centred at the origin in
// input; we shift to (0.5, 0.5) so the screen centre is its
// natural focal point.
fn curve_to_screen(p: vec2<f32>) -> vec2<f32> {
    let aspect = u.rt_width / u.rt_height;
    return vec2<f32>(p.x / aspect + 0.5, p.y + 0.5);
}

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> VsOut {
    let edge = edges[iid];
    let a = curve_to_screen(points[edge.a].xy);
    let b = curve_to_screen(points[edge.b].xy);
    let alpha = bitcast<f32>(edge.alpha_bits);

    var half_thick: f32;
    if iid < u.num_edges {
        half_thick = u.edge_half_thickness;
    } else {
        half_thick = u.dot_half_thickness;
    }

    let dx = (b.x - a.x) * u.rt_width;
    let dy = (b.y - a.y) * u.rt_height;
    let len = sqrt(dx * dx + dy * dy);

    var perp: vec2<f32>;
    var dir: vec2<f32>;
    var line_len: f32;
    if len < 0.001 {
        // Degenerate (dot): axis-aligned square quad.
        perp = vec2<f32>(half_thick / u.rt_width, 0.0);
        dir = vec2<f32>(0.0, half_thick / u.rt_height);
        line_len = 0.0;
    } else {
        let inv = 1.0 / len;
        perp = vec2<f32>(
            -dy * inv * half_thick / u.rt_width,
             dx * inv * half_thick / u.rt_height,
        );
        dir = vec2<f32>(
            dx * inv * half_thick / u.rt_width,
            dy * inv * half_thick / u.rt_height,
        );
        line_len = len / half_thick;
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
    out.alpha = alpha;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Capsule SDF: line segment from y=0 to y=line_len, radius 1.
    // For dots (line_len=0) this degenerates to a circle SDF.
    let t = clamp(in.uv.y, 0.0, in.line_len);
    let d = length(vec2<f32>(in.uv.x, in.uv.y - t));

    // fwidth-based AA: ~1 pixel soft edge at any thickness.
    let fw = fwidth(d);
    let aa = 1.0 - smoothstep(1.0 - fw, 1.0, d);

    // Beat flash — matches legacy generator_lines.wgsl. Setting
    // `beat_flash_amount = 0` skips the boost (smoothstep still
    // evaluates but contributes zero), which the editor can do for
    // a non-pulsing line render.
    let beat_frac = fract(u.beat);
    let flash = smoothstep(0.1, 0.0, beat_frac) * u.beat_flash_amount;
    let lum = clamp(aa + flash * aa, 0.0, 1.0) * in.alpha;

    return vec4<f32>(u.color.rgb * lum, u.color.a * lum);
}
