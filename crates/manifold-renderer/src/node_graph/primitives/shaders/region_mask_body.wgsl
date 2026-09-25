// node.region_mask — categorical label rasterizer with optional box filling.
// Labels are uploaded as Rgba8Unorm with red = label / 255. Integer texel
// loads plus round recover the label exactly; filtering is intentionally absent.
// The tracks input is tagged BufferIndex and is available as `buf_tracks`.
// PARAMS: [selection (Enum -> u32), shape (Float -> f32)].
fn contains_box(uv: vec2<f32>, x: f32, y: f32, width: f32, height: f32) -> bool {
    let max_corner = vec2<f32>(x + width, y + height);
    return width > 0.0
        && height > 0.0
        && uv.x >= x
        && uv.x < max_corner.x
        && uv.y >= y
        && uv.y < max_corner.y;
}

fn body(
    tex_labels: texture_2d<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    selection: u32,
    shape: f32,
) -> vec4<f32> {
    let coordinate = vec2<i32>(uv * dims);
    let label = u32(round(textureLoad(tex_labels, coordinate, 0).r * 255.0));
    // `shape` is port-shadowed and can be driven from a scalar wire. Keep the
    // shader-side guard as well so fused regions remain safe for non-finite
    // values that bypass the standalone Rust clamp.
    let safe_shape = select(
        clamp(shape, 0.0, 1.0),
        0.0,
        (bitcast<u32>(shape) & 0x7f800000u) == 0x7f800000u,
    );
    var label_coverage = 0.0;
    var box_coverage = 0.0;

    if selection == 0u {
        for (var i = 0u; i < arrayLength(&buf_tracks); i = i + 1u) {
            let track = buf_tracks[i];
            if track.observed == 0u || track.id == 0u || track.label == 0u {
                continue;
            }
            if track.label == label {
                label_coverage = 1.0;
            }
            if safe_shape > 0.0 && contains_box(uv, track.x, track.y, track.width, track.height) {
                box_coverage = 1.0;
            }
            if label_coverage == 1.0 && (safe_shape == 0.0 || box_coverage == 1.0) {
                break;
            }
        }
    } else {
        var largest_area = -1.0;
        var largest_id = 0xffffffffu;
        var largest_label = 0u;
        var largest_x = 0.0;
        var largest_y = 0.0;
        var largest_width = 0.0;
        var largest_height = 0.0;
        for (var i = 0u; i < arrayLength(&buf_tracks); i = i + 1u) {
            let track = buf_tracks[i];
            if track.observed == 0u || track.id == 0u || track.label == 0u {
                continue;
            }
            if track.area > largest_area || (track.area == largest_area && track.id < largest_id) {
                largest_area = track.area;
                largest_id = track.id;
                largest_label = track.label;
                largest_x = track.x;
                largest_y = track.y;
                largest_width = track.width;
                largest_height = track.height;
            }
        }
        label_coverage = select(0.0, 1.0, largest_label != 0u && label == largest_label);
        if safe_shape > 0.0 {
            box_coverage = select(
                0.0,
                1.0,
                largest_label != 0u
                    && contains_box(uv, largest_x, largest_y, largest_width, largest_height),
            );
        }
    }

    let coverage = mix(label_coverage, box_coverage, safe_shape);
    return vec4<f32>(coverage, coverage, coverage, 1.0);
}
