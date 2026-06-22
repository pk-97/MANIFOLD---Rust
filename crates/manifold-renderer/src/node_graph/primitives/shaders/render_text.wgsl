// node.render_text — composite a CPU-rasterized R8Unorm glyph bitmap into
// the output with position / aspect / vertical alignment. The bitmap is
// rasterized at its full on-screen footprint (size × scale folded in), so it
// samples ~1:1 and a linear sampler keeps CoreText's anti-aliasing crisp.
// `display_scale` is the residual magnification (> 1 only when the bitmap was
// capped at MAX_BITMAP_DIM and must be upscaled to the requested size).
// Two coverage masks: `fill_tex` (glyph interior) and `stroke_tex` (outline,
// only sampled when has_stroke > 0). Output is premultiplied alpha.

struct Uniforms {
    pos_x: f32,
    pos_y: f32,
    display_scale: f32,
    aspect_ratio: f32,
    // -- 16-byte boundary --
    tex_width: f32,
    tex_height: f32,
    output_width: f32,
    output_height: f32,
    // -- 16-byte boundary --
    v_align: f32, // 0=Top, 1=Center, 2=Bottom
    has_stroke: f32, // 0 = ignore stroke_tex, 1 = blend stroke under fill
    _pad0: f32,
    _pad1: f32,
    // -- 16-byte boundary --
    fill_color: vec4<f32>,
    // -- 16-byte boundary --
    stroke_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
// R8Unorm bound as texture_2d + sampler (R8Unorm is filterable on Metal).
@group(0) @binding(1) var fill_tex: texture_2d<f32>;
@group(0) @binding(2) var stroke_tex: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output);
    if id.x >= dims.x || id.y >= dims.y { return; }

    // Pixel → normalized UV [0,1]
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    // Center-relative, aspect-corrected coordinates, then offset by position.
    var p = uv - 0.5;
    p.x *= u.aspect_ratio;
    p = p - vec2<f32>(u.pos_x, -u.pos_y);

    // Map to text texture UV space. The bitmap covers
    // (tex_width / output_width) × (tex_height / output_height) of the
    // normalized output; `display_scale` enlarges that footprint only when the
    // bitmap was capped (otherwise 1.0, so the raster size IS the screen size).
    let text_extent = vec2<f32>(u.tex_width, u.tex_height)
        / vec2<f32>(u.output_width, u.output_height);
    // Correct for aspect in x: text_extent.x is in pixel-space ratio,
    // but p.x is aspect-corrected. Undo aspect on the extent.
    let half = vec2<f32>(text_extent.x * u.aspect_ratio, text_extent.y)
        * 0.5 * u.display_scale;

    // Vertical alignment offset (in normalized coords).
    // v_align: 0=Top, 1=Center, 2=Bottom.
    // At Center (1), offset=0. At Top (0), shift text down by +half.y - 0.5.
    // At Bottom (2), shift text up by -(half.y - 0.5).
    let v_shift = (1.0 - u.v_align) * (0.5 - half.y);

    let tex_uv = (p + half + vec2<f32>(0.0, v_shift)) / (half * 2.0);

    // Bounds check — outside the glyph bitmap is fully transparent so the
    // text keys over the layer below (premultiplied alpha contract).
    if tex_uv.x < 0.0 || tex_uv.x > 1.0 || tex_uv.y < 0.0 || tex_uv.y > 1.0 {
        textureStore(output, vec2<i32>(id.xy), vec4<f32>(0.0, 0.0, 0.0, 0.0));
        return;
    }

    // Linear-sample the coverage masks. The bitmap carries CoreText's own
    // anti-aliasing plus a transparent PADDING border, so bilinear filtering
    // keeps the edges smooth at any size instead of stair-stepping. No Y-flip:
    // the CG bitmap row order matches the GPU texture upload path.
    let fill_cov = textureSampleLevel(fill_tex, tex_sampler, tex_uv, 0.0).r;
    var stroke_cov = 0.0;
    if u.has_stroke > 0.5 {
        stroke_cov = textureSampleLevel(stroke_tex, tex_sampler, tex_uv, 0.0).r;
    }

    // Premultiplied-alpha "over": fill on top of stroke on top of transparent.
    // The outline keys under the fill so glyph interiors keep the fill colour
    // and the stroke shows as a ring; everything anti-aliases over the layer
    // below instead of painting a black box. With fill=white and no stroke
    // this reduces to (white * coverage, coverage) — the original behaviour.
    let fa = fill_cov * u.fill_color.a;
    let sa = stroke_cov * u.stroke_color.a;
    let rgb = u.fill_color.rgb * fa + u.stroke_color.rgb * sa * (1.0 - fa);
    let a = fa + sa * (1.0 - fa);
    textureStore(output, vec2<i32>(id.xy), vec4<f32>(rgb, a));
}
