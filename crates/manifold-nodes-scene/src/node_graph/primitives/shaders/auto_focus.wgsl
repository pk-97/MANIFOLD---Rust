// node.auto_focus: the median raw depth of nine points around the frame
// centre, 5% of the frame apart, for the CPU to read back a frame later.
// The median of raw depth is the median of linear depth (the mapping is
// monotonic), so the CPU linearizes one value.
@group(0) @binding(0) var depth_tex: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> probe: array<f32>;

@compute @workgroup_size(1)
fn cs_main() {
    let dims = textureDimensions(depth_tex);
    var z: array<f32, 9>;
    for (var i = 0u; i < 9u; i++) {
        let cell = vec2<f32>(f32(i % 3u), f32(i / 3u)) - 1.0;
        let p = min(vec2<u32>((0.5 + cell * 0.05) * vec2<f32>(dims)), dims - 1u);
        z[i] = textureLoad(depth_tex, p, 0).r;
    }
    for (var i = 1u; i < 9u; i++) {
        let v = z[i];
        var j = i;
        while j > 0u && z[j - 1u] > v {
            z[j] = z[j - 1u];
            j--;
        }
        z[j] = v;
    }
    probe[0] = z[4];
}
