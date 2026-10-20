//! GPU proofs (SCENE_SETUP_PROGRAMME.md E1, D2). node.swarm_forces lays a
//! moving vortex in a wind onto the shared force lattice tick by tick, through its
//! clock, the history replay and the staged upload, and node.move_swarm moves
//! 100k particles with it. At 30 fps every frame runs two ticks on two
//! lattices; at 120 fps every other frame runs none. Every particle lands
//! where the lattice's CPU twin integrates it. Under a Speed ramp, 30 and
//! 120 fps land on the same particles. And the fused swarm kernel matches the
//! unfused pair.

use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::effect_node::NodeInstanceId;
use manifold_node_engine::exec::backend::Backend;
use manifold_node_engine::exec::effect_node::SimStep;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::particles::Particle;
use manifold_node_engine::ports::PortType;
use manifold_node_engine::freeze::codegen::InputSource;
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_node_engine::testkit::fused_atoms::{fused_into, member};
use manifold_physics::clock::TICK;
use manifold_physics::force_lattice::ForceLattice;
use manifold_physics::{FieldValue, Seconds, VectorField};

use super::move_swarm::MoveSwarm;
use super::swarm_forces::SwarmForces;
use super::swarm_to_copies::SwarmToCopies;
use crate::tick_sampled::TickSampled;

const COUNT: usize = 100_000;

/// node.swarm_forces's scalars node.move_swarm reads, by the same name.
const WIRED: [&str; 10] = [
    "ticks", "first_tick", "force_lattices", "field_nodes_x", "field_nodes_y", "field_nodes_z",
    "field_spacing", "field_min_x", "field_min_y", "field_min_z",
];

fn particle(i: usize) -> Particle {
    // A golden-angle disc of radii 0.3..1.8 m around the vortex axis,
    // heights 1..3 m inside the default box, at rest.
    let t = i as f32 / COUNT as f32;
    let angle = i as f32 * 2.399_963;
    let radius = 0.3 + 1.5 * t.sqrt();
    Particle {
        position: [radius * angle.cos(), 1.0 + 2.0 * ((i * 7919) % COUNT) as f32 / COUNT as f32, radius * angle.sin()],
        life: 1.0,
        ..bytemuck::Zeroable::zeroed()
    }
}

/// A vortex about the box's vertical axis that swells with transport time, in
/// a wind of 0.3 m/s along x at drag rate 0.8/s: every tick has its own lattice.
fn field(seconds: f64) -> FieldValue {
    let vortex = FieldValue::vortex([0.0, 2.0, 0.0], [0.0, 1.0, 0.0], 2.0, 1.0).unwrap().scaled(1.0 + seconds as f32).unwrap();
    vortex.sum(&FieldValue::uniform([0.3, 0.0, 0.0]).unwrap().air(0.8).unwrap()).unwrap()
}

/// What a flown swarm ends as: its particles, and each tick it ran with that
/// tick's length.
struct Flown {
    particles: Vec<Particle>,
    ran: Vec<(u64, f32)>,
    costs: Vec<f64>,
}

/// node.swarm_forces into node.move_swarm over frames at `times`, the
/// history replay sampling each tick's start, with Speed `speed` over
/// transport time.
fn fly(start: &[Particle], times: &[f64], speed: fn(f64) -> f32) -> Flown {
    let mut h = Harness::new();
    let (particles, buffer) = h.array(start, start.len());
    let field_slot = h.cpu_input(PortType::VectorField, field(0.0));
    let (forces, _) = h.array::<f32>(&[], 1);
    let (tick_seconds, _) = h.array::<f32>(&[], 1);
    let scalars: Vec<(&'static str, Slot)> = WIRED.iter().map(|&name| (name, h.scalar())).collect();
    let mut forces_outputs = scalars.clone();
    forces_outputs.extend([("forces", forces), ("tick_seconds", tick_seconds)]);
    let mut mover_inputs = vec![("in", particles), ("forces", forces), ("tick_seconds", tick_seconds)];
    mover_inputs.extend(&scalars);
    let mover_params = params(&[("active_count", start.len() as f32)]);
    let step = SimStep::export(Seconds(TICK));
    let mut forces_node = SwarmForces::new();
    let mut mover = MoveSwarm::new();
    let (mut previous, mut ran, mut costs) = (0.0, Vec::new(), Vec::new());
    for &now in times {
        let mut starts = Vec::new();
        forces_node.request_physics_samples(previous, now, &mut starts);
        for at in starts {
            Backend::cpu_values_mut(&mut h.backend).set(field_slot, field(at));
            let knob = params(&[("speed", speed(at))]);
            let (_, errors) = h.run_at(&mut forces_node, &[("acceleration_field", field_slot)], &[], &knob, at, step.authored_sample());
            assert!(errors.is_empty(), "{errors:?}");
        }
        Backend::cpu_values_mut(&mut h.backend).set(field_slot, field(now));
        let knob = params(&[("speed", speed(now))]);
        let (writes, errors) = h.run_at(&mut forces_node, &[("acceleration_field", field_slot)], &forces_outputs, &knob, now, step);
        assert!(errors.is_empty(), "{errors:?}");
        let told = |name: &str| {
            let slot = scalars.iter().find(|(port, _)| *port == name).map(|&(_, slot)| slot).unwrap();
            writes.iter().find(|(s, _)| *s == slot).map(|(_, value)| match value { ParamValue::Float(v) => *v, other => panic!("{other:?}") }).unwrap()
        };
        let (ticks, first, lattices) = (told("ticks") as u64, told("first_tick") as u64, told("force_lattices"));
        assert!(ticks == 0 || lattices == ticks as f32, "a moving field lays one lattice per tick: {ticks} ticks, {lattices} lattices");
        assert_eq!(forces_node.tick_seconds.len() as u64, ticks, "one length per tick");
        ran.extend((first..first + ticks).zip(forces_node.tick_seconds.iter().copied()));
        for (slot, value) in writes {
            h.backend.set_scalar(slot, value);
        }
        let began = std::time::Instant::now();
        let (_, errors) = h.run_at(&mut mover, &mover_inputs, &[("out", particles)], &mover_params, now, step);
        if ticks > 0 {
            costs.push(began.elapsed().as_secs_f64() * 1000.0);
        }
        assert!(errors.is_empty(), "{errors:?}");
        previous = now;
    }
    Flown { particles: read(&buffer, start.len()), ran, costs }
}

#[test]
fn swarm_forces_lattices_move_100k_particles_tick_by_tick() {
    let start: Vec<Particle> = (0..COUNT).map(particle).collect();
    // One second at 30 fps, then half a second at 120 fps.
    let times: Vec<f64> = (0..=30).map(|i| f64::from(i) / 30.0).chain((1..=60).map(|i| 1.0 + f64::from(i) / 120.0)).collect();
    let Flown { particles: moved, ran, mut costs } = fly(&start, &times, |_| 1.0);
    costs.sort_by(f64::total_cmp);
    println!("move_swarm 100k particles: median frame {:.3} ms (encode + GPU + wait), worst {:.3} ms", costs[costs.len() / 2], costs[costs.len() - 1]);
    assert_eq!(ran.iter().map(|&(tick, _)| tick).collect::<Vec<_>>(), (0..90).collect::<Vec<_>>(), "every tick runs once, in order");

    // The CPU twin: tick k reads the field at its own start, k Sim Rate
    // intervals after the first frame.
    let lattice = ForceLattice::over_box([-2.0, 0.0, -2.0], [4.0; 3], 17);
    let lattices: Vec<Vec<[f32; 4]>> = ran
        .iter()
        .map(|&(tick, _)| {
            let mut values = vec![[0.0; 4]; 2 * lattice.node_count()];
            lattice.fill_forces(&mut values, |x| field(tick as f64 * TICK).sample_force(x)).unwrap();
            values
        })
        .collect();
    let mut spin = 0.0f64;
    for (i, (got, from)) in moved.iter().zip(&start).enumerate() {
        if i % 4 == 0 {
            let (mut x, mut v) = (from.position, from.velocity);
            for (values, &(_, dt)) in lattices.iter().zip(&ran) {
                v = lattice.sample_forces(values, x).step_velocity(v, dt);
                for d in 0..3 {
                    x[d] += v[d] * dt;
                }
            }
            for d in 0..3 {
                assert!((got.position[d] - x[d]).abs() <= 1e-3 * (1.0 + x[d].abs()), "particle {i} axis {d}: {} vs {}", got.position[d], x[d]);
                assert!((got.velocity[d] - v[d]).abs() <= 1e-3 * (1.0 + v[d].abs()), "particle {i} velocity {d}: {} vs {}", got.velocity[d], v[d]);
            }
        }
        // Angular momentum about the vortex axis (x = z = 0): z·vx − x·vz.
        spin += f64::from(got.position[2] * got.velocity[0] - got.position[0] * got.velocity[2]);
    }
    let pushes = f64::from(field(0.0).sample([1.0, 2.0, 0.0])[2]).signum();
    assert!(pushes != 0.0 && (-spin).signum() == pushes, "the swarm turns the way the vortex pushes: spin {spin}");
}

/// Speed rising tick by tick while the swarm plays: at 30 fps every frame runs two ticks
/// of different lengths, at 120 fps every other frame runs one. The swarm
/// lands on the same particles, bit for bit.
#[test]
fn swarm_speed_ramp_lands_the_same_at_30_and_120_fps() {
    const N: usize = 16_384;
    let start: Vec<Particle> = (0..N).map(|i| particle(i * (COUNT / N))).collect();
    // Stepped on tick starts: the clock applies a Speed edit from where it is
    // observed, so a smooth curve would differ in the clock itself.
    let ramp = |seconds: f64| 0.5 + 0.75 * ((seconds / TICK + 1e-6).floor() * TICK) as f32;
    let at = |fps: f64| (0..=(fps as u32)).map(|i| f64::from(i) / fps).collect::<Vec<_>>();
    let slow = fly(&start, &at(30.0), ramp);
    let fast = fly(&start, &at(120.0), ramp);
    assert_eq!(slow.ran.len(), 60);
    assert_eq!(slow.ran, fast.ran, "every tick runs with its own length at any frame rate");
    assert!(slow.ran.chunks(2).all(|pair| pair[1].1 > pair[0].1), "a 30 fps frame's two ticks keep their own lengths");
    let bits = |particles: &[Particle]| bytemuck::cast_slice::<Particle, u32>(particles).to_vec();
    assert_eq!(bits(&slow.particles), bits(&fast.particles), "a Speed ramp moves the swarm the same at any frame rate");
    assert!(slow.particles.iter().zip(&start).all(|(a, b)| a.position != b.position), "the ramp moved the swarm");
}

/// node.move_swarm into node.swarm_to_copies, fused into one kernel by the
/// graph compiler's codegen, matches the two standalone kernels bit for bit
/// over three ticks that each read their own lattice and length, the middle
/// one taking a hit.
#[test]
fn move_swarm_fused_with_swarm_to_copies_matches_unfused() {
    const N: usize = 4096;
    let mut h = Harness::new();
    let start: Vec<Particle> = (0..N).map(|i| particle(i * (COUNT / N))).collect();
    let lattice = ForceLattice::over_box([-2.0, 0.0, -2.0], [4.0; 3], 17);
    let flat: Vec<f32> = (0..3)
        .flat_map(|tick| {
            let mut values = vec![[0.0f32; 4]; 2 * lattice.node_count()];
            lattice.fill_forces(&mut values, |x| field(f64::from(tick)).sample_force(x)).unwrap();
            values
        })
        .flatten()
        .collect();
    let (forces, forces_buffer) = h.array(&flat, flat.len());
    let lengths = [0.8 * TICK as f32, TICK as f32, 1.3 * TICK as f32];
    let (tick_seconds, tick_seconds_buffer) = h.array(&lengths, lengths.len());
    // A hit landing on the middle tick: one impulse lattice per tick from
    // the first, zero where no hit landed. The fused kernel kicks exactly as
    // the standalone one.
    let count = lattice.node_count();
    let mut kicks = vec![[0.0f32; 4]; 3 * count];
    lattice.fill(&mut kicks[count..2 * count], |x| field(0.0).sample(x).map(|v| -0.5 * v)).unwrap();
    let kicks: Vec<f32> = kicks.into_iter().flatten().collect();
    let (impulses, impulses_buffer) = h.array(&kicks, kicks.len());
    let (particles, particles_buffer) = h.array(&start, N);
    let values = [
        ("active_count", N as f32), ("ticks", 3.0), ("first_tick", 5.0), ("force_lattices", 3.0),
        ("field_nodes_x", 17.0), ("field_nodes_y", 17.0), ("field_nodes_z", 17.0), ("field_spacing", 0.25),
        ("field_min_x", -2.0), ("field_min_y", 0.0), ("field_min_z", -2.0), ("impulse_tick", 5.0),
        ("size", 0.07),
    ];
    let region = vec![
        member::<MoveSwarm>(0, vec![InputSource::External(0), InputSource::External(1), InputSource::External(2), InputSource::External(3)]),
        member::<SwarmToCopies>(1, vec![InputSource::Node(NodeInstanceId(0))]),
    ];
    let externals = [&particles_buffer, &forces_buffer, &tick_seconds_buffer, &impulses_buffer];
    let fused: Vec<InstanceTransform> = fused_into(&mut h, region, &externals, N, &values, "copies");

    let p = params(&values);
    let inputs = [("in", particles), ("forces", forces), ("tick_seconds", tick_seconds), ("impulses", impulses)];
    let (_, errors) = h.run(&mut MoveSwarm::new(), &inputs, &[("out", particles)], &p);
    assert!(errors.is_empty(), "{errors:?}");
    let (copies, copies_buffer) = h.array::<InstanceTransform>(&[], N);
    let (_, errors) = h.run(&mut SwarmToCopies::new(), &[("particles", particles)], &[("copies", copies)], &p);
    assert!(errors.is_empty(), "{errors:?}");
    let unfused: Vec<InstanceTransform> = read(&copies_buffer, N);

    let bits = |copies: &[InstanceTransform]| bytemuck::cast_slice::<InstanceTransform, u32>(copies).to_vec();
    assert_eq!(bits(&fused), bits(&unfused), "fused and unfused swarm copies differ");
    let moved: Vec<Particle> = read(&particles_buffer, N);
    assert!(moved.iter().zip(&start).filter(|(a, b)| a.position != b.position).count() > N / 2, "the ticks moved the swarm");
}

/// A frame of three ticks with hits on the first and last: each tick reads
/// its own impulse lattice, so a particle at rest gains the first kick before
/// its first step and the second before its last.
#[test]
fn move_swarm_kicks_each_tick_from_its_own_impulse_lattice() {
    const N: usize = 1024;
    let mut h = Harness::new();
    let start: Vec<Particle> = (0..N).map(|i| particle(i * (COUNT / N))).collect();
    let lattice = ForceLattice::over_box([-2.0, 0.0, -2.0], [4.0; 3], 5);
    let count = lattice.node_count();
    let (a, b) = ([0.5f32, 0.0, 0.0], [0.0f32, 0.0, -2.0]);
    let mut kicks = vec![[0.0f32; 4]; 3 * count];
    kicks[..count].fill([a[0], a[1], a[2], 0.0]);
    kicks[2 * count..].fill([b[0], b[1], b[2], 0.0]);
    let kicks: Vec<f32> = kicks.into_iter().flatten().collect();
    let (impulses, _) = h.array(&kicks, kicks.len());
    let lengths = [0.8 * TICK as f32, TICK as f32, 1.3 * TICK as f32];
    let (tick_seconds, _) = h.array(&lengths, lengths.len());
    let (particles, particles_buffer) = h.array(&start, N);
    let values = [
        ("active_count", N as f32), ("ticks", 3.0), ("first_tick", 9.0), ("force_lattices", 0.0),
        ("field_nodes_x", 5.0), ("field_nodes_y", 5.0), ("field_nodes_z", 5.0), ("field_spacing", 1.0),
        ("field_min_x", -2.0), ("field_min_y", 0.0), ("field_min_z", -2.0), ("impulse_tick", 9.0),
    ];
    let inputs = [("in", particles), ("tick_seconds", tick_seconds), ("impulses", impulses)];
    let (_, errors) = h.run(&mut MoveSwarm::new(), &inputs, &[("out", particles)], &params(&values));
    assert!(errors.is_empty(), "{errors:?}");
    let moved: Vec<Particle> = read(&particles_buffer, N);
    for (moved, start) in moved.iter().zip(&start) {
        let mut v = [0.0f32; 3];
        let mut x = start.position;
        for (tick, dt) in lengths.into_iter().enumerate() {
            let kick = [a, [0.0; 3], b][tick];
            for axis in 0..3 {
                v[axis] += kick[axis];
                x[axis] += v[axis] * dt;
            }
        }
        // Trilinear weights sum to 1 up to rounding.
        for axis in 0..3 {
            assert!((moved.velocity[axis] - v[axis]).abs() < 1e-6, "every particle took both kicks, once each: {:?} vs {v:?}", moved.velocity);
            assert!((moved.position[axis] - x[axis]).abs() < 1e-5, "{:?} vs {x:?}", moved.position);
        }
    }
}
