// node.swarm_to_copies — fusable BUFFER body. One copy transform per
// particle: at the particle's position, uniformly scaled by `size`, turned
// so the copy's +Z points along its velocity. The Euler pair solves
// euler_xyz(a, b, 0) * +Z = (cos a sin b, -sin a, cos a cos b) = v / |v|.
// A particle at rest keeps the unturned copy. A dead particle (life <= 0)
// becomes a zero-scale hole.
// Element = Particle (coincident); Element2 = InstanceTransform.
fn body(idx: u32, count: u32, e_particles: Element, size: f32) -> Element2 {
    let scale = select(0.0, size, e_particles.life > 0.0);
    let v = e_particles.velocity;
    let moving = dot(v, v) > 1e-12;
    let pitch = select(0.0, atan2(-v.y, length(v.xz)), moving);
    let yaw = select(0.0, atan2(v.x, v.z), moving);
    return Element2(vec4<f32>(e_particles.position, scale), vec4<f32>(pitch, yaw, 0.0, 0.0));
}
