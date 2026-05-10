// Sample-and-slice compute shader for tv-led-mirror.
//
// Reads the captured-screen IOSurface (after it's been blitted into a
// mip-able pyramid by the slicer's host code), maps each output column to a
// slice of the inner-cropped content rectangle, samples 5×5 binomial taps at
// a mip LOD whose footprint matches the LED tile's coverage area (so each
// tap is itself a proper area integral instead of a single point sample),
// optionally rejects achromatic / dim regions, applies HDR roll-off + color
// grade + WB, blends with the previous frame's output for temporal
// smoothing, and writes the final value.

struct Uniforms {
    blur_radius: f32,     // multiplier on ideal tile coverage (1.0 = 5 taps span one LED tile)
    luminance_floor: f32,
    luminance_knee: f32,
    saturation_floor: f32,
    saturation_knee: f32,
    crop_left: f32,
    crop_right: f32,
    crop_top: f32,
    crop_bottom: f32,
    vibrance: f32,
    gamma: f32,
    saturation_bias: f32,
    wb_r: f32,
    wb_g: f32,
    wb_b: f32,
    max_luminance: f32,
    black_floor: f32,
    hdr_peak: f32,
    // Temporal smoothing: blended = mix(prev, new, smoothing_alpha).
    // 1.0 = no smoothing (pass new through). Smaller = more inertia.
    smoothing_alpha: f32,
    // P3-to-sRGB matrix toggle. 1.0 = apply Display-P3 → sRGB conversion
    // (when capturing extendedLinearDisplayP3); 0.0 = passthrough.
    apply_p3_to_srgb: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
// `source_tex` is now a *mipmapped* copy of the IOSurface. We sample at
// runtime-chosen LOD so each tap is the correct area average.
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba8unorm, write>;
// Previous frame's output, sampled with the same coords for temporal blend.
@group(0) @binding(4) var prev_tex: texture_2d<f32>;

// Display-P3 → sRGB primaries (D65→D65, no chromatic adaptation).
// Approximation; close enough for ambient LED match.
fn p3_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let m0 = vec3<f32>( 1.2249,  -0.2247,   0.0   );
    let m1 = vec3<f32>(-0.0420,   1.0419,   0.0   );
    let m2 = vec3<f32>(-0.0197,  -0.0786,   1.0979);
    return vec3<f32>(dot(m0, c), dot(m1, c), dot(m2, c));
}

@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }

    let raw_uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    let content_left = uniforms.crop_left;
    let content_right = 1.0 - uniforms.crop_right;
    let content_top = uniforms.crop_top;
    let content_bottom = 1.0 - uniforms.crop_bottom;
    let content_w = max(content_right - content_left, 0.0001);
    let content_h = max(content_bottom - content_top, 0.0001);

    let source_u = content_left + raw_uv.x * content_w;
    let source_v = content_top + raw_uv.y * content_h;
    let center = vec2<f32>(source_u, source_v);

    // Each LED tile covers (content_w / strip_count) × (content_h / leds_per_strip)
    // of source UV. Convert to source texels for inter-tap spacing + LOD.
    let tex_size_lod0 = vec2<f32>(textureDimensions(source_tex, 0));
    let tile_w_uv = content_w / f32(dims.x);
    let tile_h_uv = content_h / f32(dims.y);
    // Spacing = (tile size / 4) × user multiplier; with 5 taps that
    // spans exactly the LED tile at multiplier=1.0.
    let spacing_x_uv = tile_w_uv * 0.25 * uniforms.blur_radius;
    let spacing_y_uv = tile_h_uv * 0.25 * uniforms.blur_radius;
    let spacing_pix = max(
        spacing_x_uv * tex_size_lod0.x,
        spacing_y_uv * tex_size_lod0.y,
    );
    // LOD where one mip texel ≈ one inter-tap step. Each tap therefore reads
    // a properly-area-averaged region of the source instead of a point sample.
    let lod = max(log2(spacing_pix), 0.0);

    let w = array<f32, 5>(1.0, 4.0, 6.0, 4.0, 1.0);
    var sum = vec4<f32>(0.0);
    var total_w = 0.0;
    for (var dy = 0; dy < 5; dy = dy + 1) {
        for (var dx = 0; dx < 5; dx = dx + 1) {
            let off = vec2<f32>(
                (f32(dx) - 2.0) * spacing_x_uv,
                (f32(dy) - 2.0) * spacing_y_uv,
            );
            let tap = textureSampleLevel(
                source_tex, tex_sampler,
                center + off,
                lod,
            );
            let mx = max(tap.r, max(tap.g, tap.b));
            let mn = min(tap.r, min(tap.g, tap.b));
            let sat = select(0.0, (mx - mn) / mx, mx > 0.0001);
            let sat_w = 1.0 + uniforms.saturation_bias * sat * sat;
            let weight = w[dx] * w[dy] * sat_w;
            sum = sum + tap * weight;
            total_w = total_w + weight;
        }
    }
    var color = sum / total_w;

    // P3→sRGB primaries conversion (only when capturing in extendedLinearDisplayP3).
    if uniforms.apply_p3_to_srgb >= 0.5 {
        color = vec4<f32>(p3_to_srgb(color.rgb), color.a);
    }

    // HDR roll-off (Reinhard extended).
    if uniforms.hdr_peak > 1.0001 {
        let p = uniforms.hdr_peak;
        let safe = max(color.rgb, vec3<f32>(0.0));
        let mapped = safe * (1.0 + safe / (p * p)) / (1.0 + safe);
        color = vec4<f32>(mapped, color.a);
    }

    // Soft luminance gate.
    if uniforms.luminance_floor > 0.0 {
        let y = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        let knee = max(uniforms.luminance_knee, 0.0001);
        let gate = smoothstep(uniforms.luminance_floor, uniforms.luminance_floor + knee, y);
        color = vec4<f32>(color.rgb * gate, color.a);
    }

    // Soft saturation gate.
    if uniforms.saturation_floor > 0.0 {
        let mx = max(color.r, max(color.g, color.b));
        let mn = min(color.r, min(color.g, color.b));
        let sat = select(0.0, (mx - mn) / mx, mx > 0.0001);
        let knee = max(uniforms.saturation_knee, 0.0001);
        let gate = smoothstep(uniforms.saturation_floor, uniforms.saturation_floor + knee, sat);
        color = vec4<f32>(color.rgb * gate, color.a);
    }

    // Vibrance.
    if abs(uniforms.vibrance - 1.0) > 0.0001 {
        let y = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        color = vec4<f32>(mix(vec3<f32>(y), color.rgb, uniforms.vibrance), color.a);
    }

    // Output gamma — luminance-preserving form. Apply the curve to BT.709
    // luminance only and scale RGB by the ratio so chroma is preserved.
    // Per-channel pow(c, gamma) hue-shifts colored mid-tones (e.g. warm
    // whites turn red) because gamma squashes low channels harder than high
    // ones, distorting the R:G:B ratio.
    if abs(uniforms.gamma - 1.0) > 0.0001 {
        let y = dot(max(color.rgb, vec3<f32>(0.0)), vec3<f32>(0.2126, 0.7152, 0.0722));
        let safe_y = max(y, 0.0001);
        let y_new = pow(safe_y, uniforms.gamma);
        let scale = y_new / safe_y;
        color = vec4<f32>(color.rgb * scale, color.a);
    }

    // White balance.
    color = vec4<f32>(
        color.r * uniforms.wb_r,
        color.g * uniforms.wb_g,
        color.b * uniforms.wb_b,
        color.a,
    );

    // Output luminance ceiling (preserves chroma).
    if uniforms.max_luminance < 0.999 {
        let y = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        if y > uniforms.max_luminance {
            let scale = uniforms.max_luminance / max(y, 0.0001);
            color = vec4<f32>(color.rgb * scale, color.a);
        }
    }

    // Black floor.
    if uniforms.black_floor > 0.0 {
        color = vec4<f32>(
            select(0.0, color.r, color.r > uniforms.black_floor),
            select(0.0, color.g, color.g > uniforms.black_floor),
            select(0.0, color.b, color.b > uniforms.black_floor),
            color.a,
        );
    }

    // Temporal blend with previous frame's output. EMA: out = α·new + (1-α)·prev.
    if uniforms.smoothing_alpha < 0.999 {
        let prev = textureLoad(prev_tex, vec2<i32>(gid.xy), 0);
        color = vec4<f32>(
            mix(prev.rgb, color.rgb, uniforms.smoothing_alpha),
            color.a,
        );
    }

    color = vec4<f32>(clamp(color.rgb, vec3<f32>(0.0), vec3<f32>(1.0)), color.a);
    textureStore(output_tex, vec2<i32>(gid.xy), color);
}
