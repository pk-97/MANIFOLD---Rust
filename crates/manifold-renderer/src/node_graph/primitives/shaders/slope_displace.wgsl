// node.slope_displace — emboss-style displacement. Soft-light-blends a
// `base` layer over an `image` layer, takes the luminance Sobel gradient
// of that blend at a configurable step, and displaces `image` by the
// gradient. Verbatim from Watercolor's slope pass — the bleed/edge-pull
// that gives watercolor its pooled-pigment look. Reusable wherever a
// height-from-contrast displacement is wanted.

struct Uniforms {
    strength: f32,  // Sobel gradient multiplier
    step: f32,      // Sobel sample offset in pixels
    weight: f32,    // UV displacement scale
    _pad0: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var base_tex: texture_2d<f32>;
@group(0) @binding(2) var image_tex: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

// W3C Soft Light (CSS Compositing Level 1).
fn soft_light_ch(base: f32, blend: f32) -> f32 {
    if blend <= 0.5 {
        return base - (1.0 - 2.0 * blend) * base * (1.0 - base);
    }
    var d: f32;
    if base <= 0.25 {
        d = ((16.0 * base - 12.0) * base + 4.0) * base;
    } else {
        d = sqrt(base);
    }
    return base + (2.0 * blend - 1.0) * (d - base);
}

fn soft_light(base: vec3<f32>, blend: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        soft_light_ch(base.r, blend.r),
        soft_light_ch(base.g, blend.g),
        soft_light_ch(base.b, blend.b),
    );
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let texel = 1.0 / vec2<f32>(dims);
    let step_uv = vec2<f32>(u.step * texel.x, u.step * texel.y);

    let ga_r = textureSampleLevel(base_tex, tex_sampler, uv + vec2<f32>(step_uv.x, 0.0), 0.0).rgb;
    let bl_r = textureSampleLevel(image_tex, tex_sampler, uv + vec2<f32>(step_uv.x, 0.0), 0.0).rgb;
    let ga_l = textureSampleLevel(base_tex, tex_sampler, uv - vec2<f32>(step_uv.x, 0.0), 0.0).rgb;
    let bl_l = textureSampleLevel(image_tex, tex_sampler, uv - vec2<f32>(step_uv.x, 0.0), 0.0).rgb;
    let ga_u = textureSampleLevel(base_tex, tex_sampler, uv + vec2<f32>(0.0, step_uv.y), 0.0).rgb;
    let bl_u = textureSampleLevel(image_tex, tex_sampler, uv + vec2<f32>(0.0, step_uv.y), 0.0).rgb;
    let ga_d = textureSampleLevel(base_tex, tex_sampler, uv - vec2<f32>(0.0, step_uv.y), 0.0).rgb;
    let bl_d = textureSampleLevel(image_tex, tex_sampler, uv - vec2<f32>(0.0, step_uv.y), 0.0).rgb;

    let sl_r = soft_light(ga_r, bl_r);
    let sl_l = soft_light(ga_l, bl_l);
    let sl_u = soft_light(ga_u, bl_u);
    let sl_d = soft_light(ga_d, bl_d);

    let luma = vec3<f32>(0.2126, 0.7152, 0.0722);
    let dx = dot(sl_r - sl_l, luma) * u.strength;
    let dy = dot(sl_u - sl_d, luma) * u.strength;

    let slope_offset = vec2<f32>(dx, dy) * u.weight;
    let displaced_uv = uv + slope_offset;

    let color = textureSampleLevel(image_tex, tex_sampler, displaced_uv, 0.0);
    textureStore(output_tex, vec2<i32>(id.xy), color);
}
