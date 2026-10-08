// node.mesh_spatial_mask — current mesh gather body, returning one f32 weight.
//
// ABI: `in` stays BufferGather (the body indexes buf_in at neighbour corners
// for triangle-centroid / patch-cell sampling, so the standalone wrapper
// binds it whole and the fused region keeps it as a gathered external);
// `weights` is COINCIDENT — a per-element `e_weights` pre-read, gated by the
// `idx < weights_len` check (0 when unwired → identity, matching run()).
fn rotate_basis(v: vec3<f32>, yaw: f32, pitch: f32) -> vec3<f32> {
    let cp = cos(pitch);
    let sp = sin(pitch);
    let rx = vec3<f32>(v.x, cp * v.y - sp * v.z, sp * v.y + cp * v.z);
    let cy = cos(yaw);
    let sy = sin(yaw);
    return vec3<f32>(cy * rx.x + sy * rx.z, rx.y, -sy * rx.x + cy * rx.z);
}

fn body(idx: u32, count: u32, e_weights: f32, shape: u32, sample_mode: u32, center_x: f32, center_y: f32, center_z: f32, yaw: f32, pitch: f32, width: f32, feather: f32, invert: f32, amount: f32, scale: f32, source_offset_x: f32, source_offset_y: f32, source_offset_z: f32, cell_size: f32, low: f32, high: f32, weights_len: u32) -> f32 {
    let base = (idx / 3u) * 3u;
    var sample_position = buf_in[idx].position;
    if (sample_mode == 1u || sample_mode == 2u) && base + 2u < count {
        sample_position = (buf_in[base].position + buf_in[base + 1u].position + buf_in[base + 2u].position) / 3.0;
    }
    let safe_scale = max(abs(scale), 1e-6);
    let p_center = vec3<f32>(center_x, center_y, center_z);
    let source_offset = vec3<f32>(source_offset_x, source_offset_y, source_offset_z);
    var p = (sample_position + source_offset) / safe_scale - p_center;
    if sample_mode == 2u {
        p = patch_cell_center((sample_position + source_offset) / safe_scale, cell_size) - p_center;
    }
    let direction = rotate_basis(vec3<f32>(0.0, 1.0, 0.0), yaw, pitch);
    var distance = abs(dot(p, direction)) - width;
    if shape == 1u {
        distance = length(p) - width;
    } else if shape == 2u {
        distance = dot(p, direction) - width;
    }
    let edge = max(feather, 0.0);
    var mask = select(1.0, 0.0, distance > 0.0);
    if edge > 0.0 {
        mask = 1.0 - smoothstep(0.0, edge, distance);
    }
    let mask_after_invert = mix(mask, 1.0 - mask, clamp(invert, 0.0, 1.0));
    var incoming = 1.0;
    if idx < weights_len { incoming = e_weights; }
    return incoming * mix(1.0, mix(low, high, mask_after_invert), clamp(amount, 0.0, 1.0));
}
