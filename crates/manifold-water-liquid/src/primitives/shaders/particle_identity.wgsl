// Cross-frame allocation and the slots in use, single writer. No particle
// readback or atomics.
struct Particle { position_radius: vec4<f32>, velocity: vec3<f32>, id: u32 }
struct Range { start: u32, count: u32 }
// `record` is 1 when the step's own record is bound at binding 6.
struct Params { slots: u32, ranges: u32, sites: u32, mode: u32, record: u32, _pad0: u32, _pad1: u32, _pad2: u32 }
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;
// Words 0-3: next (0 means u32 exhausted), identity epoch, reserved base,
// full-reset request. Words 4-6: a dispatch triple [groups, 1, 1] at 256
// threads a group, at least one group. Word 7: n, the slots in use.
@group(0) @binding(2) var<storage, read_write> identity: array<u32>;
@group(0) @binding(3) var<storage, read> ranges: array<Range>;
@group(0) @binding(4) var<storage, read> scan: array<u32>;
@group(0) @binding(5) var<storage, read> plan: array<u32>;
// The step's own record [groups, 1, 1, n], the identity's words 4-7 again.
@group(0) @binding(6) var<storage, read_write> step_record: array<u32>;

fn inactive() -> bool {
    return plan[11] != 0u && bitcast<f32>(plan[0]) <= 0.0;
}

fn groups_for(n: u32) -> u32 {
    return max((n + 255u) / 256u, 1u);
}

fn write_count(n: u32) {
    identity[4] = groups_for(n);
    identity[5] = 1u;
    identity[6] = 1u;
    identity[7] = n;
}

fn publish(n: u32) {
    write_count(n);
    if p.record != 0u {
        step_record[0] = groups_for(n);
        step_record[1] = 1u;
        step_record[2] = 1u;
        step_record[3] = n;
    }
}

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
    write_count(p.slots);
}

// After a sort: the slots in use are the sort's live end, the last key's
// range end. An inactive slot's sort wrote nothing, so this does not either.
@compute @workgroup_size(1)
fn settle() {
    if inactive() { return; }
    let last = ranges[p.ranges - 1u];
    publish(last.start + last.count);
}

// After a birth stage: the slots in use are the live end plus the births
// granted, as many of the requested as the slots hold.
@compute @workgroup_size(1)
fn reserve() {
    if inactive() || identity[3] != 0u { return; }
    let last = ranges[p.ranges - 1u];
    let live = last.start + last.count;
    let requested = scan[p.sites - 1u];
    if live > p.slots { return; }
    var births = min(requested, p.slots - live);
    if p.mode == 2u && requested > p.slots - live { births = 0u; }
    if births > 0u {
        var next = identity[0];
        if next == 0u || births - 1u > 0xffffffffu - next {
            if identity[1] == 16777216u {
                identity[3] = 1u;
                publish(live);
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
    publish(live + births);
}
