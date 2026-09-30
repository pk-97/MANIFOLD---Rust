//! Handoff proofs for `node.whitewater_lifecycle` (`docs/GPU_WHITEWATER_DESIGN.md`
//! section 3.5, section 3.7 Handoff; I3–I6, I10): snapshots copied on the GPU
//! reach FLIP's lifecycle byte for byte, each once, oldest first, only after
//! their frame retired, and what the node publishes is FLIP's own population.
//! A hand-retired fence stands in for the frame clock; every frame is still
//! committed and waited on the device, so the copies are real.

use std::cell::Cell;

use manifold_fluids::{WhitewaterFields, WhitewaterGrid, WhitewaterKind, WhitewaterLifecycle as NativeLifecycle, WhitewaterParticle, WhitewaterSpawn};
use manifold_gpu::GpuBuffer;

use super::liquid_surface_tests::{Harness, params, read};
use super::whitewater_lifecycle::{Frame, Report, WhitewaterLifecycle};
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::fluid::{TICK, whitewater_fade};
use crate::node_graph::fluid_particles::FluidParticle;
use crate::node_graph::liquid::grid::face_len;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::transform::Transform;
use crate::node_graph::whitewater_handoff::{CaptureInputs, Fence, OutputRing, SnapshotShape};

const NODES: u32 = 21;
const CELLS: u32 = NODES - 1;
const FACE_CELLS: u32 = 14;
const PAD: u32 = 3;
const H: f32 = 0.1;
const ORIGIN: [f32; 3] = [-1.0, 0.0, -1.0];
const GRAVITY: [f32; 3] = [0.0, -9.81, 0.0];

/// A frame clock the test retires by hand.
#[derive(Default)]
struct HandFence {
    next: Cell<u64>,
    retired: Cell<u64>,
    waits: Cell<u32>,
}

impl HandFence {
    fn retire(&self, stamp: u64) {
        self.retired.set(self.retired.get().max(stamp));
    }
}

impl Fence for HandFence {
    fn stamp(&self) -> u64 {
        self.next.get()
    }

    fn is_complete(&self, stamp: u64) -> bool {
        stamp <= self.retired.get()
    }

    fn wait(&self, stamp: u64) -> bool {
        self.waits.set(self.waits.get() + 1);
        self.retire(stamp);
        true
    }
}

fn grid() -> WhitewaterGrid {
    WhitewaterGrid { cells: [CELLS; 3], cell_size: H, origin: ORIGIN }
}

/// Still air over no solid: every spawn stays spray and falls freely.
struct Fields {
    faces: [Vec<f32>; 3],
    level: Vec<f32>,
    solid: Vec<f32>,
}

impl Fields {
    fn still_air() -> Self {
        Self {
            faces: std::array::from_fn(|axis| vec![0.0; face_len([FACE_CELLS; 3], axis) as usize]),
            level: vec![1.0; grid().cell_count()],
            solid: vec![10.0; grid().node_count()],
        }
    }

    fn view(&self) -> WhitewaterFields<'_> {
        WhitewaterFields {
            face_u: &self.faces[0],
            face_v: &self.faces[1],
            face_w: &self.faces[2],
            face_cells: [FACE_CELLS; 3],
            face_offset: [PAD; 3],
            level: &self.level,
            solid: &self.solid,
            gravity: GRAVITY,
        }
    }
}

fn spray(x: f32, y: f32) -> WhitewaterSpawn {
    WhitewaterSpawn { position_lifetime: [x, y, 0.1, 4.0], velocity: [0.3, 0.0, 0.0], kind: WhitewaterKind::Spray as u32 }
}

/// FLIP's lifecycle run straight on the CPU: each batch loaded, then stepped
/// once per tick, published as the node publishes.
fn reference(capacity: u32, batches: &[(&[WhitewaterSpawn], u32)]) -> Vec<FluidParticle> {
    let fields = Fields::still_air();
    let mut lifecycle = NativeLifecycle::new(grid(), capacity, 0).expect("lifecycle");
    for &(spawns, ticks) in batches {
        lifecycle.set_fields(&fields.view()).expect("fields");
        lifecycle.load(spawns).expect("load");
        for _ in 0..ticks {
            lifecycle.step(TICK).expect("step");
        }
    }
    let mut particles = Vec::new();
    lifecycle.particles(&mut particles).expect("particles");
    particles
        .iter()
        .map(|p| FluidParticle {
            position_radius: [p.position[0], p.position[1], p.position[2], whitewater_fade(p.lifetime)],
            velocity: p.velocity,
            id: 0,
        })
        .collect()
}

/// The node, its inputs as shared GPU arrays, and the hand fence.
struct Rig {
    harness: Harness,
    fence: HandFence,
    node: WhitewaterLifecycle,
    capacity: u32,
    spawns: GpuBuffer,
    offsets: GpuBuffer,
    faces: [GpuBuffer; 3],
    level: GpuBuffer,
    solid: GpuBuffer,
}

impl Rig {
    fn new(capacity: u32) -> Self {
        let harness = Harness::new();
        let fields = Fields::still_air();
        let shared = |values: &[f32]| {
            let buffer = harness.device.create_buffer_shared((values.len() * 4) as u64);
            // SAFETY: a fresh shared buffer of exactly these bytes.
            unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
            buffer
        };
        let faces = [shared(&fields.faces[0]), shared(&fields.faces[1]), shared(&fields.faces[2])];
        let (level, solid) = (shared(&fields.level), shared(&fields.solid));
        let spawns = harness.device.create_buffer_shared(u64::from(capacity) * 32);
        spawns.zero_fill();
        let offsets = harness.device.create_buffer_shared(4);
        offsets.zero_fill();
        Self { harness, fence: HandFence::default(), node: WhitewaterLifecycle::new(), capacity, spawns, offsets, faces, level, solid }
    }

    /// The spawns the next frame snapshots, and how many the emitters asked for.
    fn emit(&self, spawns: &[WhitewaterSpawn], emitted: u32) {
        self.spawns.zero_fill();
        // SAFETY: every frame is committed and waited, so no GPU work reads these.
        unsafe {
            self.spawns.write(0, bytemuck::cast_slice(spawns));
            self.offsets.write(0, &emitted.to_ne_bytes());
        }
    }

    /// One frame stamped with its number, committed and waited on the GPU;
    /// the hand fence alone says what has retired.
    fn run(&mut self, ticks: u32, epoch: u32, offline: bool) -> Report {
        self.fence.next.set(self.fence.next.get() + 1);
        let shape = SnapshotShape { grid: grid(), face_cells: [FACE_CELLS; 3], face_offset: [PAD; 3], capacity: self.capacity };
        let frame = Frame { shape, ticks, epoch, gravity: GRAVITY };
        self.node.prepare(&frame).expect("prepare");
        let inputs = CaptureInputs {
            spawns: &self.spawns,
            offsets: Some((&self.offsets, 1)),
            faces: [&self.faces[0], &self.faces[1], &self.faces[2]],
            level: &self.level,
            solid: &self.solid,
        };
        let mut native = self.harness.device.create_encoder("whitewater handoff test");
        let report = {
            let mut gpu = GpuEncoder::new(&mut native, &self.harness.device);
            self.node.advance(&mut gpu, &self.fence, offline, &frame, &inputs)
        };
        native.commit_and_wait_completed();
        assert_eq!(report.failure, None);
        report
    }

    fn retire_all(&self) {
        self.fence.retire(self.fence.next.get());
    }

    fn spray(&self, count: u32) -> Vec<FluidParticle> {
        read(self.node.provided_array_output("spray_particles").expect("published"), count as usize)
    }
}

/// Snapshots reach FLIP's lifecycle byte for byte: what the node publishes
/// equals the lifecycle run on the CPU with the same spawns and fields, each
/// frame's spawns one frame later, faded as FLIP's native whitewater is.
#[test]
fn whitewater_handoff_matches_the_lifecycle() {
    let mut rig = Rig::new(64);
    let first = [spray(-0.2, 1.2), spray(0.0, 1.0), spray(0.2, 0.8)];
    let second = [spray(0.1, 1.1), spray(-0.1, 0.9)];
    rig.emit(&first, 3);
    let report = rig.run(1, 0, false);
    assert_eq!((report.counts, report.pending), ([0; 3], 1), "the first frame only snapshots");
    rig.retire_all();
    rig.emit(&second, 2);
    let report = rig.run(1, 0, false);
    assert_eq!((report.counts, report.emitted, report.pending), ([0, 0, 3], 3, 1));
    assert_eq!(rig.spray(3), reference(64, &[(&first, 1)]));
    rig.retire_all();
    rig.emit(&[], 0);
    let report = rig.run(2, 0, false);
    assert_eq!((report.counts, report.emitted), ([0, 0, 5], 5));
    assert_eq!(rig.spray(5), reference(64, &[(&first, 1), (&second, 1)]));
    assert!(report.lifecycle_ms >= 0.0);
}

/// I3: live never waits. Unretired snapshots stay pending; once some retire,
/// the lifecycle takes those, oldest first, and stops at the first that
/// hasn't.
#[test]
fn whitewater_live_holds_until_fence() {
    let mut rig = Rig::new(64);
    let batch = [spray(0.0, 1.0)];
    rig.emit(&batch, 1);
    for frame in 1..=3 {
        let report = rig.run(1, 0, false);
        assert_eq!((report.counts, report.pending), ([0; 3], frame), "frame {frame}");
    }
    rig.fence.retire(1);
    let report = rig.run(0, 0, false);
    assert_eq!((report.counts, report.pending), ([0, 0, 1], 2), "only the retired snapshot is taken");
    rig.retire_all();
    let report = rig.run(0, 0, false);
    assert_eq!((report.counts, report.pending, report.emitted), ([0, 0, 3], 0, 3));
    assert_eq!(rig.fence.waits.get(), 0, "live never waits");
    assert_eq!(rig.spray(3), reference(64, &[(&batch, 1), (&batch, 1), (&batch, 1)]));
}

/// Offline waits for the last frame's snapshot, so an export is the same at
/// any speed.
#[test]
fn whitewater_offline_waits_for_its_snapshot() {
    let mut rig = Rig::new(64);
    let batch = [spray(0.0, 1.0)];
    rig.emit(&batch, 1);
    rig.run(1, 0, true);
    let report = rig.run(1, 0, true);
    assert_eq!(rig.fence.waits.get(), 1, "offline waits for the one snapshot");
    assert_eq!((report.counts, report.pending), ([0, 0, 1], 1));
    assert_eq!(rig.spray(1), reference(64, &[(&batch, 1)]));
}

/// I4: with every slot still in flight, a frame's ticks are dropped and
/// counted, never silent; the three in flight are then taken in the order
/// they were captured.
#[test]
fn whitewater_ring_overflow_counts_dropped_ticks() {
    let mut rig = Rig::new(64);
    let batches = [[spray(-0.3, 1.0)], [spray(0.0, 1.0)], [spray(0.3, 1.0)]];
    for (index, batch) in batches.iter().enumerate() {
        rig.emit(batch, 1);
        let report = rig.run(1, 0, false);
        assert_eq!((report.pending, report.dropped_ticks), (index + 1, 0));
    }
    rig.emit(&[spray(0.5, 1.0)], 1);
    assert_eq!(rig.run(2, 0, false).dropped_ticks, 2, "the fourth frame's ticks are dropped");
    let report = rig.run(1, 0, false);
    assert_eq!((report.pending, report.dropped_ticks), (3, 3));
    rig.retire_all();
    let report = rig.run(0, 0, false);
    assert_eq!((report.counts, report.pending, report.emitted, report.dropped_ticks), ([0, 0, 3], 0, 3, 3));
    let expected = reference(64, &[(&batches[0], 1), (&batches[1], 1), (&batches[2], 1)]);
    assert_eq!(rig.spray(3), expected, "taken in capture order");
}

/// I5: ticks 0 hold the population and its outputs byte for byte and
/// capture nothing, whatever the spawns say.
#[test]
fn whitewater_pause_holds_population() {
    let mut rig = Rig::new(64);
    rig.emit(&[spray(0.0, 1.0), spray(0.2, 1.1)], 2);
    rig.run(1, 0, false);
    rig.retire_all();
    rig.emit(&[], 0);
    rig.run(1, 0, false);
    rig.retire_all();
    let held = rig.run(0, 0, false);
    assert_eq!((held.counts, held.pending), ([0, 0, 2], 0));
    let before = rig.spray(2);
    for _ in 0..3 {
        rig.retire_all();
        rig.emit(&[spray(0.4, 1.0)], 1);
        let report = rig.run(0, 0, false);
        assert_eq!(report, Report { lifecycle_ms: report.lifecycle_ms, ..held.clone() });
        assert_eq!(rig.spray(2), before, "paused output is unchanged");
    }
}

/// I6: a new epoch clears the population before anything loads, and a
/// snapshot from the old epoch is discarded, never loaded.
#[test]
fn whitewater_epoch_restart_clears() {
    let mut rig = Rig::new(64);
    let batch = [spray(0.0, 1.0)];
    rig.emit(&batch, 1);
    rig.run(1, 0, false);
    rig.retire_all();
    let report = rig.run(1, 0, false);
    assert_eq!((report.counts, report.pending), ([0, 0, 1], 1), "an epoch-0 snapshot is in flight");
    rig.retire_all();
    let restart = [spray(0.3, 0.9)];
    rig.emit(&restart, 1);
    let report = rig.run(1, 1, false);
    assert_eq!((report.counts, report.emitted, report.pending), ([0; 3], 0, 1), "cleared; the old snapshot is dropped");
    rig.retire_all();
    let report = rig.run(0, 1, false);
    assert_eq!((report.counts, report.emitted), ([0, 0, 1], 1));
    assert_eq!(rig.spray(1), reference(64, &[(&restart, 1)]));
}

/// I10: emission past Capacity (the spawn atom's stride) and loads past the
/// room left are both counted as thinned.
#[test]
fn whitewater_capacity_overflow_thins_and_counts() {
    let mut rig = Rig::new(4);
    let slots: Vec<WhitewaterSpawn> = (0..4).map(|i| spray(-0.3 + 0.2 * i as f32, 1.0)).collect();
    rig.emit(&slots, 10);
    rig.run(1, 0, false);
    rig.retire_all();
    rig.emit(&slots, 4);
    let report = rig.run(1, 0, false);
    assert_eq!((report.counts, report.emitted, report.thinned), ([0, 0, 4], 10, 6));
    rig.retire_all();
    let report = rig.run(0, 0, false);
    assert_eq!((report.counts, report.emitted, report.thinned), ([0, 0, 4], 14, 10), "no room: all four thinned");
}

/// D7: an output slot is rewritten only once every frame that read it
/// retired; with all three still being read, the current one stays.
#[test]
fn whitewater_output_slot_waits_for_its_readers() {
    let harness = Harness::new();
    let fence = HandFence::default();
    let mut ring = OutputRing::default();
    let population = |n: usize| {
        vec![WhitewaterParticle { position: [0.0, 1.0, 0.0], velocity: [0.0; 3], lifetime: 1.0, kind: WhitewaterKind::Foam }; n]
    };
    for stamp in 1..=3u64 {
        fence.next.set(stamp);
        assert!(ring.publish(&harness.device, &fence, &population(stamp as usize)).expect("publish"));
        ring.mark_read(&fence);
    }
    assert_eq!(ring.slots.len(), 3);
    fence.next.set(4);
    assert!(!ring.publish(&harness.device, &fence, &population(9)).expect("publish"), "every slot is being read");
    assert_eq!(ring.current().expect("current").counts, [3, 0, 0], "the last published slot stays");
    fence.retire(1);
    assert!(ring.publish(&harness.device, &fence, &population(9)).expect("publish"));
    assert_eq!((ring.current, ring.current().expect("current").counts), (Some(0), [9, 0, 0]), "the retired slot is reused");
}

/// The node through its ports: arrays, scalars and the grid box in, the
/// provided spray frame and counts out; and each placement rule refused by
/// name before anything is copied.
#[test]
fn whitewater_lifecycle_runs_through_its_ports() {
    let fields = Fields::still_air();
    let mut harness = Harness::new();
    let batch = [spray(0.0, 1.0), spray(0.2, 1.1)];
    let spawns = harness.array(&batch, 64);
    let offsets = harness.array(&[2u32], 1);
    let faces = [0, 1, 2].map(|axis| harness.array(&fields.faces[axis], fields.faces[axis].len()));
    let level = harness.array(&fields.level, fields.level.len());
    let solid = harness.array(&fields.solid, fields.solid.len());
    let bounds = harness.transform_input(Transform {
        pos: std::array::from_fn(|a| ORIGIN[a] + 0.5 * CELLS as f32 * H),
        scale: [CELLS as f32 * H; 3],
        ..Transform::default()
    });
    let outputs = ["foam_particles", "bubble_particles", "spray_particles"].map(|port| (port, harness.array::<FluidParticle>(&[], 64).0));
    let counts = ["foam_count", "bubble_count", "spray_count", "emitted"].map(|port| (port, harness.scalar()));
    let run = |harness: &mut Harness, node: &mut WhitewaterLifecycle, face_cells: f32, layers: f32| {
        let mut inputs = vec![
            ("spawns", spawns.0),
            ("offsets", offsets.0),
            ("face_u", faces[0].0),
            ("face_v", faces[1].0),
            ("face_w", faces[2].0),
            ("level", level.0),
            ("solid", solid.0),
            ("grid_bounds", bounds),
        ];
        for (port, value) in [
            ("count", 1.0),
            ("face_cells_x", face_cells),
            ("face_cells_y", face_cells),
            ("face_cells_z", face_cells),
            ("face_valid_layers", layers),
            ("grid_nodes_x", NODES as f32),
            ("grid_nodes_y", NODES as f32),
            ("grid_nodes_z", NODES as f32),
            ("ticks", 1.0),
            ("epoch", 0.0),
            ("gravity", GRAVITY[1]),
        ] {
            inputs.push((port, harness.scalar_input(value)));
        }
        let mut wired: Vec<(&'static str, _)> = outputs.to_vec();
        wired.extend(counts);
        harness.run(node, &inputs, &wired, &params(&[("capacity", 64.0)]))
    };

    let mut node = WhitewaterLifecycle::new();
    let (_, errors) = run(&mut harness, &mut node, FACE_CELLS as f32, 2.0);
    assert!(errors.is_empty(), "{errors:?}");
    let (scalars, errors) = run(&mut harness, &mut node, FACE_CELLS as f32, 2.0);
    assert!(errors.is_empty(), "{errors:?}");
    let scalar = |port: &str| {
        let slot = counts.iter().find(|(name, _)| *name == port).expect("port").1;
        scalars.iter().find(|(s, _)| *s == slot).map(|(_, v)| v.clone())
    };
    assert_eq!(scalar("spray_count"), Some(ParamValue::Float(2.0)));
    assert_eq!(scalar("emitted"), Some(ParamValue::Float(2.0)));
    let installed = harness.buffer(outputs[2].1);
    assert_eq!(read::<FluidParticle>(&installed, 2), reference(64, &[(&batch, 1)]), "the provided spray frame is installed");

    let (_, errors) = run(&mut harness, &mut WhitewaterLifecycle::new(), FACE_CELLS as f32 - 1.0, 2.0);
    assert!(errors.iter().any(|e| e.contains("does not sit centred")), "{errors:?}");
    let (_, errors) = run(&mut harness, &mut WhitewaterLifecycle::new(), FACE_CELLS as f32, 0.0);
    assert!(errors.iter().any(|e| e.contains("needs at least 1")), "{errors:?}");
}
