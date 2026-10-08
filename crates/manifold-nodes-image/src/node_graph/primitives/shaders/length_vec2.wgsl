// node.length_vec2 — per-pixel scalar magnitude of the input's RG vec2.
// out.r = length(in.rg); GBA = (0, 0, 1).
//
// The classic vec2 magnitude atom: turns a signed flow / displacement /
// gradient texture into a positive scalar field. Pair with
// `node.heightmap_to_normal` (height = vec2 magnitude — used in
// oily-fluid color → normal pipeline), with `node.smoothstep_texture`
// for thresholding, or feed into a tonemap for visualisation.

@group(0) @binding(0) var tex_in: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let inv = vec2<f32>(1.0) / vec2<f32>(dims);
    let uv = (vec2<f32>(id.xy) + 0.5) * inv;

    let v = textureSampleLevel(tex_in, tex_sampler, uv, 0.0).rg;
    let l = length(v);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(l, 0.0, 0.0, 1.0));
}
