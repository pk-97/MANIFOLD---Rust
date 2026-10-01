// Body poses for the GPU liquid atoms (LIQUID_SOLVER_SEAM_DESIGN.md section
// 3.6), declared through `wgsl_includes`. Matches liquid::bodies::body_pose_at.

// v turned by the unit quaternion q (xyzw).
fn liquid_rotate(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    let t = 2.0 * cross(q.xyz, v);
    return v + q.w * t + cross(q.xyz, t);
}

// q after turning at the constant world-frame angular velocity w for t
// seconds: d ⊗ q, d the turn by |w|·t about w.
fn liquid_turn(q: vec4<f32>, w: vec3<f32>, t: f32) -> vec4<f32> {
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

// a × b with every product-and-sum an explicit fma. Fast math contracts a
// plain cross differently in a standalone and a fused kernel; a fixed form
// keeps fused buffer regions bit-exact (docs/FREEZE_COMPILER_MAP.md
// section 7 (precision contract)).
fn liquid_cross_fma(a: vec3<f32>, b: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        fma(a.y, b.z, -(a.z * b.y)),
        fma(a.z, b.x, -(a.x * b.z)),
        fma(a.x, b.y, -(a.y * b.x)),
    );
}

// The velocity of a body's material at world point x.
fn liquid_body_velocity(linear: vec3<f32>, angular: vec3<f32>, centre: vec3<f32>, x: vec3<f32>) -> vec3<f32> {
    return linear + liquid_cross_fma(angular, x - centre);
}
