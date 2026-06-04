// node.euler_step_particles — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT multi-input. One Euler step: position.xy += forces[i] * speed *
// dt_scaled. Dead particles (life <= 0) pass through unchanged.
//
// ABI (buffer standalone codegen): both array inputs are coincident, so the
// wrapper pre-reads this element's `e_in = buf_in[idx]` (Particle) and
// `e_forces = buf_forces[idx]` (the [f32;2] force pair) and passes them; the
// body returns the stepped element, written to buf_out[idx]. In production
// `in`/`out` alias one buffer (run() binds it to both the read and read_write
// slots) — returning the element unchanged for a dead particle reproduces the
// hand kernel's early-return-no-write. `dt_scaled` (= delta*60) is a DERIVED
// uniform field (declared `derived_uniforms`, packed by run() each frame, NOT a
// param). `idx`/`count`/`active_count` are unused (DCE). The codegen synthesizes
//   struct Element  { position:vec3, velocity:vec3, life:f32, age:f32, color:vec4 }
//   struct Element2 { x: f32, y: f32 }   // the force pair, std430 stride 8 == vec2
// from the Channels signatures. Matches euler_step_particles.wgsl.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_forces: Element2,
    active_count: i32,
    speed: f32,
    dt_scaled: f32,
) -> Element {
    var p = e_in;
    if p.life <= 0.0 {
        return p;
    }
    let step = vec2<f32>(e_forces.x, e_forces.y) * speed * dt_scaled;
    p.position = vec3<f32>(p.position.x + step.x, p.position.y + step.y, 0.0);
    return p;
}
