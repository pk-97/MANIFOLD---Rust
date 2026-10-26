//! GPU proofs for `node.surface_wetness`: contact soaks, then it dries at the
//! film's evaporation rate and compounds frame to frame.

use super::*;
use manifold_node_engine::exec::backend::Backend;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};

const NODES: u32 = 9;
const SPACING: f32 = 0.1;
const SIZE: f32 = 8.0 * SPACING;
const COUNT: usize = (NODES * NODES * NODES) as usize;

/// A node and everything it needs across runs: the wired time and epoch, the
/// output slot, and the node itself.
struct Rig {
    harness: Harness,
    node: SurfaceWetness,
    time: manifold_node_engine::bindings::Slot,
    epoch: manifold_node_engine::bindings::Slot,
    out: manifold_node_engine::bindings::Slot,
    drying: f32,
}

impl Rig {
    fn new(drying: f32) -> Self {
        let mut harness = Harness::new();
        let time = harness.scalar_input(0.0);
        let epoch = harness.scalar_input(0.0);
        let (out, _) = harness.array::<f32>(&[], 1);
        Self {
            harness,
            node: SurfaceWetness::new(),
            time,
            epoch,
            out,
            drying,
        }
    }

    /// One run over `field` at `time` and `epoch`; returns the wetness.
    fn run(&mut self, field: &[f32], time: f32, epoch: f32) -> Vec<f32> {
        let (field_slot, _) = self.harness.array(field, field.len());
        self.harness
            .backend
            .set_scalar(self.time, ParamValue::Float(time));
        self.harness
            .backend
            .set_scalar(self.epoch, ParamValue::Float(epoch));
        let run_params = params(&[
            ("center_x", 0.0),
            ("center_y", 0.0),
            ("center_z", 0.0),
            ("size_x", SIZE),
            ("size_y", SIZE),
            ("size_z", SIZE),
            ("nodes_x", NODES as f32),
            ("nodes_y", NODES as f32),
            ("nodes_z", NODES as f32),
            ("drying", self.drying),
        ]);
        let (_, errors) = self.harness.run(
            &mut self.node,
            &[
                ("field", field_slot),
                ("time", self.time),
                ("epoch", self.epoch),
            ],
            &[("wetness", self.out)],
            &run_params,
        );
        assert!(errors.is_empty(), "{errors:?}");
        read(&self.harness.buffer(self.out), COUNT)
    }
}

fn node_x(index: usize) -> u32 {
    index as u32 % NODES
}

/// Water (negative) on the low-x half of the lattice, 1 m of air elsewhere.
fn half_wet() -> Vec<f32> {
    (0..COUNT)
        .map(|n| if node_x(n) < 4 { -1.0 } else { 1.0 })
        .collect()
}

fn dry() -> Vec<f32> {
    vec![1.0; COUNT]
}

#[test]
fn contact_soaks_and_the_rest_starts_dry() {
    let mut rig = Rig::new(1.0);
    let wetness = rig.run(&half_wet(), 5.0, 0.0);
    for (n, &w) in wetness.iter().enumerate() {
        let want = if node_x(n) < 4 { 1.0 } else { 0.0 };
        assert_eq!(w, want, "node {n}");
    }
}

#[test]
fn wet_nodes_dry_at_the_film_rate_and_compound() {
    let mut rig = Rig::new(10.0);
    let first = rig.run(&half_wet(), 5.0, 0.0);
    let wet_nodes: Vec<usize> = (0..COUNT).filter(|&n| first[n] == 1.0).collect();
    assert!(!wet_nodes.is_empty());
    for (dt_total, now) in [(60.0_f64, 65.0), (120.0, 125.0)] {
        let wetness = rig.run(&dry(), now, 0.0);
        let want = (-10.0 * dt_total / f64::from(DRY_SECONDS)).exp() as f32;
        for (n, &w) in wetness.iter().enumerate().take(COUNT) {
            if wet_nodes.contains(&n) {
                assert!(
                    (w - want).abs() <= 1e-5 * want,
                    "t+{dt_total}: node {n} reads {w}, expected {want}"
                );
            } else {
                assert_eq!(w, 0.0, "t+{dt_total}: node {n}");
            }
        }
    }
}

#[test]
fn time_running_backwards_or_a_new_epoch_starts_over() {
    // Time goes back from 100 s to 40 s.
    let mut rig = Rig::new(10.0);
    rig.run(&half_wet(), 100.0, 0.0);
    let wetness = rig.run(&dry(), 40.0, 0.0);
    assert!(wetness.iter().all(|&w| w == 0.0));

    // Time moves on, but the epoch changes.
    let mut rig = Rig::new(10.0);
    rig.run(&half_wet(), 100.0, 1.0);
    let wetness = rig.run(&dry(), 101.0, 2.0);
    assert!(wetness.iter().all(|&w| w == 0.0));
}
