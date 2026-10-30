// The Water a liquid particle belongs to rides in the low WATER_MASK bits of
// its radius's mantissa (lib.rs `birth_radius`): Waters that share one solver
// keep their particles apart for drawing. Every record moves whole, so the
// index survives sorts, compaction and renumbering; the radius moves by at
// most seven units in the last place.

// (3 / (4π · 8))^(1/3): the sphere of an eighth of a cell, in cells
// (lib.rs PARTICLE_RADIUS_CELLS).
const PARTICLE_RADIUS_CELLS: f32 = 0.31017524;
const WATER_MASK: u32 = 7u;

// A newborn particle's radius at `cell_size`, carrying Water `water`.
fn birth_radius(cell_size: f32, water: u32) -> f32 {
    let bits = bitcast<u32>(PARTICLE_RADIUS_CELLS * cell_size);
    return bitcast<f32>((bits & ~WATER_MASK) | (water & WATER_MASK));
}

// The Water a live particle's radius carries.
fn water_of(radius: f32) -> u32 {
    return bitcast<u32>(radius) & WATER_MASK;
}
