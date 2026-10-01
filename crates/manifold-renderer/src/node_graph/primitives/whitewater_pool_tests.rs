//! GPU value proofs for the whitewater pool atoms against their CPU
//! statements (`whitewater_pool_cpu`), and each atom folded by the codegen
//! against its standalone kernel (`docs/GPU_WHITEWATER_DESIGN.md` section
//! 3.9).

use super::advect_whitewater::AdvectWhitewater;
use super::age_whitewater::AgeWhitewater;
use super::retype_whitewater::RetypeWhitewater;
use super::liquid_surface_tests::{Harness, params, read};
use super::whitewater_cpu::Rng;
use super::whitewater_grid_tests::run;
use super::whitewater_pool_cpu::fixture::{FACE_CELLS, NODES, faces, grid, pool, tank};
use super::whitewater_pool_cpu::{self as cpu, Advect, Age, DEAD, Fields};
use crate::node_graph::effect_node::ParamValues;
use crate::node_graph::whitewater::WhitewaterParticle;

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
        if particle.position_lifetime[3] > 0.0 {
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
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::codegen::{ENTRY, FusionRegion, InputSource, RegionNode, generate_fused};
    use crate::node_graph::primitive::PrimitiveSpec;
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
            inputs: (0..5).map(InputSource::External).collect(),
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
        if particle.position_lifetime[3] > 0.0 {
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
    use crate::node_graph::effect_node::NodeInstanceId;
    use crate::node_graph::freeze::codegen::{ENTRY, FusionRegion, InputSource, RegionNode, generate_fused};
    use crate::node_graph::primitive::PrimitiveSpec;
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
            member!(0, AdvectWhitewater, (0..5).map(external).collect()),
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
