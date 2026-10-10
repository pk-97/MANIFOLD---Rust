//! Native liquid extent rules.
//!
//! The generic checker and its shared rules live in the exec extent module;
//! this module supplies liquid-specific storage, lattice, and whitewater rules.

use std::mem::size_of;
use crate::fluid_particles::{bin_counts, searched_bins, CellRange};
use manifold_node_engine::particles::FluidParticle;
use crate::liquid::grid::{face_len, FACE_GRID_PORTS, FACE_INPUT_PORTS};
use crate::liquid::lattice::LiquidLattice;
use crate::whitewater::{cell_total, face_offset, grid_cells, KnownValue};
use manifold_node_engine::exec::extent::{AtomExtent, Verdict};

// ── Rules ──────────────────────────────────────────────────────────────────

pub(crate) fn nodes_total(nodes: [f32; 3]) -> u64 {
    nodes.iter().map(|&n| n.max(0.0) as u64).product()
}

pub(crate) fn lattice_total(nodes: [u32; 3]) -> u64 {
    nodes.iter().map(|&n| u64::from(n)).product()
}

pub(crate) fn whole(x: &AtomExtent<'_>, name: &str, default: f32) -> u32 {
    x.scalar(name, default).round().max(0.0) as u32
}

/// The liquid lattice a node reads from its scalar wires.
pub(crate) fn liquid_lattice(x: &AtomExtent<'_>) -> Result<LiquidLattice, Verdict> {
    LiquidLattice::from_scalars(|name, default| x.scalar(name, default)).map_err(Verdict::Refused)
}

/// Field reads clamp to the field lattices the scalars name, so the wired
/// buffers must hold them.
pub(crate) fn field_reads(x: &AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes = ["field_nodes_x", "field_nodes_y", "field_nodes_z"].map(|name| whole(x, name, 2.0).max(2));
    let bytes = nodes.iter().map(|&n| u64::from(n)).product::<u64>() * 16;
    x.covers_if_bound("forces", u64::from(whole(x, "force_lattices", 0.0)) * bytes)?;
    x.covers_if_bound("impulses", bytes)
}

/// A frame's face grid storage: one array per wired axis over the domain's
/// cells, the one-record hint otherwise. Provided before any check can stop
/// the rule, since consumers size from it.
pub(crate) fn provide_frame_faces(x: &mut AtomExtent<'_>, cells: [u32; 3], valid_layers: f32) {
    let published = FACE_INPUT_PORTS.iter().all(|port| x.wired(port));
    x.publish(FACE_GRID_PORTS[6], if published { valid_layers } else { 0.0 });
    for axis in 0..3 {
        let bytes = face_len(cells, axis) * 4;
        if x.wired(FACE_INPUT_PORTS[axis]) {
            x.provide(FACE_GRID_PORTS[axis], bytes);
            x.hold(bytes);
        } else {
            x.provide(FACE_GRID_PORTS[axis], 4);
        }
    }
    for (&port, n) in FACE_GRID_PORTS[3..6].iter().zip(cells) {
        x.publish(port, n as f32);
    }
}

/// Each wired face input holds its whole axis: the copy never publishes a
/// partial grid.
pub(crate) fn cover_frame_faces(x: &AtomExtent<'_>, cells: [u32; 3]) -> Result<(), Verdict> {
    for (axis, port) in FACE_INPUT_PORTS.into_iter().enumerate() {
        if x.wired(port) {
            x.covers(port, face_len(cells, axis) * 4)?;
        }
    }
    Ok(())
}

/// The sort's bin grid is searched with exactly the ranges it allocates, and
/// its last bin index stays inside them in i32.
pub(crate) fn search_fits(x: &AtomExtent<'_>, bins: [u32; 3], range_bytes: u64) -> Result<(), Verdict> {
    let searched = searched_bins(bins.map(|n| n as f32), range_bytes, "search").map_err(|error| x.uncovered(error))?;
    let [bx, by, bz] = bins.map(u64::from);
    let last = (bx - 1) + bx * ((by - 1) + by * (bz - 1));
    if searched != bins || last >= range_bytes / size_of::<CellRange>() as u64 || last > i32::MAX as u64 {
        return Err(x.uncovered(format!("bin {last} of {bins:?} lies past the ranges")));
    }
    Ok(())
}

/// The bins a searching atom reads, as `read_searched_bins`, checked against
/// the ranges it indexes.
pub(crate) fn searched(x: &AtomExtent<'_>) -> Result<[u32; 3], Verdict> {
    let ports = ["bins_x", "bins_y", "bins_z"];
    let bins = ports.map(|port| x.scalar(port, 0.0));
    let ranges = x.bytes("cell_ranges").ok_or_else(|| x.uncovered("cell_ranges is unbound".into()))?;
    let bins = if bins == [0.0; 3] && ports.iter().all(|port| !x.wired(port)) {
        let size = ["size_x", "size_y", "size_z"].map(|name| x.scalar(name, 4.0));
        bin_counts(size, x.scalar("cell_size", 0.0625)).map(|n| n as f32)
    } else {
        bins
    };
    searched_bins(bins, ranges, "search").map_err(|error| x.uncovered(error))
}

pub(crate) fn required_blob_bounds(x: &AtomExtent<'_>) -> Result<(), Verdict> {
    if x.bytes("bounds") != Some(8) {
        return Err(x.uncovered("bounds must contain exactly two f32 words from Blob Bounds".into()));
    }
    Ok(())
}

pub(crate) fn brick_schedule(x: &AtomExtent<'_>, nodes: [u32; 3]) -> Result<(), Verdict> {
    if !x.wired("bricks") { return Ok(()); }
    let words = crate::primitives::liquid_bricks::schedule_words(nodes)
        .ok_or_else(|| x.uncovered("brick schedule size overflow".into()))?;
    x.covers("bricks", words * 4)
}

pub fn surface_mesh_pass(x: &mut AtomExtent<'_>, output: &str) -> Result<(), Verdict> {
    let nodes = x.nodes(["nodes_x", "nodes_y", "nodes_z"]);
    if nodes.iter().any(|&n| n < 2.0) {
        return Err(x.uncovered(format!("no lattice: nodes {nodes:?}")));
    }
    brick_schedule(x, nodes.map(|n| n as u32))?;
    x.covers("levelset", nodes_total(nodes) * 4)?;
    x.covers("scan", nodes_total(nodes.map(|n| n - 1.0)) * 4)?;
    if x.wired("edge_scan") { x.covers("edge_scan", nodes_total(nodes) * 4)?; }
    // Cell-owned intervals and neighbour reads lie below the checked live total.
    let vertices = x.bytes("vertices").ok_or_else(|| x.uncovered("vertices is unbound".into()))?;
    x.covers(output, vertices)
}

// ── GPU FLIP ─────────────────────────────────────────────────────────────────
//
// GPU FLIP's atoms read their lattice from params (P7b wires it). Several size
// their dispatch to the smallest of their arrays at run time; each rule here
// asks that no array is the smaller one, so no work is ever cut.

pub const PARTICLE: u64 = size_of::<FluidParticle>() as u64;

/// The body rows the step may read, a whole count.
pub(crate) fn body_rows(x: &AtomExtent<'_>) -> Result<u64, Verdict> {
    let rows = x.scalar("rows", 0.0);
    if !(rows >= 0.0 && rows.fract() == 0.0) {
        return Err(Verdict::Refused(format!("{rows} body rows is not a whole count")));
    }
    Ok(rows as u64)
}

// ── Whitewater ──────────────────────────────────────────────────────────────
//
// The grid atoms dispatch over the cells of the solid lattice on their
// nodes wires; the particle atoms over the smallest of their arrays, so each
// rule asks that no output is the smaller one.

/// The whitewater grid's solid nodes and cells, as `grid_nodes` reads them.
pub(crate) fn whitewater_lattice(x: &AtomExtent<'_>, names: [&str; 3]) -> Result<([u32; 3], [u32; 3]), Verdict> {
    let nodes = names.map(|name| whole(x, name, 71.0));
    let cells = grid_cells(nodes).ok_or_else(|| Verdict::Refused(format!("a {nodes:?} solid lattice has too few or too many nodes")))?;
    Ok((nodes, cells))
}

pub(crate) fn whitewater_grid(x: &AtomExtent<'_>) -> Result<(u64, u64), Verdict> {
    let (nodes, cells) = whitewater_lattice(x, ["nodes_x", "nodes_y", "nodes_z"])?;
    Ok((cell_total(nodes), cell_total(cells)))
}

/// The face grid, placed centred in the whitewater grid, and each face
/// array covering its axis.
pub(crate) fn whitewater_faces(x: &AtomExtent<'_>, nodes: [u32; 3], names: [&str; 3]) -> Result<[u32; 3], Verdict> {
    let face_cells = names.map(|name| whole(x, name, 64.0));
    face_offset(nodes, face_cells).map_err(Verdict::Refused)?;
    for (axis, port) in ["face_u", "face_v", "face_w"].into_iter().enumerate() {
        x.covers(port, face_len(face_cells, axis) * 4)?;
    }
    Ok(face_cells)
}

pub(crate) const KNOWN_VALUE: u64 = size_of::<KnownValue>() as u64;

/// One particle record out per particle in.
pub(crate) fn particle_map(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.bytes("particles").unwrap_or(0))
}

/// One f32 out per particle in.
pub(crate) fn particle_values(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.items("particles").unwrap_or(0) * 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    use manifold_core::fluid_domain::domain_layout;
    use crate::matter::{block_sort_box, lattice_blocks, lattice_nodes};

    use crate::primitives::matter_domain::admit_lattice;

    /// MPM's lattice arithmetic at every resolution the domain allows: Grid
    /// Budget admits or names the refusal, the block sort's bins are P2G's
    /// blocks, and the default budget stops at 193 (200³ nodes).
    #[test]
    fn matter_grid_budget_gates_every_resolution() {
        let mut largest_default = 0;
        for resolution in 8..=512 {
            let layout = domain_layout(None, 4.0, resolution).expect("layout");
            let lattice = LiquidLattice::from_layout(&layout);
            let nodes = lattice_nodes(lattice.nodes());
            for budget in [8.0f32, 512.0] {
                let fits = nodes as f64 <= f64::from(budget) * 1e6;
                match admit_lattice(&lattice, budget) {
                    Ok(()) => assert!(fits, "res {resolution}"),
                    Err(error) => assert!(!fits && error.contains("Grid Budget"), "res {resolution}: {error}"),
                }
            }
            if admit_lattice(&lattice, 8.0).is_ok() {
                largest_default = resolution;
            }
            let (_, size, bin) = block_sort_box(&lattice);
            assert_eq!(bin_counts(size, bin), lattice_blocks(&lattice), "res {resolution}");
        }
        assert_eq!(largest_default, 193);
    }
}

