// Body poses for the matter atoms (GPU_MPM_SOLVER_DESIGN.md D29), declared
// through `wgsl_includes`. Matches matter::body_pose_at.

// v turned by the unit quaternion q (xyzw).
fn matter_rotate(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    let t = 2.0 * cross(q.xyz, v);
    return v + q.w * t + cross(q.xyz, t);
}

// q after turning at the constant world-frame angular velocity w for t
// seconds: d ⊗ q, d the turn by |w|·t about w.
fn matter_turn(q: vec4<f32>, w: vec3<f32>, t: f32) -> vec4<f32> {
    let speed = length(w);
    let angle = speed * t;
    if !(angle > 0.0) {
        return q;
    }
    let d = vec4<f32>(w * (sin(0.5 * angle) / speed), cos(0.5 * angle));
    return vec4<f32>(
        d.w * q.x + d.x * q.w + d.y * q.z - d.z * q.y,
        d.w * q.y - d.x * q.z + d.y * q.w + d.z * q.x,
        d.w * q.z + d.x * q.y - d.y * q.x + d.z * q.w,
        d.w * q.w - d.x * q.x - d.y * q.y - d.z * q.z,
    );
}

// The velocity of a body's material at world point x.
fn matter_body_velocity(linear: vec3<f32>, angular: vec3<f32>, centre: vec3<f32>, x: vec3<f32>) -> vec3<f32> {
    return linear + cross(angular, x - centre);
}
