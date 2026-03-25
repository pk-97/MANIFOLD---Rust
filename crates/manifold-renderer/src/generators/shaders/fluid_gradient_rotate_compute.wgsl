// Compute variant of fluid_gradient_rotate.wgsl.
// Identical math — only I/O mechanism changes:
//   - textureStore to output storage texture instead of fragment return
//   - @compute @workgroup_size(16,16) instead of vertex+fragment
// Uses textureLoad (texelFetch) with explicit toroidal wrapping — same as fragment.

struct GradientUniforms {
    texel_x: f32,
    texel_y: f32,
    slope_strength: f32,
    rot_cos: f32,
    rot_sin: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> params: GradientUniforms;
@group(0) @binding(1) var t_density: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let w = dims.x;
    let h = dims.y;

    // Integer texel coordinates — same as fragment path
    let tc = vec2<u32>(gid.x, gid.y);

    // texelFetch with explicit toroidal wrapping (Unity: (tc.x + 1u) % uw)
    let dR = textureLoad(t_density, vec2<i32>(i32((tc.x + 1u) % w), i32(tc.y)), 0).r;
    let dL = textureLoad(t_density, vec2<i32>(i32((tc.x + w - 1u) % w), i32(tc.y)), 0).r;
    let dU = textureLoad(t_density, vec2<i32>(i32(tc.x), i32((tc.y + 1u) % h)), 0).r;
    let dD = textureLoad(t_density, vec2<i32>(i32(tc.x), i32((tc.y + h - 1u) % h)), 0).r;

    // UV-space gradient: both axes proportional to 1/(W*H)
    // Unity: float2 texel = _MainTex_TexelSize.xy; grad = float2(dR-dL, dU-dD) / (2.0 * texel)
    let texel = vec2<f32>(params.texel_x, params.texel_y);
    let grad = vec2<f32>(dR - dL, dU - dD) / (2.0 * texel);

    // Scale by slope strength (negative = repulsion, area-normalized in Rust)
    let scaled = grad * params.slope_strength;

    // Rotate by fluid curl angle (pre-computed cos/sin passed from CPU)
    // Unity: force.x = scaled.x * _RotCos - scaled.y * _RotSin
    //        force.y = scaled.x * _RotSin + scaled.y * _RotCos
    let force = vec2<f32>(
        scaled.x * params.rot_cos - scaled.y * params.rot_sin,
        scaled.x * params.rot_sin + scaled.y * params.rot_cos,
    );

    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(force.x, force.y, 0.0, 1.0));
}
