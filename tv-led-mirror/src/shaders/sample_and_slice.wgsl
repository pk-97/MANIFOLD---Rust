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
        // scales with `blur_radius` in source-texel units.
        let tex_size = vec2<f32>(textureDimensions(source_tex, 0));
        let r = uniforms.blur_radius / tex_size;
        let w = array<f32, 5>(1.0, 4.0, 6.0, 4.0, 1.0);
        var sum = vec4<f32>(0.0);
        var total_w = 0.0;
        for (var dy = 0; dy < 5; dy = dy + 1) {
            for (var dx = 0; dx < 5; dx = dx + 1) {
                let ox = (f32(dx) - 2.0) * r.x;
                let oy = (f32(dy) - 2.0) * r.y;
                let weight = w[dx] * w[dy];
                sum = sum + textureSampleLevel(
                    source_tex, tex_sampler,
                    center + vec2<f32>(ox, oy),
                    0.0,
                ) * weight;
                total_w = total_w + weight;
            }
        }
        color = sum / total_w;
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

    textureStore(output_tex, vec2<i32>(gid.xy), color);
}
