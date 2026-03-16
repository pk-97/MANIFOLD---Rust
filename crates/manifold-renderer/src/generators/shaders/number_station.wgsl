struct Uniforms {
    time_val: f32,
    beat: f32,
    aspect_ratio: f32,
    anim_speed: f32,
    mode: f32,
    density: f32,
    font_size: f32,
    glow: f32,
    flicker: f32,
    color_mode: f32,
    columns: f32,
    trigger_count: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Fullscreen triangle — 3 vertices, no vertex buffer
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// ── Hash functions for pseudo-random ──

fn hash11(p: f32) -> f32 {
    var p2 = fract(p * 0.1031);
    p2 = p2 * (p2 + 33.33);
    p2 = p2 * (p2 + p2);
    return fract(p2);
}

fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(p * vec2<f32>(0.1031, 0.1030));
    p3 = p3 + vec2<f32>(dot(p3, p3.yx + 33.33));
    return fract((p3.x + p3.y) * p3.x);
}

// ── 7-segment display ──
// Segment layout in a 4x5 grid:
//  _
// |_|
// |_|
//
// Segments: 0=top, 1=top-right, 2=bottom-right, 3=bottom,
//           4=bottom-left, 5=top-left, 6=middle

// Segment bitmasks for digits 0-F
// Bit 0=top, 1=top-right, 2=bot-right, 3=bottom, 4=bot-left, 5=top-left, 6=middle
fn digit_segments(digit: i32) -> i32 {
    // 0: top, tr, br, bot, bl, tl       = 0b0111111 = 63
    // 1: tr, br                         = 0b0000110 = 6
    // 2: top, tr, mid, bl, bot          = 0b1011011 = 91
    // 3: top, tr, mid, br, bot          = 0b1001111 = 79
    // 4: tl, mid, tr, br               = 0b1100110 = 102
    // 5: top, tl, mid, br, bot         = 0b1101101 = 109
    // 6: top, tl, mid, br, bot, bl     = 0b1111101 = 125
    // 7: top, tr, br                    = 0b0000111 = 7
    // 8: all                            = 0b1111111 = 127
    // 9: top, tl, tr, mid, br, bot     = 0b1101111 = 111
    // A: top, tl, tr, mid, bl, br      = 0b1110111 = 119
    // B: tl, mid, bl, br, bot          = 0b1111100 = 124
    // C: top, tl, bl, bot              = 0b0111001 = 57
    // D: tr, mid, bl, br, bot          = 0b1011110 = 94
    // E: top, tl, mid, bl, bot         = 0b1111001 = 121
    // F: top, tl, mid, bl             = 0b1110001 = 113
    switch digit {
        case 0: { return 63; }
        case 1: { return 6; }
        case 2: { return 91; }
        case 3: { return 79; }
        case 4: { return 102; }
        case 5: { return 109; }
        case 6: { return 125; }
        case 7: { return 7; }
        case 8: { return 127; }
        case 9: { return 111; }
        case 10: { return 119; } // A
        case 11: { return 124; } // b
        case 12: { return 57; }  // C
        case 13: { return 94; }  // d
        case 14: { return 121; } // E
        case 15: { return 113; } // F
        default: { return 0; }
    }
}

// Render a single segment as an SDF
// p is in [0,1] x [0,1] cell-local coordinates (4 wide, 5 tall conceptually)
fn render_segment(p: vec2<f32>, seg: i32) -> f32 {
    let thickness = 0.12;
    var d: f32 = 1000.0;

    // Segments map to rectangular regions
    // Cell is [0,1] x [0,1], digit drawn in center portion
    let margin = 0.15;
    let left = margin;
    let right = 1.0 - margin;
    let top = margin;
    let mid_y = 0.5;
    let bottom = 1.0 - margin;

    // Horizontal segments (top=0, middle=6, bottom=3)
    if (seg == 0) {
        // Top horizontal
        let center = vec2<f32>(0.5, top);
        let half = vec2<f32>((right - left) * 0.5, thickness);
        let dp = abs(p - center) - half;
        d = max(dp.x, dp.y);
    } else if (seg == 6) {
        // Middle horizontal
        let center = vec2<f32>(0.5, mid_y);
        let half = vec2<f32>((right - left) * 0.5, thickness);
        let dp = abs(p - center) - half;
        d = max(dp.x, dp.y);
    } else if (seg == 3) {
        // Bottom horizontal
        let center = vec2<f32>(0.5, bottom);
        let half = vec2<f32>((right - left) * 0.5, thickness);
        let dp = abs(p - center) - half;
        d = max(dp.x, dp.y);
    }
    // Vertical segments
    else if (seg == 5) {
        // Top-left vertical
        let center = vec2<f32>(left, (top + mid_y) * 0.5);
        let half = vec2<f32>(thickness, (mid_y - top) * 0.5);
        let dp = abs(p - center) - half;
        d = max(dp.x, dp.y);
    } else if (seg == 1) {
        // Top-right vertical
        let center = vec2<f32>(right, (top + mid_y) * 0.5);
        let half = vec2<f32>(thickness, (mid_y - top) * 0.5);
        let dp = abs(p - center) - half;
        d = max(dp.x, dp.y);
    } else if (seg == 4) {
        // Bottom-left vertical
        let center = vec2<f32>(left, (mid_y + bottom) * 0.5);
        let half = vec2<f32>(thickness, (bottom - mid_y) * 0.5);
        let dp = abs(p - center) - half;
        d = max(dp.x, dp.y);
    } else if (seg == 2) {
        // Bottom-right vertical
        let center = vec2<f32>(right, (mid_y + bottom) * 0.5);
        let half = vec2<f32>(thickness, (bottom - mid_y) * 0.5);
        let dp = abs(p - center) - half;
        d = max(dp.x, dp.y);
    }

    return d;
}

// Render a complete digit at position, returns intensity
fn render_digit(p: vec2<f32>, digit: i32) -> f32 {
    let segs = digit_segments(digit);
    var intensity: f32 = 0.0;

    for (var s: i32 = 0; s < 7; s = s + 1) {
        if ((segs & (1 << s)) != 0) {
            let d = render_segment(p, s);
            let aa = 0.02;
            intensity = max(intensity, 1.0 - smoothstep(-aa, aa, d));
        }
    }

    return intensity;
}

// Get the digit to display at a given cell position
fn get_digit(col: i32, row: i32, mode: i32, time_seed: f32) -> i32 {
    let cell_hash = hash21(vec2<f32>(f32(col) + 0.5, f32(row) + 0.5 + time_seed));

    switch mode {
        // Hex: 0-F
        case 0: { return i32(cell_hash * 16.0); }
        // Binary: 0-1
        case 1: {
            if (cell_hash > 0.5) { return 1; } else { return 0; }
        }
        // Decimal: 0-9
        case 2: { return i32(cell_hash * 10.0); }
        // Mixed: random mode per cell
        default: {
            let sub_mode = i32(hash21(vec2<f32>(f32(col) * 7.31, f32(row) * 13.17)) * 3.0);
            if (sub_mode == 0) { return i32(cell_hash * 16.0); }
            else if (sub_mode == 1) {
                if (cell_hash > 0.5) { return 1; } else { return 0; }
            }
            else { return i32(cell_hash * 10.0); }
        }
    }
}

// Get color based on color mode
fn get_color(mode: i32) -> vec3<f32> {
    switch mode {
        case 1: { return vec3<f32>(1.0, 0.7, 0.0); }      // Amber
        case 2: { return vec3<f32>(0.85, 0.85, 0.85); }    // White
        case 3: { return vec3<f32>(0.0, 0.8, 0.9); }       // Cyan
        default: { return vec3<f32>(0.0, 0.9, 0.0); }      // Green
    }
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let num_cols = i32(u.columns);
    let cell_aspect = 0.6; // digits are taller than wide
    let num_rows = i32(f32(num_cols) / (u.aspect_ratio * cell_aspect));
    let actual_rows = max(num_rows, 1);

    // Grid cell coordinates
    let cell_x = in.uv.x * f32(num_cols);
    let cell_y = in.uv.y * f32(actual_rows);

    let col = i32(floor(cell_x));
    let row = i32(floor(cell_y));

    // Local UV within the cell [0,1]
    let local_uv = vec2<f32>(fract(cell_x), fract(cell_y));

    // Per-column scroll speed variation
    let col_speed = (hash11(f32(col) * 17.31) * 0.7 + 0.3) * u.anim_speed;

    // Time seed for digit changes — floor for discrete stepping
    let time_seed = floor(u.time_val * col_speed * 3.0);

    // Density gating: some cells are dark
    let density_hash = hash21(vec2<f32>(f32(col) * 3.71, f32(row) * 5.13 + time_seed * 0.1));
    if (density_hash > u.density) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    // Get digit value
    let mode = i32(u.mode);
    let digit = get_digit(col, row, mode, time_seed);

    // Scale local UV by font size (centered)
    let font_scale = 1.0 / u.font_size;
    let scaled_uv = (local_uv - 0.5) * font_scale + 0.5;

    // Render the 7-segment digit
    var intensity = render_digit(scaled_uv, digit);

    // Glow effect: softer falloff around segments
    if (u.glow > 0.0) {
        let glow_scale = 1.0 / (u.font_size * 0.8);
        let glow_uv = (local_uv - 0.5) * glow_scale + 0.5;
        let glow_intensity = render_digit(glow_uv, digit);
        intensity = max(intensity, glow_intensity * u.glow * 0.5);
    }

    // Flicker: random per-cell brightness modulation
    if (u.flicker > 0.0) {
        let flicker_hash = hash21(vec2<f32>(f32(col) + time_seed * 0.37, f32(row) * 2.71));
        let flicker_amount = 1.0 - u.flicker * (1.0 - flicker_hash);
        intensity *= flicker_amount;
    }

    // Scanline effect
    let scanline = 0.85 + 0.15 * sin(in.uv.y * f32(actual_rows) * 3.14159 * 2.0);
    intensity *= scanline;

    // Apply color
    let base_color = get_color(i32(u.color_mode));
    let color = base_color * intensity;

    return vec4<f32>(color, max(color.r, max(color.g, color.b)));
}
