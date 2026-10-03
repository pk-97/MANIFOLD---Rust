// Stable binary radix sort of a publication copy. Solver order is immutable.
struct Particle { position_radius: vec4<f32>, velocity: vec3<f32>, id: u32 }
struct Params { count: u32, slots: u32, bit: u32, pad: u32 }
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> source: array<Particle>;
@group(0) @binding(2) var<storage, read_write> published: array<Particle>;
@group(0) @binding(3) var<storage, read_write> scan: array<u32>;
@group(0) @binding(4) var<storage, read> identity: array<u32>;
@group(0) @binding(5) var<storage, read> stats: array<u32>;
// count, identity epoch, accepted, reserved. Only metadata is fenced/read back.
@group(0) @binding(6) var<storage, read_write> metadata: array<u32>;
fn zero_bit(i: u32) -> bool {
    if p.bit == 32u { return source[i].position_radius.w > 0.0; }
    return (source[i].id & (1u << p.bit)) == 0u;
}
@compute @workgroup_size(256)
fn initialize(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x; if i >= p.slots { return; }
    var particle: Particle;
    if i < p.count {
        if source[i].position_radius.w > 0.0 { particle = source[i]; }
    }
    published[i] = particle;
}
@compute @workgroup_size(256)
fn flags(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x; if i >= p.slots { return; }
    scan[i] = select(0u, 1u, zero_bit(i));
}
@compute @workgroup_size(256)
fn scatter(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x; if i >= p.slots { return; }
    let total = scan[p.slots - 1u];
    var destination = total + i - scan[i];
    if zero_bit(i) { destination = scan[i] - 1u; }
    published[destination] = source[i];
}
@compute @workgroup_size(1)
fn publish_metadata() {
    metadata[0] = scan[p.slots - 1u];
    metadata[1] = identity[1];
    metadata[2] = select(0u, 1u, stats[0] == 0u && stats[NARROW_BAND_SHORTAGE_WORD] == 0u && identity[3] == 0u);
    metadata[3] = 0u;
}
