// Mechanical port of StylizedFeedbackEffect.shader.
// Same logic, same variables, same constants, same edge cases.

struct Uniforms {
    feedback_amount: f32,  // _FeedbackAmount — clamped to <=0.98 in Rust
    zoom:            f32,  // _Zoom
    rotation:        f32,  // _Rotation (radians; Rust side converts degrees * DEG_TO_RAD)
    mode:            f32,  // _Mode — 0=Screen, 1=Additive, 2=Max (rounded in Rust)
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var main_tex:   texture_2d<f32>;  // _MainTex  — current frame
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var prev_tex:   texture_2d<f32>;  // _PrevTex  — previous frame state

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // StylizedFeedbackEffect.shader lines 62-74: Transform UVs around center
    let center = vec2<f32>(0.5, 0.5);
    var uv = in.uv - center;

    // Apply zoom (>1 = zoom in = copies shrink toward center)
    uv = uv / uniforms.zoom;

    // Apply rotation (radians)
    let s = sin(uniforms.rotation);
    let c = cos(uniforms.rotation);
    let rotated = vec2<f32>(uv.x * c - uv.y * s, uv.x * s + uv.y * c);
    uv = rotated;

    uv = uv + center;

    // StylizedFeedbackEffect.shader lines 77-78: Edge fade
    let edge_smooth = smoothstep(vec2<f32>(0.0, 0.0), vec2<f32>(0.02, 0.02), uv)
                    * smoothstep(vec2<f32>(0.0, 0.0), vec2<f32>(0.02, 0.02), vec2<f32>(1.0, 1.0) - uv);
    let edge_mask = edge_smooth.x * edge_smooth.y;

    // StylizedFeedbackEffect.shader lines 80-81: Sample
    // prev samples at transformed uv * edgeMask; current samples at original uv
    let prev    = textureSample(prev_tex,  tex_sampler, uv)    * edge_mask;
    let current = textureSample(main_tex, tex_sampler, in.uv);

    let amt = uniforms.feedback_amount;

    // StylizedFeedbackEffect.shader lines 88-104: Three blend modes
    var result: vec4<f32>;
    if uniforms.mode < 0.5 {
        // Mode 0: Screen — HDR-safe, trails fade in bright areas
        result = current + prev * amt * clamp(vec4<f32>(1.0) - current, vec4<f32>(0.0), vec4<f32>(1.0));
    } else if uniforms.mode < 1.5 {
        // Mode 1: Additive — unconditional accumulation
        result = current + prev * amt;
    } else {
        // Mode 2: Max — hard edges, sharpest copies
        result = max(current, prev * amt);
    }

    // StylizedFeedbackEffect.shader lines 106-124: Alpha handling per mode
    let prev_alpha = prev.a * edge_mask;

    if uniforms.mode < 0.5 {
        // Screen: preserve current alpha, fade prev in
        result.a = max(current.a, prev_alpha * amt);
    } else if uniforms.mode < 1.5 {
        // Additive: blend alphas additively
        result.a = clamp(current.a + prev_alpha * amt, 0.0, 1.0);
    } else {
        // Max: take max of alphas
        result.a = max(current.a, prev_alpha * amt);
    }

    return result;
}
