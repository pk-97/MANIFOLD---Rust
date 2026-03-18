// Mechanical port of BlobTrackingEffect.shader.
// Same math, same variable names, same constants, same logic flow.
//
// BlobTrackingEffect.shader line 29: #define MAX_BLOBS 16

// ---- Uniform struct ----
// Must match BlobUniforms in blob_tracking.rs exactly.
struct BlobUniforms {
    amount:           f32,          // _Amount
    blob_count:       i32,          // _BlobCount
    connection_count: i32,          // _ConnectionCount
    _pad0:            f32,
    resolution:       vec2<f32>,    // _Resolution.xy
    texel_size:       vec2<f32>,    // 1/resolution
    // _BlobCenterSize[MAX_BLOBS]: each vec4 is [cx, cy, sw, sh]
    blob_center_size: array<vec4<f32>, 16>,
    // _BlobConnections[MAX_BLOBS]: each vec4 is [ax, ay, bx, by]
    blob_connections: array<vec4<f32>, 16>,
}

@group(0) @binding(0) var<uniform> uniforms: BlobUniforms;
@group(0) @binding(1) var main_tex:      texture_2d<f32>;
@group(0) @binding(2) var main_sampler:  sampler;
@group(0) @binding(3) var font_tex:      texture_2d<f32>;
@group(0) @binding(4) var point_sampler: sampler;

// ---- Vertex shader ----
// Standard fullscreen triangle (matches all other effects in this codebase).
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0)       uv:       vec2<f32>,
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

// ---- Drawing primitives ----
// BlobTrackingEffect.shader lines 64-73 — lineSeg()

fn line_seg(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>, thickness: f32) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let len_sq = dot(ba, ba);
    if len_sq < 0.000001 { return 0.0; }
    let h = saturate(dot(pa, ba) / len_sq);
    let d = length(pa - ba * h);
    return 1.0 - saturate(d / thickness);
}

// BlobTrackingEffect.shader lines 75-81 — cornerBracket()
fn corner_bracket(p: vec2<f32>, corner: vec2<f32>, dir: vec2<f32>, bracket_len: f32, thickness: f32) -> f32 {
    var cov = 0.0;
    cov = max(cov, line_seg(p, corner, corner - vec2<f32>(dir.x * bracket_len, 0.0), thickness));
    cov = max(cov, line_seg(p, corner, corner - vec2<f32>(0.0, dir.y * bracket_len), thickness));
    return cov;
}

// BlobTrackingEffect.shader lines 83-101 — hBar()
fn h_bar(p: vec2<f32>, origin: vec2<f32>, total_w: f32, fill_frac: f32, h: f32, thickness: f32) -> f32 {
    // Outline
    let tl = origin;
    let tr = origin + vec2<f32>(total_w, 0.0);
    let bl = origin + vec2<f32>(0.0, -h);
    let br = origin + vec2<f32>(total_w, -h);
    var cov = 0.0;
    cov = max(cov, line_seg(p, tl, tr, thickness));
    cov = max(cov, line_seg(p, bl, br, thickness));
    cov = max(cov, line_seg(p, tl, bl, thickness));
    cov = max(cov, line_seg(p, tr, br, thickness));
    // Fill
    let rel = p - origin;
    if rel.x >= 0.0 && rel.x <= total_w * fill_frac && rel.y <= 0.0 && rel.y >= -h {
        cov = max(cov, 0.4);
    }
    return cov;
}

// ---- Font atlas sampling (5x7 pixel glyphs) ----
// BlobTrackingEffect.shader lines 103-120 — sampleGlyph()
// Atlas: 16 chars/row, 2 rows, 80x14 px total
// Chars: 0-9, A(10)-F(15), X(16), Y(17), .(18), :(19), %(20)

fn sample_glyph(char_code: f32, local_px: vec2<f32>) -> f32 {
    if local_px.x < 0.0 || local_px.x >= 5.0 || local_px.y < 0.0 || local_px.y >= 7.0 { return 0.0; }

    let c = floor(char_code + 0.5);
    let atlas_col = floor(c % 16.0);
    let atlas_row = floor(c / 16.0);

    var atlas_uv: vec2<f32>;
    atlas_uv.x = (atlas_col * 5.0 + floor(local_px.x) + 0.5) / 80.0;
    atlas_uv.y = (atlas_row * 7.0 + floor(local_px.y) + 0.5) / 14.0;

    return textureSampleLevel(font_tex, point_sampler, atlas_uv, 0.0).r;
}

// BlobTrackingEffect.shader lines 122-126 — drawChar()
fn draw_char(char_code: f32, p: vec2<f32>, origin: vec2<f32>, pixel_size: f32) -> f32 {
    let local = (p - origin) / pixel_size;
    return sample_glyph(char_code, local);
}

// BlobTrackingEffect.shader lines 128-141 — draw3Digits()
// Draw a 3-digit number (000-999)
fn draw_3_digits(num: f32, p: vec2<f32>, origin: vec2<f32>, pixel_size: f32) -> f32 {
    let local = (p - origin) / pixel_size;
    let n = clamp(floor(num), 0.0, 999.0);
    let hundreds = floor(n / 100.0);
    let tens = floor((n % 100.0) / 10.0);
    let ones = n % 10.0;
    var cov = 0.0;
    cov = max(cov, sample_glyph(hundreds, local));
    cov = max(cov, sample_glyph(tens, local - vec2<f32>(6.0, 0.0)));
    cov = max(cov, sample_glyph(ones, local - vec2<f32>(12.0, 0.0)));
    return cov;
}

// BlobTrackingEffect.shader lines 143-156 — drawHexLabel()
// Draw "0x" prefix + 2 hex digits
fn draw_hex_label(num: f32, p: vec2<f32>, origin: vec2<f32>, pixel_size: f32) -> f32 {
    let local = (p - origin) / pixel_size;
    let n = clamp(floor(num), 0.0, 255.0);
    let hi = floor(n / 16.0);
    let lo = n % 16.0;
    var cov = 0.0;
    cov = max(cov, sample_glyph(0.0, local));                         // '0'
    cov = max(cov, sample_glyph(16.0, local - vec2<f32>(6.0, 0.0)));  // 'X'
    cov = max(cov, sample_glyph(hi, local - vec2<f32>(13.0, 0.0)));
    cov = max(cov, sample_glyph(lo, local - vec2<f32>(19.0, 0.0)));
    return cov;
}

// BlobTrackingEffect.shader lines 158-181 — drawCoordLabel()
// Draw coordinate readout: "XXX.YYY"
fn draw_coord_label(coord: vec2<f32>, p: vec2<f32>, origin: vec2<f32>, pixel_size: f32) -> f32 {
    let local = (p - origin) / pixel_size;
    let x_val = clamp(floor(coord.x * 999.0), 0.0, 999.0);
    let y_val = clamp(floor(coord.y * 999.0), 0.0, 999.0);

    let x_h = floor(x_val / 100.0);
    let x_t = floor((x_val % 100.0) / 10.0);
    let x_o = x_val % 10.0;
    let y_h = floor(y_val / 100.0);
    let y_t = floor((y_val % 100.0) / 10.0);
    let y_o = y_val % 10.0;

    var cov = 0.0;
    cov = max(cov, sample_glyph(x_h, local));
    cov = max(cov, sample_glyph(x_t, local - vec2<f32>(6.0, 0.0)));
    cov = max(cov, sample_glyph(x_o, local - vec2<f32>(12.0, 0.0)));
    cov = max(cov, sample_glyph(18.0, local - vec2<f32>(17.0, 0.0)));  // '.'
    cov = max(cov, sample_glyph(y_h, local - vec2<f32>(22.0, 0.0)));
    cov = max(cov, sample_glyph(y_t, local - vec2<f32>(28.0, 0.0)));
    cov = max(cov, sample_glyph(y_o, local - vec2<f32>(34.0, 0.0)));
    return cov;
}

// ---- Fragment shader ----
// BlobTrackingEffect.shader lines 183-287

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(main_tex, main_sampler, in.uv);
    let original = src.rgb;

    // BlobTrackingEffect.shader lines 188-193
    let px_u = uniforms.texel_size.x;
    let px_v = uniforms.texel_size.y;
    let line_thick = 2.0 * px_u;
    let thin_line   = 1.5 * px_u;
    let digit_size  = px_u * 2.0;

    var overlay = 0.0;
    // BlobTrackingEffect.shader line 195: overlayColor = float3(0.85, 0.92, 1.0)
    let overlay_color = vec3<f32>(0.85, 0.92, 1.0);

    // Draw blob overlays
    // BlobTrackingEffect.shader lines 198-248
    for (var b = 0; b < 16; b++) {
        if b >= uniforms.blob_count { break; }

        let blob = uniforms.blob_center_size[b];
        let center    = blob.xy;
        let half_size = blob.zw * 0.5;

        // Corner brackets
        // BlobTrackingEffect.shader line 207
        let bracket_len = min(half_size.x, half_size.y) * 0.4;
        overlay = max(overlay, corner_bracket(in.uv, center + half_size * vec2<f32>(-1.0, -1.0), vec2<f32>(-1.0, -1.0), bracket_len, line_thick));
        overlay = max(overlay, corner_bracket(in.uv, center + half_size * vec2<f32>( 1.0, -1.0), vec2<f32>( 1.0, -1.0), bracket_len, line_thick));
        overlay = max(overlay, corner_bracket(in.uv, center + half_size * vec2<f32>(-1.0,  1.0), vec2<f32>(-1.0,  1.0), bracket_len, line_thick));
        overlay = max(overlay, corner_bracket(in.uv, center + half_size * vec2<f32>( 1.0,  1.0), vec2<f32>( 1.0,  1.0), bracket_len, line_thick));

        // Crosshair at center
        // BlobTrackingEffect.shader lines 213-216
        let ch_size = min(half_size.x, half_size.y) * 0.3;
        overlay = max(overlay, line_seg(in.uv, center - vec2<f32>(ch_size, 0.0), center + vec2<f32>(ch_size, 0.0), thin_line));
        overlay = max(overlay, line_seg(in.uv, center - vec2<f32>(0.0, ch_size), center + vec2<f32>(0.0, ch_size), thin_line));

        // Center dot
        // BlobTrackingEffect.shader lines 218-220
        let dot_dist = length(in.uv - center);
        overlay = max(overlay, 1.0 - saturate(dot_dist / (px_u * 4.0)));

        // ---- Data labels ----

        // Hex ID label: top-left, outside bracket
        // BlobTrackingEffect.shader lines 225-227
        let hex_pos = center + vec2<f32>(-half_size.x, half_size.y + px_v * 8.0);
        let hex_id  = f32(b) * 17.0 + 48.0;
        overlay = max(overlay, draw_hex_label(hex_id, in.uv, hex_pos, digit_size));

        // Coordinate readout: bottom-left, outside bracket
        // BlobTrackingEffect.shader lines 229-231
        let coord_pos = center + vec2<f32>(-half_size.x, -half_size.y - px_v * 38.0);
        overlay = max(overlay, draw_coord_label(center, in.uv, coord_pos, digit_size));

        // Size gauge bar: bottom, below coords
        // BlobTrackingEffect.shader lines 233-237
        let gauge_pos  = center + vec2<f32>(-half_size.x, -half_size.y - px_v * 50.0);
        let gauge_w    = max(half_size.x * 2.0, px_u * 80.0);
        let gauge_fill = saturate(blob.z * blob.w * 20.0);
        overlay = max(overlay, h_bar(in.uv, gauge_pos, gauge_w, gauge_fill, px_v * 8.0, thin_line));

        // Tick marks on right side of box
        // BlobTrackingEffect.shader lines 239-247
        let tick_base    = center + vec2<f32>(half_size.x + px_u * 8.0, half_size.y);
        let tick_spacing = half_size.y * 0.5;
        for (var t = 0; t < 4; t++) {
            let tick_start = tick_base - vec2<f32>(0.0, tick_spacing * f32(t));
            // BlobTrackingEffect.shader line 245:
            // ((uint)t % 2u == 0u) ? pxU * 12.0 : pxU * 6.0
            let tick_len = select(px_u * 6.0, px_u * 12.0, (u32(t) % 2u) == 0u);
            overlay = max(overlay, line_seg(in.uv, tick_start, tick_start + vec2<f32>(tick_len, 0.0), thin_line) * 0.5);
        }
    }

    // Connection lines
    // BlobTrackingEffect.shader lines 251-278
    for (var c = 0; c < 16; c++) {
        if c >= uniforms.connection_count { break; }

        let conn = uniforms.blob_connections[c];
        let len = length(conn.zw - conn.xy);
        if len > 0.001 {
            // Dashed effect: use frac of distance along line
            // BlobTrackingEffect.shader lines 260-266
            let pa = in.uv - conn.xy;
            let ba = conn.zw - conn.xy;
            let t_val = saturate(dot(pa, ba) / dot(ba, ba));
            let dash_phase = fract(t_val * len / (px_u * 12.0));
            let dash_mask  = step(0.4, dash_phase);

            overlay = max(overlay, line_seg(in.uv, conn.xy, conn.zw, thin_line) * 0.5 * dash_mask);

            // Midpoint diamond
            // BlobTrackingEffect.shader lines 268-271
            let mid = (conn.xy + conn.zw) * 0.5;
            let mid_dist = length(in.uv - mid);
            overlay = max(overlay, (1.0 - saturate(mid_dist / (px_u * 5.0))) * 0.4);

            // Distance readout at midpoint
            // BlobTrackingEffect.shader lines 273-276
            let dist_label_pos = mid + vec2<f32>(px_u * 8.0, px_v * 4.0);
            let dist_val = len * 1000.0;
            overlay = max(overlay, draw_3_digits(dist_val, in.uv, dist_label_pos, digit_size * 0.7) * 0.6);
        }
    }

    // Subtle scanline
    // BlobTrackingEffect.shader lines 280-282
    let scanline  = abs(fract(in.uv.y * uniforms.resolution.y * 0.5) - 0.5) * 2.0;
    let scan_alpha = (1.0 - smoothstep(0.4, 0.5, scanline)) * 0.04;

    // Composite
    // BlobTrackingEffect.shader line 285:
    // lerp(original, original + overlayColor * overlay + scanAlpha * overlayColor, _Amount)
    let result = mix(original, original + overlay_color * overlay + scan_alpha * overlay_color, uniforms.amount);
    return vec4<f32>(result, src.a);
}
