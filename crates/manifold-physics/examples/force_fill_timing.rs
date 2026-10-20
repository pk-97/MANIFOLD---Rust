//! What one tick's force lattice fill costs on the CPU for the stock Wind
//! card's field, at the water resolutions and swarm Force Detail a show uses.
//!
//! `cargo run --release -p manifold-physics --example force_fill_timing`
//!
//! A field that holds still between ticks is laid once and cached
//! (`TickForces::prepare`); the "per tick" column is what a field that moves
//! every tick (drifting gusts) pays. "laid again" says whether two tick starts
//! read different fields, so the cache misses.

use std::time::Instant;

use manifold_physics::FieldValue;
use manifold_physics::force_lattice::{ForceLattice, FORCE_LATTICE_STRIDE};
use manifold_physics::VectorField;

/// The Wind card's graph (Wind.json) at transport time `t`: steady air plus
/// gusts drifting with the wind, felt at the card's Air Resistance, gated by
/// Enabled.
fn wind(t: f64, speed: f32, gusts: f32, enabled: f32) -> FieldValue {
    let steady = FieldValue::uniform([1.0, 0.0, 0.0]).unwrap().scaled(speed).unwrap();
    let travel = (t * f64::from(speed)) as f32;
    let turbulence = FieldValue::turbulence(2.0, [travel, 0.0, 0.0], 0).unwrap().scaled(gusts).unwrap();
    steady.sum(&turbulence).unwrap().air(1.0).unwrap().scaled(enabled).unwrap()
}

/// The liquid force lattice for a cubic domain of `resolution` cells: the
/// solver lattice is cells + 1 + 2 · 3 padding nodes (manifold-water-liquid
/// `PADDING_NODES`), the force lattice every `FORCE_LATTICE_STRIDE` of them.
fn water(resolution: u32) -> ForceLattice {
    let dx = 4.0 / resolution as f32;
    ForceLattice::covering([-2.0; 3], dx, [resolution + 7; 3])
}

fn time_fill(lattice: ForceLattice, field: &FieldValue) -> f64 {
    let mut values = vec![[0.0f32; 4]; 2 * lattice.node_count()];
    let runs = 3;
    let start = Instant::now();
    for _ in 0..runs {
        lattice.fill_forces(&mut values, |x| field.sample_force(x)).unwrap();
    }
    start.elapsed().as_secs_f64() * 1e3 / f64::from(runs)
}

fn main() {
    let tick = 1.0 / 60.0;
    let cases = [
        ("default (Turbulence 0)", 5.0, 0.0, 1.0),
        ("gusts (Turbulence 2 m/s)", 5.0, 2.0, 1.0),
        ("disabled", 5.0, 2.0, 0.0),
    ];
    let lattices = [
        ("water 64", water(64)),
        ("water 128", water(128)),
        ("water 256", water(256)),
        ("swarm detail 64", ForceLattice::over_box([-2.0; 3], [4.0; 3], 64)),
    ];
    println!("force lattice stride {FORCE_LATTICE_STRIDE}; times are one fill, ms");
    for (label, lattice) in lattices {
        for (case, speed, gusts, enabled) in cases {
            let now = wind(10.0, speed, gusts, enabled);
            let next = wind(10.0 + tick, speed, gusts, enabled);
            println!(
                "{label:16} {nodes:>7} nodes  {case:26} {ms:8.2} ms per fill  laid again each tick: {moves}",
                nodes = lattice.node_count(),
                ms = time_fill(lattice, &now),
                moves = now != next,
            );
        }
    }
}
