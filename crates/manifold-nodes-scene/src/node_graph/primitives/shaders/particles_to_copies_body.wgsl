// node.particles_to_copies — FluidParticle to InstanceTransform
// (GPU_FLUID_SURFACE_DESIGN.md section 9 (audit) row "Whitewater to instances").
// The copy sits at the particle with uniform scale = radius and no rotation, so
// a radius-0 particle (the seam's unused slot) is already a zero-scale hole. A
// slot at or past the live count is a hole too: producers may leave stale
// records there. A negative live count converts every slot.
fn body(idx: u32, count: u32, e_particles: Element, live_count: f32) -> Element2 {
    if live_count >= 0.0 && f32(idx) >= live_count {
        return Element2(vec4<f32>(0.0), vec4<f32>(0.0));
    }
    return Element2(e_particles.position_radius, vec4<f32>(0.0));
}
