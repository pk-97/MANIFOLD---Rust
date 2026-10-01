//! GPU value proofs for the whitewater pool atoms against their CPU
//! statements (`whitewater_pool_cpu`), and each atom folded by the codegen
//! against its standalone kernel (`docs/GPU_WHITEWATER_DESIGN.md` section
//! 3.9).

use super::advect_whitewater::AdvectWhitewater;
use super::liquid_surface_tests::{Harness, params, read};
use super::whitewater_cpu::Rng;
use super::whitewater_grid_tests::run;
use super::whitewater_pool_cpu::fixture::{FACE_CELLS, NODES, faces, grid, pool, tank};
use super::whitewater_pool_cpu::{self as cpu, Advect, DEAD, Fields};
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
        let pairs = got.position_lifetime[..3].iter().zip(&want.position_lifetime[..3]).chain(got.velocity.iter().zip(&want.velocity));
        assert!(pairs.clone().all(|(&a, &b)| close(a, b)), "slot {i}: GPU {got:?} CPU {want:?} from {particle:?}");
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
