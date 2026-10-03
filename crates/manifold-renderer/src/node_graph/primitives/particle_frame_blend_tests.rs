//! P3 cross-atom proofs. CPU tests exercise the real preset partitioner;
//! device tests compare generated fused and standalone presentation paths.

use super::{InterpolateParticleFrames, ParticlesToCopies, PushOutOfSolid};
use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::freeze::classify::CapacityExpr;
use crate::node_graph::freeze::codegen::{FusionRegion, InputSource, RegionNode, generate_fused};
use crate::node_graph::primitive::PrimitiveSpec;

fn member<P: PrimitiveSpec>(id: u32, inputs: Vec<InputSource>) -> RegionNode<'static> {
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
        type_id: P::TYPE_ID.to_owned(),
        derived_camera_ext: None,
        output_storage: "rgba16float",
        stencil_fetch: false,
        quantize_f16: false,
    }
}

fn region() -> FusionRegion<'static> {
    FusionRegion {
        nodes: vec![
            member::<InterpolateParticleFrames>(
                0,
                vec![InputSource::External(0), InputSource::External(1)],
            ),
            member::<PushOutOfSolid>(
                1,
                vec![
                    InputSource::Node(NodeInstanceId(0)),
                    InputSource::External(2),
                ],
            ),
            member::<ParticlesToCopies>(2, vec![InputSource::Node(NodeInstanceId(1))]),
        ],
        num_external_inputs: 3,
        outputs: vec![(NodeInstanceId(2), "copies".to_owned())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(),
        camera_externals: 0,
        output_capacity: Some(CapacityExpr::Slot(1)),
    }
}

#[test]
fn fluid_particle_blend_fused_codegen_validates() {
    let generated = generate_fused(&region()).expect("interpolate → push → copies codegen");
    let module = naga::front::wgsl::parse_str(&generated.wgsl)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&generated.wgsl)));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&generated.wgsl)));
}

#[test]
fn fluid_particle_blend_presets_share_display_clock_and_fuse() {
    use crate::node_graph::{PrimitiveRegistry, fusion_report};
    use manifold_core::effect_graph_def::EffectGraphDef;
    let registry = PrimitiveRegistry::with_builtin();
    for text in [
        include_str!("../../../assets/generator-presets/WaterDamBreakGpuFlip.json"),
        include_str!("../../../assets/generator-presets/WaterDamBreakParticles.json"),
    ] {
        let def: EffectGraphDef = serde_json::from_str(text).unwrap();
        let report = fusion_report(&def, &registry);
        assert!(report.preparation_error.is_none(), "{report:?}");
        // Flattening groups remaps document ids. Identify the one push-out
        // by type, then require a keyed interpolator in its actual region.
        let push = report
            .nodes
            .iter()
            .find(|n| n.type_id == PushOutOfSolid::TYPE_ID)
            .unwrap();
        assert!(push.fused, "push-out refused: {:?}", push.cut_reason);
        assert!(
            report
                .nodes
                .iter()
                .any(|n| n.type_id == InterpolateParticleFrames::TYPE_ID
                    && n.fused
                    && n.region_index == push.region_index),
            "{report:?}"
        );
        let json: serde_json::Value = serde_json::from_str(text).unwrap();
        let wires = json["wires"].as_array().unwrap();
        let has = |from: u32, port: &str, to: u32, input: &str| {
            wires.iter().any(|w| {
                w["fromNode"] == from
                    && w["fromPort"] == port
                    && w["toNode"] == to
                    && w["toPort"] == input
            })
        };
        for display in [502, 506, 507, 508] {
            assert!(has(9, "blend", display, "blend"));
            assert!(has(9, "span", display, "span"));
        }
        assert!(has(503, "out", 504, "solid"));
        assert!(has(502, "out", 504, "particles"));
        for (display, copies) in [(506, 485), (507, 486), (508, 487)] {
            assert!(has(display, "out", copies, "particles"));
        }
        if json["presetMetadata"]["id"] == "WaterDamBreakGpuFlip" {
            assert!(has(504, "out", 14, "particles"));
            assert!(has(503, "out", 14, "solid"));
        } else {
            assert!(has(504, "out", 509, "particles"));
            assert!(has(509, "copies", 442, "instances"));
        }
    }
}

#[cfg(feature = "gpu-proofs")]
mod gpu_tests {
    use super::super::liquid_surface_tests::{Harness, params, read};
    use super::*;
    use crate::generators::mesh_common::InstanceTransform;
    use crate::node_graph::fluid_particles::FluidParticle;
    use crate::node_graph::freeze::codegen::ENTRY;
    use manifold_gpu::GpuBinding;

    #[test]
    fn fluid_particle_blend_fused_matches_unfused() {
        let mut h = Harness::new();
        let particle = |x, y, id| FluidParticle {
            position_radius: [x, y, 0.0, 0.2],
            velocity: [0.0; 3],
            id,
        };
        // A is shorter than B and the solid is longer: neither gathered input
        // may determine the fused dispatch extent. Slot 4 is a stale B tail.
        let a = [particle(0.0, -0.5, 1), particle(2.0, 0.5, 3)];
        let b = [
            particle(1.0, -0.5, 1),
            particle(0.5, -0.3, 2),
            particle(3.0, 0.5, 3),
            particle(1.0, 0.25, 0),
            particle(8.0, 8.0, 9),
        ];
        let solid: Vec<f32> = (0..125).map(|i| ((i / 5) % 5) as f32 - 2.0).collect();
        let (sa, ba) = h.array(&a, a.len());
        let (sb, bb) = h.array(&b, b.len());
        let (ss, bs) = h.array(&solid, solid.len());
        let (si, _) = h.array::<FluidParticle>(&[], b.len());
        let (sp, _) = h.array::<FluidParticle>(&[], b.len());
        let (sc, bc) = h.array::<InstanceTransform>(&[], b.len());
        let ip = params(&[
            ("count_a", 2.0),
            ("count_b", 4.0),
            ("blend", 0.5),
            ("span", 0.5),
        ]);
        let pp = params(&[]);
        for errors in [
            h.run(
                &mut InterpolateParticleFrames::new(),
                &[("particles_a", sa), ("particles_b", sb)],
                &[("out", si)],
                &ip,
            )
            .1,
            h.run(
                &mut PushOutOfSolid::new(),
                &[("particles", si), ("solid", ss)],
                &[("out", sp)],
                &pp,
            )
            .1,
            h.run(
                &mut ParticlesToCopies::new(),
                &[("particles", sp)],
                &[("copies", sc)],
                &pp,
            )
            .1,
        ] {
            assert!(errors.is_empty(), "{errors:?}");
        }
        let unfused: Vec<InstanceTransform> = read(&bc, b.len());
        let r = region();
        let generated = generate_fused(&r).unwrap();
        let mut words = Vec::<u32>::new();
        for &(id, name) in &generated.param_order {
            let spec = r.nodes.iter().find(|n| n.node_id == id).unwrap();
            let value = if id.0 == 0 {
                ip.get(name)
            } else {
                pp.get(name)
            }
            .or_else(|| {
                spec.params
                    .iter()
                    .find(|p| p.name == name)
                    .map(|p| &p.default)
            })
            .unwrap()
            .as_scalar()
            .unwrap();
            words.push(value.to_bits());
        }
        while !words.len().is_multiple_of(4) {
            words.push(0);
        }
        let output = h
            .device
            .create_buffer_shared((b.len() * size_of::<InstanceTransform>()) as u64);
        let pipeline =
            h.device
                .create_compute_pipeline(&generated.wgsl, ENTRY, "particle-blend-fused");
        let mut enc = h.device.create_encoder("particle-blend-fused");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&words),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &ba,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: &bb,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &bs,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: &output,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "particle-blend-fused",
        );
        enc.commit_and_wait_completed();
        let fused: Vec<InstanceTransform> = read(&output, b.len());
        // At t=1/2 with zero endpoint velocities Hermite is the midpoint.
        // Births have zero velocity and half radius; the plane clamps y to 0.
        let expected = [
            [0.5, 0.0, 0.0, 0.2],
            [0.5, 0.0, 0.0, 0.1],
            [2.5, 0.5, 0.0, 0.2],
            [1.0, 0.25, 0.0, 0.1],
            [0.0; 4],
        ];
        for ((actual, standalone), want) in fused.iter().zip(&unfused).zip(expected) {
            for ((&f, &u), e) in actual.pos_scale.iter().zip(&standalone.pos_scale).zip(want) {
                assert!((f - u).abs() <= 2.0e-5, "fused {f}, standalone {u}");
                assert!((f - e).abs() <= 2.0e-5, "fused {f}, CPU {e}");
            }
            assert_eq!(actual.rot_pad, [0.0; 4]);
        }
    }
}
