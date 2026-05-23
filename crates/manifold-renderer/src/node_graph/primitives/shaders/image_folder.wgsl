// Sample the currently-loaded source image into the output texture
// with aspect-correct fit + user zoom. Matches the legacy
// mri_slice_compute.wgsl fit math (minus the windowing pass — that
// moves downstream so the primitive stays generic).

struct Uniforms {
    aspect_ratio: f32,
    uv_scale: f32,
    tex_width: f32,
    tex_height: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv_raw = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    let img_aspect = u.tex_width / u.tex_height;
    let ratio = u.aspect_ratio / img_aspect;
    var uv = uv_raw - 0.5;
    if (ratio > 1.0) {
        uv.x *= ratio;
    } else {
        uv.y /= ratio;
    }
    uv = uv * u.uv_scale + 0.5;

    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(0.0, 0.0, 0.0, 1.0));
        return;
    }

    let c = textureSampleLevel(src_tex, src_sampler, uv, 0.0);
    textureStore(output_tex, vec2<i32>(gid.xy), c);
}
