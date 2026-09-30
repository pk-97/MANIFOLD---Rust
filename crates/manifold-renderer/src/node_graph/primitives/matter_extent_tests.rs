//! CPU proof, at every Resolution the matter presets allow, that each
//! lattice-sized buffer holds everything its dispatch reads and writes, and
//! that whatever cannot fit is refused by name first (BUG-bnp9 (GPU MPM water
//! locks the Mac above resolution 64)). No GPU: every size and extent comes
//! from the functions the atoms themselves size and dispatch with.

use std::mem::size_of;

use super::matter_domain::admit_lattice;
use super::matter_fill::fill_count;
use super::particle_volume::{ParticleVolume, refined_nodes};
use super::sort_particles_into_cells::range_storage_bytes;
use crate::node_graph::effect_node::ParamValues;
use crate::node_graph::fluid::domain_layout;
use crate::node_graph::fluid_particles::{CellRange, MAX_BINS, bin_counts, bin_total, searched_bins};
use crate::node_graph::matter::{
    ACCUM_WORDS_PER_NODE, MatterGridNode, MatterLattice, grid_accum_bytes, grid_bytes, lattice_nodes, solid_bytes,
};
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;

/// The Resolution card on every matter preset, and node.matter_domain's range.
const RESOLUTIONS: std::ops::RangeInclusive<u32> = 8..=512;
/// Grid Budget's default and its largest value, in million nodes.
const BUDGETS: [f32; 2] = [8.0, 512.0];
/// The fixed range and solid capacities the presets shipped with.
const OLD_CAP: u64 = 1 << 20;

/// A search over the sort's `bins`: the searcher accepts exactly that grid
/// against the ranges the sort allocates for it, and its last bin index lands
/// inside them, in i32.
fn assert_search_fits(bins: [u32; 3], what: &str) {
    let range_bytes = range_storage_bytes(bins);
    assert_eq!(searched_bins(bins.map(|n| n as f32), range_bytes, what), Ok(bins), "{what}");
    let [x, y, z] = bins.map(u64::from);
    let last = (x - 1) + x * ((y - 1) + y * (z - 1));
    assert!(last < range_bytes / size_of::<CellRange>() as u64, "{what}: bin {last} lies past the ranges");
    assert!(last <= i32::MAX as u64 && bin_total(bins) <= MAX_BINS, "{what}: {bins:?}");
}

#[test]
fn matter_buffers_cover_their_dispatch_at_every_resolution() {
    let mut largest_default = 0;
    let mut first_past_old_cap = (None, None);
    for res in RESOLUTIONS {
        let lattice = MatterLattice::from_layout(&domain_layout(None, 4.0, res).expect("layout"));
        let nodes = lattice_nodes(lattice.nodes);

        // The domain admits a lattice or names the refusal; nothing downstream
        // ever sees a refused one (it holds the last good outputs).
        for budget in BUDGETS {
            let fits = nodes as f64 <= f64::from(budget) * 1e6;
            match admit_lattice(&lattice, budget) {
                Ok(()) => assert!(fits, "res {res}"),
                Err(error) => assert!(!fits && error.contains("Grid Budget"), "res {res}: {error}"),
            }
        }
        if admit_lattice(&lattice, BUDGETS[0]).is_ok() {
            largest_default = res;
        }
        // The largest budget admits every resolution, so everything below is
        // reachable from the card.
        assert_eq!(admit_lattice(&lattice, BUDGETS[1]), Ok(()), "res {res}");

        // One thread per lattice node; P2G writes accumulator word node·4 + w
        // from an i32 node index, G2P and the grid update read node records.
        assert_eq!(u64::from(lattice.node_count()), nodes, "res {res}");
        assert!(nodes <= i32::MAX as u64, "res {res}");
        assert!(nodes * u64::from(ACCUM_WORDS_PER_NODE) <= u64::from(u32::MAX), "res {res}: accumulator word index");
        assert_eq!(grid_accum_bytes(lattice.nodes), nodes * u64::from(ACCUM_WORDS_PER_NODE) * 4, "res {res}");
        assert_eq!(grid_bytes(lattice.nodes), nodes * size_of::<MatterGridNode>() as u64, "res {res}");
        // The solid lattice: node.liquid_solid_distance writes one f32 per
        // node and node.matter_frame's ring copies the same bytes.
        assert_eq!(solid_bytes(lattice.nodes), nodes * 4, "res {res}");

        // The block sort's bins are P2G's blocks exactly, so P2G's range check
        // passes and its last block reads a written range.
        let (_, size, bin) = lattice.block_sort_box();
        let blocks = bin_counts(size, bin);
        assert_eq!(blocks, lattice.blocks(), "res {res}: block sort bins");
        assert_search_fits(blocks, &format!("res {res} block sort"));

        // The Liquid Surface group sorts over grid_bounds with bins one
        // simulation cell wide (size_x / (nodes_x − 1) × Bin Cells 1).
        let bounds = lattice.bounds().scale;
        let cell = bounds[0] / (lattice.nodes[0] - 1) as f32 * 1.0;
        let bins = bin_counts(bounds, cell);
        assert!(bins.iter().zip(lattice.nodes).all(|(&b, n)| b == n - 1 || b == n), "res {res}: {bins:?}");
        assert_search_fits(bins, &format!("res {res} liquid sort"));
        if bin_total(bins) > OLD_CAP {
            first_past_old_cap.0.get_or_insert(res);
        }
        if nodes > OLD_CAP {
            first_past_old_cap.1.get_or_insert(res);
        }

        // The level set: node.particle_volume's declared storage (the solid
        // ring's node count × scale³) covers its refined lattice at every
        // Resolution Scale, or saturates and the executor refuses the growth
        // by name before the atom's own count check.
        for scale in 1..=4_u32 {
            let refined = lattice_nodes(refined_nodes(lattice.nodes.map(|n| n as f32), scale));
            let mut params = ParamValues::default();
            params.insert("resolution_scale".into(), ParamValue::Float(scale as f32));
            let capacity = ParticleVolume::new()
                .array_output_capacity("levelset", &params, &[("solid", nodes as u32)])
                .expect("levelset capacity");
            assert!(refined <= u64::from(capacity) || capacity == u32::MAX, "res {res} scale {scale}");
        }

        // Points: the fullest fill any preset can ask for (the whole box)
        // either fits the u32 count every point kernel dispatches over, or
        // node.matter_fill refuses it by name.
        for points_per_cell in [8, 27] {
            let full = u64::from(lattice.cells[0]) * u64::from(lattice.cells[1]) * u64::from(lattice.cells[2]);
            match fill_count(lattice.cells, lattice.cells[1], [[0, 0]; 3], points_per_cell) {
                Ok(count) => assert_eq!(u64::from(count), full * u64::from(points_per_cell), "res {res}"),
                Err(error) => {
                    assert!(full * u64::from(points_per_cell) > u64::from(u32::MAX), "res {res}");
                    assert!(error.contains("32-bit"), "{error}");
                }
            }
        }
    }
    // The root-cause arithmetic: the default budget stops at 193 (200³
    // nodes); the old fixed 2^20 caps broke at 96 (surface bins, 102³) and
    // 95 (solid nodes, 102³).
    assert_eq!(largest_default, 193);
    assert_eq!(first_past_old_cap, (Some(96), Some(95)));
}
