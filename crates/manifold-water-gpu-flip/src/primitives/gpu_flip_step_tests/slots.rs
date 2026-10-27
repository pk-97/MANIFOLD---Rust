//! The slots in use: the step reads none past them.

use super::super::gpu_flip_step::GpuFlipStep;
use super::*;
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_water_liquid::primitives::liquid_stats::SOLVER_WORDS;

/// Cells of water along the floor of the resting block, in y.
const BLOCK_HEIGHT: usize = 2;

/// A block of water at rest on the floor of the small box, eight particles a
/// cell at the sub-sites a fill uses, ids from 1.
fn resting_block() -> Vec<FluidParticle> {
    let mut block = Vec::new();
    for z in 0..N[2] {
        for y in 0..BLOCK_HEIGHT {
            for x in 0..N[0] {
                for sub in 0..8usize {
                    let at = [x, y, z];
                    let corner = [sub & 1, (sub >> 1) & 1, (sub >> 2) & 1];
                    let position: [f32; 3] = std::array::from_fn(|a| {
                        MIN[a] + (at[a] as f32 + 0.25 + 0.5 * corner[a] as f32) * H
                    });
                    block.push(FluidParticle {
                        position_radius: [position[0], position[1], position[2], 0.31017524 * H],
                        velocity: [0.0; 3],
                        id: block.len() as u32 + 1,
                    });
                }
            }
        }
    }
    block
}

/// A record past the slots in use that would change the water if the step read
/// it: alternately all NaN with a positive radius, and a fast live particle in
/// the empty air above the block.
fn stale_record(slot: usize) -> FluidParticle {
    if slot.is_multiple_of(2) {
        FluidParticle {
            position_radius: [f32::NAN, f32::NAN, f32::NAN, 0.08],
            velocity: [f32::NAN; 3],
            id: 7777,
        }
    } else {
        FluidParticle {
            position_radius: [
                MIN[0] + 0.5 * H,
                MIN[1] + (N[1] as f32 - 0.5) * H,
                MIN[2] + 0.5 * H,
                0.08,
            ],
            velocity: [3.0, 3.0, 3.0],
            id: 8888,
        }
    }
}

/// The step node over the small box with a resting block of water in a pool of
/// `pool` slots, the slots past the block holding `stale_record`s, and the
/// birth identity as the previous tick leaves it: `[groups, 1, 1, n]` in words
/// 4-7.
struct RestingPool {
    h: Harness,
    node: GpuFlipStep,
    inputs: [(&'static str, Slot); 2],
    outputs: [(&'static str, Slot); 5],
    params: ParamValues,
    particles: GpuBuffer,
    identity: GpuBuffer,
    faces: Slot,
    distance: Slot,
    tiles: Slot,
    capped: GpuBuffer,
    live: usize,
}

impl RestingPool {
    fn new(pool: impl Fn(usize) -> usize) -> Self {
        use manifold_node_engine::primitive::Primitive;
        let block = resting_block();
        let live = block.len();
        let slots = pool(live);
        assert!(slots >= live);
        let mut records = block;
        records.extend((live..slots).map(stale_record));
        let mut h = Harness::new();
        let mut node = GpuFlipStep::new();
        node.prepare_pipelines(&h.device);
        let particles = h.array::<FluidParticle>(&records, slots);
        let capped = h.array::<u32>(&[], 2 * slots + SOLVER_WORDS as usize);
        let faces = h.array::<FaceSample>(&[], 1);
        let distance = h.array::<f32>(&[], 1);
        let tiles = h.array::<u32>(&[], 1);
        let pad = PADDING_NODES as f32;
        let params = params(&[
            ("nodes_x", N[0] as f32 + 1.0 + 2.0 * pad),
            ("nodes_y", N[1] as f32 + 1.0 + 2.0 * pad),
            ("nodes_z", N[2] as f32 + 1.0 + 2.0 * pad),
            ("lattice_min_x", MIN[0] - pad * H),
            ("lattice_min_y", MIN[1] - pad * H),
            ("lattice_min_z", MIN[2] - pad * H),
            ("cell_size", H),
            ("interval_duration", 1.0 / 60.0),
            ("volume_projection", 0.0),
        ]);
        let groups = live.div_ceil(256).max(1) as u32;
        let identity = h.array::<u32>(&[live as u32 + 1, 1, 0, 0, groups, 1, 1, live as u32], 8);
        let inputs = [("particles", particles.0), ("identity", identity.0)];
        let outputs = [
            ("out", particles.0),
            ("faces", faces.0),
            ("distance", distance.0),
            ("tiles", tiles.0),
            ("capped", capped.0),
        ];
        Self {
            h,
            node,
            inputs,
            outputs,
            params,
            particles: particles.1,
            identity: identity.1,
            faces: faces.0,
            distance: distance.0,
            tiles: tiles.0,
            capped: capped.1,
            live,
        }
    }

    fn tick(&mut self, tick: i32) {
        for name in ["tick_index", "first_tick"] {
            self.params
                .insert(name.into(), ParamValue::Float(tick as f32));
        }
        let (_, errors) = self
            .h
            .run(&mut self.node, &self.inputs, &self.outputs, &self.params);
        assert!(errors.is_empty(), "{errors:?}");
    }

    /// The water after the last tick: the live records, the identity's `n`,
    /// the pooled faces and distance with their slot table, and the solver
    /// words of the step's tally.
    fn left(&self) -> Left {
        let words = |slot: Slot| {
            let buffer = self.h.buffer(slot);
            read_staged::<u32>(&self.h.device, &buffer, (buffer.size / 4) as usize)
        };
        let capped: Vec<u32> = read(&self.capped, (self.capped.size / 4) as usize);
        let solver = capped[capped.len() - SOLVER_WORDS as usize..].to_vec();
        Left {
            particles: read(&self.particles, self.live),
            n: read::<u32>(&self.identity, 8)[7],
            per_slot: capped[..2 * self.live].to_vec(),
            faces: words(self.faces),
            distance: words(self.distance),
            tiles: words(self.tiles),
            solver,
        }
    }
}

struct Left {
    particles: Vec<FluidParticle>,
    n: u32,
    per_slot: Vec<u32>,
    faces: Vec<u32>,
    distance: Vec<u32>,
    tiles: Vec<u32>,
    solver: Vec<u32>,
}

/// Slots past the count are never read. The same resting block in a pool of
/// exactly its slots and in a pool of four times as many, the extra slots
/// holding records that would show if anything read them (NaN positions with a
/// positive radius, fast live particles), comes out of three ticks with the
/// same particles in `[0, n)`, the same `n`, the same pooled faces and
/// distance and the same solver words.
#[test]
pub(super) fn gpu_flip_step_never_reads_slots_past_the_count() {
    let mut exact = RestingPool::new(|live| live);
    let mut roomy = RestingPool::new(|live| 4 * live);
    assert_eq!(exact.live, 8 * N[0] * N[2] * BLOCK_HEIGHT);
    for tick in 0..3 {
        exact.tick(tick);
        roomy.tick(tick);
        let (a, b) = (exact.left(), roomy.left());
        assert_eq!(a.n, b.n, "tick {tick}: the slots in use");
        assert_eq!(
            a.n as usize, exact.live,
            "tick {tick}: a closed tank loses no water"
        );
        let bits = |left: &Left| bytemuck::cast_slice::<_, u8>(&left.particles).to_vec();
        assert!(
            bits(&a) == bits(&b),
            "tick {tick}: the particles in [0, n) differ"
        );
        assert!(
            a.particles.iter().all(|p| {
                p.position_radius
                    .iter()
                    .chain(&p.velocity)
                    .all(|v| v.is_finite())
                    && p.position_radius[3] > 0.0
            }),
            "tick {tick}: every particle stays live and finite"
        );
        assert!(
            a.per_slot == b.per_slot,
            "tick {tick}: the per-slot words differ"
        );
        assert!(a.faces == b.faces, "tick {tick}: the pooled faces differ");
        assert!(
            a.distance == b.distance,
            "tick {tick}: the pooled distance differs"
        );
        assert_eq!(a.tiles, b.tiles, "tick {tick}: the slot table differs");
        assert_eq!(a.solver, b.solver, "tick {tick}: the solver words differ");
    }
}
