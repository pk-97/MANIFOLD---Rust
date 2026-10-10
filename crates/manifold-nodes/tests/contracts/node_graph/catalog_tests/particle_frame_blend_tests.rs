//! P3 cross-atom proofs. CPU tests exercise the real preset partitioner;
//! device tests compare generated fused and standalone presentation paths.

use {manifold_nodes_image::node_graph::primitives::interpolate_particle_frames::InterpolateParticleFrames, manifold_nodes_scene::node_graph::primitives::particles_to_copies::ParticlesToCopies, manifold_water_gpu_flip::primitives::push_out_of_solid::PushOutOfSolid};
use manifold_node_engine::exec::effect_node::NodeInstanceId;
use manifold_node_engine::freeze::classify::CapacityExpr;
use manifold_node_engine::freeze::codegen::{FusionRegion, InputSource, RegionNode, generate_fused};
use manifold_node_engine::primitive::PrimitiveSpec;

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

/// Pass-2 CPU reference fixtures, not proof of the current GPU publisher.
pub(super) mod publication_contract {
    use manifold_node_engine::particles::FluidParticle;

    struct ReferenceIds {
        next: u64,
        epoch: u32,
    }

    impl ReferenceIds {
        fn emit(&mut self, live: &mut Vec<FluidParticle>, positions: &[f32]) {
            if self.next + positions.len() as u64 > u64::from(u32::MAX) + 1 {
                self.epoch += 1;
                for (i, particle) in live.iter_mut().enumerate() {
                    particle.id = i as u32 + 1;
                }
                self.next = live.len() as u64 + 1;
            }
            for &x in positions {
                live.push(FluidParticle {
                    position_radius: [x, 0.0, 0.0, 0.1],
                    velocity: [1.0, 0.0, 0.0],
                    id: self.next as u32,
                });
                self.next += 1;
            }
        }
    }

    pub(crate) fn publish(state: &[FluidParticle], capacity: usize) -> (Vec<FluidParticle>, usize) {
        let mut frame: Vec<_> = state.iter().copied().filter(|p| p.position_radius[3] > 0.0).collect();
        frame.sort_unstable_by_key(|p| p.id);
        let count = frame.len();
        assert!(count <= capacity);
        frame.resize(capacity, FluidParticle::default());
        assert!(valid_publication(&frame, count));
        (frame, count)
    }

    fn valid_publication(frame: &[FluidParticle], count: usize) -> bool {
        count <= frame.len()
            && frame[..count].iter().all(|p| p.id != 0 && p.position_radius[3] > 0.0)
            && frame[..count].windows(2).all(|p| p[0].id < p[1].id)
            && frame[count..].iter().all(|p| *p == FluidParticle::default())
    }

    #[test]
    fn particle_identity_survives_reorder_death_and_multiple_substeps() {
        let mut ids = ReferenceIds { next: 1, epoch: 7 };
        let mut state = Vec::new();
        ids.emit(&mut state, &[0.0, 10.0, 20.0]);
        let (a, a_count) = publish(&state, 8);
        // Changing working slots never changes a surviving birth identity.
        for _ in 0..3 {
            state.reverse();
            for p in &mut state { p.position_radius[0] += 0.25; }
        }
        state.retain(|p| p.id != 2);
        ids.emit(&mut state, &[30.0, 40.0]);
        let working = state.clone();
        let (b, b_count) = publish(&state, 8);
        assert_eq!(state, working, "publication must not reorder solver state");
        assert_eq!(b[..b_count].iter().map(|p| p.id).collect::<Vec<_>>(), [1, 3, 4, 5]);
        for p in &b[..b_count] {
            if let Ok(i) = a[..a_count].binary_search_by_key(&p.id, |p| p.id) {
                assert_eq!(p.position_radius[0], a[i].position_radius[0] + 0.75);
            } else {
                assert!(p.id > 3, "birth must not reuse even a dead identity");
            }
        }
        assert_eq!(ids.epoch, 7);
        state.clear();
        ids.emit(&mut state, &[50.0]);
        assert_eq!(state[0].id, 6, "empty pool must not reset the birth counter");
    }

    #[test]
    fn particle_publication_is_compact_sorted_and_clears_retired_tail() {
        let mut ids = ReferenceIds { next: 1, epoch: 1 };
        let mut state = Vec::new();
        ids.emit(&mut state, &[0.0, 1.0, 2.0, 3.0]);
        state[1].position_radius[3] = 0.0;
        state.reverse();
        let (frame, count) = publish(&state, 8);
        assert_eq!(count, 3);
        assert_eq!(frame[..count].iter().map(|p| p.id).collect::<Vec<_>>(), [1, 3, 4]);
        let mut corrupt = frame.clone();
        corrupt.swap(0, 1);
        assert!(!valid_publication(&corrupt, count), "cell order is not publication order");
        corrupt = frame.clone();
        corrupt[1].id = corrupt[0].id;
        assert!(!valid_publication(&corrupt, count), "duplicate identity must fail");
        corrupt = frame.clone();
        corrupt[0].id = 0;
        assert!(!valid_publication(&corrupt, count), "id-zero is not an identity frame");
        corrupt = frame.clone();
        corrupt[count] = frame[0];
        assert!(!valid_publication(&corrupt, count), "retired capacity cannot remain visible");
        assert!(!valid_publication(&frame, frame.len() + 1));
        assert_eq!(publish(&[], 8), (vec![FluidParticle::default(); 8], 0));
    }

    #[test]
    fn particle_identity_rollover_changes_epoch_before_reusing_ids() {
        let mut ids = ReferenceIds { next: u64::from(u32::MAX), epoch: 12 };
        let mut state = Vec::new();
        ids.emit(&mut state, &[10.0]);
        assert_eq!(state[0].id, u32::MAX);
        let (a, _) = publish(&state, 4);
        ids.emit(&mut state, &[20.0, 30.0]);
        let (b, count) = publish(&state, 4);
        assert_eq!(ids.epoch, 13);
        assert_eq!(b[..count].iter().map(|p| p.id).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(a[0].position_radius, b[0].position_radius);
        assert_ne!(a[0].id, b[0].id);
        // The frame ring must collapse/reset before matching this new epoch.
    }
}

#[test]
fn fluid_particle_blend_presets_share_display_clock_and_fuse() {
    use manifold_node_engine::{persistence::PrimitiveRegistry, freeze::fusion_report};
    use manifold_core::effect_graph_def::EffectGraphDef;
    let registry = PrimitiveRegistry::with_builtin();
    for text in [
        include_str!("../../../../assets/generator-presets/WaterDamBreakGpuFlip.json"),
        include_str!("../../../../assets/generator-presets/WaterDamBreakParticles.json"),
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
        let grouped: EffectGraphDef = serde_json::from_str(text).unwrap();
        if grouped
            .preset_metadata
            .as_ref()
            .is_some_and(|metadata| metadata.id.as_str() == "WaterDamBreakGpuFlip")
        {
            let family = grouped
                .nodes
                .iter()
                .find(|node| node.node_id.as_str() == "water_family")
                .expect("Water family group");
            let group = family.group.as_ref().expect("Water family body");
            let id = |name: &str| {
                manifold_core::effect_graph_def::find_node(&group.nodes, name)
                    .unwrap_or_else(|| panic!("missing Water family node {name}"))
                    .id
            };
            let surface = id("surface");
            assert!(group.wires.iter().any(|wire| {
                wire.from_node == id("particle_push_out")
                    && wire.from_port == "out"
                    && wire.to_node == surface
                    && wire.to_port == "particles"
            }));
            assert!(group.wires.iter().any(|wire| {
                wire.from_node == id("solid_blend")
                    && wire.from_port == "out"
                    && wire.to_node == surface
                    && wire.to_port == "solid"
            }));
        }
        let flat = manifold_core::flatten::flatten_groups(&grouped).unwrap();
        let json = serde_json::to_value(&flat).unwrap();
        let wires = json["wires"].as_array().unwrap();
        let has = |from: u32, port: &str, to: u32, input: &str| {
            wires.iter().any(|w| {
                w["fromNode"] == from
                    && w["fromPort"] == port
                    && w["toNode"] == to
                    && w["toPort"] == input
            })
        };
        let id = |name: &str| -> u32 {
            manifold_core::effect_graph_def::find_node(&flat.nodes, name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .id
        };
        let frame = id("frame");
        for display in ["particle_blend", "foam_blend", "bubble_blend", "spray_blend"] {
            assert!(has(frame, "blend", id(display), "blend"));
            assert!(has(frame, "span", id(display), "span"));
        }
        assert!(has(id("solid_blend"), "out", id("particle_push_out"), "solid"));
        assert!(has(id("particle_blend"), "out", id("particle_push_out"), "particles"));
        for (display, copies) in [("foam_blend", "foam_copies"), ("bubble_blend", "bubble_copies"), ("spray_blend", "spray_copies")] {
            assert!(has(id(display), "out", id(copies), "particles"));
        }
        if json["presetMetadata"]["id"] == "WaterDamBreakGpuFlip" {
            assert!(has(id("state"), "identity", frame, "identity"));
        } else {
            assert!(has(id("particle_push_out"), "out", id("liquid_particle_copies"), "particles"));
            assert!(has(id("liquid_particle_copies"), "copies", id("water_object"), "instances"));
        }
    }
}

#[cfg(feature = "gpu-proofs")]
mod gpu_tests {
    use manifold_node_engine::testkit::array_harness::{Harness, params, read};
    use crate::contracts::node_graph::catalog_tests::particle_frame_blend_tests::*;
    use manifold_node_engine::mesh::InstanceTransform;
    use manifold_node_engine::particles::FluidParticle;
    use manifold_node_engine::freeze::codegen::ENTRY;
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
        // Halfway counts exposed a standalone/fused rounding mismatch: Rust
        // round used 5, WGSL round used 4. Both paths must sample the 5³ grid.
        let pp = params(&[("nodes_x", 4.5), ("nodes_y", 4.5), ("nodes_z", 4.5)]);
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
