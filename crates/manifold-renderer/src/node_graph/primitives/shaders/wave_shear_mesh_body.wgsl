// node.wave_shear_mesh — analytic shear wave with Jacobian-aware frame transport.
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

fn body(idx: u32, count: u32, e_in: Element, amplitude: f32, frequency: f32, phase: f32, yaw: f32, pitch: f32, scale: f32, origin_x: f32, origin_y: f32, origin_z: f32, enabled: f32, phase_offset: f32, axis: u32) -> Element {
    if enabled <= 0.0 || amplitude == 0.0 { return e_in; }
    let sample_axis = rotate_basis(vec3<f32>(0.0, 1.0, 0.0), yaw, pitch);
    let displacement_axis = select(rotate_basis(vec3<f32>(1.0, 0.0, 0.0), yaw, pitch), rotate_basis(vec3<f32>(0.0, 0.0, 1.0), yaw, pitch), axis != 0u);
    let safe_scale = max(abs(scale), 1e-6);
    let q = (e_in.position - vec3<f32>(origin_x, origin_y, origin_z)) / safe_scale;
    let f = TAU * (dot(q, sample_axis) * frequency - phase - phase_offset);
    // Use the same clamped magnitude for spatial normalisation and displacement
    // so zero/tiny/negative scale remains finite and the Jacobian matches the
    // position response exactly.
    let p = e_in.position + safe_scale * amplitude * enabled * sin(f) * displacement_axis;
    let k = amplitude * enabled * TAU * frequency * cos(f);

    let normal_len = length(e_in.normal);
    let normal_base = select(vec3<f32>(0.0), e_in.normal / normal_len, normal_len > EPS);
    let normal_raw = normal_base - sample_axis * k * dot(displacement_axis, normal_base);
    let transported_len = length(normal_raw);
    let new_normal = select(normal_base, normal_raw / transported_len, transported_len > EPS);

    // tangent.xyz == 0 is MeshVertex's absent-tangent sentinel.
    var tangent = e_in.tangent;
    if length(e_in.tangent.xyz) > EPS {
        let forward = e_in.tangent.xyz + displacement_axis * k * dot(sample_axis, e_in.tangent.xyz);
        let orthogonal = forward - new_normal * dot(new_normal, forward);
        let tangent_len = length(orthogonal);
        if tangent_len > EPS { tangent = vec4<f32>(orthogonal / tangent_len, e_in.tangent.w); }
    }
    return Element(p, new_normal, e_in.uv, tangent);
}
