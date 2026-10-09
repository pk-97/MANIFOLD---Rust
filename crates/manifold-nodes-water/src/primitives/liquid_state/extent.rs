//! Buffer extent rule owned by this node.
use crate::liquid::extent::liquid_lattice;
use std::mem::size_of;
use crate::liquid::lattice::FlipSolverGrid;
use crate::primitives::liquid_stats::LIQUID_STATS_WORDS;
use crate::primitives::gpu_flip_step::face_bytes;
use crate::primitives::whitewater_step::DEFAULT_CAPACITY as STEP_CAPACITY;
use crate::primitives::whitewater_step::MAX_CAPACITY as STEP_MAX_CAPACITY;
use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
use crate::liquid::extent::PARTICLE;

fn liquid_state(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
    x.provide("identity", 16);
    x.hold(4 * 16); // persistent identity and the three fenced metadata copies
    x.covers_if_bound("identity_in", 16)?;
    let mut whitewater_check = Ok(());
    let capacity = x.count("whitewater_capacity", STEP_CAPACITY as f32)?;
    if !(1..=STEP_MAX_CAPACITY).contains(&capacity) {
        return Err(Verdict::Refused(format!("whitewater capacity {capacity} is outside 1 to {STEP_MAX_CAPACITY}")));
    }
    let pool = u64::from(capacity) * size_of::<crate::whitewater::WhitewaterParticle>() as u64;
    for (capture, output, bytes) in [
        ("whitewater_pool_in", "whitewater_pool", pool),
        ("whitewater_state_in", "whitewater_state", 32),
        ("whitewater_counts_in", "whitewater_counts", 36),
        ("foam_particles_in", "foam_particles", u64::from(capacity) * PARTICLE),
        ("bubble_particles_in", "bubble_particles", u64::from(capacity) * PARTICLE),
        ("spray_particles_in", "spray_particles", u64::from(capacity) * PARTICLE),
        ("dust_particles_in", "dust_particles", u64::from(capacity) * PARTICLE),
    ] {
        let active = x.input(capture).is_some();
        x.provide(output, if active { bytes } else { 0 });
        if active {
            x.hold(bytes);
            // Captures become bound on the second walk. Publish ALL sizes
            // before returning an uncovered capture from the first walk.
            whitewater_check = whitewater_check.and(x.covers(capture, bytes));
        }
    }
    if x.input("whitewater_pool_in").is_some() { x.hold(pool); }

    let mut interior_check = Ok(());
    if x.wired("interior_in") {
        let bytes = crate::liquid::grid::interior_bytes(FlipSolverGrid::from_lattice(liquid_lattice(x)?).cells());
        x.provide("interior", bytes);
        x.hold(bytes);
        if x.bytes("interior_in") != Some(bytes) {
            interior_check = Err(x.uncovered(format!("interior_in must hold exactly {bytes} bytes for the cell-centred lattice")));
        }
    } else {
        x.provide("interior", 0);
    }
    // The faces are the lattice's face grid, sized before the region runs and
    // held only while something reads them. The tick's faces (written later
    // in the plan: the second pass sees them) must be exactly that grid.
    let mut faces_check = Ok(());
    if x.input("faces_in").is_some() {
        if ["nodes_x", "nodes_y", "nodes_z"].iter().any(|port| x.input(port).is_none()) {
            return Err(Verdict::Refused("Liquid State: faces_in needs the lattice on nodes_x, nodes_y and nodes_z".into()));
        }
        let faces = face_bytes(FlipSolverGrid::from_lattice(liquid_lattice(x)?).cells());
        let fed = x.feeds("faces");
        x.provide("faces", if fed { faces } else { 0 });
        if fed {
            x.hold(faces);
            faces_check = match x.bytes("faces_in") {
                Some(have) if have == faces => Ok(()),
                Some(have) => Err(x.uncovered(format!("faces_in holds {have} bytes; the lattice's face grid is {faces}"))),
                None => Err(x.uncovered("faces_in is unbound".into())),
            };
        }
    } else {
        x.provide("faces", 0);
    }
    let stats = u64::from(LIQUID_STATS_WORDS) * 4;
    // The zeroed stats a new epoch copies, and the readback ring.
    x.hold(4 * stats + 3 * 32);
    x.covers_if_bound("clock_status_in", 32)?;
    x.covers_if_bound("clock_status", 32)?;
    x.publish("tick_index", 0.0);
    let records = u64::from(x.count("count", 0.0)?) * PARTICLE;
    // A new epoch copies the fill into the state; each tick's capture copies
    // the tick's particles and stats back (written later in the plan: the
    // second pass sees them).
    for port in ["seed", "out", "in"] {
        x.covers(port, records)?;
    }
    x.covers("stats", stats)?;
    x.covers("stats_in", stats)?;
    faces_check.and(whitewater_check).and(interior_check)
}
inventory::submit! {
    ExtentRule { type_id: "node.liquid_state", check: liquid_state }
}
