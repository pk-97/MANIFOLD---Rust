// Sample-and-slice compute shader for tv-led-mirror.
//
// Reads the captured-screen IOSurface as Bgra8UnormSrgb so the GPU's
// hardware bilinear filter mixes pixels in *linear* space (sRGB→linear
// happens per-tap before filtering — averaging in sRGB byte-space is what
// would desaturate vibrant colors). Maps each output column to a slice of
// the LEFT or RIGHT edge of the source per the user's CLI widths, blurs
// (5×5 binomial), optionally soft-gates dim regions, and writes linear
// values to the strip×LED output. The downstream LED edge-extend pass
// treats this strip×LED texture as a per-strip pre-sliced source (its
// hardcoded 0.5/0.5 widths act as identity at strip-aligned input
// resolution), so DMX bytes match perceptual brightness on the TV.

struct Uniforms {
    blur_radius: f32,     // in source texels; 0 = no blur
    luminance_floor: f32, // soft-gate: pixels with Y below this fade out
    luminance_knee: f32,  // soft-gate: width of the smoothstep transition
    saturation_floor: f32,// soft-gate: pixels below this HSV saturation fade out
    saturation_knee: f32, // soft-gate: width of the smoothstep transition
    // Inset margins as fractions of the source. The slicer stretches the
    // strip×LED output grid across the cropped content rectangle
    // [crop_left, 1-crop_right] × [crop_top, 1-crop_bottom] so HUD chrome
    // at the screen edges is excluded.
    crop_left: f32,
    crop_right: f32,
    crop_top: f32,
    crop_bottom: f32,
    // Vibrance multiplier in linear space. 1.0 = no change, >1 boosts color
    // toward the LED's punchier look, <1 desaturates. Mixes around BT.709
    // luma so neutral grays stay neutral.
    vibrance: f32,
    // Output gamma. 1.0 = linear (current behavior — pow(x, 1) = x).
    // 2.2 = perceptual: maps linear photons to a curve that matches how the
    // eye sees screen mid-tones, so a "50% grey" pixel doesn't blast the LED
    // at 50% PWM (which looks much brighter perceptually than a screen at 50%).
    gamma: f32,
    // Saturation bias on the blur weights. 0 = pure binomial average (a small
    // bright orange region averages with dark surroundings into a smeared
    // warm-white). >0 multiplies each tap's binomial weight by
    // (1 + bias·sat²) so brightly-colored taps pull harder, preserving punchy
    // colors against desaturated backgrounds. 4-10 is a useful range.
    saturation_bias: f32,
    // Per-channel white balance trim (R, G, B). For SK9822 strips that skew
    // cool-white (~7500K) vs the TV's D65 (~6500K), pull blue down.
    wb_r: f32,
    wb_g: f32,
    wb_b: f32,
    // Output luminance ceiling. <1 caps how bright the LEDs can ever go;
    // RGB is rescaled to preserve chroma when the cap engages.
    max_luminance: f32,
    // Below this output value (per channel, post-gamma), drop to 0. Kills
    // the flickery low-PWM region where SK9822 strips strobe rather than
    // dim cleanly. Try 0.015 (= 4/255).
    black_floor: f32,
    // HDR peak: max linear input value rolled off to 1.0 via Reinhard
    // (extended). 1.0 = SDR (effectively no-op). Higher = preserve more
    // headroom — values above 1.0 in extendedLinearSRGB get squashed
    // smoothly instead of clipped. Applied right after sample.
    hdr_peak: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }

    let raw_uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    // Stretch the strip×LED grid across the entire cropped content rectangle.
    // strip 0 → leftmost column of content, strip N-1 → rightmost. Each LED
    // row is a horizontal slice through the cropped region.
    let content_left = uniforms.crop_left;
    let content_right = 1.0 - uniforms.crop_right;
    let content_top = uniforms.crop_top;
    let content_bottom = 1.0 - uniforms.crop_bottom;
    let content_w = max(content_right - content_left, 0.0001);
    let content_h = max(content_bottom - content_top, 0.0001);

    let source_u = content_left + raw_uv.x * content_w;
    let source_v = content_top + raw_uv.y * content_h;
    let center = vec2<f32>(source_u, source_v);

    var color: vec4<f32>;
    if uniforms.blur_radius <= 0.0 {
        color = textureSampleLevel(source_tex, tex_sampler, center, 0.0);
    } else {
        // 5×5 binomial weights sampled in 2D (25 taps). Inter-tap spacing
        // scales with `blur_radius` in source-texel units. Each tap's binomial
        // weight is multiplied by (1 + saturation_bias · sat²) so that small
        // bright-colored regions (an orange flame against dark) punch through
        // instead of getting smeared into the desaturated linear average.
        let tex_size = vec2<f32>(textureDimensions(source_tex, 0));
        let r = uniforms.blur_radius / tex_size;
        let w = array<f32, 5>(1.0, 4.0, 6.0, 4.0, 1.0);
        var sum = vec4<f32>(0.0);
        var total_w = 0.0;
        for (var dy = 0; dy < 5; dy = dy + 1) {
            for (var dx = 0; dx < 5; dx = dx + 1) {
                let ox = (f32(dx) - 2.0) * r.x;
                let oy = (f32(dy) - 2.0) * r.y;
                let tap = textureSampleLevel(
                    source_tex, tex_sampler,
                    center + vec2<f32>(ox, oy),
                    0.0,
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
        color = sum / total_w;
    }

    // HDR roll-off (Reinhard extended). Maps [0, hdr_peak] → [0, 1] smoothly,
    // preserving headroom that would otherwise clip. peak=1.0 is identity for
    // SDR content. Applied per-channel so saturated highlights keep their hue.
    if uniforms.hdr_peak > 1.0001 {
        let p = uniforms.hdr_peak;
        let safe = max(color.rgb, vec3<f32>(0.0));
        let mapped = safe * (1.0 + safe / (p * p)) / (1.0 + safe);
        color = vec4<f32>(mapped, color.a);
    }

    // Soft luminance gate. Linear-luma weights (BT.709). Below `floor` the
    // pixel fades to black; above `floor + knee` it passes through. In
    // between, smoothstep gives a smooth fade so dark scenes don't bleed
    // grey ambient onto the wall but highlights stay vivid.
    if uniforms.luminance_floor > 0.0 {
        let y = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        let knee = max(uniforms.luminance_knee, 0.0001);
        let gate = smoothstep(uniforms.luminance_floor, uniforms.luminance_floor + knee, y);
        color = vec4<f32>(color.rgb * gate, color.a);
    }

    // Soft saturation gate. White desktops, document editors, and other
    // achromatic content have ~0 saturation and would otherwise blast the
    // LEDs full-white because they're high-luminance. HSV saturation =
    // (max - min) / max — pure white = 0, pure red/blue/green = 1. Below
    // `floor` we fade to black; above `floor + knee` we pass through.
    if uniforms.saturation_floor > 0.0 {
        let mx = max(color.r, max(color.g, color.b));
        let mn = min(color.r, min(color.g, color.b));
        let sat = select(0.0, (mx - mn) / mx, mx > 0.0001);
        let knee = max(uniforms.saturation_knee, 0.0001);
        let gate = smoothstep(uniforms.saturation_floor, uniforms.saturation_floor + knee, sat);
        color = vec4<f32>(color.rgb * gate, color.a);
    }

    // Vibrance: mix around the BT.709 gray equivalent. >1 boosts saturation
    // (good against the LEDs' diffuse look), <1 desaturates. 1.0 = no-op.
    if abs(uniforms.vibrance - 1.0) > 0.0001 {
        let y = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        color = vec4<f32>(mix(vec3<f32>(y), color.rgb, uniforms.vibrance), color.a);
    }

    // Output gamma. Squashes mid-tones if gamma > 1 so 50%-grey pixels stop
    // blasting the LEDs perceptually. Linear→perceptual transform; runs
    // before the white-balance trim and luminance ceiling so those operate
    // on output-space values.
    if abs(uniforms.gamma - 1.0) > 0.0001 {
        let safe = max(color.rgb, vec3<f32>(0.0));
        color = vec4<f32>(pow(safe, vec3<f32>(uniforms.gamma)), color.a);
    }

    // White-balance trim. SK9822 strips skew ~7500K cool-white; multiplying
    // blue (and slightly green) below 1.0 pulls them toward D65 to match a
    // typical TV white point. Per-channel multiplier; identity at 1/1/1.
    color = vec4<f32>(
        color.r * uniforms.wb_r,
        color.g * uniforms.wb_g,
        color.b * uniforms.wb_b,
        color.a,
    );

    // Output luminance ceiling. Cap the perceived brightness without
    // dragging colors toward gray — scale RGB uniformly so chroma is
    // preserved while Y stays at or below the cap.
    if uniforms.max_luminance < 0.999 {
        let y = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        if y > uniforms.max_luminance {
            let scale = uniforms.max_luminance / max(y, 0.0001);
            color = vec4<f32>(color.rgb * scale, color.a);
        }
    }

    // Black floor: anything below the threshold becomes 0 per channel.
    // Eliminates the flickery sub-PWM region where SK9822 strips strobe.
    if uniforms.black_floor > 0.0 {
        color = vec4<f32>(
            select(0.0, color.r, color.r > uniforms.black_floor),
            select(0.0, color.g, color.g > uniforms.black_floor),
            select(0.0, color.b, color.b > uniforms.black_floor),
            color.a,
        );
    }

    color = vec4<f32>(clamp(color.rgb, vec3<f32>(0.0), vec3<f32>(1.0)), color.a);
    textureStore(output_tex, vec2<i32>(gid.xy), color);
}
