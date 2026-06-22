// node.render_text — composite a CPU-rasterized R8Unorm glyph bitmap
// into the output with position / scale / aspect / vertical alignment.

struct Uniforms {
    pos_x: f32,
    pos_y: f32,
    scale: f32,
    aspect_ratio: f32,
    // -- 16-byte boundary --
    tex_width: f32,
    tex_height: f32,
    output_width: f32,
    output_height: f32,
    // -- 16-byte boundary --
    v_align: f32, // 0=Top, 1=Center, 2=Bottom
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
// R8Unorm bound as texture_2d (NOT storage — R8Unorm can't be storage on Metal).
@group(0) @binding(1) var text_tex: texture_2d<f32>;
@group(0) @binding(2) var output: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output);
    if id.x >= dims.x || id.y >= dims.y { return; }

    // Pixel → normalized UV [0,1]
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    // Center-relative, aspect-corrected coordinates
    var p = uv - 0.5;
    p.x *= u.aspect_ratio;

    // Apply position offset and inverse scale
    p = (p - vec2<f32>(u.pos_x, -u.pos_y)) / u.scale;

    // Map to text texture UV space.
    // The text bitmap covers (tex_width / output_width) × (tex_height / output_height)
    // of the normalized output space. Center it at origin.
    let text_extent = vec2<f32>(u.tex_width, u.tex_height)
        / vec2<f32>(u.output_width, u.output_height);
    // Correct for aspect in x: text_extent.x is in pixel-space ratio,
    // but p.x is aspect-corrected. Undo aspect on the extent.
    let half = vec2<f32>(text_extent.x * u.aspect_ratio, text_extent.y) * 0.5;

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

    // No Y-flip needed: CG bitmap context row ordering matches GPU texture
    // layout for this upload path (Metal replace_region preserves row order).
    let texel = vec2<i32>(vec2<f32>(u.tex_width, u.tex_height) * tex_uv);
    let coverage = textureLoad(text_tex, texel, 0).r;

    // White glyphs, premultiplied alpha: rgb = white * coverage, a = coverage.
    // Where coverage is 0 the pixel is fully transparent, so the glyph edges
    // anti-alias against whatever is below and the background no longer paints
    // an opaque black box over the layer beneath.
    textureStore(output, vec2<i32>(id.xy), vec4<f32>(vec3<f32>(coverage), coverage));
}
