// node.water_impulse — fusable BUFFER body, coincident. One radial-falloff
// velocity change (design section 6): within radius R of the centre, add
// max(0, 1 - distance/R)^2 * impulse_vector to the particle velocity. This is
// a velocity change in m/s — never a force multiplied by dt again. The event
// latch (which substep applies it, multiplicity, cap, discard) lives CPU-side
// in node.water_impulse; run() only dispatches this kernel on substeps that
// consume an event. Inactive slots and slots at or beyond R pass through
// untouched — an impulse never resets or creates particles.
//
// ABI (buffer standalone codegen): `in` is coincident (pre-read into
// e_particles); centre/radius/impulse are the scalar params (port-shadowed by
// same-named optional scalar wires, resolved CPU-side).
fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    centre_x: f32,
    centre_y: f32,
    centre_z: f32,
    radius: f32,
    impulse_x: f32,
    impulse_y: f32,
    impulse_z: f32,
) -> Element {
    var out = e_particles;
    let m = e_particles.position_mass.w;
    if (m == 0.0 || radius <= 0.0) {
        return out;
    }
    let pos = e_particles.position_mass.xyz;
    if (!water_finite3(pos)) {
        return out;
    }
    let d = distance(pos, vec3<f32>(centre_x, centre_y, centre_z));
    if (d >= radius) {
        return out;
    }
    let falloff = 1.0 - d / radius;
    let kick = vec3<f32>(impulse_x, impulse_y, impulse_z) * (falloff * falloff);
    out.velocity_density = vec4<f32>(
        e_particles.velocity_density.xyz + kick,
        e_particles.velocity_density.w,
    );
    return out;
}
