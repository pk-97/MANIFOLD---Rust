// Single-knob 4-neighbour Laplacian unsharp mask. Bit-equivalent to
// the legacy mri_slice_compute.wgsl sharpen pass when applied to a
// grayscale source — the math here generalises to RGBA by applying
// the same Laplacian per-channel.

struct Uniforms {
    amount: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
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
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let center = textureSampleLevel(src_tex, src_sampler, uv, 0.0);

    if (u.amount <= 0.0) {
        textureStore(output_tex, vec2<i32>(gid.xy), center);
        return;
    }

    let src_dims = textureDimensions(src_tex);
    let dx = vec2<f32>(1.0 / f32(src_dims.x), 1.0 / f32(src_dims.y));

    let s_l = textureSampleLevel(src_tex, src_sampler, uv + vec2<f32>(-dx.x, 0.0), 0.0);
    let s_r = textureSampleLevel(src_tex, src_sampler, uv + vec2<f32>( dx.x, 0.0), 0.0);
    let s_u = textureSampleLevel(src_tex, src_sampler, uv + vec2<f32>(0.0, -dx.y), 0.0);
    let s_d = textureSampleLevel(src_tex, src_sampler, uv + vec2<f32>(0.0,  dx.y), 0.0);

    let laplacian = 4.0 * center - (s_l + s_r + s_u + s_d);
    let sharpened = center + laplacian * u.amount * 0.5;
    textureStore(output_tex, vec2<i32>(gid.xy), sharpened);
}
