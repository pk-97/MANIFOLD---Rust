// ---- The slots in use. ----
// The step's record for the slot being encoded, [groups, 1, 1, n]: slots
// [0, n) of the particle buffers may hold particles, and `groups` 256-thread
// groups cover them (at least one, none in an inactive clock slot). The
// particle passes dispatch from the triple and stop at `n`, so an idle slot
// launches no particle work. The birth identity holds the same record in its
// words 4-7 across ticks (particle_identity.wgsl); the sorts' `settle` and
// the births' `reserve` rewrite both as `n` changes.
@group(0) @binding(42) var<storage, read_write> slot_record: array<u32>;

fn slots_in_use() -> u32 {
    return slot_record[3];
}

// One thread, before the tick's first clock: the record is the identity's,
// which a reset or a recovery rewrote since the last tick. Later slots find
// it kept current by the sorts and the births.
@compute @workgroup_size(1)
fn slot_load() {
    for (var i = 0u; i < 4u; i = i + 1u) {
        slot_record[i] = birth_identity[4u + i];
    }
}

// One thread, after each slot's clock plan: an active slot's record is the
// identity's; an inactive slot has no groups, so none of its particle passes
// launches. It keeps `n`, which the next slot's clock measures the markers by.
@compute @workgroup_size(1)
fn slot_open() {
    if clock_active() {
        for (var i = 0u; i < 4u; i = i + 1u) {
            slot_record[i] = birth_identity[4u + i];
        }
    } else {
        slot_record[0] = 0u;
        slot_record[1] = 1u;
        slot_record[2] = 1u;
    }
}
