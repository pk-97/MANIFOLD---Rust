// Compute variant of fx_stylized_feedback.wgsl for ComputeDualBlitHelper.
// Two source textures: source_tex_a (_MainTex = current frame),
//                      source_tex_b (_PrevTex = previous frame state).

@id(0) override MODE: f32 = 0.0;

struct Uniforms {
    feedback_amount: f32,  // _FeedbackAmount — clamped to <=0.98 in Rust
    zoom:            f32,  // _Zoom
    rotation:        f32,  // _Rotation (radians)
    _pad:            f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex_a: texture_2d<f32>;
@group(0) @binding(2) var source_tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    // StylizedFeedbackEffect.shader lines 62-74: Transform UVs around center
    let center = vec2<f32>(0.5, 0.5);
    var transformed_uv = uv - center;

    // Apply zoom (>1 = zoom in = copies shrink toward center)
    // Guard against division by zero — OSC/MIDI can bypass registry bounds
    let safe_zoom = max(uniforms.zoom, 0.001);
    transformed_uv = transformed_uv / safe_zoom;

    // Apply rotation (radians)
    let s = sin(uniforms.rotation);
    let c = cos(uniforms.rotation);
    let rotated = vec2<f32>(
        transformed_uv.x * c - transformed_uv.y * s,
        transformed_uv.x * s + transformed_uv.y * c,
    );
    transformed_uv = rotated;

    transformed_uv = transformed_uv + center;

    // StylizedFeedbackEffect.shader lines 77-78: Edge fade
    let edge_smooth = smoothstep(vec2<f32>(0.0, 0.0), vec2<f32>(0.02, 0.02), transformed_uv)
                    * smoothstep(vec2<f32>(0.0, 0.0), vec2<f32>(0.02, 0.02), vec2<f32>(1.0, 1.0) - transformed_uv);
    let edge_mask = edge_smooth.x * edge_smooth.y;

    // StylizedFeedbackEffect.shader lines 80-81: Sample
    let prev    = textureSampleLevel(source_tex_b, tex_sampler, transformed_uv, 0.0) * edge_mask;
    let current = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);

    let amt = uniforms.feedback_amount;

    // StylizedFeedbackEffect.shader lines 88-104: Three blend modes
    var result: vec4<f32>;
    if MODE < 0.5 {
        // Mode 0: Screen — HDR-safe, trails fade in bright areas
        result = current + prev * amt * clamp(vec4<f32>(1.0) - current, vec4<f32>(0.0), vec4<f32>(1.0));
    } else if MODE < 1.5 {
        // Mode 1: Additive — unconditional accumulation
        result = current + prev * amt;
    } else {
        // Mode 2: Max — hard edges, sharpest copies
        result = max(current, prev * amt);
    }

    // StylizedFeedbackEffect.shader lines 106-124: Alpha handling per mode
    let prev_alpha = prev.a * edge_mask;

    if MODE < 0.5 {
        result.a = max(current.a, prev_alpha * amt);
    } else if MODE < 1.5 {
        result.a = clamp(current.a + prev_alpha * amt, 0.0, 1.0);
    } else {
        result.a = max(current.a, prev_alpha * amt);
    }

    // NaN/Inf guard: prevent corrupt values from entering feedback state buffer
    // Under fast_math, NaN comparisons are undefined, so use clamp as primary defense
    var safe = result;
    safe = clamp(safe, vec4<f32>(-100.0), vec4<f32>(100.0));
    // Secondary defense: if any component is still NaN (NaN != NaN under IEEE 754)
    if (any(safe != safe)) {
        safe = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    textureStore(output_tex, vec2<i32>(gid.xy), safe);
}
