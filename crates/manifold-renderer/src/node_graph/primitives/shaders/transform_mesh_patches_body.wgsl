// node.transform_mesh_patches — fixed-cell rigid reference patch response.
const TAU: f32 = 6.283185307179586;
const EPS: f32 = 1e-8;

fn rotate_basis(v: vec3<f32>, yaw: f32, pitch: f32) -> vec3<f32> {
    let cp = cos(pitch);
    let sp = sin(pitch);
    let rx = vec3<f32>(v.x, cp * v.y - sp * v.z, sp * v.y + cp * v.z);
    let cy = cos(yaw);
    let sy = sin(yaw);
    return vec3<f32>(cy * rx.x + sy * rx.z, rx.y, -sy * rx.x + cy * rx.z);
}

fn safe_unit(v: vec3<f32>, fallback: vec3<f32>) -> vec3<f32> {
    let l = length(v);
    if l > EPS { return v / l; }
    return fallback;
}

fn rotate_about(v: vec3<f32>, axis: vec3<f32>, angle: f32) -> vec3<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return v * c + cross(axis, v) * s + axis * dot(axis, v) * (1.0 - c);
}

fn fallback_rotation_axis(axis: vec3<f32>) -> vec3<f32> {
    let basis = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), abs(axis.x) > 0.9);
    return safe_unit(cross(axis, basis), vec3<f32>(0.0, 0.0, 1.0));
}

fn body(idx: u32, count: u32, e_in: Element, separation: f32, rotation: f32, orbit: f32, spread: f32, phase: f32, frequency: f32, yaw: f32, pitch: f32, cell_size: f32, scale: f32, source_offset_x: f32, source_offset_y: f32, source_offset_z: f32, enabled: f32) -> Element {
    if enabled <= 0.0 || (separation == 0.0 && rotation == 0.0 && orbit == 0.0 && spread == 0.0) { return e_in; }
    let base = (idx / 3u) * 3u;
    if base + 2u >= count { return e_in; }
    let source_offset = vec3<f32>(source_offset_x, source_offset_y, source_offset_z);
    let safe_scale = max(abs(scale), 1e-6);
    let reference_centroid = (buf_reference[base].position + buf_reference[base + 1u].position + buf_reference[base + 2u].position) / 3.0 + source_offset;
    let centroid_normalized = reference_centroid / safe_scale;
    let safe_cell = max(abs(cell_size), 1e-6);
    let cell_center_normalized = floor(centroid_normalized / safe_cell + vec3<f32>(0.5)) * safe_cell;
    let cell_center_world = cell_center_normalized * safe_scale;
    let axis = safe_unit(rotate_basis(vec3<f32>(0.0, 1.0, 0.0), yaw, pitch), vec3<f32>(0.0, 1.0, 0.0));
    let radial_normal = safe_unit(cell_center_world, vec3<f32>(1.0, 0.0, 0.0));
    let local_axis = safe_unit(cross(radial_normal, axis), fallback_rotation_axis(axis));
    let mask = 0.5 + 0.5 * sin(TAU * (dot(cell_center_normalized, axis) * frequency - phase));
    let w = enabled * mask;
    let local = e_in.position + source_offset - cell_center_world;
    let local_response = rotate_about(local, local_axis, rotation * w) + cell_center_world;
    let rotated_world = rotate_about(local_response, axis, orbit * w);
    let translated_world = rotated_world + safe_scale * w * (separation * radial_normal + spread * axis);
    let p = translated_world - source_offset;
    let n = rotate_about(rotate_about(e_in.normal, local_axis, rotation * w), axis, orbit * w);
    let t = rotate_about(rotate_about(e_in.tangent.xyz, local_axis, rotation * w), axis, orbit * w);
    return Element(p, n, e_in.uv, vec4<f32>(t, e_in.tangent.w));
}
