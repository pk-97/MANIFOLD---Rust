mod fused_tests {
    #[test]
    fn whitewater_refuses_to_run_without_the_liquid_tiles() {
        use manifold_node_engine::exec::effect_node::{EffectNodeContext, FrameTime, ParamValues};
        use manifold_node_engine::primitive::Primitive;
        use manifold_node_engine::{
            bindings::NodeInputs, bindings::NodeOutputs, exec::backend::MockBackend,
        };
        use manifold_water_whitewater::primitives::whitewater_step::WhitewaterStep;
        let backend = MockBackend::new();
        let (
            mut scalars,
            mut cameras,
            mut lights,
            mut materials,
            mut transforms,
            mut atmospheres,
            mut modes,
            mut objects,
        ) = (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let outputs = NodeOutputs::new(
            &[],
            &backend,
            &mut scalars,
            &mut cameras,
            &mut lights,
            &mut materials,
            &mut transforms,
            &mut atmospheres,
            &mut modes,
            &mut objects,
        );
        let params = ParamValues::default();
        let mut errors = Vec::new();
        let time = FrameTime {
            beats: manifold_core::Beats(0.0),
            seconds: manifold_core::Seconds(0.0),
            delta: manifold_core::Seconds(0.0),
            frame_count: 0,
        };
        let mut ctx = EffectNodeContext::new(
            time,
            &params,
            NodeInputs::new(&[], &backend, &[]),
            outputs,
            None,
        )
        .with_errors(&mut errors);
        WhitewaterStep::new().run(&mut ctx);
        assert!(!ctx.gpu_accessed, "refusal must precede any GPU access");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("tiles"), "{:?}", errors);
    }

    #[cfg(feature = "gpu-proofs")]
    mod gpu {
        use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
        use manifold_node_engine::particles::FluidParticle;
        use manifold_node_engine::testkit::array_harness::read;
        use manifold_nodes_physics::force_lattice::ForceLatticeBinding;
        use manifold_water_liquid::fluid_particles::FaceSample;
        use manifold_water_liquid::whitewater::WhitewaterParticle;
        use manifold_water_whitewater::primitives::whitewater_step::fused_tests::gpu::*;
        use manifold_water_whitewater::primitives::whitewater_step::*;

        /// Mixed-sign face weights and poisoned fourth lanes reach the fused
        /// kernels and the atoms alike, with a two-row substep history,
        /// across three ticks of changing epoch and foam preservation.
        #[test]
        fn whitewater_mixed_weight_faces_match_reference() {
            let device = manifold_gpu::testkit::test_device();
            let fixture = Fixture::new(&device);
            let m = fixture.middle();
            let records: Vec<_> = fixture
                .records
                .iter()
                .enumerate()
                .map(|(i, p)| FluidParticle {
                    position_radius: [
                        m[0] - 0.12 + i as f32 / 113.0,
                        m[1] - 0.05,
                        m[2] - 0.09,
                        p.position_radius[3],
                    ],
                    ..*p
                })
                .collect();
            let particles = shared(&device, &records);
            let shape = fixture.shape;
            let elements = shape.elements() as usize;
            // Nonzero values behind invalid weights must be masked, including
            // negative weights. The fourth lanes and cells past the box are
            // poison or canonical.
            let cells = fixture.table.cells.iter().product::<u32>() as usize;
            let faces = shared(
                &device,
                &fixture
                    .table
                    .to_pool(&mixed_faces(cells), FaceSample::default()),
            );
            let schedule = shared(
                &device,
                &[1.0f32 / 128.0, 0.0, 0.0, 0.0, 1.0 / 128.0, 0.0, 0.0, 0.0],
            );
            let history = stepped_history(&device, 2, elements);
            let mut pool = vec![empty_slot(); 256];
            for (i, p) in pool[..12].iter_mut().enumerate() {
                *p = WhitewaterParticle {
                    position_lifetime: [m[0] - 0.12, m[1] - 0.05, m[2] - 0.09, 7.0],
                    kind: (i % 5) as u32,
                    id: i as u32,
                    ..Default::default()
                };
            }
            let pool = shared(&device, &pool);
            let state = shared(&device, &[12u32, 12, 0, 0, 0, 0, 0, 0]);
            // The fused kernels, then the atoms.
            let mut stages = [Step::default(), Step::default()];
            for stage in &mut stages {
                stage.set_reference_capture_for_test(true);
            }
            stages[1].set_reference_enabled_for_test(true);
            for tick in 0..3 {
                let mut enc = device
                    .create_encoder("whitewater pooled faces, mixed weights and substep history");
                let mut captures = Vec::new();
                for stage in &mut stages {
                    let mut inputs = fixture.inputs();
                    inputs.particles = &particles;
                    inputs.faces = &faces;
                    inputs.motion = Some(MotionInputs {
                        schedule: &schedule,
                        faces: &history,
                        count: 2,
                        fields: ForceLatticeBinding {
                            nodes: [2; 3],
                            spacing: 0.5,
                            min: [0.0; 3],
                            force_lattices: 0,
                            impulse_tick: 0,
                            first_tick: 0,
                            forces: None,
                            impulses: None,
                        },
                        tick_index: tick as f32,
                        regions: None,
                        shapes: None,
                        atlas: None,
                        collider_nodes: None,
                        collider_triangles: None,
                        region_count: 0,
                    });
                    let mut frame = fixture.frame(true);
                    frame.ticks = 1;
                    frame.epoch += tick;
                    frame.preserve_foam = tick % 2 == 0;
                    stage
                        .advance_tick(
                            &mut GpuEncoder::new(&mut enc, &device),
                            &frame,
                            &inputs,
                            &pool,
                            &state,
                            true,
                        )
                        .unwrap();
                    let mut row = snapshots(&device, &mut enc, stage);
                    row.push(copy_shared(&device, &mut enc, stage.turbulence_for_test()));
                    for src in stage.particle_snapshots_for_test().unwrap() {
                        row.push(copy_shared(&device, &mut enc, src));
                    }
                    for port in [
                        "pool_out",
                        "state_out",
                        "counts_out",
                        "foam_particles",
                        "bubble_particles",
                        "spray_particles",
                        "dust_particles",
                    ] {
                        row.push(copy_shared(
                            &device,
                            &mut enc,
                            stage.tick_output(port).unwrap(),
                        ));
                    }
                    captures.push(row);
                }
                enc.commit_and_wait().expect("GPU work completes");
                let names = [
                    "proof_sampled",
                    "proof_unscaled",
                    "proof_energy",
                    "proof_counts",
                    "proof_dust_energy",
                    "proof_dust_counts",
                    "proof_turbulence",
                    "proof_typed",
                    "proof_dust_typed",
                    "proof_lifecycle",
                    "pool_out",
                    "state_out",
                    "counts_out",
                    "foam_particles",
                    "bubble_particles",
                    "spray_particles",
                    "dust_particles",
                ];
                for (i, name) in names.into_iter().enumerate() {
                    compare_buffers(&captures[0][i], &captures[1][i], tick as usize, name);
                }
                assert!(
                    read::<f32>(&captures[0][6], elements)
                        .iter()
                        .any(|&v| v > 0.0),
                    "mixed weights must exercise turbulence"
                );
                let lifecycle = read::<WhitewaterParticle>(&captures[0][9], 256);
                assert!(
                    lifecycle[1].velocity[0] > 0.0,
                    "foam must read substep history beyond the stand-in length; bitwise values are checked against the atoms above"
                );
            }
            for stage in &stages {
                assert_eq!(stage.turbulence_dispatches_for_test(), 3);
            }
        }
        #[test]
        fn whitewater_fused_synthetic_matches_reference() {
            synthetic(false);
            synthetic(true);
        }
        #[test]
        fn whitewater_spawn_boundaries_match_reference() {
            spawn_boundaries();
        }
        #[test]
        fn whitewater_spawn_overflow_matches_reference() {
            spawn_overflow();
        }
        #[test]
        fn whitewater_lifecycle_history_matches_reference() {
            lifecycle_history();
        }
    }
}
