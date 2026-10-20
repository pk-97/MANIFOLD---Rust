//! GPU proofs (SCENE_SETUP_PROGRAMME.md E1). node.swarm_forces lays a
//! moving vortex onto the shared force lattice tick by tick, through its
//! clock, the history replay and the staged upload, and node.move_swarm moves
//! 100k particles with it. At 30 fps every frame runs two ticks on two
//! lattices; at 120 fps every other frame runs none. Every particle lands
//! where the lattice's CPU twin integrates it. And the fused swarm kernel
//! matches the unfused pair.

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

/// node.swarm_forces's outputs node.move_swarm reads, by the same name.
const WIRED: [&str; 11] = [
    "ticks", "first_tick", "force_lattices", "field_nodes_x", "field_nodes_y", "field_nodes_z",
    "field_spacing", "field_min_x", "field_min_y", "field_min_z", "tick_seconds",
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

/// A vortex about the box's vertical axis that swells with transport time:
/// every tick has its own lattice.
fn field(seconds: f64) -> FieldValue {
    FieldValue::vortex([0.0, 2.0, 0.0], [0.0, 1.0, 0.0], 2.0, 1.0).unwrap().scaled(1.0 + seconds as f32).unwrap()
}

#[test]
fn swarm_forces_lattices_move_100k_particles_tick_by_tick() {
    let start: Vec<Particle> = (0..COUNT).map(particle).collect();
    let mut h = Harness::new();
    let (particles, buffer) = h.array(&start, COUNT);
    let field_slot = h.cpu_input(PortType::VectorField, field(0.0));
    let (forces, _) = h.array::<f32>(&[], 1);
    let scalars: Vec<(&'static str, Slot)> = WIRED.iter().map(|&name| (name, h.scalar())).collect();
    let mut forces_outputs = scalars.clone();
    forces_outputs.push(("forces", forces));
    let mut mover_inputs = vec![("in", particles), ("forces", forces)];
    mover_inputs.extend(&scalars);
    let none = params(&[]);
    let mover_params = params(&[("active_count", COUNT as f32)]);
    let step = SimStep::export(Seconds(TICK));
    let mut forces_node = SwarmForces::new();
    let mut mover = MoveSwarm::new();

    // One second at 30 fps, then half a second at 120 fps.
    let times: Vec<f64> = (0..=30).map(|i| f64::from(i) / 30.0).chain((1..=60).map(|i| 1.0 + f64::from(i) / 120.0)).collect();
    let (mut previous, mut ran, mut costs) = (0.0, Vec::new(), Vec::new());
    for &now in &times {
        let mut starts = Vec::new();
        forces_node.request_physics_samples(previous, now, &mut starts);
        for at in starts {
            Backend::cpu_values_mut(&mut h.backend).set(field_slot, field(at));
            let (_, errors) = h.run_at(&mut forces_node, &[("acceleration_field", field_slot)], &[], &none, at, step.authored_sample());
            assert!(errors.is_empty(), "{errors:?}");
        }
        Backend::cpu_values_mut(&mut h.backend).set(field_slot, field(now));
        let (writes, errors) = h.run_at(&mut forces_node, &[("acceleration_field", field_slot)], &forces_outputs, &none, now, step);
        assert!(errors.is_empty(), "{errors:?}");
        let told = |name: &str| {
            let slot = scalars.iter().find(|(port, _)| *port == name).map(|&(_, slot)| slot).unwrap();
            writes.iter().find(|(s, _)| *s == slot).map(|(_, value)| match value { ParamValue::Float(v) => *v, other => panic!("{other:?}") }).unwrap()
        };
        let (ticks, first, lattices, dt) = (told("ticks") as u64, told("first_tick") as u64, told("force_lattices"), told("tick_seconds"));
        assert!(ticks == 0 || lattices == ticks as f32, "a moving field lays one lattice per tick: {ticks} ticks, {lattices} lattices");
        ran.extend((first..first + ticks).map(|tick| (tick, dt)));
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
    costs.sort_by(f64::total_cmp);
    println!("move_swarm 100k particles: median frame {:.3} ms (encode + GPU + wait), worst {:.3} ms", costs[costs.len() / 2], costs[costs.len() - 1]);
    assert_eq!(ran.iter().map(|&(tick, _)| tick).collect::<Vec<_>>(), (0..90).collect::<Vec<_>>(), "every tick runs once, in order");

    // The CPU twin: tick k reads the field at its own start, k Sim Rate
    // intervals after the first frame.
    let lattice = ForceLattice::over_box([-2.0, 0.0, -2.0], [4.0; 3], 17);
    let lattices: Vec<Vec<[f32; 4]>> = ran
        .iter()
        .map(|&(tick, _)| {
            let mut values = vec![[0.0; 4]; lattice.node_count()];
            lattice.fill(&mut values, |x| field(tick as f64 * TICK).sample(x)).unwrap();
            values
        })
        .collect();
    let moved: Vec<Particle> = read(&buffer, COUNT);
    let mut spin = 0.0f64;
    for (i, (got, from)) in moved.iter().zip(&start).enumerate() {
        if i % 4 == 0 {
            let (mut x, mut v) = (from.position, from.velocity);
            for (values, &(_, dt)) in lattices.iter().zip(&ran) {
                let a = lattice.sample(values, x);
                for d in 0..3 {
                    v[d] += a[d] * dt;
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

/// node.move_swarm into node.swarm_to_copies, fused into one kernel by the
/// graph compiler's codegen, matches the two standalone kernels bit for bit
/// over three ticks that each read their own lattice.
#[test]
fn move_swarm_fused_with_swarm_to_copies_matches_unfused() {
    const N: usize = 4096;
    let mut h = Harness::new();
    let start: Vec<Particle> = (0..N).map(|i| particle(i * (COUNT / N))).collect();
    let lattice = ForceLattice::over_box([-2.0, 0.0, -2.0], [4.0; 3], 17);
    let flat: Vec<f32> = (0..3)
        .flat_map(|tick| {
            let mut values = vec![[0.0f32; 4]; lattice.node_count()];
            lattice.fill(&mut values, |x| field(f64::from(tick)).sample(x)).unwrap();
            values
        })
        .flatten()
        .collect();
    let (forces, forces_buffer) = h.array(&flat, flat.len());
    let (particles, particles_buffer) = h.array(&start, N);
    let values = [
        ("active_count", N as f32), ("ticks", 3.0), ("first_tick", 5.0), ("force_lattices", 3.0),
        ("field_nodes_x", 17.0), ("field_nodes_y", 17.0), ("field_nodes_z", 17.0), ("field_spacing", 0.25),
        ("field_min_x", -2.0), ("field_min_y", 0.0), ("field_min_z", -2.0), ("tick_seconds", TICK as f32), ("size", 0.07),
    ];
    let region = vec![
        member::<MoveSwarm>(0, vec![InputSource::External(0), InputSource::External(1)]),
        member::<SwarmToCopies>(1, vec![InputSource::Node(NodeInstanceId(0))]),
    ];
    let fused: Vec<InstanceTransform> = fused_into(&mut h, region, &[&particles_buffer, &forces_buffer], N, &values, "copies");

    let p = params(&values);
    let (_, errors) = h.run(&mut MoveSwarm::new(), &[("in", particles), ("forces", forces)], &[("out", particles)], &p);
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
