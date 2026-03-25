// Compute variant of mycelium_diffuse.wgsl — same 3x3 box blur with
// multiplicative decay and subtractive evaporation.
// Reads source via textureSampleLevel, writes target via textureStore.

struct DiffuseUniforms {
    decay: f32,
    sub_decay: f32,
    texel_x: f32,
    texel_y: f32,
};

@group(0) @binding(0) var<uniform> params: DiffuseUniforms;
@group(0) @binding(1) var t_trail: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let tx = params.texel_x;
    let ty = params.texel_y;

    var sum: f32 = 0.0;
    sum += textureSampleLevel(t_trail, s, uv + vec2<f32>(-tx, -ty), 0.0).r;
    sum += textureSampleLevel(t_trail, s, uv + vec2<f32>( 0.0, -ty), 0.0).r;
    sum += textureSampleLevel(t_trail, s, uv + vec2<f32>( tx, -ty), 0.0).r;
    sum += textureSampleLevel(t_trail, s, uv + vec2<f32>(-tx,  0.0), 0.0).r;
    sum += textureSampleLevel(t_trail, s, uv, 0.0).r;
    sum += textureSampleLevel(t_trail, s, uv + vec2<f32>( tx,  0.0), 0.0).r;
    sum += textureSampleLevel(t_trail, s, uv + vec2<f32>(-tx,  ty), 0.0).r;
    sum += textureSampleLevel(t_trail, s, uv + vec2<f32>( 0.0,  ty), 0.0).r;
    sum += textureSampleLevel(t_trail, s, uv + vec2<f32>( tx,  ty), 0.0).r;

    let blurred = sum / 9.0;
    let result = max(0.0, blurred * params.decay - params.sub_decay);

    textureStore(output_tex, gid.xy, vec4<f32>(result, 0.0, 0.0, 1.0));
}
