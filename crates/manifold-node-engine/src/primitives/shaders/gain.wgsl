// node.gain — multiply input texture's RGB by a scalar `gain`,
// preserve alpha. Smallest possible "respond to a scalar wire"
// consumer for the texture domain.

struct GainParam {
    gain: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> param: GainParam;
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
    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    textureStore(
        output_tex,
        vec2<i32>(id.xy),
        vec4<f32>(src.rgb * param.gain, src.a),
    );
}
