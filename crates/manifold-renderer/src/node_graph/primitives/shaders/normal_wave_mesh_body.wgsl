// node.normal_wave_mesh — current-only triangle gather body.
// The wave displaces smooth input normals. The current triangle's deformed
// edges define a local map used to transport input normals and tangents.
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

fn wave_position(e: Element, amplitude: f32, frequency: f32, phase: f32, yaw: f32, pitch: f32, scale: f32, source_offset: vec3<f32>) -> vec3<f32> {
    let safe_scale = max(abs(scale), 1e-6);
    let direction = safe_unit(rotate_basis(vec3<f32>(0.0, 1.0, 0.0), yaw, pitch), vec3<f32>(0.0, 1.0, 0.0));
    let world_position = e.position + source_offset;
    let wave_phase = TAU * (dot(world_position / safe_scale, direction) * frequency - phase);
    let normal = safe_unit(e.normal, vec3<f32>(0.0, 0.0, 1.0));
    return e.position + normal * (safe_scale * amplitude * sin(wave_phase));
}

fn forward_map(v: vec3<f32>, q1: vec3<f32>, q2: vec3<f32>, q3: vec3<f32>, a1: vec3<f32>, a2: vec3<f32>, a3: vec3<f32>) -> vec3<f32> {
    return a1 * dot(v, q1) + a2 * dot(v, q2) + a3 * dot(v, q3);
}

fn inverse_transpose(v: vec3<f32>, q1: vec3<f32>, q2: vec3<f32>, q3: vec3<f32>, a1: vec3<f32>, a2: vec3<f32>, a3: vec3<f32>, determinant: f32) -> vec3<f32> {
    let c1 = cross(a2, a3);
    let c2 = cross(a3, a1);
    let c3 = cross(a1, a2);
    // A = P * Q^T, with Q the orthonormal source basis and P the mapped
    // basis. A^-T v = P^-T * (Q^T v), hence the cofactors stay in world
    // coordinates while the input vector is projected onto Q.
    return (c1 * dot(q1, v) + c2 * dot(q2, v) + c3 * dot(q3, v)) / determinant;
}

fn body(
    idx: u32,
    count: u32,
    amplitude: f32,
    frequency: f32,
    phase: f32,
    yaw: f32,
    pitch: f32,
    scale: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    enabled: f32,
) -> Element {
    let self_v = buf_in[idx];
    if enabled <= 0.0 || amplitude == 0.0 { return self_v; }
    let base = (idx / 3u) * 3u;
    if base + 2u >= count { return self_v; }

    let v0 = buf_in[base];
    let v1 = buf_in[base + 1u];
    let v2 = buf_in[base + 2u];
    let edge1 = v1.position - v0.position;
    let edge2 = v2.position - v0.position;
    let edge1_length = length(edge1);
    if edge1_length <= EPS { return self_v; }
    let q1 = edge1 / edge1_length;
    let edge2_perpendicular = edge2 - q1 * dot(edge2, q1);
    let edge2_length = length(edge2_perpendicular);
    if edge2_length <= EPS { return self_v; }
    let q2 = edge2_perpendicular / edge2_length;
    let q3 = safe_unit(cross(q1, q2), vec3<f32>(0.0, 0.0, 1.0));
    let source_offset = vec3<f32>(source_offset_x, source_offset_y, source_offset_z);

    let p0 = wave_position(v0, amplitude, frequency, phase, yaw, pitch, scale, source_offset);
    let p1 = wave_position(v1, amplitude, frequency, phase, yaw, pitch, scale, source_offset);
    let p2 = wave_position(v2, amplitude, frequency, phase, yaw, pitch, scale, source_offset);
    let deformed_edge1 = p1 - p0;
    let a1 = deformed_edge1 / edge1_length;
    let edge2_along_q1 = dot(edge2, q1);
    let a2 = (p2 - p0 - a1 * edge2_along_q1) / edge2_length;
    let a3 = safe_unit(cross(a1, a2), q3);
    let determinant = dot(a1, cross(a2, a3));
    if abs(determinant) <= EPS { return self_v; }

    let position = wave_position(self_v, amplitude, frequency, phase, yaw, pitch, scale, source_offset);
    let transported_normal_raw = inverse_transpose(self_v.normal, q1, q2, q3, a1, a2, a3, determinant);
    let normal = safe_unit(transported_normal_raw, safe_unit(self_v.normal, vec3<f32>(0.0, 0.0, 1.0)));
    var tangent = self_v.tangent;
    if length(self_v.tangent.xyz) > EPS {
        let forward_tangent = forward_map(self_v.tangent.xyz, q1, q2, q3, a1, a2, a3);
        let orthogonal = forward_tangent - normal * dot(normal, forward_tangent);
        if length(orthogonal) > EPS {
            tangent = vec4<f32>(normalize(orthogonal), self_v.tangent.w);
        }
    }
    return Element(position, normal, self_v.uv, self_v.uv1, tangent, self_v.color);
}
