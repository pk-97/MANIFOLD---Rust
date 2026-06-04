// node.scatter_particles — BUFFER body (freeze §12, buffer domain), ATOMIC
// SCATTER. Each live particle atomic-adds `scaled_energy` to its nearest
// accumulator cell. Matches scatter_particles.wgsl `splat_main`.
//
// ABI: `particles` (Particle) coincident → e_particles. The `accum` output is
// an ATOMIC u32 accumulator (`atomic_outputs: ["accum"]`) — the body writes it
// directly via `atomicAdd` on the `buf_accum` global; the wrapper calls
// `body(...)` as a statement (no single-element return). `width` / `height`
// are DERIVED u32 uniforms (run() resolves them from the wired width/height
// scalar inputs each frame). Params: `active_count` (i32, unused here — the
// wrapper's dispatch_count guard bounds the dispatch), `scaled_energy` (i32 →
// the u32 add amount), `boundary` (Enum→u32; 0 = Wrap toroidal, 1 = Discard
// out-of-bounds). An early return on a dead/OOB particle reproduces the hand
// kernel's no-write. Element = the Particle struct.
fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    active_count: i32,
    scaled_energy: i32,
    boundary: u32,
    width: u32,
    height: u32,
) {
    if e_particles.life <= 0.0 {
        return;
    }
    let pos = e_particles.position;
    let energy = u32(scaled_energy);

    if boundary == 1u {
        // Discard: drop out-of-bounds particles entirely (no edge seam).
        if pos.x < 0.0 || pos.x >= 1.0 || pos.y < 0.0 || pos.y >= 1.0 {
            return;
        }
        let coord = vec2<u32>(
            u32(pos.x * f32(width)),
            u32(pos.y * f32(height)),
        );
        let cell = coord.y * width + coord.x;
        atomicAdd(&buf_accum[cell], energy);
    } else {
        // Wrap: nearest texel + toroidal wrap.
        let coord = vec2<u32>(
            u32(pos.x * f32(width)) % width,
            u32(pos.y * f32(height)) % height,
        );
        let cell = coord.y * width + coord.x;
        atomicAdd(&buf_accum[cell], energy);
    }
}
