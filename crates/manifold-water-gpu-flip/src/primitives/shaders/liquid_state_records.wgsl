// node.liquid_state — the tick's live particle records, copied into the
// state. One thread per 32-byte record, dispatched from the identity's
// [groups, 1, 1] triple (words 4-6); word 7 is the slots in use. Slots at or
// past it keep what the state held.

struct Particle { position_radius: vec4<f32>, velocity: vec3<f32>, id: u32 }

@group(0) @binding(0) var<storage, read> identity: array<u32>;
@group(0) @binding(1) var<storage, read> src: array<Particle>;
@group(0) @binding(2) var<storage, read_write> dst: array<Particle>;

@compute @workgroup_size(256)
fn copy_records(@builtin(global_invocation_id) gid: vec3<u32>) {
    let slots = min(identity[7], min(arrayLength(&src), arrayLength(&dst)));
    if gid.x < slots {
        dst[gid.x] = src[gid.x];
    }
}
