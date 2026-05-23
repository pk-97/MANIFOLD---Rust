// node.normalize_vec2 — per-pixel safe-normalize of the RG channels.
// Reads vec2 = in.rg, writes (v/length(v), 0, 1) when length > eps, else
// (0, 0, 0, 1). Blue and alpha channels are forced; the GBA of the
// input are ignored.
//
// Used by curl-forcing extraction (normalize gradients before summing),
// any flow-field op that wants direction without magnitude. Caller scales
// the result with downstream `node.gain` for magnitude restoration.
//
// Bindings:
//   @binding(0) tex_in
//   @binding(1) tex_sampler
//   @binding(2) output_tex (rgba16float storage)

@group(0) @binding(0) var tex_in: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

const EPS: f32 = 1e-6;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let inv = vec2<f32>(1.0) / vec2<f32>(dims);
    let uv = (vec2<f32>(id.xy) + 0.5) * inv;

    let v = textureSampleLevel(tex_in, tex_sampler, uv, 0.0).rg;
    let len = length(v);
    let n = select(vec2<f32>(0.0), v / len, len >= EPS);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(n, 0.0, 1.0));
}
