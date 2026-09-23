// node.region_mask — categorical label rasterizer.
// Labels are uploaded as Rgba8Unorm with red = label / 255. Integer texel
// loads plus round recover the label exactly; filtering is intentionally absent.
// The tracks input is tagged BufferIndex and is available as `buf_tracks`.
// PARAMS: [selection (Enum -> u32)].
fn body(
    tex_labels: texture_2d<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    selection: u32,
) -> vec4<f32> {
    let coordinate = vec2<i32>(uv * dims);
    let label = u32(round(textureLoad(tex_labels, coordinate, 0).r * 255.0));
    var selected_label = 0u;

    if selection == 0u {
        for (var i = 0u; i < arrayLength(&buf_tracks); i = i + 1u) {
            let track = buf_tracks[i];
            if track.observed != 0u && track.id != 0u && track.label != 0u && track.label == label {
                selected_label = label;
                break;
            }
        }
    } else {
        var largest_area = -1.0;
        var largest_id = 0xffffffffu;
        for (var i = 0u; i < arrayLength(&buf_tracks); i = i + 1u) {
            let track = buf_tracks[i];
            if track.observed == 0u || track.id == 0u || track.label == 0u {
                continue;
            }
            if track.area > largest_area || (track.area == largest_area && track.id < largest_id) {
                largest_area = track.area;
                largest_id = track.id;
                selected_label = track.label;
            }
        }
    }

    let coverage = select(0.0, 1.0, selected_label != 0u && label == selected_label);
    return vec4<f32>(coverage, coverage, coverage, 1.0);
}
