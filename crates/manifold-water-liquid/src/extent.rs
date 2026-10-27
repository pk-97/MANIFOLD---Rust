//! Native liquid extent rules.
//!
//! The generic checker and its shared rules live in the exec extent module;
//! this module supplies liquid-specific storage, pool and whitewater rules.

use crate::whitewater::KnownValue;
use manifold_node_engine::exec::extent::{AtomExtent, Verdict};
use manifold_node_engine::particles::FluidParticle;
use std::mem::size_of;

// ── Rules ──────────────────────────────────────────────────────────────────

pub fn whole(x: &AtomExtent<'_>, name: &str, default: f32) -> u32 {
    x.scalar(name, default).round().max(0.0) as u32
}

/// Field reads clamp to the field lattices the scalars name, so the wired
/// buffers must hold them.
pub fn field_reads(x: &AtomExtent<'_>) -> Result<(), Verdict> {
    let nodes =
        ["field_nodes_x", "field_nodes_y", "field_nodes_z"].map(|name| whole(x, name, 2.0).max(2));
    let node_count = nodes.iter().map(|&n| u64::from(n)).product::<u64>();
    let force_bytes = node_count * manifold_physics::force_lattice::FORCE_NODE_FLOATS as u64 * 4;
    x.covers_if_bound(
        "forces",
        u64::from(whole(x, "force_lattices", 0.0)) * force_bytes,
    )?;
    x.covers_if_bound("impulses", node_count * 16)
}

// ── GPU FLIP ─────────────────────────────────────────────────────────────────
//
// GPU FLIP's atoms read their lattice from params (P7b wires it). Several size
// their dispatch to the smallest of their arrays at run time; each rule here
// asks that no array is the smaller one, so no work is ever cut.

pub const PARTICLE: u64 = size_of::<FluidParticle>() as u64;

/// The body rows the step may read, a whole count.
pub fn body_rows(x: &AtomExtent<'_>) -> Result<u64, Verdict> {
    let rows = x.scalar("rows", 0.0);
    if !(rows >= 0.0 && rows.fract() == 0.0) {
        return Err(Verdict::Refused(format!(
            "{rows} body rows is not a whole count"
        )));
    }
    Ok(rows as u64)
}

// ── Whitewater ──────────────────────────────────────────────────────────────
//
// The grid atoms dispatch over the elements of the step's tile pool; the
// particle atoms over the smallest of their arrays, so each rule asks that no
// output is the smaller one.

/// A pooled atom's lattice: the `tiles` covering the table of its
/// `pool_tiles` pool, and the elements every array on it holds.
pub fn tile_pool(x: &AtomExtent<'_>) -> Result<u64, Verdict> {
    let pool_tiles = whole(x, "pool_tiles", 1.0).max(1);
    x.covers(
        "tiles",
        manifold_node_engine::scene::world_tiles::table_words(pool_tiles) * 4,
    )?;
    Ok(manifold_node_engine::scene::world_tiles::pool_len(
        pool_tiles,
    ))
}

pub const KNOWN_VALUE: u64 = size_of::<KnownValue>() as u64;

/// One particle record out per particle in.
pub fn particle_map(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.bytes("particles").unwrap_or(0))
}

/// One f32 out per particle in.
pub fn particle_values(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.covers("out", x.items("particles").unwrap_or(0) * 4)
}

/// A liquid domain's role wires as the walk resolves them: the box its
/// Initial Fill places ([`crate::fluid_role::initial_fill`], the node's own
/// rule) and every role's kind. `solver` names the liquid in a refusal.
pub fn walked_roles(
    x: &AtomExtent<'_>,
    solver: &str,
) -> Result<
    (
        Option<manifold_node_engine::scene::transform::Transform>,
        Vec<crate::fluid_role::FluidRoleKind>,
    ),
    Verdict,
> {
    use crate::fluid_role::{FluidRoleKind, initial_fill};
    let objects: Vec<_> = (0..crate::role_count(x.params()))
        .filter_map(|index| x.object(&format!("role_{index}")))
        .collect();
    let kinds: Vec<_> = objects
        .iter()
        .filter_map(|(kind, _)| FluidRoleKind::from_index(*kind))
        .collect();
    let fills = objects
        .iter()
        .filter(|(kind, _)| *kind == FluidRoleKind::InitialFill.index())
        .map(|(_, placed)| *placed);
    let fill =
        initial_fill(fills).map_err(|reason| Verdict::Refused(format!("{solver}: {reason}")))?;
    Ok((fill, kinds))
}
