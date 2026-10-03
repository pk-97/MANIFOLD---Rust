//! Small value proofs for the reference emitters. Run only through gpu_queue.
//! The turbulence reference is independently checked against the vendored C++ engine.
use super::liquid_surface_tests::{Harness, params, read};
use super::whitewater_emitter_cpu as reference;
use super::whitewater_grid_tests::run;
use super::whitewater_particle_cpu::{self as cpu, Box3};
use super::{
    divide_by_value::DivideByValue,
    dust_potential::DustPotential,
    energy_potential::EnergyPotential,
    inside_turbulence_potential::InsideTurbulencePotential,
    turbulence_emission_count::TurbulenceEmissionCount,
    turbulence_field::TurbulenceField,
    whitewater_emitter_velocity::WhitewaterEmitterVelocity,
    whitewater_influence::WhitewaterInfluence,
    whitewater_obstacle_source::{
        ObstacleSourceJob, WhitewaterObstacleSource, WhitewaterSource, encode_obstacle_source,
    },
};
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::{
    fluid_particles::FluidParticle,
    freeze::{
        classify::CapacityExpr,
        codegen::{FusionRegion, InputSource, RegionNode, generate_fused},
    },
    parameters::ParamValue,
    ports::KnownItem,
    primitive::PrimitiveSpec,
};
use manifold_gpu::{GpuBinding, GpuBuffer};

fn values(extra: &[(&'static str, f32)]) -> Vec<(&'static str, f32)> {
    let mut v = vec![
        ("center_x", 4.0),
        ("center_y", 4.0),
        ("center_z", 4.0),
        ("size_x", 8.0),
        ("size_y", 8.0),
        ("size_z", 8.0),
        ("nodes_x", 9.0),
        ("nodes_y", 9.0),
        ("nodes_z", 9.0),
        ("cell_size", 1.0),
        ("face_cells_x", 8.0),
        ("face_cells_y", 8.0),
        ("face_cells_z", 8.0),
    ];
    v.extend_from_slice(extra);
    v
}
pub(super) fn member<P: PrimitiveSpec>(id: u32, inputs: Vec<InputSource>) -> RegionNode<'static> {
    RegionNode {
        node_id: NodeInstanceId(id),
        fusion_kind: P::FUSION_KIND,
        body: P::WGSL_BODY.unwrap(),
        params: P::PARAMS,
        inputs,
        input_access: P::INPUT_ACCESS.to_vec(),
        node_inputs: P::INPUTS,
        node_outputs: P::OUTPUTS,
        node_includes: P::WGSL_INCLUDES,
        derived_uniforms: P::DERIVED_UNIFORMS,
        type_id: P::TYPE_ID.to_string(),
        derived_camera_ext: None,
        output_storage: "rgba16float",
        stencil_fetch: false,
        quantize_f16: false,
    }
}
pub(super) fn fused<T: bytemuck::Pod + KnownItem>(
    h: &mut Harness,
    nodes: Vec<RegionNode<'_>>,
    external: &[&GpuBuffer],
    count: usize,
    values: &[(&str, f32)],
) -> Vec<T> {
    let last = nodes.last().unwrap().node_id;
    let region = FusionRegion {
        nodes,
        num_external_inputs: external.len(),
        outputs: vec![(last, "out".to_owned())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: vec![],
        sampled_externals: vec![],
        camera_externals: 0,
        output_capacity: Some(CapacityExpr::Slot(0)),
    };
    let generated = generate_fused(&region).unwrap();
    let mut words: Vec<u32> = generated
        .param_order
        .iter()
        .map(|(node, name)| {
            let param = region
                .nodes
                .iter()
                .find(|n| n.node_id == *node)
                .unwrap()
                .params
                .iter()
                .find(|p| p.name == *name)
                .unwrap();
            let value = values
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, v)| *v)
                .unwrap_or_else(|| match param.default {
                    ParamValue::Float(v) => v,
                    _ => panic!("unexpected uniform {name}"),
                });
            match param.ty {
                crate::node_graph::parameters::ParamType::Int => value as i32 as u32,
                _ => value.to_bits(),
            }
        })
        .collect();
    words.resize(words.len().next_multiple_of(4), 0);
    let output = h.array::<T>(&[], count);
    let pipeline = h.device.create_compute_pipeline(
        &generated.wgsl,
        crate::node_graph::freeze::codegen::ENTRY,
        "whitewater-reference-fused",
    );
    let mut bindings = vec![GpuBinding::Bytes {
        binding: 0,
        data: bytemuck::cast_slice(&words),
    }];
    for (i, b) in external.iter().enumerate() {
        bindings.push(GpuBinding::Buffer {
            binding: i as u32 + 1,
            buffer: b,
            offset: 0,
        });
    }
    bindings.push(GpuBinding::Buffer {
        binding: external.len() as u32 + 1,
        buffer: &output.1,
        offset: 0,
    });
    let mut enc = h.device.create_encoder("whitewater-reference-fused");
    enc.dispatch_compute(
        &pipeline,
        &bindings,
        [(count as u32).div_ceil(256), 1, 1],
        "whitewater-reference-fused",
    );
    enc.commit_and_wait_completed();
    read(&output.1, count)
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
fn whitewater_turbulence_values_and_fusion() {
    let grid = Box3 {
        cells: [8; 3],
        center: [4.0; 3],
        size: [8.0; 3],
    };
    let faces: [Vec<f32>; 3] = std::array::from_fn(|a| {
        (0..576)
            .map(|i| ((i * 7 + a * 13) % 31) as f32 - 15.0)
            .collect()
    });
    let phi: Vec<f32> = (0..512)
        .map(|i| if i % 7 == 0 { 1.0 } else { -1.0 })
        .collect();
    let want = reference::turbulence(faces.each_ref().map(Vec::as_slice), [8; 3], &phi, grid);
    let mut h = Harness::new();
    let d = h.array(&phi, 512);
    let f = faces.each_ref().map(|v| h.array(v, 576));
    let v = values(&[]);
    let p = params(&v);
    let got: Vec<f32> = run(
        &mut h,
        &mut TurbulenceField::new(),
        &[
            ("distance", d.0),
            ("face_u", f[0].0),
            ("face_v", f[1].0),
            ("face_w", f[2].0),
        ],
        512,
        &p,
    );
    close(&got, &want);
    let scalar = h.array(&[2.0f32], 1);
    let t = h.array(&got, 512);
    let unfused: Vec<f32> = run(
        &mut h,
        &mut DivideByValue::new(),
        &[("values", t.0), ("divisor", scalar.0)],
        512,
        &p,
    );
    let fused = fused::<f32>(
        &mut h,
        vec![
            member::<TurbulenceField>(0, (0..4).map(InputSource::External).collect()),
            member::<DivideByValue>(
                1,
                vec![
                    InputSource::Node(NodeInstanceId(0)),
                    InputSource::External(4),
                ],
            ),
        ],
        &[&d.1, &f[0].1, &f[1].1, &f[2].1, &scalar.1],
        512,
        &v,
    );
    close(&fused, &unfused);
}

#[test]
fn whitewater_inside_dust_counts_and_fusion() {
    let mut h = Harness::new();
    let particles: Vec<FluidParticle> = (0..32)
        .map(|i| FluidParticle {
            position_radius: [3.5, 3.5, 2.5, 0.1],
            velocity: [12.0, 0.0, 0.0],
            id: i + 1,
        })
        .collect();
    let part = h.array(&particles, 32);
    let phi = h.array(&vec![-4.0f32; 512], 512);
    let turb = h.array(&vec![150.0f32; 512], 512);
    let cells = h.array(&vec![1u32; 512], 512);
    let energy = h.array(&vec![1.0f32; 32], 32);
    let wave = h.array(&vec![0.0f32; 32], 32);
    let influence = h.array(&vec![2.0f32; 729], 729);
    let solid = h.array(&vec![1.0f32; 729], 729);
    let source = h.array(
        &vec![
            WhitewaterSource {
                influence: 2.0,
                dust_strength: 0.5,
                kind: 2,
                pad: 0
            };
            729
        ],
        729,
    );
    let dt = 1.0 / 30.0;
    let v = values(&[
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
        ],
        32,
        &p,
    );
    close(&inside, &[0.5; 32]);
    let dust: Vec<f32> = run(
        &mut h,
        &mut DustPotential::new(),
        &[
            ("particles", part.0),
            ("solid", solid.0),
            ("turbulence", turb.0),
            ("source", source.0),
        ],
        32,
        &p,
    );
    close(&dust, &[0.3; 32]);
    for (is_dust, potential) in [(false, inside), (true, dust)] {
        let t = h.array(&potential, 32);
        let got: Vec<u32> = run(
            &mut h,
            &mut TurbulenceEmissionCount::new(),
            &[
                ("particles", part.0),
                ("energy", energy.0),
                ("wavecrest", wave.0),
                ("turbulence", t.0),
                ("influence", influence.0),
            ],
            32,
            &p,
        );
        let want: Vec<u32> = (0..32)
            .map(|i| {
                if cpu::random(i, 7.0f32.to_bits(), 0, 10) >= 0.6 {
                    0
                } else {
                    super::whitewater_emitter_cpu::emission_count(
                        1.0, [0.0, potential[i as usize]], [175.0; 2], 2.0, 8.0, 1.0, dt,
                    )
                }
            })
            .collect();
        assert_eq!(got, want);
        let a = if is_dust {
            member::<DustPotential>(
                0,
                vec![
                    InputSource::External(0),
                    InputSource::External(1),
                    InputSource::External(2),
                    InputSource::External(3),
                ],
            )
        } else {
            member::<InsideTurbulencePotential>(0, (0..4).map(InputSource::External).collect())
        };
        let fused = fused::<u32>(
            &mut h,
            vec![
                a,
                member::<TurbulenceEmissionCount>(
                    1,
                    vec![
                        InputSource::External(0),
                        InputSource::External(4),
                        InputSource::External(5),
                        InputSource::Node(NodeInstanceId(0)),
                        InputSource::External(6),
                    ],
                ),
            ],
            &[
                &part.1,
                if is_dust { &solid.1 } else { &phi.1 },
                &turb.1,
                if is_dust { &source.1 } else { &cells.1 },
                &energy.1,
                &wave.1,
                &influence.1,
            ],
            32,
            &v,
        );
        assert_eq!(fused, got);
    }
    // Boundary dust is disabled by default, and only the bottom three cells emit when enabled.
    let domain = h.array(
        &vec![
            WhitewaterSource {
                influence: 1.0,
                dust_strength: 1.0,
                kind: 1,
                pad: 0
            };
            729
        ],
        729,
    );
    for (boundary, expected) in [(0.0, 0.0), (1.0, 0.6)] {
        let p = params(&values(&[
            ("dust_enabled", 1.0),
            ("boundary_dust", boundary),
        ]));
        let got: Vec<f32> = run(
            &mut h,
            &mut DustPotential::new(),
            &[
                ("particles", part.0),
                ("solid", solid.0),
                ("turbulence", turb.0),
                ("source", domain.0),
            ],
            32,
            &p,
        );
        close(&got, &[expected; 32]);
    }
    let air = h.array(&vec![0u32; 512], 512);
    let surface = h.array(&vec![0.0f32; 512], 512);
    let got: Vec<f32> = run(
        &mut h,
        &mut InsideTurbulencePotential::new(),
        &[
            ("particles", part.0),
            ("distance", surface.0),
            ("turbulence", turb.0),
            ("cells", air.0),
        ],
        32,
        &p,
    );
    close(&got, &[0.0; 32]);
}

#[test]
fn whitewater_influence_values_and_fusion() {
    let mut h = Harness::new();
    let old = [0.0, 4.0, 0.0, 4.0, 4.0, 0.0];
    let src = [WhitewaterSource {
        influence: 0.25,
        dust_strength: 1.0,
        kind: 2,
        pad: 0,
    }; 6];
    let source = h.array(&src, 6);
    let solid = h.array(&[4.0f32, -4.0, 0.0, 2.99, -3.01, 3.0], 6);
    let previous = h.array(&old, 6);
    let divisor = h.array(&[2.0f32], 1);
    let v = values(&[("dt", 0.25), ("decay_rate", 2.0), ("base_level", 1.0)]);
    let p = params(&v);
    let got: Vec<f32> = run(
        &mut h,
        &mut WhitewaterInfluence::new(),
        &[
            ("values", previous.0),
            ("solid", solid.0),
            ("source", source.0),
        ],
        6,
        &p,
    );
    close(&got, &[0.5, 3.5, 0.25, 0.25, 3.5, 0.25]);
    let input = h.array(&got, 6);
    let unfused: Vec<f32> = run(
        &mut h,
        &mut DivideByValue::new(),
        &[("values", input.0), ("divisor", divisor.0)],
        6,
        &p,
    );
    let folded = fused::<f32>(
        &mut h,
        vec![
            member::<WhitewaterInfluence>(0, (0..3).map(InputSource::External).collect()),
            member::<DivideByValue>(
                1,
                vec![
                    InputSource::Node(NodeInstanceId(0)),
                    InputSource::External(3),
                ],
            ),
        ],
        &[&previous.1, &solid.1, &source.1, &divisor.1],
        6,
        &v,
    );
    close(&folded, &unfused);
}

#[test]
fn whitewater_emitter_speed_values_and_fusion() {
    let particles: Vec<FluidParticle> = (0..32)
        .map(|i| FluidParticle {
            position_radius: [3.5; 4],
            velocity: [2.0, 1.0, 0.0],
            id: i + 1,
        })
        .collect();
    let mut h = Harness::new();
    let input = h.array(&particles, 32);
    let phi = h.array(&vec![0.0f32; 512], 512);
    let air = h.array(&vec![0u32; 512], 512);
    let v = values(&[("spray_speed", 2.0), ("seed", 9.0), ("epoch", 3.0)]);
    let p = params(&v);
    let got: Vec<FluidParticle> = run(
        &mut h,
        &mut WhitewaterEmitterVelocity::new(),
        &[
            ("particles", input.0),
            ("distance", phi.0),
            ("cells", air.0),
        ],
        32,
        &p,
    );
    for (i, particle) in got.iter().enumerate() {
        let speed = 1.0 + cpu::random(i as u32, 9.0f32.to_bits(), 3, 9);
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
            member::<WhitewaterEmitterVelocity>(0, (0..3).map(InputSource::External).collect()),
            member::<EnergyPotential>(1, vec![InputSource::Node(NodeInstanceId(0))]),
        ],
        &[&input.1, &phi.1, &air.1],
        32,
        &v,
    );
    close(&folded, &unfused);
}

#[test]
fn whitewater_obstacle_source_closed_and_open_domain() {
    use crate::node_graph::liquid::bodies::{LiquidBody, LiquidShape};
    let mut h = Harness::new();
    let bodies = h.array::<LiquidBody>(&[], 1);
    let shapes = h.array::<LiquidShape>(&[], 1);
    let atlas = h.array::<u32>(&[], 1);
    let output = h.array::<WhitewaterSource>(&[], 729);
    let mut pipeline = None;
    for (closed, kind) in [(63, 1), (0, 0)] {
        let job = ObstacleSourceJob {
            influence: 2.0,
            dust_strength: 0.5,
            min: [0.0; 3],
            cell_size: 1.0,
            nodes: [9; 3],
            closed_faces: closed,
            wall_inset: 0,
            body_count: 0,
            rows: 0,
            tick_seconds: 1.0 / 60.0,
            bodies: &bodies.1,
            shapes: &shapes.1,
            atlas: &atlas.1,
            out: &output.1,
        };
        let mut enc = h.device.create_encoder("whitewater-source-reference");
        encode_obstacle_source(
            &mut pipeline,
            &h.device,
            &mut enc,
            &job,
            "whitewater-source-reference",
        );
        enc.commit_and_wait_completed();
        let got: Vec<WhitewaterSource> = read(&output.1, 729);
        assert!(got.iter().all(|s| s.kind == kind
            && s.influence == 2.0
            && s.dust_strength == if kind == 1 { 1.0 } else { 0.5 }));
    }
    let body = h.array(
        &[LiquidBody {
            rotation: [0.0, 0.0, 0.0, 1.0],
            ..Default::default()
        }],
        1,
    );
    let shape = h.array(
        &[LiquidShape {
            origin_spacing: [0.0, 0.0, 0.0, 1.0],
            dims_x: 9,
            dims_y: 9,
            dims_z: 9,
            atlas_offset: 0,
            scale_min: [1.0; 4],
        }],
        1,
    );
    // Constant negative obstacle SDF beats the nonnegative closed-wall distance.
    let negative = h.array(&vec![0xbc00_bc00u32; 365], 365);
    let job = ObstacleSourceJob {
        influence: 2.0,
        dust_strength: 0.5,
        min: [0.0; 3],
        cell_size: 1.0,
        nodes: [9; 3],
        closed_faces: 63,
        wall_inset: 0,
        body_count: 1,
        rows: 1,
        tick_seconds: 1.0 / 60.0,
        bodies: &body.1,
        shapes: &shape.1,
        atlas: &negative.1,
        out: &output.1,
    };
    let mut enc = h.device.create_encoder("whitewater-source-obstacle");
    encode_obstacle_source(
        &mut pipeline,
        &h.device,
        &mut enc,
        &job,
        "whitewater-source-obstacle",
    );
    enc.commit_and_wait_completed();
    let got: Vec<WhitewaterSource> = read(&output.1, 729);
    assert!(
        got.iter()
            .all(|s| s.kind == 2 && s.influence == 2.0 && s.dust_strength == 0.5)
    );
    let metadata = h.array(&got, 729);
    let previous = h.array(&vec![1.0f32; 729], 729);
    let solid = h.array(&vec![-1.0f32; 729], 729);
    let v = values(&[
        ("lattice_min_x", 0.0),
        ("lattice_min_y", 0.0),
        ("lattice_min_z", 0.0),
        ("closed_faces", 63.0),
        ("wall_inset", 0.0),
        ("body_count", 1.0),
        ("rows", 1.0),
        ("influence", 2.0),
        ("dust_strength", 0.5),
    ]);
    let unfused: Vec<f32> = run(
        &mut h,
        &mut WhitewaterInfluence::new(),
        &[
            ("values", previous.0),
            ("solid", solid.0),
            ("source", metadata.0),
        ],
        729,
        &params(&v),
    );
    let folded = fused::<f32>(
        &mut h,
        vec![
            member::<WhitewaterObstacleSource>(0, (1..4).map(InputSource::External).collect()),
            member::<WhitewaterInfluence>(
                1,
                vec![
                    InputSource::External(0),
                    InputSource::External(4),
                    InputSource::Node(NodeInstanceId(0)),
                ],
            ),
        ],
        &[&previous.1, &body.1, &shape.1, &negative.1, &solid.1],
        729,
        &v,
    );
    close(&folded, &unfused);
    close(&folded, &vec![2.0; 729]);
}

#[test]
fn whitewater_emitter_fusion_compiles_without_device() {
    let node = |n| InputSource::Node(NodeInstanceId(n));
    let ext = InputSource::External;
    let cases = [
        (
            vec![
                member::<TurbulenceField>(0, (0..4).map(ext).collect()),
                member::<DivideByValue>(1, vec![node(0), ext(4)]),
            ],
            5,
        ),
        (
            vec![
                member::<InsideTurbulencePotential>(0, (0..4).map(ext).collect()),
                member::<TurbulenceEmissionCount>(1, vec![ext(0), ext(4), ext(5), node(0), ext(6)]),
            ],
            7,
        ),
        (
            vec![
                member::<DustPotential>(0, (0..4).map(ext).collect()),
                member::<TurbulenceEmissionCount>(1, vec![ext(0), ext(4), ext(5), node(0), ext(6)]),
            ],
            7,
        ),
        (
            vec![
                member::<WhitewaterInfluence>(0, (0..3).map(ext).collect()),
                member::<DivideByValue>(1, vec![node(0), ext(3)]),
            ],
            4,
        ),
        (
            vec![
                member::<WhitewaterEmitterVelocity>(0, (0..3).map(ext).collect()),
                member::<EnergyPotential>(1, vec![node(0)]),
            ],
            3,
        ),
        (
            vec![
                member::<WhitewaterObstacleSource>(0, (1..4).map(ext).collect()),
                member::<WhitewaterInfluence>(1, vec![ext(0), ext(4), node(0)]),
            ],
            5,
        ),
    ];
    for (nodes, num_external_inputs) in cases {
        let name = nodes[0].type_id.clone();
        let region = FusionRegion {
            nodes,
            num_external_inputs,
            outputs: vec![(NodeInstanceId(1), "out".to_owned())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: vec![],
            sampled_externals: vec![],
            camera_externals: 0,
            output_capacity: Some(CapacityExpr::Slot(0)),
        };
        let g = generate_fused(&region).unwrap();
        let m = naga::front::wgsl::parse_str(&g.wgsl)
            .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&g.wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&m)
        .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(&g.wgsl)));
    }
}

#[test]
fn whitewater_dust_lifecycle_values_and_fusion() {
    use super::{
        advect_whitewater::AdvectWhitewater, age_whitewater::AgeWhitewater,
        whitewater_pool_cpu as pool,
    };
    use crate::node_graph::whitewater::WhitewaterParticle;
    let grid = Box3 {
        cells: [8; 3],
        center: [4.0; 3],
        size: [8.0; 3],
    };
    let particles: Vec<_> = (0..32)
        .map(|i| WhitewaterParticle {
            position_lifetime: [4.0, 4.0, 4.0, 2.0],
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
    let f = faces.each_ref().map(|f| h.array(f, 576));
    let s = h.array(&solid, 729);
    let v = values(&[("dt", 1.0 / 60.0)]);
    let p = params(&v);
    let got: Vec<WhitewaterParticle> = run(
        &mut h,
        &mut AdvectWhitewater::new(),
        &[
            ("pool", input.0),
            ("face_u", f[0].0),
            ("face_v", f[1].0),
            ("face_w", f[2].0),
            ("solid", s.0),
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
    let folded = fused::<WhitewaterParticle>(
        &mut h,
        vec![
            member::<AdvectWhitewater>(0, (0..5).chain(std::iter::repeat_n(4,6)).map(InputSource::External).collect()),
            member::<AgeWhitewater>(1, vec![InputSource::Node(NodeInstanceId(0))]),
        ],
        &[&input.1, &f[0].1, &f[1].1, &f[2].1, &s.1],
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
fn whitewater_fresh_spray_speed_and_dust_typing() {
    use super::whitewater_type::WhitewaterType;
    use manifold_fluids::WhitewaterSpawn;
    let mut h = Harness::new();
    let spawns = vec![
        WhitewaterSpawn {
            position_lifetime: [1.0, 3.0, 3.0, 2.0],
            velocity: [2.0, 1.0, 0.0],
            kind: 0
        };
        32
    ];
    let input = h.array(&spawns, 32);
    let phi = h.array(&vec![0.0f32; 512], 512);
    let air = h.array(&vec![0u32; 512], 512);
    for dust in [0.0, 1.0] {
        let p = params(&values(&[
            ("spray_speed", 2.0),
            ("seed", 9.0),
            ("epoch", 3.0),
            ("dust", dust),
        ]));
        let got: Vec<WhitewaterSpawn> = run(
            &mut h,
            &mut WhitewaterType::new(),
            &[("spawns", input.0), ("distance", phi.0), ("cells", air.0)],
            32,
            &p,
        );
        for (i, s) in got.iter().enumerate() {
            let factor = if dust > 0.5 {
                1.0
            } else {
                1.0 + cpu::random(i as u32, 9.0f32.to_bits(), 3, 11)
            };
            close(&s.velocity, &spawns[i].velocity.map(|v| v * factor));
            assert_eq!(s.kind, if dust > 0.5 { 4 } else { 2 });
        }
    }
}

#[test]
fn whitewater_dust_step_publishes_a_distinct_population() {
    use super::whitewater_step::{Step, StepFrame, StepInputs, StepShape};
    use crate::gpu_encoder::GpuEncoder;
    use crate::node_graph::transform::Transform;
    use crate::node_graph::whitewater::WhitewaterParticle;
    let mut h = Harness::new();
    let particles = h.array(
        &vec![
            FluidParticle {
                position_radius: [4.0, 4.0, 4.0, 0.05],
                velocity: [0.0; 3],
                id: 1
            };
            32
        ],
        32,
    );
    let phi = h.array(&vec![-5.0f32; 512], 512);
    let solid = h.array(&vec![1.0f32; 729], 729);
    let source = h.array(
        &vec![
            WhitewaterSource {
                influence: 1.0,
                dust_strength: 1.0,
                kind: 2,
                pad: 0
            };
            729
        ],
        729,
    );
    let f = [
        h.array(&vec![12.0f32; 576], 576),
        h.array(
            &(0..576)
                .map(|i| if i % 8 % 2 == 0 { 24.0f32 } else { -24.0 })
                .collect::<Vec<_>>(),
            576,
        ),
        h.array(&vec![0.0f32; 576], 576),
    ];
    let pool = h.array(&vec![super::whitewater_pool_cpu::empty_slot(); 256], 256);
    let state = h.array(&[0u32; 8], 8);
    let shape = StepShape::new(
        [9; 3],
        [9; 3],
        [8; 3],
        1.0,
        Some(Transform {
            pos: [4.0; 3],
            scale: [8.0; 3],
            ..Default::default()
        }),
        256,
    )
    .unwrap();
    for dust in [false, true] {
        let mut step = Step::default();
        let frame = StepFrame {
            shape,
            count: Some(32),
            ticks: 1,
            dt: 1.0 / 60.0,
            epoch: 0,
            seed: 0.0,
            gravity: [0.0, -9.81, 0.0],
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
        let inputs = StepInputs { motion: None,
            particles: &particles.1,
            solid: &solid.1,
            obstacle_source: Some(&source.1),
            faces: [&f[0].1, &f[1].1, &f[2].1],
            level_set: &phi.1,
            distance: Some(&phi.1),
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
        let pool_out = h.array(&vec![super::whitewater_pool_cpu::empty_slot(); 256], 256);
        let dust_out = h.array::<FluidParticle>(&[], 256);
        for (port, destination) in [
            ("counts_out", &counts_out.1),
            ("pool_out", &pool_out.1),
            ("dust_particles", &dust_out.1),
        ] {
            native.copy_buffer_to_buffer(step.tick_output(port).unwrap(), destination, destination.size);
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
