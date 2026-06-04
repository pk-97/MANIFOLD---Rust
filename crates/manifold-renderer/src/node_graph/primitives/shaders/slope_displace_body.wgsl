// node.slope_displace — fusable body (freeze §12), 2-input GATHER (base + image).
// Emboss-style displacement: soft-light-blend `base` over `image`, take the
// luminance Sobel gradient of that blend at a configurable step, displace `image`
// by the gradient. Both inputs are gathered (neighbour taps + a final dependent
// sample of image at the displaced UV), so each arrives as a texture+sampler arg
// (the two samplers are the one shared sampler). texel = 1/dims. Matches
// slope_displace.wgsl. PARAMS: [strength, step, weight].
fn sl_soft_light_ch(base: f32, blend: f32) -> f32 {
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

fn sl_soft_light(base: vec3<f32>, blend: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        sl_soft_light_ch(base.r, blend.r),
        sl_soft_light_ch(base.g, blend.g),
        sl_soft_light_ch(base.b, blend.b),
    );
}

fn body(base_tex: texture_2d<f32>, s_base: sampler, image_tex: texture_2d<f32>, s_image: sampler, uv: vec2<f32>, dims: vec2<f32>, strength: f32, step: f32, weight: f32) -> vec4<f32> {
    let texel = 1.0 / dims;
    let step_uv = vec2<f32>(step * texel.x, step * texel.y);

    let ga_r = textureSampleLevel(base_tex, s_base, uv + vec2<f32>(step_uv.x, 0.0), 0.0).rgb;
    let bl_r = textureSampleLevel(image_tex, s_image, uv + vec2<f32>(step_uv.x, 0.0), 0.0).rgb;
    let ga_l = textureSampleLevel(base_tex, s_base, uv - vec2<f32>(step_uv.x, 0.0), 0.0).rgb;
    let bl_l = textureSampleLevel(image_tex, s_image, uv - vec2<f32>(step_uv.x, 0.0), 0.0).rgb;
    let ga_u = textureSampleLevel(base_tex, s_base, uv + vec2<f32>(0.0, step_uv.y), 0.0).rgb;
    let bl_u = textureSampleLevel(image_tex, s_image, uv + vec2<f32>(0.0, step_uv.y), 0.0).rgb;
    let ga_d = textureSampleLevel(base_tex, s_base, uv - vec2<f32>(0.0, step_uv.y), 0.0).rgb;
    let bl_d = textureSampleLevel(image_tex, s_image, uv - vec2<f32>(0.0, step_uv.y), 0.0).rgb;

    let sl_r = sl_soft_light(ga_r, bl_r);
    let sl_l = sl_soft_light(ga_l, bl_l);
    let sl_u = sl_soft_light(ga_u, bl_u);
    let sl_d = sl_soft_light(ga_d, bl_d);

    let luma = vec3<f32>(0.2126, 0.7152, 0.0722);
    let dx = dot(sl_r - sl_l, luma) * strength;
    let dy = dot(sl_u - sl_d, luma) * strength;

    let slope_offset = vec2<f32>(dx, dy) * weight;
    let displaced_uv = uv + slope_offset;

    return textureSampleLevel(image_tex, s_image, displaced_uv, 0.0);
}
