// node.scale_offset_texture — per-pixel affine remap a*x + b on
// RGB (alpha pass-through). Two scalar params: scale (a) and
// offset (b). The companion to per-pixel field generators that
// produce [0, 1] outputs — use a=2, b=-1 to recover signed
// [-1, 1] noise; use a=0.5, b=0.5 to compress signed sin output
// to [0, 1].

struct Uniforms {
    scale:  f32,
    offset: f32,
    _pad0:  f32,
    _pad1:  f32,
};

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
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let s = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    let out = vec4<f32>(
        s.r * u.scale + u.offset,
        s.g * u.scale + u.offset,
        s.b * u.scale + u.offset,
        s.a,
    );
    textureStore(output_tex, vec2<i32>(id.xy), out);
}
