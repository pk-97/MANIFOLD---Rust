// node.render_lines — instanced capsule renderer for an
// Array<CurvePoint>. One instance per edge or dot; the vertex
// shader expands 6 vertices into a screen-space quad. The fragment
// shader evaluates a capsule SDF with fwidth() AA.
//
// Input positions are in pre-aspect curve space centred at the
// origin (the natural output of `node.pack_curve_xy` and the
// other curve-emitting primitives); this shader applies the aspect
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
    // 1 when the optional `widths` storage buffer carries real
    // per-point thickness multipliers; 0 when the host bound a
    // dummy buffer (widths port unwired) and every point is 1.0.
    has_widths: u32,
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
// Per-point thickness multipliers, parallel to `points`. Only read
// when `u.has_widths == 1`; otherwise a 1-element dummy is bound.
@group(0) @binding(3) var<storage, read> widths: array<f32>;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) line_len: f32,
    @location(2) alpha: f32,
    // Per-endpoint width multipliers (constant across the instance;
    // linear interpolation of a constant stays constant).
    @location(3) width_a: f32,
    @location(4) width_b: f32,
};

// Width multiplier for point index `i`: 1.0 unless the widths
// buffer is wired. Out-of-range indices clamp to the last entry so
// a short buffer degrades gracefully instead of faulting.
fn point_width(i: u32) -> f32 {
    if u.has_widths == 0u {
        return 1.0;
    }
    let n = arrayLength(&widths);
    let idx = min(i, n - 1u);
    return max(widths[idx], 0.0);
}

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

    // Per-endpoint width multipliers (tapered capsule). Without a
    // wired widths buffer both are 1.0 and the math below reduces
    // exactly to the pre-taper geometry. `w_max` sizes the quad so
    // the fat end is fully covered; the fragment SDF carves the taper.
    let w_a = point_width(edge.a);
    let w_b = point_width(edge.b);
    let w_max = max(w_a, w_b);

    let dx = (b.x - a.x) * u.rt_width;
    let dy = (b.y - a.y) * u.rt_height;
    let len = sqrt(dx * dx + dy * dy);

    var perp: vec2<f32>;
    var dir: vec2<f32>;
    var line_len: f32;
    if len < 0.001 {
        // Degenerate (dot): axis-aligned square quad. For a dot
        // a == b so w_max == w_a; `dir` stays unit-radius here and
        // picks up the w_a scale at the corner offsets below.
        perp = vec2<f32>(w_max * half_thick / u.rt_width, 0.0);
        dir = vec2<f32>(0.0, half_thick / u.rt_height);
        line_len = 0.0;
    } else {
        let inv = 1.0 / len;
        perp = vec2<f32>(
            -dy * inv * w_max * half_thick / u.rt_width,
             dx * inv * w_max * half_thick / u.rt_height,
        );
        // Unit-length (in half_thick units) cap-extension direction;
        // scaled per end below so each cap extends by its own radius.
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
            pos = a - perp - dir * w_a;
            uv_out = vec2<f32>(-w_max, -w_a);
        }
        case 1u: {
            pos = a + perp - dir * w_a;
            uv_out = vec2<f32>(w_max, -w_a);
        }
        case 2u: {
            pos = b + perp + dir * w_b;
            uv_out = vec2<f32>(w_max, line_len + w_b);
        }
        case 3u: {
            pos = a - perp - dir * w_a;
            uv_out = vec2<f32>(-w_max, -w_a);
        }
        case 4u: {
            pos = b + perp + dir * w_b;
            uv_out = vec2<f32>(w_max, line_len + w_b);
        }
        case 5u: {
            pos = b - perp + dir * w_b;
            uv_out = vec2<f32>(-w_max, line_len + w_b);
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
    out.width_a = w_a;
    out.width_b = w_b;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Tapered capsule SDF: line segment from y=0 to y=line_len with
    // radius interpolating width_a → width_b (both 1.0 when the
    // widths port is unwired, recovering the plain capsule). For
    // dots (line_len=0) this degenerates to a circle of radius
    // width_a.
    let t = clamp(in.uv.y, 0.0, in.line_len);
    let d = length(vec2<f32>(in.uv.x, in.uv.y - t));
    let r = mix(in.width_a, in.width_b, t / max(in.line_len, 1e-4));

    // fwidth-based AA: ~1 pixel soft edge at any thickness. Radii
    // below the AA footprint fade out rather than alias — hairline
    // branch tips vanish smoothly.
    let fw = fwidth(d);
    let aa = 1.0 - smoothstep(r - fw, r, d);

    // Beat flash — matches legacy generator_lines.wgsl. Setting
    // `beat_flash_amount = 0` skips the boost (smoothstep still
    // evaluates but contributes zero), which the editor can do for
    // a non-pulsing line render.
    let beat_frac = fract(u.beat);
    let flash = smoothstep(0.1, 0.0, beat_frac) * u.beat_flash_amount;
    let lum = clamp(aa + flash * aa, 0.0, 1.0) * in.alpha;

    return vec4<f32>(u.color.rgb * lum, u.color.a * lum);
}
