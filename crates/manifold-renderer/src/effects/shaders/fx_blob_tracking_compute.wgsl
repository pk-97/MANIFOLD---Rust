// Compute variant of fx_blob_tracking.wgsl — same math, no TBDR tile overhead.
// Two entry points:
//   cs_downsample — bilinear blit to smaller storage texture (replaces downsample render pass)
//   cs_main       — overlay pass: reads source + font atlas, draws procedural SDF shapes

struct BlobUniforms {
    amount:           f32,
    blob_count:       i32,
    connection_count: i32,
    _pad0:            f32,
    resolution:       vec2<f32>,
    texel_size:       vec2<f32>,
    blob_center_size: array<vec4<f32>, 8>,
    blob_connections: array<vec4<f32>, 8>,
}

@group(0) @binding(0) var<uniform> uniforms: BlobUniforms;
@group(0) @binding(1) var main_tex:      texture_2d<f32>;
@group(0) @binding(2) var main_sampler:  sampler;
@group(0) @binding(3) var font_tex:      texture_2d<f32>;
@group(0) @binding(4) var point_sampler: sampler;
@group(0) @binding(5) var output_tex: texture_storage_2d<rgba16float, write>;

// ---- Drawing primitives ----
fn line_seg(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>, thickness: f32) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let len_sq = dot(ba, ba);
    if len_sq < 0.000001 { return 0.0; }
    let h = saturate(dot(pa, ba) / len_sq);
    let d = length(pa - ba * h);
    return 1.0 - saturate(d / thickness);
}

fn corner_bracket(p: vec2<f32>, corner: vec2<f32>, dir: vec2<f32>, bracket_len: f32, thickness: f32) -> f32 {
    var cov = 0.0;
    cov = max(cov, line_seg(p, corner, corner - vec2<f32>(dir.x * bracket_len, 0.0), thickness));
    cov = max(cov, line_seg(p, corner, corner - vec2<f32>(0.0, dir.y * bracket_len), thickness));
    return cov;
}

fn h_bar(p: vec2<f32>, origin: vec2<f32>, total_w: f32, fill_frac: f32, h: f32, thickness: f32) -> f32 {
    let tl = origin;
    let tr = origin + vec2<f32>(total_w, 0.0);
    let bl = origin + vec2<f32>(0.0, -h);
    let br = origin + vec2<f32>(total_w, -h);
    var cov = 0.0;
    cov = max(cov, line_seg(p, tl, tr, thickness));
    cov = max(cov, line_seg(p, bl, br, thickness));
    cov = max(cov, line_seg(p, tl, bl, thickness));
    cov = max(cov, line_seg(p, tr, br, thickness));
    let rel = p - origin;
    if rel.x >= 0.0 && rel.x <= total_w * fill_frac && rel.y <= 0.0 && rel.y >= -h {
        cov = max(cov, 0.4);
    }
    return cov;
}

// ---- Font atlas sampling (5x7 pixel glyphs) ----
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

fn draw_char(char_code: f32, p: vec2<f32>, origin: vec2<f32>, pixel_size: f32) -> f32 {
    let local = (p - origin) / pixel_size;
    return sample_glyph(char_code, local);
}

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

fn draw_hex_label(num: f32, p: vec2<f32>, origin: vec2<f32>, pixel_size: f32) -> f32 {
    let local = (p - origin) / pixel_size;
    let n = clamp(floor(num), 0.0, 255.0);
    let hi = floor(n / 16.0);
    let lo = n % 16.0;
    var cov = 0.0;
    cov = max(cov, sample_glyph(0.0, local));
    cov = max(cov, sample_glyph(16.0, local - vec2<f32>(6.0, 0.0)));
    cov = max(cov, sample_glyph(hi, local - vec2<f32>(13.0, 0.0)));
    cov = max(cov, sample_glyph(lo, local - vec2<f32>(19.0, 0.0)));
    return cov;
}

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
    cov = max(cov, sample_glyph(18.0, local - vec2<f32>(17.0, 0.0)));
    cov = max(cov, sample_glyph(y_h, local - vec2<f32>(22.0, 0.0)));
    cov = max(cov, sample_glyph(y_t, local - vec2<f32>(28.0, 0.0)));
    cov = max(cov, sample_glyph(y_o, local - vec2<f32>(34.0, 0.0)));
    return cov;
}

// ---- Compute shader ----
@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }

    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let src = textureSampleLevel(main_tex, main_sampler, uv, 0.0);
    let original = src.rgb;

    // Flip Y for overlay drawing (Unity Y-up convention)
    let draw_uv = vec2<f32>(uv.x, 1.0 - uv.y);

    // Scale drawing sizes relative to 1080p so overlays look the same at any resolution.
    let dpi_scale = uniforms.resolution.y / 1080.0;
    let px_u = uniforms.texel_size.x * dpi_scale;
    let px_v = uniforms.texel_size.y * dpi_scale;
    let line_thick = 2.0 * px_u;
    let thin_line   = 1.5 * px_u;
    let digit_size  = px_u * 2.0;

    // Padding for per-blob AABB early-out. Covers all decorations:
    // ticks (right 20*px_u), hex label (above 20*px_v), gauge (below 60*px_v).
    let pad_x = px_u * 100.0;
    let pad_y = px_v * 70.0;

    var overlay = 0.0;
    let overlay_color = vec3<f32>(0.85, 0.92, 1.0);

    // Draw blob overlays
    for (var b = 0; b < 8; b++) {
        if b >= uniforms.blob_count { break; }

        let blob = uniforms.blob_center_size[b];
        let center    = blob.xy;
        let half_size = blob.zw * 0.5;

        // AABB early-out: skip all drawing for this blob if pixel is far away.
        // Covers brackets, labels, gauge, and ticks with generous padding.
        let box_min = center - half_size - vec2<f32>(pad_x, pad_y);
        let box_max = center + half_size + vec2<f32>(pad_x, pad_y);
        if draw_uv.x < box_min.x || draw_uv.x > box_max.x || draw_uv.y < box_min.y || draw_uv.y > box_max.y {
            continue;
        }

        let bracket_len = min(half_size.x, half_size.y) * 0.4;
        overlay = max(overlay, corner_bracket(draw_uv, center + half_size * vec2<f32>(-1.0, -1.0), vec2<f32>(-1.0, -1.0), bracket_len, line_thick));
        overlay = max(overlay, corner_bracket(draw_uv, center + half_size * vec2<f32>( 1.0, -1.0), vec2<f32>( 1.0, -1.0), bracket_len, line_thick));
        overlay = max(overlay, corner_bracket(draw_uv, center + half_size * vec2<f32>(-1.0,  1.0), vec2<f32>(-1.0,  1.0), bracket_len, line_thick));
        overlay = max(overlay, corner_bracket(draw_uv, center + half_size * vec2<f32>( 1.0,  1.0), vec2<f32>( 1.0,  1.0), bracket_len, line_thick));

        let ch_size = min(half_size.x, half_size.y) * 0.3;
        overlay = max(overlay, line_seg(draw_uv, center - vec2<f32>(ch_size, 0.0), center + vec2<f32>(ch_size, 0.0), thin_line));
        overlay = max(overlay, line_seg(draw_uv, center - vec2<f32>(0.0, ch_size), center + vec2<f32>(0.0, ch_size), thin_line));

        let dot_dist = length(draw_uv - center);
        overlay = max(overlay, 1.0 - saturate(dot_dist / (px_u * 4.0)));

        let hex_pos = center + vec2<f32>(-half_size.x, half_size.y + px_v * 8.0);
        let hex_id  = f32(b) * 17.0 + 48.0;
        overlay = max(overlay, draw_hex_label(hex_id, draw_uv, hex_pos, digit_size));

        let coord_pos = center + vec2<f32>(-half_size.x, -half_size.y - px_v * 38.0);
        overlay = max(overlay, draw_coord_label(center, draw_uv, coord_pos, digit_size));

        let gauge_pos  = center + vec2<f32>(-half_size.x, -half_size.y - px_v * 50.0);
        let gauge_w    = max(half_size.x * 2.0, px_u * 80.0);
        let gauge_fill = saturate(blob.z * blob.w * 20.0);
        overlay = max(overlay, h_bar(draw_uv, gauge_pos, gauge_w, gauge_fill, px_v * 8.0, thin_line));

        let tick_base    = center + vec2<f32>(half_size.x + px_u * 8.0, half_size.y);
        let tick_spacing = half_size.y * 0.5;
        for (var t = 0; t < 4; t++) {
            let tick_start = tick_base - vec2<f32>(0.0, tick_spacing * f32(t));
            let tick_len = select(px_u * 6.0, px_u * 12.0, (u32(t) % 2u) == 0u);
            overlay = max(overlay, line_seg(draw_uv, tick_start, tick_start + vec2<f32>(tick_len, 0.0), thin_line) * 0.5);
        }
    }

    // Connection lines
    for (var c = 0; c < 8; c++) {
        if c >= uniforms.connection_count { break; }

        let conn = uniforms.blob_connections[c];
        let conn_a = conn.xy;
        let conn_b = conn.zw;

        // AABB early-out for connection line + midpoint label
        let conn_pad = vec2<f32>(px_u * 30.0, px_v * 20.0);
        let conn_min = min(conn_a, conn_b) - conn_pad;
        let conn_max = max(conn_a, conn_b) + conn_pad;
        if draw_uv.x < conn_min.x || draw_uv.x > conn_max.x || draw_uv.y < conn_min.y || draw_uv.y > conn_max.y {
            continue;
        }

        let len = length(conn_b - conn_a);
        if len > 0.001 {
            let pa = draw_uv - conn_a;
            let ba = conn_b - conn_a;
            let t_val = saturate(dot(pa, ba) / dot(ba, ba));
            let dash_phase = fract(t_val * len / (px_u * 12.0));
            let dash_mask  = step(0.4, dash_phase);

            overlay = max(overlay, line_seg(draw_uv, conn_a, conn_b, thin_line) * 0.5 * dash_mask);

            let mid = (conn_a + conn_b) * 0.5;
            let mid_dist = length(draw_uv - mid);
            overlay = max(overlay, (1.0 - saturate(mid_dist / (px_u * 5.0))) * 0.4);

            let dist_label_pos = mid + vec2<f32>(px_u * 8.0, px_v * 4.0);
            let dist_val = len * 1000.0;
            overlay = max(overlay, draw_3_digits(dist_val, draw_uv, dist_label_pos, digit_size * 0.7) * 0.6);
        }
    }

    // Scanline (uses screen-space UV, not draw_uv)
    let scanline  = abs(fract(uv.y * uniforms.resolution.y * 0.5) - 0.5) * 2.0;
    let scan_alpha = (1.0 - smoothstep(0.4, 0.5, scanline)) * 0.04;

    // Composite
    let result = mix(original, original + overlay_color * overlay + scan_alpha * overlay_color, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, src.a));
}
