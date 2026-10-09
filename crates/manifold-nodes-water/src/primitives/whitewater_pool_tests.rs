//! GPU value proofs for the whitewater pool atoms against their CPU
//! statements (`whitewater_pool_cpu`), and each atom folded by the codegen
//! against its standalone kernel (`docs/GPU_WHITEWATER_DESIGN.md` section
//! 3.9).

use super::advect_whitewater::AdvectWhitewater;
use super::age_whitewater::AgeWhitewater;
use super::preserve_foam::PreserveFoam;
use super::sort_particles_into_cells::SortParticlesIntoCells;
use super::retype_whitewater::RetypeWhitewater;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use super::whitewater_cpu::Rng;
use manifold_node_engine::testkit::water_codegen::run;
use super::whitewater_pool_cpu::fixture::{FACE_CELLS, NODES, faces, grid, pool, tank};
use {crate::primitives::whitewater_pool_cpu as cpu, super::whitewater_pool_cpu::Advect, super::whitewater_pool_cpu::Age, super::whitewater_pool_cpu::DEAD, super::whitewater_pool_cpu::Fields, super::whitewater_pool_cpu::Preserve};
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::particles::FluidParticle;
use crate::fluid_particles::{CellRange, bin_counts};
use manifold_node_engine::exec::effect_node::ParamValues;
use crate::whitewater::{WHITEWATER_EMPTY, WhitewaterParticle};

const SLOTS: usize = 4000;

fn advect_values(s: Advect) -> Vec<(&'static str, f32)> {
    let g = grid();
    vec![
        ("center_x", g.center[0]),
        ("center_y", g.center[1]),
        ("center_z", g.center[2]),
        ("size_x", g.size[0]),
        ("size_y", g.size[1]),
        ("size_z", g.size[2]),
        ("nodes_x", NODES[0] as f32),
        ("nodes_y", NODES[1] as f32),
        ("nodes_z", NODES[2] as f32),
        ("face_cells_x", FACE_CELLS[0] as f32),
        ("face_cells_y", FACE_CELLS[1] as f32),
        ("face_cells_z", FACE_CELLS[2] as f32),
        ("gravity_x", s.gravity[0]),
        ("gravity_y", s.gravity[1]),
        ("gravity_z", s.gravity[2]),
        ("dt", s.dt),
        ("foam_advection", s.foam_advection),
        ("bubble_buoyancy", s.bubble_buoyancy),
        ("bubble_drag", s.bubble_drag),
        ("spray_drag", s.spray_drag),
        ("spray_drag_variance", s.spray_drag_variance),
        ("spray_restitution", s.spray_restitution),
        ("spray_friction", s.spray_friction),
        ("substep_count",0.0), ("field_nodes_x",2.0), ("field_nodes_y",2.0), ("field_nodes_z",2.0),
        ("field_spacing",0.25), ("force_lattices",0.0), ("tick_index",0.0), ("first_tick",0.0),
    ]
}

/// Drag, friction and a sideways gravity on, so every term shows.
fn settings() -> Advect {
    Advect { gravity: [0.7, -9.81, -0.4], spray_drag: 0.5, spray_friction: 0.1, ..Advect::flip() }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 2e-5 * b.abs().max(1.0)
}

/// A velocity read from the faces is a weighted sum of values up to the
/// face speed, so its rounding scales with that speed, not with the result.
fn velocity_close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-5 * (5.0 * grid().cell_size() * 60.0).max(b.abs())
}

struct Fixture {
    solid: Vec<f32>,
    faces: [Vec<f32>; 3],
    pool: Vec<WhitewaterParticle>,
}

impl Fixture {
    fn new(seed: u64) -> Self {
        let mut rng = Rng(seed);
        let speed = 5.0 * grid().cell_size() * 60.0;
        let faces = faces(&mut rng, speed / 3f32.sqrt());
        Self { solid: tank(), faces, pool: pool(&mut rng, SLOTS, speed) }
    }

    fn fields(&self) -> Fields<'_> {
        Fields { faces: self.faces.each_ref().map(Vec::as_slice), face_cells: FACE_CELLS, solid: &self.solid }
    }

    fn advect(&self, harness: &mut Harness, params: &ParamValues) -> Vec<WhitewaterParticle> {
        let pool = harness.array(&self.pool, SLOTS);
        let faces = self.faces.each_ref().map(|f| harness.array(f, f.len()));
        let solid = harness.array(&self.solid, self.solid.len());
        run(
            harness,
            &mut AdvectWhitewater::new(),
            &[("pool", pool.0), ("face_u", faces[0].0), ("face_v", faces[1].0), ("face_w", faces[2].0), ("solid", solid.0)],
            SLOTS,
            params,
        )
    }
}

/// Every type moved, collided and killed as the CPU does it.
#[test]
fn advect_whitewater_matches_cpu() {
    let fixture = Fixture::new(0xad7e_0001);
    let s = settings();
    let mut harness = Harness::new();
    let got = fixture.advect(&mut harness, &params(&advect_values(s)));
    let g = grid();
    let (mut checked, mut edges, mut killed, mut bounced) = (0, 0, 0, 0);
    let mut kinds = [0; 3];
    for (i, (got, particle)) in got.iter().zip(&fixture.pool).enumerate() {
        let (want, margin) = cpu::advect(*particle, &fixture.fields(), &g, s, None);
        if margin < 1e-5 {
            edges += 1;
            continue;
        }
        assert_eq!((got.kind, got.id), (particle.kind, particle.id), "slot {i}");
        assert_eq!(got.position_lifetime[3], want.position_lifetime[3], "slot {i}: GPU {got:?} CPU {want:?} from {particle:?}");
        let near = got.position_lifetime[..3].iter().zip(&want.position_lifetime[..3]).all(|(&a, &b)| close(a, b));
        let moving = got.velocity.iter().zip(&want.velocity).all(|(&a, &b)| velocity_close(a, b));
        assert!(near && moving, "slot {i}: GPU {got:?} CPU {want:?} from {particle:?}");
        if particle.kind < 3 {
            checked += 1;
            kinds[particle.kind as usize] += 1;
            killed += usize::from(want.position_lifetime[3] == DEAD);
            let free: [f32; 3] = std::array::from_fn(|a| particle.position_lifetime[a] + want.velocity[a] * s.dt);
            bounced += usize::from(particle.kind == 1 && (0..3).any(|a| (free[a] - want.position_lifetime[a]).abs() > 1e-4));
        }
    }
    println!("{checked} checked {kinds:?}, {edges} on an edge, {killed} killed, {bounced} foam stopped by a solid");
    assert!(kinds.iter().all(|&n| n > 500) && edges < SLOTS / 50 && killed > 50 && bounced > 50);
}

/// The advect folded by the codegen alone in its region, bit for bit the
/// standalone kernel.
#[test]
fn advect_whitewater_fused_matches_unfused() {
    use manifold_node_engine::exec::effect_node::NodeInstanceId;
    use manifold_node_engine::freeze::codegen::{ENTRY, FusionRegion, InputSource, RegionNode, generate_fused};
    use manifold_node_engine::primitive::PrimitiveSpec;
    use manifold_gpu::GpuBinding;

    let fixture = Fixture::new(0xad7e_0002);
    let values = advect_values(settings());
    let mut harness = Harness::new();
    let unfused = fixture.advect(&mut harness, &params(&values));

    let region = FusionRegion {
        nodes: vec![RegionNode {
            node_id: NodeInstanceId(0),
            fusion_kind: <AdvectWhitewater as PrimitiveSpec>::FUSION_KIND,
            body: <AdvectWhitewater as PrimitiveSpec>::WGSL_BODY.expect("body"),
            params: <AdvectWhitewater as PrimitiveSpec>::PARAMS,
            inputs: (0..5).chain(std::iter::repeat_n(4,6)).map(InputSource::External).collect(),
            input_access: <AdvectWhitewater as PrimitiveSpec>::INPUT_ACCESS.to_vec(),
            node_inputs: <AdvectWhitewater as PrimitiveSpec>::INPUTS,
            node_outputs: <AdvectWhitewater as PrimitiveSpec>::OUTPUTS,
            node_includes: <AdvectWhitewater as PrimitiveSpec>::WGSL_INCLUDES,
            derived_uniforms: <AdvectWhitewater as PrimitiveSpec>::DERIVED_UNIFORMS,
            type_id: <AdvectWhitewater as PrimitiveSpec>::TYPE_ID.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba16float",
            stencil_fetch: false,
            quantize_f16: false,
        }],
        num_external_inputs: 5,
        outputs: vec![(NodeInstanceId(0), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(),
        camera_externals: 0,
        output_capacity: None,
    };
    let fused = generate_fused(&region).expect("the advect fuses");
    assert!(naga::front::wgsl::parse_str(&fused.wgsl).is_ok(), "fused WGSL parses:\n{}", fused.wgsl);
    let lookup = |name: &str| values.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
    let mut words: Vec<u32> = fused
        .param_order
        .iter()
        .map(|&(member, name)| lookup(name).unwrap_or_else(|| panic!("unexpected fused param {name} on {member:?}")).to_bits())
        .collect();
    words.push(SLOTS as u32);
    while !words.len().is_multiple_of(4) {
        words.push(0);
    }
    let pool = harness.array(&fixture.pool, SLOTS);
    let faces = fixture.faces.each_ref().map(|f| harness.array(f, f.len()));
    let solid = harness.array(&fixture.solid, fixture.solid.len());
    let dst = harness.array::<WhitewaterParticle>(&[], SLOTS);
    let pipeline = harness.device.create_compute_pipeline(&fused.wgsl, ENTRY, "advect-whitewater-fused");
    let mut enc = harness.device.create_encoder("advect-whitewater-fused");
    let externals = [&pool.1, &faces[0].1, &faces[1].1, &faces[2].1, &solid.1];
    let mut bindings = vec![GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(&words) }];
    for (i, buffer) in externals.iter().enumerate() {
        bindings.push(GpuBinding::Buffer { binding: i as u32 + 1, buffer, offset: 0 });
    }
    bindings.push(GpuBinding::Buffer { binding: externals.len() as u32 + 1, buffer: &dst.1, offset: 0 });
    enc.dispatch_compute(&pipeline, &bindings, [(SLOTS as u32).div_ceil(256), 1, 1], "advect-whitewater-fused");
    enc.commit_and_wait_completed();
    let fused_out: Vec<WhitewaterParticle> = read(&dst.1, SLOTS);
    for j in 0..SLOTS {
        assert_eq!(bytemuck::bytes_of(&fused_out[j]), bytemuck::bytes_of(&unfused[j]), "slot {j}: fused {:?} standalone {:?}", fused_out[j], unfused[j]);
    }
    assert!(unfused.iter().zip(&fixture.pool).filter(|(a, b)| a != b).count() > SLOTS / 2, "the advect moved the pool");
}

fn grid_values() -> Vec<(&'static str, f32)> {
    advect_values(settings()).into_iter().take(12).collect()
}

fn age_values(s: Age) -> Vec<(&'static str, f32)> {
    vec![
        ("dt", s.dt),
        ("bubble_lifetime_modifier", s.bubble),
        ("foam_lifetime_modifier", s.foam),
        ("spray_lifetime_modifier", s.spray),
    ]
}

/// The surface height in cells at cell column (x, z).
fn level(x: f32, z: f32) -> f32 {
    0.5 * grid().cells[1] as f32 + 4.0 * (0.3 * x).sin() + 3.0 * (0.2 * z).cos()
}

/// A rolling liquid surface sampled at cell centres (negative in the liquid,
/// metres), and its cell types: liquid below, air above, with a third of
/// the air cells walled off as solid so some foam and spray sit with no air
/// around them.
fn surface(rng: &mut Rng) -> (Vec<f32>, Vec<u32>) {
    let g = grid();
    let h = g.cell_size();
    let [nx, ny, nz] = g.cells;
    let total = (nx * ny * nz) as usize;
    let (mut distance, mut cells) = (Vec::with_capacity(total), Vec::with_capacity(total));
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let d = (j as f32 + 0.5 - level(i as f32, k as f32)) * h;
                distance.push(d);
                cells.push(if d < 0.0 {
                    1
                } else if rng.unit() < 0.33 {
                    2
                } else {
                    0
                });
            }
        }
    }
    (distance, cells)
}

/// Moves every particle to within six cells of the surface, so every branch
/// of the retype fires.
fn near_surface(rng: &mut Rng, pool: &mut [WhitewaterParticle]) {
    let g = grid();
    let h = g.cell_size();
    let origin: [f32; 3] = std::array::from_fn(|a| g.center[a] - 0.5 * g.size[a]);
    for p in pool.iter_mut() {
        let x = g.cells[0] as f32 * rng.unit();
        let z = g.cells[2] as f32 * rng.unit();
        let y = level(x - 0.5, z - 0.5) + 6.0 * (2.0 * rng.unit() - 1.0);
        p.position_lifetime[0] = origin[0] + x * h;
        p.position_lifetime[1] = origin[1] + y * h;
        p.position_lifetime[2] = origin[2] + z * h;
    }
}

struct Typing {
    base: Fixture,
    distance: Vec<f32>,
    cells: Vec<u32>,
}

impl Typing {
    fn new(seed: u64) -> Self {
        let mut base = Fixture::new(seed);
        let mut rng = Rng(seed ^ 0x5eed);
        near_surface(&mut rng, &mut base.pool);
        let (distance, cells) = surface(&mut rng);
        Self { base, distance, cells }
    }

    fn retype(&self, harness: &mut Harness, pool: &[WhitewaterParticle]) -> Vec<WhitewaterParticle> {
        let pool = harness.array(pool, SLOTS);
        let faces = self.base.faces.each_ref().map(|f| harness.array(f, f.len()));
        let (d, c) = (harness.array(&self.distance, self.distance.len()), harness.array(&self.cells, self.cells.len()));
        run(
            harness,
            &mut RetypeWhitewater::new(),
            &[("pool", pool.0), ("distance", d.0), ("cells", c.0), ("face_u", faces[0].0), ("face_v", faces[1].0), ("face_w", faces[2].0)],
            SLOTS,
            &params(&grid_values()),
        )
    }
}

/// Every retype branch as the CPU decides it, the foam buffer and the
/// bubble's velocity pickup included.
#[test]
fn retype_whitewater_matches_cpu() {
    let fixture = Typing::new(0x2e7f_0001);
    let mut harness = Harness::new();
    let got = fixture.retype(&mut harness, &fixture.base.pool);
    let g = grid();
    let fields = fixture.base.fields();
    let (mut edges, mut buffered, mut picked) = (0, 0, 0);
    let mut moves = [[0; 3]; 3];
    for (i, (got, particle)) in got.iter().zip(&fixture.base.pool).enumerate() {
        let (want, margin) = cpu::retype(*particle, &fields, &fixture.distance, &fixture.cells, &g);
        if margin < 1e-4 {
            edges += 1;
            continue;
        }
        assert_eq!((got.kind, got.id, got.position_lifetime), (want.kind, want.id, want.position_lifetime), "slot {i}: GPU {got:?} CPU {want:?}");
        assert!(got.velocity.iter().zip(&want.velocity).all(|(&a, &b)| velocity_close(a, b)), "slot {i}: GPU {got:?} CPU {want:?}");
        if particle.kind < 3 {
            moves[particle.kind as usize][want.kind as usize] += 1;
            picked += usize::from(particle.kind == 0 && want.kind != 0);
            if particle.kind == 1 && want.kind == 1 {
                let fresh = cpu::retype(WhitewaterParticle { kind: 2, ..*particle }, &fields, &fixture.distance, &fixture.cells, &g).0;
                buffered += usize::from(fresh.kind == 0);
            }
        }
    }
    println!("{moves:?} (from row to column), {edges} on an edge, {buffered} kept as foam by the buffer, {picked} bubbles took the liquid velocity");
    assert!(edges < SLOTS / 20 && buffered > 20 && picked > 100);
    assert!(moves.iter().flatten().all(|&n| n > 10), "every type change happens: {moves:?}");
}

/// Each type ages at its own rate.
#[test]
fn age_whitewater_matches_cpu() {
    let fixture = Fixture::new(0xa9e0_0001);
    let s = Age { dt: 1.0 / 50.0, bubble: 0.25, foam: 1.5, spray: 3.0 };
    let mut harness = Harness::new();
    let pool = harness.array(&fixture.pool, SLOTS);
    let got: Vec<WhitewaterParticle> = run(&mut harness, &mut AgeWhitewater::new(), &[("pool", pool.0)], SLOTS, &params(&age_values(s)));
    for (i, (got, particle)) in got.iter().zip(&fixture.pool).enumerate() {
        let want = cpu::age(*particle, s);
        assert_eq!(bytemuck::bytes_of(got), bytemuck::bytes_of(&want), "slot {i}: GPU {got:?} CPU {want:?}");
    }
    assert!(got.iter().zip(&fixture.pool).filter(|(a, b)| a != b).count() > SLOTS / 2, "the age changed the pool");
}

/// Advect, retype and age folded into one kernel by the codegen, bit for
/// bit the three standalone kernels in a row.
#[test]
fn whitewater_tick_fused_matches_unfused() {
    use manifold_node_engine::exec::effect_node::NodeInstanceId;
    use manifold_node_engine::freeze::codegen::{ENTRY, FusionRegion, InputSource, RegionNode, generate_fused};
    use manifold_node_engine::primitive::PrimitiveSpec;
    use manifold_gpu::GpuBinding;

    let fixture = Typing::new(0x71c4_0001);
    let s = settings();
    let a = Age::flip();
    let mut harness = Harness::new();
    let advected = fixture.base.advect(&mut harness, &params(&advect_values(s)));
    let retyped = fixture.retype(&mut harness, &advected);
    let pool_in = harness.array(&retyped, SLOTS);
    let unfused: Vec<WhitewaterParticle> = run(&mut harness, &mut AgeWhitewater::new(), &[("pool", pool_in.0)], SLOTS, &params(&age_values(a)));

    macro_rules! member {
        ($n:expr, $atom:ty, $inputs:expr) => {
            RegionNode {
                node_id: NodeInstanceId($n),
                fusion_kind: <$atom as PrimitiveSpec>::FUSION_KIND,
                body: <$atom as PrimitiveSpec>::WGSL_BODY.expect("body"),
                params: <$atom as PrimitiveSpec>::PARAMS,
                inputs: $inputs,
                input_access: <$atom as PrimitiveSpec>::INPUT_ACCESS.to_vec(),
                node_inputs: <$atom as PrimitiveSpec>::INPUTS,
                node_outputs: <$atom as PrimitiveSpec>::OUTPUTS,
                node_includes: <$atom as PrimitiveSpec>::WGSL_INCLUDES,
                derived_uniforms: <$atom as PrimitiveSpec>::DERIVED_UNIFORMS,
                type_id: <$atom as PrimitiveSpec>::TYPE_ID.to_string(),
                derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            }
        };
    }
    let external = InputSource::External;
    let node = |n: u32| InputSource::Node(NodeInstanceId(n));
    let region = FusionRegion {
        nodes: vec![
            member!(0, AdvectWhitewater, (0..5).chain(std::iter::repeat_n(4,6)).map(external).collect()),
            member!(1, RetypeWhitewater, vec![node(0), external(5), external(6), external(1), external(2), external(3)]),
            member!(2, AgeWhitewater, vec![node(1)]),
        ],
        num_external_inputs: 7,
        outputs: vec![(NodeInstanceId(2), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(),
        camera_externals: 0,
        output_capacity: None,
    };
    let fused = generate_fused(&region).expect("the tick fuses");
    assert!(naga::front::wgsl::parse_str(&fused.wgsl).is_ok(), "fused WGSL parses:\n{}", fused.wgsl);
    let (advect_side, age_side) = (advect_values(s), age_values(a));
    let lookup = |member: NodeInstanceId, name: &str| {
        let side = if member == NodeInstanceId(2) { &age_side } else { &advect_side };
        side.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    };
    let mut words: Vec<u32> = fused
        .param_order
        .iter()
        .map(|&(member, name)| lookup(member, name).unwrap_or_else(|| panic!("unexpected fused param {name} on {member:?}")).to_bits())
        .collect();
    words.push(SLOTS as u32);
    while !words.len().is_multiple_of(4) {
        words.push(0);
    }
    let pool = harness.array(&fixture.base.pool, SLOTS);
    let faces = fixture.base.faces.each_ref().map(|f| harness.array(f, f.len()));
    let solid = harness.array(&fixture.base.solid, fixture.base.solid.len());
    let (d, c) = (harness.array(&fixture.distance, fixture.distance.len()), harness.array(&fixture.cells, fixture.cells.len()));
    let dst = harness.array::<WhitewaterParticle>(&[], SLOTS);
    let pipeline = harness.device.create_compute_pipeline(&fused.wgsl, ENTRY, "whitewater-tick-fused");
    let mut enc = harness.device.create_encoder("whitewater-tick-fused");
    let externals = [&pool.1, &faces[0].1, &faces[1].1, &faces[2].1, &solid.1, &d.1, &c.1];
    let mut bindings = vec![GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(&words) }];
    for (i, buffer) in externals.iter().enumerate() {
        bindings.push(GpuBinding::Buffer { binding: i as u32 + 1, buffer, offset: 0 });
    }
    bindings.push(GpuBinding::Buffer { binding: externals.len() as u32 + 1, buffer: &dst.1, offset: 0 });
    enc.dispatch_compute(&pipeline, &bindings, [(SLOTS as u32).div_ceil(256), 1, 1], "whitewater-tick-fused");
    enc.commit_and_wait_completed();
    let fused_out: Vec<WhitewaterParticle> = read(&dst.1, SLOTS);
    for j in 0..SLOTS {
        assert_eq!(bytemuck::bytes_of(&fused_out[j]), bytemuck::bytes_of(&unfused[j]), "slot {j}: fused {:?} standalone {:?}", fused_out[j], unfused[j]);
    }
    let changed = advected.iter().zip(&retyped).filter(|(a, b)| a.kind != b.kind).count();
    assert!(changed > SLOTS / 20, "{changed} retyped");
}

/// Foam crowded into 70 cells, cell k holding k foam particles (a quarter of
/// them dead, which FLIP still counts), plus bubbles, spray, empty slots and
/// foam with no finite position, shuffled. Every position sits clear of its
/// cell's faces, so the sort's bin and FLIP's cell agree.
fn crowded(rng: &mut Rng) -> Vec<WhitewaterParticle> {
    let g = grid();
    let h = g.cell_size();
    let origin: [f32; 3] = std::array::from_fn(|a| g.center[a] - 0.5 * g.size[a]);
    let mut pool = Vec::new();
    let add = |pool: &mut Vec<WhitewaterParticle>, rng: &mut Rng, cell: [u32; 3], kind: u32, lifetime: f32| {
        let p: [f32; 3] = std::array::from_fn(|a| origin[a] + (cell[a] as f32 + 0.02 + 0.96 * rng.unit()) * h);
        pool.push(WhitewaterParticle { position_lifetime: [p[0], p[1], p[2], lifetime], kind, id: pool.len() as u32 % 256, ..Default::default() });
    };
    for k in 0..70u32 {
        let cell: [u32; 3] = std::array::from_fn(|a| (rng.unit() * g.cells[a] as f32) as u32 % g.cells[a]);
        for n in 0..k {
            let lifetime = if n % 4 == 3 { -0.01 } else { 0.2 + rng.unit() };
            add(&mut pool, rng, cell, 1, lifetime);
        }
        for kind in [0, 0, 0, 2, 2, WHITEWATER_EMPTY] {
            add(&mut pool, rng, cell, kind, 0.5);
        }
    }
    for _ in 0..5 {
        add(&mut pool, rng, [1, 1, 1], 1, 0.5);
        pool.last_mut().expect("added").position_lifetime[1] = f32::NAN;
    }
    for i in (1..pool.len()).rev() {
        let j = (rng.unit() * (i + 1) as f32) as usize % (i + 1);
        pool.swap(i, j);
    }
    pool
}

fn preserve_values(s: Preserve, enabled: bool) -> Vec<(&'static str, f32)> {
    let g = grid();
    let bins = bin_counts(g.size, g.cell_size());
    vec![
        ("enabled", if enabled { 1.0 } else { 0.0 }),
        ("dt", s.dt),
        ("rate", s.rate),
        ("min_density", s.min_density),
        ("max_density", s.max_density),
        ("center_x", g.center[0]),
        ("center_y", g.center[1]),
        ("center_z", g.center[2]),
        ("size_x", g.size[0]),
        ("size_y", g.size[1]),
        ("size_z", g.size[2]),
        ("cell_size", g.cell_size()),
        ("bins_x", bins[0] as f32),
        ("bins_y", bins[1] as f32),
        ("bins_z", bins[2] as f32),
    ]
}

/// The pool's bins from the sort over the whitewater grid: (pool, ranges, order) slots.
fn sort_pool(harness: &mut Harness, pool: &[WhitewaterParticle]) -> (Slot, Slot, Slot) {
    let input = harness.array(pool, pool.len());
    let (ranges, _) = harness.array::<CellRange>(&[], 1);
    let (order, _) = harness.array::<u32>(&[], pool.len());
    let values = preserve_values(Preserve::flip(), true);
    let (_, errors) = harness.run(
        &mut SortParticlesIntoCells::new(),
        &[("particles", input.0)],
        &[("cell_ranges", ranges), ("order", order)],
        &params(&values[5..12]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    (input.0, ranges, order)
}

/// Whitewater particles sort in place: ranges and order are byte-identical to
/// sorting liquid particle records at the same positions, live exactly when
/// the slot holds a particle (dead ones too) at a finite position.
#[test]
fn sort_bins_whitewater_particles_dead_and_alive() {
    let mut harness = Harness::new();
    let pool = crowded(&mut Rng(0x5047_0001));
    let liquid: Vec<FluidParticle> = pool
        .iter()
        .map(|p| {
            let live = p.kind < 3 && p.position_lifetime[..3].iter().all(|v| v.is_finite());
            let [x, y, z, _] = p.position_lifetime;
            FluidParticle { position_radius: [x, y, z, if live { 0.02 } else { 0.0 }], velocity: [0.0; 3], id: 1 }
        })
        .collect();
    let bins = |harness: &mut Harness, sorted: (Slot, Slot, Slot)| {
        let (ranges, order) = (harness.buffer(sorted.1), harness.buffer(sorted.2));
        (read::<u8>(&ranges, ranges.size as usize), read::<u8>(&order, order.size as usize))
    };
    let whitewater = {
        let slots = sort_pool(&mut harness, &pool);
        bins(&mut harness, slots)
    };
    let input = harness.array(&liquid, liquid.len());
    let (ranges, _) = harness.array::<CellRange>(&[], 1);
    let (order, _) = harness.array::<u32>(&[], liquid.len());
    let values = preserve_values(Preserve::flip(), true);
    let (_, errors) = harness.run(
        &mut SortParticlesIntoCells::new(),
        &[("particles", input.0)],
        &[("cell_ranges", ranges), ("order", order)],
        &params(&values[5..12]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let expected = bins(&mut harness, (input.0, ranges, order));
    let live: u32 = bytemuck::cast_slice::<u8, CellRange>(&expected.0).iter().map(|r| r.count).sum();
    let dead = pool.iter().filter(|p| p.kind < 3 && p.position_lifetime[3] <= 0.0).count();
    assert!(dead > 100 && (live as usize) < pool.len() && live > 2000, "live {live}, dead {dead}");
    assert_eq!(whitewater, expected, "whitewater bins and order exactly as the equivalent liquid particles");
}

fn preserve_run(harness: &mut Harness, pool: &[WhitewaterParticle], values: &[(&'static str, f32)]) -> Vec<WhitewaterParticle> {
    let (input, ranges, order) = sort_pool(harness, pool);
    run(
        harness,
        &mut PreserveFoam::new(),
        &[("pool", input), ("binned", input), ("cell_ranges", ranges), ("order", order)],
        pool.len(),
        &params(values),
    )
}

/// The sort then the preservation, against FLIP's own statement: every foam
/// particle, dead or alive, gains by its cell's foam count, saturating
/// above the max density and nothing below the min; off, nothing changes.
#[test]
fn preserve_foam_matches_flip() {
    let mut harness = Harness::new();
    let pool = crowded(&mut Rng(0x9e5e_0001));
    let s = Preserve { dt: 1.0 / 50.0, rate: 0.9, min_density: 15.0, max_density: 50.0 };
    let got = preserve_run(&mut harness, &pool, &preserve_values(s, true));
    let g = grid();
    let origin: [f32; 3] = std::array::from_fn(|a| g.center[a] - 0.5 * g.size[a]);
    let want = cpu::preserve(&pool, origin, g.cell_size(), g.cells, s);
    let (mut partial, mut full, mut none, mut revived) = (0, 0, 0, 0);
    for (i, ((got, want), before)) in got.iter().zip(&want).zip(&pool).enumerate() {
        let (mut a, mut b) = (*got, *want);
        assert!(close(a.position_lifetime[3], b.position_lifetime[3]) || a.position_lifetime[3].is_nan() && b.position_lifetime[3].is_nan(), "slot {i}: GPU {got:?} CPU {want:?}");
        a.position_lifetime[3] = 0.0;
        b.position_lifetime[3] = 0.0;
        assert_eq!(bytemuck::bytes_of(&a), bytemuck::bytes_of(&b), "slot {i}: GPU {got:?} CPU {want:?}");
        if before.kind == 1 {
            let gain = want.position_lifetime[3] - before.position_lifetime[3];
            full += usize::from((gain - s.rate * s.dt).abs() < 1e-6);
            none += usize::from(gain == 0.0);
            partial += usize::from(gain > 1e-6 && gain < s.rate * s.dt - 1e-6);
            revived += usize::from(before.position_lifetime[3] <= 0.0 && want.position_lifetime[3] > 0.0);
        }
    }
    println!("{partial} partial gains, {full} saturated, {none} none, {revived} dead foam revived");
    assert!(partial > 300 && full > 300 && none > 60 && revived > 50, "{partial} {full} {none} {revived}");

    let off = preserve_run(&mut harness, &pool, &preserve_values(s, false));
    assert_eq!(bytemuck::cast_slice::<_, u8>(&off), bytemuck::cast_slice::<_, u8>(&pool), "off, the pool passes whole");
}

/// The preservation folded by the codegen alone in its region, bit for bit
/// the standalone kernel.
#[test]
fn preserve_foam_fused_matches_unfused() {
    use manifold_node_engine::exec::effect_node::NodeInstanceId;
    use manifold_node_engine::freeze::codegen::{ENTRY, FusionRegion, InputSource, RegionNode, generate_fused};
    use manifold_node_engine::primitive::PrimitiveSpec;
    use manifold_gpu::GpuBinding;

    let mut harness = Harness::new();
    let pool = crowded(&mut Rng(0x9e5e_0002));
    let values = preserve_values(Preserve::flip(), true);
    let unfused = preserve_run(&mut harness, &pool, &values);
    let region = FusionRegion {
        nodes: vec![RegionNode {
            node_id: NodeInstanceId(0),
            fusion_kind: <PreserveFoam as PrimitiveSpec>::FUSION_KIND,
            body: <PreserveFoam as PrimitiveSpec>::WGSL_BODY.expect("body"),
            params: <PreserveFoam as PrimitiveSpec>::PARAMS,
            inputs: (0..4).map(InputSource::External).collect(),
            input_access: <PreserveFoam as PrimitiveSpec>::INPUT_ACCESS.to_vec(),
            node_inputs: <PreserveFoam as PrimitiveSpec>::INPUTS,
            node_outputs: <PreserveFoam as PrimitiveSpec>::OUTPUTS,
            node_includes: <PreserveFoam as PrimitiveSpec>::WGSL_INCLUDES,
            derived_uniforms: <PreserveFoam as PrimitiveSpec>::DERIVED_UNIFORMS,
            type_id: <PreserveFoam as PrimitiveSpec>::TYPE_ID.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba16float",
            stencil_fetch: false,
            quantize_f16: false,
        }],
        num_external_inputs: 4,
        outputs: vec![(NodeInstanceId(0), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(),
        camera_externals: 0,
        output_capacity: None,
    };
    let fused = generate_fused(&region).expect("the preservation fuses");
    assert!(naga::front::wgsl::parse_str(&fused.wgsl).is_ok(), "fused WGSL parses:\n{}", fused.wgsl);
    let mut words: Vec<u32> = fused
        .param_order
        .iter()
        .map(|&(member, name)| {
            let v = values.iter().find(|(n, _)| *n == name).map(|(_, v)| *v).unwrap_or_else(|| panic!("unexpected fused param {name} on {member:?}"));
            if name.starts_with("bins_") { (v as i32) as u32 } else { v.to_bits() }
        })
        .collect();
    words.push(pool.len() as u32);
    while !words.len().is_multiple_of(4) {
        words.push(0);
    }
    let (input, ranges, order) = sort_pool(&mut harness, &pool);
    let externals = [harness.buffer(input), harness.buffer(input), harness.buffer(ranges), harness.buffer(order)];
    let dst = harness.array::<WhitewaterParticle>(&[], pool.len());
    let pipeline = harness.device.create_compute_pipeline(&fused.wgsl, ENTRY, "preserve-foam-fused");
    let mut enc = harness.device.create_encoder("preserve-foam-fused");
    let mut bindings = vec![GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(&words) }];
    for (i, buffer) in externals.iter().enumerate() {
        bindings.push(GpuBinding::Buffer { binding: i as u32 + 1, buffer, offset: 0 });
    }
    bindings.push(GpuBinding::Buffer { binding: externals.len() as u32 + 1, buffer: &dst.1, offset: 0 });
    enc.dispatch_compute(&pipeline, &bindings, [(pool.len() as u32).div_ceil(256), 1, 1], "preserve-foam-fused");
    enc.commit_and_wait_completed();
    let fused_out: Vec<WhitewaterParticle> = read(&dst.1, pool.len());
    for j in 0..pool.len() {
        assert_eq!(bytemuck::bytes_of(&fused_out[j]), bytemuck::bytes_of(&unfused[j]), "slot {j}: fused {:?} standalone {:?}", fused_out[j], unfused[j]);
    }
    assert!(unfused.iter().zip(&pool).filter(|(a, b)| a != b).count() > 300, "the preservation changed the pool");
}
