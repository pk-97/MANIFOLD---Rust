// FluidGradientRotate — port of Unity FluidGradientRotate.shader
// Gradient + rotation pass: compute central-difference gradient of blurred
// density field using texelFetch (toroidal modulo), scale by slope, rotate
// by pre-computed cos/sin to produce 2D force field.
//
// Uses textureLoad (texelFetch) with explicit toroidal wrapping — NOT textureSample.
// Unity: _MainTex.Load(int3((tc.x + 1u) % uw, tc.y, 0))

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

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let dims = textureDimensions(t_density);
    let w = dims.x;
    let h = dims.y;

    // Integer texel coordinates — bypasses all UV interpolation (Unity: int2 tc = int2(uv * float2(w, h)))
    let tc = vec2<u32>(vec2<f32>(in.uv) * vec2<f32>(f32(w), f32(h)));

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

    return vec4<f32>(force.x, force.y, 0.0, 1.0);
}
