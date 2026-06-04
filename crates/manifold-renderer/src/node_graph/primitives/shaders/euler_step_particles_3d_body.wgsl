// node.euler_step_particles_3d — fusable BUFFER body (freeze §12, buffer
// domain), COINCIDENT multi-input. One Euler step: position.xyz += forces[i] *
// speed * dt_scaled. Dead particles (life <= 0) pass through unchanged. The 3D
// sibling of euler_step_particles. Matches euler_step_particles_3d.wgsl.
//
// ABI (buffer standalone codegen): both array inputs are coincident — the
// wrapper pre-reads `e_in = buf_in[idx]` (Particle) and `e_forces =
// buf_forces[idx]` (the [f32;3] force triple) and passes them; the body returns
// the stepped element written to buf_out[idx]. In production `in`/`out` alias one
// buffer (run() binds it to both the read + read_write slots) — returning the
// element unchanged for a dead particle reproduces the hand kernel's early
// return. `dt_scaled` (= delta*60) is a DERIVED uniform (declared
// derived_uniforms). The codegen synthesizes:
//   struct Element  { position:vec3, velocity:vec3, life:f32, age:f32, color:vec4 }
//   struct Element2 { x: f32, y: f32, z: f32 }   // the [f32;3] force (stride 12)
// `idx`/`count`/`active_count` are unused here (DCE).
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
    let force = vec3<f32>(e_forces.x, e_forces.y, e_forces.z);
    p.position = p.position + force * speed * dt_scaled;
    return p;
}
