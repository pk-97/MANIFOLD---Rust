// Scene gizmo lines, drawn as screen-space quads over a finished picture and
// tested against the scene's own depth. One draw serves the preview, the
// editor viewport and an export; the mode picks what a hidden line does.
//
// `scene_depth` is the scene's raw device depth (reversed-Z: larger is
// nearer, cleared to 0), so a line is hidden when the scene stored a larger
// value than the line's own depth at that pixel.

struct Params {
    view_proj: mat4x4<f32>,
    target_size: vec2<f32>,
    depth_size: vec2<f32>,
    half_width: f32,
    ghost_alpha: f32,
    // 0 occluded, 1 on top, 2 ghost.
    mode: u32,
    _pad: f32,
};

struct Line {
    a: vec4<f32>,
    b: vec4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> lines: array<Line>;
@group(0) @binding(2) var scene_depth: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) edge: f32,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let line = lines[vid / 6u];
    let corner = vid % 6u;
    let ca = p.view_proj * vec4<f32>(line.a.xyz, 1.0);
    let cb = p.view_proj * vec4<f32>(line.b.xyz, 1.0);
    let sa = (ca.xy / ca.w) * 0.5 * p.target_size;
    let sb = (cb.xy / cb.w) * 0.5 * p.target_size;
    var dir = sb - sa;
    let len = length(dir);
    if len > 1e-4 {
        dir = dir / len;
    } else {
        dir = vec2<f32>(1.0, 0.0);
    }
    let normal = vec2<f32>(-dir.y, dir.x);
    // Two triangles: (a-, a+, b-) and (b-, a+, b+).
    let at_b = corner == 2u || corner == 3u || corner == 5u;
    let plus = corner == 1u || corner == 4u || corner == 5u;
    var clip = ca;
    var screen = sa;
    var along = -1.0;
    if at_b {
        clip = cb;
        screen = sb;
        along = 1.0;
    }
    var side = -1.0;
    if plus {
        side = 1.0;
    }
    let moved = screen + (normal * side + dir * along) * p.half_width;
    let ndc = moved * 2.0 / p.target_size;
    var out: VsOut;
    out.pos = vec4<f32>(ndc * clip.w, clip.z, clip.w);
    out.color = line.color;
    out.edge = side;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var alpha = in.color.a * clamp((1.0 - abs(in.edge)) * p.half_width + 0.5, 0.0, 1.0);
    if p.mode != 1u {
        let uv = in.pos.xy / p.target_size;
        let texel = vec2<i32>(clamp(uv * p.depth_size, vec2<f32>(0.0), p.depth_size - vec2<f32>(1.0)));
        let stored = textureLoad(scene_depth, texel, 0).r;
        // A line lying on a surface must not hide itself: allow a small
        // relative distance before the surface counts as in front.
        if stored > in.pos.z * 1.004 + 0.000001 {
            if p.mode == 0u {
                discard;
            }
            alpha = alpha * p.ghost_alpha;
        }
    }
    return vec4<f32>(in.color.rgb, alpha);
}
