// Cross-frame allocation, single writer. No particle readback or atomics.
struct Particle { position_radius: vec4<f32>, velocity: vec3<f32>, id: u32 }
struct Range { start: u32, count: u32 }
struct Params { slots: u32, ranges: u32, sites: u32, mode: u32 }
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;
// next (0 means u32 exhausted), identity epoch, reserved base, full-reset request.
@group(0) @binding(2) var<storage, read_write> identity: array<u32>;
@group(0) @binding(3) var<storage, read> ranges: array<Range>;
@group(0) @binding(4) var<storage, read> scan: array<u32>;
@group(0) @binding(5) var<storage, read> plan: array<u32>;
@compute @workgroup_size(1)
fn seed() {
    if p.ranges == 0xffffffffu && identity[1] == 16777216u {
        identity[3] = 1u;
        return;
    }
    var largest = 0u;
    for (var i = 0u; i < p.slots; i++) {
        largest = max(largest, particles[i].id);
    }
    identity[0] = largest + 1u;
    if p.ranges == 0xffffffffu {
        identity[1] += 1u;
    } else {
        identity[1] = p.ranges;
    }
    identity[2] = 0u;
    identity[3] = 0u;
}
@compute @workgroup_size(1)
fn reserve() {
    if (plan[11] != 0u && bitcast<f32>(plan[0]) <= 0.0) || identity[3] != 0u { return; }
    let last = ranges[p.ranges - 1u];
    let live = last.start + last.count;
    let requested = scan[p.sites - 1u];
    if live > p.slots || (p.mode == 2u && requested > p.slots - live) { return; }
    let births = min(requested, p.slots - live);
    if births == 0u { return; }
    var next = identity[0];
    if next == 0u || births - 1u > 0xffffffffu - next {
        if identity[1] == 16777216u {
            identity[3] = 1u;
            return;
        }
        // Rare rollover only: the solver cell order stays untouched.
        for (var i = 0u; i < live; i++) { particles[i].id = i + 1u; }
        identity[1] += 1u;
        next = live + 1u;
    }
    identity[2] = next;
    identity[0] = next + births;
}
