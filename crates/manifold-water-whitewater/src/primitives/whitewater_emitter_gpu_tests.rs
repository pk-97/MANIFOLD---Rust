//! Small value proofs for the reference emitters. Run only through gpu_queue.
//! The turbulence reference is independently checked against the vendored C++ engine.
//! The grid arrays sit on world tiles: a box of nine one-metre nodes a side
//! whose first cell is world cell (-4, 0, 0), so it straddles tiles -1 and 0
//! on x. Particles lie well inside it, where the pooled atoms read the box's
//! fields, or in a tile the table does not hold, where they read the
//! canonical element.
use super::{
    dust_potential::DustPotential,
    energy_potential::EnergyPotential,
    inside_turbulence_potential::InsideTurbulencePotential,
    turbulence_emission_count::TurbulenceEmissionCount,
    whitewater_cpu::{CANONICAL_SOLID, canonical_distance},
    whitewater_emitter_velocity::WhitewaterEmitterVelocity,
    whitewater_influence::WhitewaterInfluence,
    whitewater_obstacle_source::{ObstacleSourceJob, WhitewaterSource, encode_obstacle_source},
    whitewater_particle_tests::Rig,
};
use manifold_gpu::GpuComputePipeline;
use manifold_node_engine::exec::effect_node::NodeInstanceId;
use manifold_node_engine::scene::world_tiles::TILE_CELLS;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_node_engine::testkit::fused_atoms::{fused, member, run};
use manifold_node_engine::{freeze::codegen::InputSource, particles::FluidParticle};
use manifold_water_liquid::tile_pool::TilePool;
use manifold_water_liquid::whitewater::CELL_AIR;
use {super::whitewater_particle_cpu::Box3, crate::primitives::whitewater_particle_cpu as cpu};

const ORIGIN: [i32; 3] = [-4, 0, 0];
const NODES: [u32; 3] = [9; 3];
const H: f32 = 1.0;
/// Metres along x that carry a particle into a tile the table does not hold.
const FAR: f32 = 64.0 * H;

fn rig() -> Rig {
    Rig::new(ORIGIN, NODES, H)
}

/// 32 live particles `offset` metres from the box's first node, every fourth
/// carried `FAR` along x into a tile the table does not hold.
fn placed(rig: &Rig, offset: [f32; 3], velocity: [f32; 3]) -> Vec<FluidParticle> {
    let at = rig.point(offset);
    (0..32)
        .map(|i| FluidParticle {
            position_radius: [at[0] + if far(i) { FAR } else { 0.0 }, at[1], at[2], 0.1],
            velocity,
            id: i as u32 + 1,
        })
        .collect()
}

fn far(i: usize) -> bool {
    i % 4 == 3
}

/// `near` in the box's slots of a 32-slot array, 0 in the far ones.
fn in_box(near: f32) -> Vec<f32> {
    (0..32).map(|i| if far(i) { 0.0 } else { near }).collect()
}

fn close(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (i, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (a - b).abs() <= 3e-5 * b.abs().max(1.0),
            "element {i}: GPU {a}, CPU {b}"
        );
    }
}

#[test]
fn whitewater_inside_dust_counts_and_fusion() {
    let rig = rig();
    let mut h = Harness::new();
    let part = h.array(&placed(&rig, [3.5, 3.5, 2.5], [12.0, 0.0, 0.0]), 32);
    let phi = rig.cell_pool(&mut h, &vec![-4.0f32; 512], canonical_distance(H));
    let turb = rig.cell_pool(&mut h, &vec![150.0f32; 512], 0.0);
    let cells = rig.cell_pool(&mut h, &vec![1u32; 512], CELL_AIR);
    let energy = h.array(&[1.0f32; 32], 32);
    let wave = h.array(&[0.0f32; 32], 32);
    let influence = rig.node_pool(&mut h, &vec![2.0f32; 729], 1.0);
    let solid = rig.node_pool(&mut h, &vec![1.0f32; 729], CANONICAL_SOLID);
    let obstacle = WhitewaterSource {
        influence: 2.0,
        dust_strength: 0.5,
        kind: 2,
        pad: 0,
    };
    let source = rig.node_pool(&mut h, &vec![obstacle; 729], WhitewaterSource::default());
    let tiles = rig.tiles(&mut h);
    let dt = 1.0 / 30.0;
    let v = rig.values(&[
        ("dt", dt),
        ("dust_enabled", 1.0),
        ("generation_rate", 0.6),
        ("seed", 7.0),
    ]);
    let p = params(&v);
    let inside: Vec<f32> = run(
        &mut h,
        &mut InsideTurbulencePotential::new(),
        &[
            ("particles", part.0),
            ("distance", phi.0),
            ("turbulence", turb.0),
            ("cells", cells.0),
            ("tiles", tiles.0),
        ],
        32,
        &p,
    );
    close(&inside, &in_box(0.5));
    let dust: Vec<f32> = run(
        &mut h,
        &mut DustPotential::new(),
        &[
            ("particles", part.0),
            ("solid", solid.0),
            ("turbulence", turb.0),
            ("source", source.0),
            ("tiles", tiles.0),
        ],
        32,
        &p,
    );
    close(&dust, &in_box(0.3));
    let count_ports = |h: &mut Harness, potential: &[f32]| {
        let t = h.array(potential, 32);
        let ports = [
            ("particles", part.0),
            ("energy", energy.0),
            ("wavecrest", wave.0),
            ("turbulence", t.0),
            ("influence", influence.0),
            ("tiles", tiles.0),
        ];
        run::<_, u32>(h, &mut TurbulenceEmissionCount::new(), &ports, 32, &p)
    };
    let expected = |i: u32, potential: f32| -> u32 {
        if cpu::random(i, 7.0f32.to_bits(), 0, 10) >= 0.6 {
            0
        } else {
            super::whitewater_emitter_cpu::emission_count(
                1.0,
                [0.0, potential],
                [175.0; 2],
                2.0,
                8.0,
                1.0,
                dt,
            )
        }
    };
    for (is_dust, potential) in [(false, inside), (true, dust)] {
        let got = count_ports(&mut h, &potential);
        let want: Vec<u32> = (0..32)
            .map(|i| expected(i, potential[i as usize]))
            .collect();
        assert_eq!(got, want);
        assert!(want.iter().any(|&n| n > 0), "some slots emit");
        let inputs: Vec<InputSource> = (0..5).map(InputSource::External).collect();
        let a = if is_dust {
            member::<DustPotential>(0, inputs)
        } else {
            member::<InsideTurbulencePotential>(0, inputs)
        };
        let fused = fused::<u32>(
            &mut h,
            vec![
                a,
                member::<TurbulenceEmissionCount>(
                    1,
                    vec![
                        InputSource::External(0),
                        InputSource::External(5),
                        InputSource::External(6),
                        InputSource::Node(NodeInstanceId(0)),
                        InputSource::External(7),
                        InputSource::External(4),
                    ],
                ),
            ],
            &[
                &part.1,
                if is_dust { &solid.1 } else { &phi.1 },
                &turb.1,
                if is_dust { &source.1 } else { &cells.1 },
                &tiles.1,
                &energy.1,
                &wave.1,
                &influence.1,
            ],
            32,
            &v,
        );
        assert_eq!(fused, got);
    }
    // A particle in no stored tile emits nothing whatever its potential.
    let got = count_ports(&mut h, &[0.5; 32]);
    let want: Vec<u32> = (0..32)
        .map(|i| if far(i as usize) { 0 } else { expected(i, 0.5) })
        .collect();
    assert_eq!(got, want);
    assert!(want.iter().any(|&n| n > 0), "some slots emit");
    // Boundary dust is disabled by default, and when enabled only markers
    // within 1.5 cells of the Bounds' low z face emit: the box's first node
    // sits 1.5 cells below it.
    let domain = WhitewaterSource {
        influence: 1.0,
        dust_strength: 1.0,
        kind: 1,
        pad: 0,
    };
    let domain = rig.node_pool(&mut h, &vec![domain; 729], WhitewaterSource::default());
    let high = h.array(&placed(&rig, [3.5, 3.5, 6.5], [12.0, 0.0, 0.0]), 32);
    for (boundary, bottom, top) in [(0.0, 0.0, 0.0), (1.0, 0.6, 0.0)] {
        let p = rig.params(&[
            ("dust_enabled", 1.0),
            ("boundary_dust", boundary),
            ("bounds_lo_z", rig.first_node()[2] + 1.5 * H),
        ]);
        for (at, want) in [(&part, bottom), (&high, top)] {
            let got: Vec<f32> = run(
                &mut h,
                &mut DustPotential::new(),
                &[
                    ("particles", at.0),
                    ("solid", solid.0),
                    ("turbulence", turb.0),
                    ("source", domain.0),
                    ("tiles", tiles.0),
                ],
                32,
                &p,
            );
            close(&got, &in_box(want));
        }
    }
    let air = rig.cell_pool(&mut h, &vec![0u32; 512], CELL_AIR);
    let surface = rig.cell_pool(&mut h, &vec![0.0f32; 512], canonical_distance(H));
    let got: Vec<f32> = run(
        &mut h,
        &mut InsideTurbulencePotential::new(),
        &[
            ("particles", part.0),
            ("distance", surface.0),
            ("turbulence", turb.0),
            ("cells", air.0),
            ("tiles", tiles.0),
        ],
        32,
        &p,
    );
    close(&got, &[0.0; 32]);
}

#[test]
fn whitewater_emitter_speed_values_and_fusion() {
    let rig = rig();
    let particles = placed(&rig, [3.5; 3], [2.0, 1.0, 0.0]);
    let mut h = Harness::new();
    let input = h.array(&particles, 32);
    let phi = rig.cell_pool(&mut h, &vec![0.0f32; 512], canonical_distance(H));
    let air = rig.cell_pool(&mut h, &vec![0u32; 512], CELL_AIR);
    let tiles = rig.tiles(&mut h);
    let v = rig.values(&[("spray_speed", 2.0), ("seed", 9.0), ("epoch", 3.0)]);
    let p = params(&v);
    let got: Vec<FluidParticle> = run(
        &mut h,
        &mut WhitewaterEmitterVelocity::new(),
        &[
            ("particles", input.0),
            ("distance", phi.0),
            ("cells", air.0),
            ("tiles", tiles.0),
        ],
        32,
        &p,
    );
    for (i, particle) in got.iter().enumerate() {
        // The surface cells of the box speed up; a cell in no stored tile is
        // three cells from the surface, and keeps its speed.
        let speed = if far(i) {
            1.0
        } else {
            1.0 + cpu::random(i as u32, 9.0f32.to_bits(), 3, 9)
        };
        close(
            &particle.velocity,
            &particles[i].velocity.map(|x| x * speed),
        );
        assert_eq!(particle.position_radius, particles[i].position_radius);
    }
    let boosted = h.array(&got, 32);
    let unfused: Vec<f32> = run(
        &mut h,
        &mut EnergyPotential::new(),
        &[("particles", boosted.0)],
        32,
        &p,
    );
    let folded = fused::<f32>(
        &mut h,
        vec![
            member::<WhitewaterEmitterVelocity>(0, (0..4).map(InputSource::External).collect()),
            member::<EnergyPotential>(1, vec![InputSource::Node(NodeInstanceId(0))]),
        ],
        &[&input.1, &phi.1, &air.1, &tiles.1],
        32,
        &v,
    );
    close(&folded, &unfused);
}

/// The kernel into the outputs of `job`, then both read back.
fn encode_source(
    h: &Harness,
    pipeline: &mut Option<GpuComputePipeline>,
    job: &ObstacleSourceJob<'_>,
) -> (Vec<WhitewaterSource>, Vec<f32>) {
    let mut enc = h.device.create_encoder("whitewater-source-reference");
    encode_obstacle_source(
        pipeline,
        &h.device,
        &mut enc,
        job,
        "whitewater-source-reference",
    );
    enc.commit_and_wait_completed();
    let elements = job.pool.elements() as usize;
    (read(job.source, elements), read(job.distance, elements))
}

fn fields(s: &WhitewaterSource) -> (f32, f32, u32, u32) {
    (s.influence, s.dust_strength, s.kind, s.pad)
}

#[test]
fn whitewater_obstacle_source_closed_and_open_domain() {
    use manifold_water_liquid::bodies::{LiquidBody, LiquidShape};
    use manifold_water_liquid::bounds::bounds_distance;
    use manifold_water_liquid::testkit::world_box::table_words_of;
    let rig = rig();
    // A pool one slot longer than the box's tiles: the table leaves that slot
    // free, and the pool's last element is the canonical one.
    let pool = TilePool {
        pool_tiles: rig.nodes.pool_tiles + 1,
        cell_size: H,
    };
    let words = table_words_of(pool.pool_tiles, &rig.nodes.held());
    let elements = pool.elements() as usize;
    let boxed = rig.nodes.pool_tiles as usize * TILE_CELLS as usize;
    // Every node of the box: its pool index and scene position.
    let mut nodes = Vec::new();
    for k in 0..NODES[2] {
        for j in 0..NODES[1] {
            for i in 0..NODES[0] {
                let c = [i, j, k];
                let x: [f32; 3] =
                    std::array::from_fn(|a| ((ORIGIN[a] + c[a] as i32) as f32 - 0.5) * H);
                nodes.push((rig.nodes.pool_index(c), x));
            }
        }
    }
    let mut h = Harness::new();
    let tiles = h.array(&words, words.len());
    let bodies = h.array::<LiquidBody>(&[], 1);
    let shapes = h.array::<LiquidShape>(&[], 1);
    let atlas = h.array::<u32>(&[], 1);
    // Outputs the kernel must overwrite everywhere.
    let stale = WhitewaterSource {
        influence: 9.0,
        dust_strength: 9.0,
        kind: 9,
        pad: 9,
    };
    let source = h.array(&vec![stale; elements], elements);
    let distance = h.array(&vec![9.0f32; elements], elements);
    // A box off the node grid, different on every axis.
    let (lo, hi) = (rig.point([0.5, 1.25, 2.0]), rig.point([7.5, 6.75, 8.5]));
    let mut job = ObstacleSourceJob {
        influence: 2.0,
        dust_strength: 0.5,
        pool,
        closed_faces: 0,
        bounds_lo: lo,
        bounds_hi: hi,
        body_count: 0,
        rows: 0,
        tick_seconds: 1.0 / 60.0,
        bodies: &bodies.1,
        shapes: &shapes.1,
        atlas: &atlas.1,
        tiles: &tiles.1,
        source: &source.1,
        distance: &distance.1,
    };
    let mut pipeline = None;
    let canonical = |got: &[WhitewaterSource], distances: &[f32]| {
        assert!(got[boxed..].iter().all(|s| fields(s) == (0.0, 0.0, 0, 0)));
        assert!(distances[boxed..].iter().all(|&d| d == 1.0e30));
    };
    for (closed, kind, dust) in [(0b11_1111, 1, 1.0), (0b10_1101, 1, 1.0), (0, 0, 0.5)] {
        job.closed_faces = closed;
        let (got, distances) = encode_source(&h, &mut pipeline, &job);
        for &(at, _) in &nodes {
            assert_eq!(fields(&got[at]), (2.0, dust, kind, 0), "walls {closed:b}");
        }
        let seen: Vec<f32> = nodes.iter().map(|&(at, _)| distances[at]).collect();
        let wanted: Vec<f32> = nodes
            .iter()
            .map(|&(_, x)| bounds_distance(x, lo, hi, closed as u32))
            .collect();
        close(&seen, &wanted);
        canonical(&got, &distances);
    }
    // A constant -1 distance over a lattice covering part of the box: past the
    // lattice it grows by the gap to it. Where it is under the wall distance
    // the obstacle is the nearest object, elsewhere the domain is.
    let position = rig.point([1.3, 0.7, 2.2]);
    let dims = [5u32, 4, 6];
    let body = h.array(
        &[LiquidBody {
            position_inv_mass: [position[0], position[1], position[2], 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            ..Default::default()
        }],
        1,
    );
    let shape = h.array(
        &[LiquidShape {
            origin_spacing: [0.0, 0.0, 0.0, H],
            dims_x: dims[0],
            dims_y: dims[1],
            dims_z: dims[2],
            atlas_offset: 0,
            scale_min: [1.0; 4],
        }],
        1,
    );
    let negative = h.array(&vec![0xbc00_bc00u32; 60], 60);
    job.closed_faces = 0b11_1111;
    job.body_count = 1;
    job.rows = 1;
    job.bodies = &body.1;
    job.shapes = &shape.1;
    job.atlas = &negative.1;
    let (got, distances) = encode_source(&h, &mut pipeline, &job);
    let mut kinds = [0; 3];
    let mut wanted = Vec::new();
    for &(at, x) in &nodes {
        let gap: f32 = (0..3)
            .map(|a| {
                let g = (x[a] - position[a]) / H;
                (g - g.clamp(0.0, (dims[a] - 1) as f32)).powi(2)
            })
            .sum::<f32>()
            .sqrt();
        let obstacle = -1.0 + gap * H;
        let wall = bounds_distance(x, lo, hi, 0b11_1111);
        let (kind, dust, nearest) = if obstacle < wall {
            (2, 0.5, obstacle)
        } else {
            (1, 1.0, wall)
        };
        assert_eq!(fields(&got[at]), (2.0, dust, kind, 0), "node at {x:?}");
        wanted.push(nearest);
        kinds[kind as usize] += 1;
    }
    let seen: Vec<f32> = nodes.iter().map(|&(at, _)| distances[at]).collect();
    close(&seen, &wanted);
    assert!(
        kinds[1] > 50 && kinds[2] > 50,
        "the obstacle and the walls each set some nodes: {kinds:?}"
    );
    canonical(&got, &distances);
    // The influence over the node arrays: an obstacle within three cells sets
    // its influence, a domain wall the base level, the rest decay toward it.
    let metadata = h.array(&got, elements);
    let solid = h.array(&distances, elements);
    let previous = h.array(&vec![0.5f32; elements], elements);
    let influence: Vec<f32> = run(
        &mut h,
        &mut WhitewaterInfluence::new(),
        &[
            ("values", previous.0),
            ("solid", solid.0),
            ("source", metadata.0),
        ],
        elements,
        &params(&[("cell_size", H)]),
    );
    let decayed = 0.5f32 + 2.0 * (1.0 / 60.0);
    let want: Vec<f32> = got
        .iter()
        .zip(&distances)
        .map(|(s, &d)| match s.kind {
            _ if d.abs() > 3.0 * H => decayed,
            1 => 1.0,
            2 => s.influence,
            _ => decayed,
        })
        .collect();
    close(&influence, &want);
    for level in [1.0, 2.0, decayed] {
        assert!(want.contains(&level), "{level} is set somewhere");
    }
}

#[test]
fn whitewater_fresh_spray_speed_and_dust_typing() {
    use super::whitewater_type::WhitewaterType;
    use manifold_water_liquid::fluid_particles::WhitewaterSpawn;
    let rig = rig();
    let at = rig.point([4.5; 3]);
    // Even slots sit in the box's surface cells, odd slots in a tile the table
    // does not hold.
    let spawns: Vec<WhitewaterSpawn> = (0..32)
        .map(|i| WhitewaterSpawn {
            position_lifetime: [
                at[0] + if i % 2 == 1 { FAR } else { 0.0 },
                at[1],
                at[2],
                2.0,
            ],
            velocity: [2.0, 1.0, 0.0],
            kind: 0,
        })
        .collect();
    let mut h = Harness::new();
    let input = h.array(&spawns, 32);
    let phi = rig.cell_pool(&mut h, &vec![0.0f32; 512], canonical_distance(H));
    let air = rig.cell_pool(&mut h, &vec![0u32; 512], CELL_AIR);
    let tiles = rig.tiles(&mut h);
    for dust in [0.0, 1.0] {
        let p = rig.params(&[
            ("spray_speed", 2.0),
            ("seed", 9.0),
            ("epoch", 3.0),
            ("dust", dust),
        ]);
        let got: Vec<WhitewaterSpawn> = run(
            &mut h,
            &mut WhitewaterType::new(),
            &[
                ("spawns", input.0),
                ("distance", phi.0),
                ("cells", air.0),
                ("tiles", tiles.0),
            ],
            32,
            &p,
        );
        for (i, s) in got.iter().enumerate() {
            // Foam at the surface keeps its speed; spray outside every stored
            // tile is scaled; dust is typed 4 and left alone.
            let (kind, factor) = if dust > 0.5 {
                (4, 1.0)
            } else if i % 2 == 0 {
                (1, 1.0)
            } else {
                (2, 1.0 + cpu::random(i as u32, 9.0f32.to_bits(), 3, 11))
            };
            close(&s.velocity, &spawns[i].velocity.map(|v| v * factor));
            assert_eq!(s.kind, kind, "slot {i}");
        }
    }
}

#[test]
fn whitewater_dust_lifecycle_values_and_fusion() {
    use super::{
        advect_whitewater::AdvectWhitewater, age_whitewater::AgeWhitewater,
        whitewater_pool_cpu as pool,
    };
    use manifold_water_liquid::fluid_particles::FaceSample;
    use manifold_water_liquid::whitewater::WhitewaterParticle;
    let rig = rig();
    let at = rig.point([4.0; 3]);
    // The dense grid the CPU statement reads: the box's eight cells.
    let grid = Box3 {
        cells: [8; 3],
        center: at,
        size: [8.0; 3],
    };
    let particles: Vec<_> = (0..32)
        .map(|i| WhitewaterParticle {
            position_lifetime: [at[0], at[1], at[2], 2.0],
            velocity: [0.2, 0.0, 0.0],
            kind: 4,
            id: i * 8,
            ..Default::default()
        })
        .collect();
    let faces = [vec![0.5f32; 576], vec![0.0; 576], vec![0.0; 576]];
    let solid = vec![10.0f32; 729];
    let fields = pool::Fields {
        faces: faces.each_ref().map(Vec::as_slice),
        face_cells: [8; 3],
        solid: &solid,
    };
    let mut h = Harness::new();
    let input = h.array(&particles, 32);
    // Every face of the box's nodes moves 0.5 m/s along x; the faces' weights
    // are valid.
    let record = FaceSample {
        velocity: [0.5, 0.0, 0.0, 0.0],
        weight: [1.0, 1.0, 1.0, 0.0],
    };
    let f = rig.node_pool(&mut h, &vec![record; 729], FaceSample::default());
    let s = rig.node_pool(&mut h, &solid, CANONICAL_SOLID);
    let tiles = rig.tiles(&mut h);
    let v = rig.values(&[("dt", 1.0 / 60.0)]);
    let p = params(&v);
    let got: Vec<WhitewaterParticle> = run(
        &mut h,
        &mut AdvectWhitewater::new(),
        &[
            ("pool", input.0),
            ("faces", f.0),
            ("solid", s.0),
            ("tiles", tiles.0),
        ],
        32,
        &p,
    );
    for (a, b) in got.iter().zip(&particles) {
        let want = pool::advect(*b, &fields, &grid, pool::Advect::flip(), None).0;
        close(&a.position_lifetime, &want.position_lifetime);
        close(&a.velocity, &want.velocity);
        assert_eq!(a.kind, 4);
    }
    let advanced = h.array(&got, 32);
    let unfused: Vec<WhitewaterParticle> = run(
        &mut h,
        &mut AgeWhitewater::new(),
        &[("pool", advanced.0)],
        32,
        &p,
    );
    // Pool, faces, solid, the four optional arrays unwired (any array
    // satisfies them: the substep count is 0), tiles.
    let advect_inputs = [0, 1, 2, 2, 2, 2, 2, 3].map(InputSource::External);
    let folded = fused::<WhitewaterParticle>(
        &mut h,
        vec![
            member::<AdvectWhitewater>(0, advect_inputs.to_vec()),
            member::<AgeWhitewater>(1, vec![InputSource::Node(NodeInstanceId(0))]),
        ],
        &[&input.1, &f.1, &s.1, &tiles.1],
        32,
        &v,
    );
    for ((a, b), old) in folded.iter().zip(&unfused).zip(&got) {
        close(&a.position_lifetime, &b.position_lifetime);
        close(&a.velocity, &b.velocity);
        assert!((b.position_lifetime[3] - (old.position_lifetime[3] - 1.0 / 60.0)).abs() < 1e-6);
    }
}

#[test]
fn whitewater_dust_step_publishes_a_distinct_population() {
    use super::whitewater_step::{Removal, Step, StepFrame, StepInputs, StepShape};
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
    use manifold_water_liquid::fluid_particles::FaceSample;
    use manifold_water_liquid::whitewater::WhitewaterParticle;
    let rig = rig();
    let at = rig.point([4.0; 3]);
    let mut h = Harness::new();
    let particles = h.array(
        &vec![
            FluidParticle {
                position_radius: [at[0], at[1], at[2], 0.05],
                velocity: [0.0; 3],
                id: 1
            };
            32
        ],
        32,
    );
    let shape = StepShape::new(
        TilePool {
            pool_tiles: rig.cells.pool_tiles,
            cell_size: H,
        },
        256,
    )
    .unwrap();
    let phi = rig.cell_pool(&mut h, &vec![-5.0f32; 512], canonical_distance(H));
    let tiles = rig.tiles(&mut h);
    let solid = rig.node_pool(&mut h, &vec![1.0f32; 729], CANONICAL_SOLID);
    let source = rig.node_pool(
        &mut h,
        &vec![
            WhitewaterSource {
                influence: 1.0,
                dust_strength: 1.0,
                kind: 2,
                pad: 0
            };
            729
        ],
        WhitewaterSource::default(),
    );
    // A shear: x faces carry 12 m/s, y faces alternate +-24 m/s along x.
    let shear: Vec<FaceSample> = (0..729)
        .map(|i| FaceSample {
            velocity: [12.0, if i % 9 % 2 == 0 { 24.0 } else { -24.0 }, 0.0, 0.0],
            weight: [1.0, 1.0, 1.0, 0.0],
        })
        .collect();
    let faces = rig.node_pool(&mut h, &shear, FaceSample::default());
    let identity = h.array(&[0u32, 0, 0, 0, 1, 1, 1, 32], 8);
    let pool = h.array(&vec![super::whitewater_step::empty_slot(); 256], 256);
    let state = h.array(&[0u32; 8], 8);
    for dust in [false, true] {
        let mut step = Step::default();
        let frame = StepFrame {
            shape,
            ticks: 1,
            dt: 1.0 / 60.0,
            epoch: 0,
            seed: 0.0,
            gravity: [0.0, -9.81, 0.0],
            bounds_lo: rig.first_node(),
            removal: Removal::NONE,
            wavecrest_emission: 175.0,
            turbulence_emission: 175.0,
            min_turbulence: 100.0,
            max_turbulence: 200.0,
            inside_emission: true,
            generation_rate: 1.0,
            spray_speed: 1.0,
            dust_emission: dust,
            boundary_dust: false,
            dust_rate: 175.0,
            influence_base: 1.0,
            influence_decay: 2.0,
            min_energy: 0.1,
            max_energy: 60.0,
            preserve_foam: false,
        };
        let inputs = StepInputs {
            motion: None,
            particles: &particles.1,
            identity: &identity.1,
            solid: &solid.1,
            obstacle_source: Some(&source.1),
            faces: &faces.1,
            distance: &phi.1,
            tiles: &tiles.1,
        };
        let mut native = h.device.create_encoder("whitewater-dust-population");
        step.advance_tick(
            &mut GpuEncoder::new(&mut native, &h.device),
            &frame,
            &inputs,
            &pool.1,
            &state.1,
            true,
        )
        .unwrap();
        // Tick outputs are private storage; read them through shared copies.
        let counts_out = h.array(&[0u32; 9], 9);
        let pool_out = h.array(&vec![super::whitewater_step::empty_slot(); 256], 256);
        let dust_out = h.array::<FluidParticle>(&[], 256);
        for (port, destination) in [
            ("counts_out", &counts_out.1),
            ("pool_out", &pool_out.1),
            ("dust_particles", &dust_out.1),
        ] {
            native.copy_buffer_to_buffer(
                step.tick_output(port).unwrap(),
                destination,
                destination.size,
            );
        }
        native.commit_and_wait_completed();
        let counts: Vec<u32> = read(&counts_out.1, 9);
        assert!(counts[1] > 0, "inside bubbles must emit: {counts:?}");
        assert_eq!(
            counts[8] > 0,
            dust,
            "dust toggle controls the separate population: {counts:?}"
        );
        let rows: Vec<WhitewaterParticle> = read(&pool_out.1, 256);
        assert_eq!(
            rows.iter().filter(|p| p.kind == 4).count(),
            counts[8] as usize
        );
        let visible: Vec<FluidParticle> = read(&dust_out.1, 256);
        assert_eq!(
            visible
                .iter()
                .filter(|p| p.position_radius[3] > 0.0)
                .count(),
            counts[8] as usize
        );
    }
}
