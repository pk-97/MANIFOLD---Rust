// node.feedback — Stylized feedback / trail effect.
//
// Reads the current frame from `tex_source` and the previous frame from
// `tex_prev`, applies zoom + rotation to the previous frame's UV
// sampling, then blends the two together with one of three modes:
//   0 = Screen   — HDR-safe, trails fade in bright areas
//   1 = Additive — unconditional accumulation
//   2 = Max      — hard edges, sharpest copies
//
// The previous-frame texture is held by the host (StateStore) and
// updated after this dispatch via a separate copy. This shader does
// not write to `tex_prev`; the caller copies the output back.
//
// Bindings (one-input + persistent-state primitive):
//   @binding(0) uniforms
//   @binding(1) tex_source   (current frame)
//   @binding(2) tex_prev     (previous frame state)
//   @binding(3) tex_sampler
//   @binding(4) output_tex   (rgba16float storage)

struct Uniforms {
    feedback_amount: f32,  // 0..0.98 (caller clamps)
    zoom: f32,             // 0.001..10 (caller clamps)
    rotation: f32,         // radians (caller converts from degrees)
    mode: u32,             // 0=Screen, 1=Additive, 2=Max
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_source: texture_2d<f32>;
@group(0) @binding(2) var tex_prev: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    // Transform UV around center for the previous-frame sample.
    let center = vec2<f32>(0.5, 0.5);
    var transformed_uv = uv - center;

    // Zoom (>1 = zoom in = copies shrink toward center). Guard against
    // zero zoom — OSC/MIDI can bypass registry bounds.
    let safe_zoom = max(uniforms.zoom, 0.001);
    transformed_uv = transformed_uv / safe_zoom;

    // Rotation.
    let s = sin(uniforms.rotation);
    let c = cos(uniforms.rotation);
    let rotated = vec2<f32>(
        transformed_uv.x * c - transformed_uv.y * s,
        transformed_uv.x * s + transformed_uv.y * c,
    );
    transformed_uv = rotated + center;

    // Edge fade — smoothstep into the [0.02, 0.98] interior so the
    // mirrored copies fade out at the borders rather than wrapping or
    // clamping abruptly.
    let edge_smooth = smoothstep(vec2<f32>(0.0), vec2<f32>(0.02), transformed_uv)
                    * smoothstep(vec2<f32>(0.0), vec2<f32>(0.02), vec2<f32>(1.0) - transformed_uv);
    let edge_mask = edge_smooth.x * edge_smooth.y;

    let prev = textureSampleLevel(tex_prev, tex_sampler, transformed_uv, 0.0) * edge_mask;
    let current = textureSampleLevel(tex_source, tex_sampler, uv, 0.0);

    let amt = uniforms.feedback_amount;

    // Color blend. Three modes share one if-else chain (Metal compiler
    // dead-codes the inactive branches when called via specialization,
    // but the function-constant path is the host's job — for now we
    // accept the conditional cost).
    var result: vec4<f32>;
    if uniforms.mode == 0u {
        // Screen — HDR-safe.
        result = current
               + prev * amt * clamp(vec4<f32>(1.0) - current, vec4<f32>(0.0), vec4<f32>(1.0));
    } else if uniforms.mode == 1u {
        // Additive.
        result = current + prev * amt;
    } else {
        // Max.
        result = max(current, prev * amt);
    }

    // Alpha blend follows the mode but with conservative clamping so
    // additive mode doesn't drift > 1.
    let prev_alpha = prev.a * edge_mask;
    if uniforms.mode == 1u {
        result.a = clamp(current.a + prev_alpha * amt, 0.0, 1.0);
    } else {
        result.a = max(current.a, prev_alpha * amt);
    }

    // NaN/Inf guard — feedback loops can amplify garbage values
    // catastrophically. Clamp first (works under fast-math), then a
    // secondary NaN check (`x != x` only when NaN under IEEE 754).
    var safe = clamp(result, vec4<f32>(-100.0), vec4<f32>(100.0));
    if any(safe != safe) {
        safe = vec4<f32>(0.0);
    }
    textureStore(output_tex, vec2<i32>(gid.xy), safe);
}
