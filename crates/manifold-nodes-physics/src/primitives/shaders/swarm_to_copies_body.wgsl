// node.swarm_to_copies — fusable BUFFER body. One copy transform per
// particle: at the particle's position, uniformly scaled by `size`,
// unrotated. A dead particle (life <= 0) becomes a zero-scale hole.
// Element = Particle (coincident); Element2 = InstanceTransform.
fn body(idx: u32, count: u32, e_particles: Element, size: f32) -> Element2 {
    let scale = select(0.0, size, e_particles.life > 0.0);
    return Element2(vec4<f32>(e_particles.position, scale), vec4<f32>(0.0));
}
