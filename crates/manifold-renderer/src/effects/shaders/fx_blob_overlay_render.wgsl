// Instanced quad renderer for blob tracking overlay.
//
// Each instance is a screen-space quad (bracket arm, crosshair, gauge edge,
// tick mark, connection dash, or font glyph). The vertex shader generates
// 6 vertices (2 triangles) per instance from a position rect. The fragment
// shader outputs either a solid color or a font atlas sample.
//
// This replaces the full-screen compute overlay — only pixels actually covered
// by geometry get shaded. Typical overlay coverage is 2-5% of the frame.

struct Uniforms {
    overlay_color: vec3<f32>,
    amount: f32,
};

struct QuadInstance {
    // Quad rect in clip space: (x0, y0, x1, y1)
    rect: vec4<f32>,
    // Font atlas UVs: (u0, v0, u1, v1). If u0 == u1, solid quad.
    atlas_rect: vec4<f32>,
    // Alpha multiplier for this instance
    alpha: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> quads: array<QuadInstance>;
@group(0) @binding(2) var font_tex: texture_2d<f32>;
@group(0) @binding(3) var point_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) atlas_uv: vec2<f32>,
    @location(1) alpha: f32,
    @location(2) is_textured: f32,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> VertexOutput {
    let q = quads[iid];

    // Quad corners: 2 triangles from 6 vertices
    // (x0,y0), (x1,y0), (x1,y1), (x0,y0), (x1,y1), (x0,y1)
    let x = array<f32, 6>(q.rect.x, q.rect.z, q.rect.z, q.rect.x, q.rect.z, q.rect.x);
    let y = array<f32, 6>(q.rect.y, q.rect.y, q.rect.w, q.rect.y, q.rect.w, q.rect.w);
    let au = array<f32, 6>(q.atlas_rect.x, q.atlas_rect.z, q.atlas_rect.z, q.atlas_rect.x, q.atlas_rect.z, q.atlas_rect.x);
    let av = array<f32, 6>(q.atlas_rect.y, q.atlas_rect.y, q.atlas_rect.w, q.atlas_rect.y, q.atlas_rect.w, q.atlas_rect.w);

    var out: VertexOutput;
    out.position = vec4<f32>(x[vid], y[vid], 0.0, 1.0);
    out.atlas_uv = vec2<f32>(au[vid], av[vid]);
    out.alpha = q.alpha;
    // Textured if atlas rect has non-zero width
    out.is_textured = select(0.0, 1.0, abs(q.atlas_rect.z - q.atlas_rect.x) > 0.0001);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var a = in.alpha * u.amount;
    if in.is_textured > 0.5 {
        a *= textureSampleLevel(font_tex, point_sampler, in.atlas_uv, 0.0).r;
    }
    if a < 0.004 {
        discard;
    }
    return vec4<f32>(u.overlay_color * a, 0.0);
}
