// Instanced capsule-line renderer with fwidth() AA and round caps.
// Each instance is one edge (or dot). The vertex shader expands 6 vertices
// per instance into a screen-space quad. The fragment shader evaluates a
// capsule SDF for pixel-perfect anti-aliasing at any thickness.

struct Uniforms {
    rt_width: f32,
    rt_height: f32,
    edge_half_thick: f32,
    beat: f32,
    dot_half_thick: f32,
    num_edges: u32,
    _pad0: f32,
    _pad1: f32,
};

struct EdgeInstance {
    a: u32,
    b: u32,
    alpha_bits: u32,
    _pad: u32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> positions: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read> edges: array<EdgeInstance>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) line_len: f32,
    @location(2) alpha: f32,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> VertexOutput {
    let edge = edges[iid];
    let a = positions[edge.a];
    let b = positions[edge.b];
    let alpha = bitcast<f32>(edge.alpha_bits);

    // Choose thickness: edges vs dots (dots appended after edges)
    var half_thick: f32;
    if (iid < u.num_edges) {
        half_thick = u.edge_half_thick;
    } else {
        half_thick = u.dot_half_thick;
    }

    // Direction in pixel space
    let dx = (b.x - a.x) * u.rt_width;
    let dy = (b.y - a.y) * u.rt_height;
    let len = sqrt(dx * dx + dy * dy);

    var perp: vec2<f32>;
    var dir: vec2<f32>;
    var line_len: f32;

    if (len < 0.001) {
        // Degenerate (dot): axis-aligned square quad
        perp = vec2<f32>(half_thick / u.rt_width, 0.0);
        dir = vec2<f32>(0.0, half_thick / u.rt_height);
        line_len = 0.0;
    } else {
        let inv = 1.0 / len;
        perp = vec2<f32>(
            -dy * inv * half_thick / u.rt_width,
             dx * inv * half_thick / u.rt_height
        );
        dir = vec2<f32>(
            dx * inv * half_thick / u.rt_width,
            dy * inv * half_thick / u.rt_height
        );
        line_len = len / half_thick;
    }

    // Quad with round-cap extension (1 radius past each endpoint).
    //   v0: a - perp - dir   UV(-1, -1)
    //   v1: a + perp - dir   UV( 1, -1)
    //   v2: b + perp + dir   UV( 1, line_len + 1)
    //   v3: b - perp + dir   UV(-1, line_len + 1)
    // Triangles: (v0, v1, v2), (v0, v2, v3)

    var pos: vec2<f32>;
    var uv_out: vec2<f32>;

    switch (vid) {
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

    // [0,1] screen UV -> NDC [-1,1]
    let ndc = pos * 2.0 - 1.0;

    var out: VertexOutput;
    out.position = vec4<f32>(ndc.x, ndc.y, 0.0, 1.0);
    out.uv = uv_out;
    out.line_len = line_len;
    out.alpha = alpha;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Capsule SDF: line segment from y=0 to y=line_len, radius 1.
    // For dots (line_len=0) this degenerates to a circle SDF.
    let t = clamp(in.uv.y, 0.0, in.line_len);
    let d = length(vec2<f32>(in.uv.x, in.uv.y - t));

    // fwidth-based AA: exactly 1 pixel soft edge at any thickness
    let fw = fwidth(d);
    let aa = 1.0 - smoothstep(1.0 - fw, 1.0, d);

    // Beat flash (matches Unity GeneratorLines.shader)
    let beat_frac = fract(u.beat);
    let flash = smoothstep(0.1, 0.0, beat_frac) * 0.4;
    let lum = clamp(aa + flash * aa, 0.0, 1.0) * in.alpha;

    return vec4<f32>(lum, lum, lum, lum);
}
