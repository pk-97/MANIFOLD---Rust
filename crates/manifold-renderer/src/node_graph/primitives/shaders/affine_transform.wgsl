// primitive.affine_transform — pixel-exact replacement for legacy
// `effects/shaders/fx_transform.wgsl`. 2D affine UV transform with
// aspect-correct rotation. Bindings, math, OOB-clamp behavior, and
// dispatch shape preserved verbatim.

struct Uniforms {
    translate_x: f32,
    translate_y: f32,
    scale: f32,
    rotation: f32,
    aspect_ratio: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv_orig = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    var uv = uv_orig - vec2<f32>(0.5, 0.5);

    uv.x = uv.x * u.aspect_ratio;

    let cos_r = cos(u.rotation);
    let sin_r = sin(u.rotation);
    uv = vec2<f32>(
        uv.x * cos_r - uv.y * sin_r,
        uv.x * sin_r + uv.y * cos_r,
    );

    uv.x = uv.x / u.aspect_ratio;

    uv = uv / max(u.scale, 0.01);
    uv = uv - vec2<f32>(u.translate_x, u.translate_y);
    uv = uv + vec2<f32>(0.5, 0.5);

    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(0.0, 0.0, 0.0, 0.0));
        return;
    }

    textureStore(output_tex, vec2<i32>(id.xy), textureSampleLevel(source_tex, tex_sampler, uv, 0.0));
}
