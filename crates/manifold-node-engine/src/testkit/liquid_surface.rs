use crate::water::primitives::sort_particles_into_cells::SortParticlesIntoCells;
use crate::bindings::Slot;
use crate::exec::effect_node::ParamValues;
use crate::particles::{FluidParticle};
use crate::testkit::array_harness::{Harness, params, read};
use crate::water::fluid_particles::{CellRange, FluidBlob, bin_counts};

/// Deterministic pseudo-random stream (xorshift) for fixtures.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self { Self(seed) }
    pub fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

pub struct Lattice {
    pub center: [f32; 3],
    pub size: [f32; 3],
    pub cell: f32,
}

impl Lattice {
    /// The box, and the bin grid the sort publishes for it (searchers take
    /// it as params here, as they take the sort's wires in a graph).
    pub fn params(&self, extra: &[(&'static str, f32)]) -> ParamValues {
        let bins = bin_counts(self.size, self.cell);
        let mut values = vec![
            ("center_x", self.center[0]),
            ("center_y", self.center[1]),
            ("center_z", self.center[2]),
            ("size_x", self.size[0]),
            ("size_y", self.size[1]),
            ("size_z", self.size[2]),
            ("cell_size", self.cell),
            ("bins_x", bins[0] as f32),
            ("bins_y", bins[1] as f32),
            ("bins_z", bins[2] as f32),
        ];
        values.extend_from_slice(extra);
        params(&values)
    }

    pub fn min(&self) -> [f32; 3] {
        std::array::from_fn(|axis| self.center[axis] - 0.5 * self.size[axis])
    }

    /// The sort's bin rule, evaluated with the kernel's f32 operations.
    pub fn bin(&self, p: [f32; 3]) -> usize {
        let bins = bin_counts(self.size, self.cell);
        let min = self.min();
        let inv = 1.0 / self.cell;
        let b: [usize; 3] = std::array::from_fn(|axis| {
            (((p[axis] - min[axis]) * inv).floor() as i64).clamp(0, i64::from(bins[axis]) - 1) as usize
        });
        b[0] + bins[0] as usize * (b[1] + bins[1] as usize * b[2])
    }
}

pub fn particle(position: [f32; 3], radius: f32, id: u32) -> FluidParticle {
    FluidParticle {
        position_radius: [position[0], position[1], position[2], radius],
        velocity: [0.0; 3],
        id,
    }
}

/// Sort then blobs, read back: (sorted, ranges, blobs, their GPU slots in that order).
type Shaped = (Vec<FluidParticle>, Vec<CellRange>, Vec<FluidBlob>, (Slot, Slot, Slot));

pub fn sort_and_shape(
    harness: &mut Harness,
    lattice: &Lattice,
    particles: &[FluidParticle],
    count: usize,
    shape: &[(&'static str, f32)],
) -> Shaped {
    let bins = bin_counts(lattice.size, lattice.cell);
    let bin_total = bins.iter().product::<u32>() as usize;
    let (input, _) = harness.array(particles, particles.len());
    let (sorted_slot, sorted_buf) = harness.array::<FluidParticle>(&[], particles.len());
    let (ranges_slot, _) = harness.array::<CellRange>(&[], 1);
    let count_slot = harness.scalar_input(count as f32);
    let mut sort = SortParticlesIntoCells::new();
    let (_, errors) = harness.run(
        &mut sort,
        &[("particles", input), ("count", count_slot)],
        &[("sorted", sorted_slot), ("cell_ranges", ranges_slot)],
        &lattice.params(&[]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let (blobs_slot, blobs_buf) = harness.array::<FluidBlob>(&[], particles.len());
    let mut shape_node = crate::water::primitives::testkit::shape_particle_blobs();
    let (_, errors) = harness.run(
        &mut shape_node,
        &[("sorted", sorted_slot), ("cell_ranges", ranges_slot)],
        &[("blobs", blobs_slot)],
        &lattice.params(shape),
    );
    assert!(errors.is_empty(), "{errors:?}");
    (
        read(&sorted_buf, particles.len()),
        read(&harness.buffer(ranges_slot), bin_total),
        read(&blobs_buf, particles.len()),
        (sorted_slot, ranges_slot, blobs_slot),
    )
}


pub fn blob_matrix(blob: &FluidBlob) -> [[f64; 3]; 3] {
    let d = blob.shape_diag.map(f64::from);
    let o = blob.shape_off.map(f64::from);
    [[d[0], o[0], o[1]], [o[0], d[1], o[2]], [o[1], o[2], d[2]]]
}

/// The clamp's wires as params: the box, the level-set lattice, the solid
/// lattice and the bin size.
pub fn clamp_params(center: [f32; 3], size: [f32; 3], nodes: [u32; 3], solid_nodes: [u32; 3], cell: f32) -> ParamValues {
    params(&[
        ("center_x", center[0]),
        ("center_y", center[1]),
        ("center_z", center[2]),
        ("size_x", size[0]),
        ("size_y", size[1]),
        ("size_z", size[2]),
        ("nodes_x", nodes[0] as f32),
        ("nodes_y", nodes[1] as f32),
        ("nodes_z", nodes[2] as f32),
        ("solid_nodes_x", solid_nodes[0] as f32),
        ("solid_nodes_y", solid_nodes[1] as f32),
        ("solid_nodes_z", solid_nodes[2] as f32),
        ("cell_size", cell),
    ])
}

/// The level set's cap outside the liquid, as a fraction of a bin; the WGSL of
/// `node.particle_volume` and `node.shape_particle_blobs` both hold it (P6e).
/// The volume's cap, as a fraction of a bin; the blob reach cap is the rest.
pub fn native_support(ijk: [u32; 3], centre: [f64; 3], radius: f64, min: [f64; 3], h: [f64; 3], extra: f64) -> bool {
    (0..3).all(|a| {
        let lo = ((centre[a] - 1.5 * radius - extra - min[a]) / h[a]).floor();
        let hi = ((centre[a] + 1.5 * radius + extra - min[a]) / h[a]).floor() + 1.0;
        f64::from(ijk[a]) >= lo && f64::from(ijk[a]) <= hi
    })
}

/// f64 trilinear sample of a solid lattice spanning `min`..`min + size`: the
/// rule node.particle_volume and node.clamp_liquid_to_solids share.
pub fn solid_sample(solid: &[f32], nodes: [u32; 3], min: [f32; 3], size: [f32; 3], p: [f64; 3]) -> f64 {
    let n = nodes.map(|v| v as usize);
    let mut base = [0usize; 3];
    let mut frac = [0f64; 3];
    for a in 0..3 {
        let spacing = f64::from(size[a]) / (n[a] - 1) as f64;
        let g = ((p[a] - f64::from(min[a])) / spacing).clamp(0.0, (n[a] - 1) as f64);
        base[a] = (g.floor() as usize).min(n[a] - 2);
        frac[a] = g - base[a] as f64;
    }
    (0..8usize)
        .map(|corner| {
            let o = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
            let w: f64 = (0..3).map(|a| if o[a] == 1 { frac[a] } else { 1.0 - frac[a] }).product();
            let at: [usize; 3] = std::array::from_fn(|a| base[a] + o[a]);
            w * f64::from(solid[at[0] + n[0] * (at[1] + n[1] * at[2])])
        })
        .sum()
}
