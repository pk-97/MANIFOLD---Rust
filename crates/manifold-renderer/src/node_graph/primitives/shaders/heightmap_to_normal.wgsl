// node.heightmap_to_normal — central-difference height → unit normal.
//
// Reads input.r as scalar height per pixel, computes (dh/dx, dh/dy) via
// half-difference of adjacent samples, then assembles the normal in one
// of two coordinate spaces (selected by `coord_space`):
//
//   coord_space=0 TangentZ (default):
//       n = normalize(-gx, -gy * aspect, z_scale)
//     Surface-normal direction on .z. Flat-surface tangent-space
//     convention used by lambert_directional / matcap_two_tone /
//     blinn_specular / fresnel_rim. OpenGL-style (y-up): height increase
//     along +x → normal points in -x.
//
//   coord_space=1 WorldYUp:
//       n = normalize(-gx, z_scale, -gy * aspect)
//     Surface-normal direction on .y. World-space convention for 3D
//     meshes laid out in the XZ plane (Y is up). Used by cook_torrance
//     and equirect_envmap_sample with world_pos wired — the MetallicGlass
//     full-resolution-reflection trick.
//
// `aspect` scales the Y gradient so non-square world quads (canvas
// aspect ≠ 1) keep the right slope. Default 1.0 = no correction.
// Larger z_scale flattens the normal; smaller steepens it.
//
// Output: RGB = signed unit normal in [-1, 1]. Alpha = 1.
//
// D6(a) (docs/DEPTH_RELIGHT_DESIGN.md): reads via exact integer textureLoad
// + manual ClampToEdge (no sampler) — every offset lands on an exact texel
// center, so this agrees bit-for-bit with the prior filtering-sampler read.
//
// Bindings:
//   @binding(0) uniforms (16 bytes)
//   @binding(1) tex_in
//   @binding(2) output_tex (rgba16float storage)

struct Uniforms {
    z_scale: f32,
    aspect: f32,
    coord_space: u32,
    _pad0: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_in: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let dims_i = vec2<i32>(dims);
    let max_c = dims_i - vec2<i32>(1, 1);
    let c = vec2<i32>(id.xy);

    let cL = clamp(c - vec2<i32>(1, 0), vec2<i32>(0, 0), max_c);
    let cR = clamp(c + vec2<i32>(1, 0), vec2<i32>(0, 0), max_c);
    let cD = clamp(c - vec2<i32>(0, 1), vec2<i32>(0, 0), max_c);
    let cU = clamp(c + vec2<i32>(0, 1), vec2<i32>(0, 0), max_c);

    let hL = textureLoad(tex_in, cL, 0).r;
    let hR = textureLoad(tex_in, cR, 0).r;
    let hD = textureLoad(tex_in, cD, 0).r;
    let hU = textureLoad(tex_in, cU, 0).r;
    let gx = (hR - hL) * 0.5;
    let gy = (hU - hD) * 0.5 * uniforms.aspect;
    let z = max(uniforms.z_scale, 1e-4);

    var n: vec3<f32>;
    if uniforms.coord_space == 1u {
        n = normalize(vec3<f32>(-gx, z, -gy));
    } else {
        n = normalize(vec3<f32>(-gx, -gy, z));
    }
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(n, 1.0));
}
