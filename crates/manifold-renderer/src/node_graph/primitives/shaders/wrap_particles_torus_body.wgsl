// node.wrap_particles_torus — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT form. Per-particle toroidal wrap of position.xy to [0, 1]² via
// fract(position.xy + 1). Dead particles (life <= 0) pass through unchanged.
//
// ABI (buffer standalone codegen): the wrapper pre-reads this particle's own
// element `e_in = buf_in[idx]` and passes it; the body returns the wrapped
// element, which the wrapper writes to `buf_out[idx]`. In production `in`/`out`
// alias one buffer (the chain resolves them to the same physical storage), so
// returning the element unchanged for a dead particle reproduces the hand
// kernel's early-return-no-write exactly. `idx`/`count`/`active_count` are
// unused here (DCE drops them). The codegen synthesizes
//   struct Element { position: vec3<f32>, velocity: vec3<f32>, life: f32,
//                    age: f32, color: vec4<f32> }
// from Particle's Channels signature. Matches wrap_particles_torus.wgsl.
fn body(idx: u32, count: u32, e_in: Element, active_count: i32) -> Element {
    var p = e_in;
    if p.life <= 0.0 {
        return p;
    }
    p.position = vec3<f32>(
        fract(p.position.x + 1.0),
        fract(p.position.y + 1.0),
        0.0,
    );
    return p;
}
