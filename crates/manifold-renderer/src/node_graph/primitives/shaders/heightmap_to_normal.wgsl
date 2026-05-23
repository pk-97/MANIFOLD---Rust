// node.heightmap_to_normal — central-difference height → tangent-space
// normal. Reads input.r as the scalar height at each pixel, computes
// (dh/dx, dh/dy) via half-difference of adjacent samples, then forms
// the unnormalised tangent-space normal vec3(-dh/dx, -dh/dy, z_scale)
// and normalises. Larger z_scale = flatter normals; smaller z_scale =
// steeper normals.
//
// The sign convention matches OpenGL-style tangent-space normals
// (y-up): a height increase along +x gives a normal pointing in the
// -x direction.
//
// Output: RGB = signed tangent-space normal in [-1, 1]. Alpha = 1.
//
// Bindings:
//   @binding(0) uniforms (16 bytes)
//   @binding(1) tex_in
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    z_scale: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_in: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let inv = vec2<f32>(1.0) / vec2<f32>(dims);
    let uv = (vec2<f32>(id.xy) + 0.5) * inv;

    let hL = textureSampleLevel(tex_in, tex_sampler, uv + vec2<f32>(-inv.x, 0.0), 0.0).r;
    let hR = textureSampleLevel(tex_in, tex_sampler, uv + vec2<f32>( inv.x, 0.0), 0.0).r;
    let hD = textureSampleLevel(tex_in, tex_sampler, uv + vec2<f32>(0.0, -inv.y), 0.0).r;
    let hU = textureSampleLevel(tex_in, tex_sampler, uv + vec2<f32>(0.0,  inv.y), 0.0).r;
    let gx = (hR - hL) * 0.5;
    let gy = (hU - hD) * 0.5;
    let n = normalize(vec3<f32>(-gx, -gy, max(uniforms.z_scale, 1e-4)));
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(n, 1.0));
}
