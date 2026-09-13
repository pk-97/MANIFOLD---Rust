// node.mesh_stagger_envelope — current mesh gather body, returning f32.
fn rotate_basis(v: vec3<f32>, yaw: f32, pitch: f32) -> vec3<f32> {
    let cp = cos(pitch);
    let sp = sin(pitch);
    let rx = vec3<f32>(v.x, cp * v.y - sp * v.z, sp * v.y + cp * v.z);
    let cy = cos(yaw);
    let sy = sin(yaw);
    return vec3<f32>(cy * rx.x + sy * rx.z, rx.y, -sy * rx.x + cy * rx.z);
}

fn envelope(age: f32, attack: f32, hold: f32, release: f32) -> f32 {
    if age < 0.0 { return 0.0; }
    let a = max(attack, 0.0);
    let h = max(hold, 0.0);
    let r = max(release, 0.0);
    if a > 0.0 && age < a { return age / a; }
    if age < a + h { return 1.0; }
    if r <= 0.0 { return 0.0; }
    if age < a + h + r { return 1.0 - (age - a - h) / r; }
    return 0.0;
}

fn body(idx: u32, count: u32, sample_mode: u32, elapsed_beats: f32, attack_beats: f32, hold_beats: f32, release_beats: f32, stagger_beats: f32, yaw: f32, pitch: f32, scale: f32, source_offset_x: f32, source_offset_y: f32, source_offset_z: f32, weights_len: u32) -> f32 {
    if elapsed_beats < 0.0 { return 0.0; }
    let base = (idx / 3u) * 3u;
    var sample_position = buf_in[idx].position;
    if sample_mode == 1u && base + 2u < count {
        sample_position = (buf_in[base].position + buf_in[base + 1u].position + buf_in[base + 2u].position) / 3.0;
    }
    let direction = rotate_basis(vec3<f32>(0.0, 1.0, 0.0), yaw, pitch);
    let safe_scale = max(abs(scale), 1e-6);
    let world = (sample_position + vec3<f32>(source_offset_x, source_offset_y, source_offset_z)) / safe_scale;
    let order = clamp(0.5 + 0.5 * dot(world, direction), 0.0, 1.0);
    let age = elapsed_beats - stagger_beats * order;
    var incoming = 1.0;
    if idx < weights_len { incoming = buf_weights[idx]; }
    return incoming * envelope(age, attack_beats, hold_beats, release_beats);
}
