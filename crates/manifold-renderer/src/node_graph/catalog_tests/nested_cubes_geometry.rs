#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! Smoke tests on the real GPU. Verifies the new primitive produces
    //! non-trivial output (cubes visible, not a black frame) and that
    //! repeated runs with identical inputs produce identical pixels.
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use manifold_node_engine::exec::execution_plan::ResourceId;
    use manifold_node_engine::parameters::TableData;
    use crate::node_graph::primitives::CycleTableRow;
    use manifold_node_engine::{exec::execution::Executor, scene::boundary_nodes::FinalOutput, exec::effect_node::FrameTime, graph::Graph, exec::metal_backend::MetalBackend, exec::effect_node::NodeInstanceId, parameters::ParamValue, exec::execution_plan::compile};
    use manifold_node_engine::gpu::render_target::RenderTarget;

    use crate::node_graph::primitives::NestedCubesGeometry;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn output_resource(
        plan: &manifold_node_engine::exec::execution_plan::ExecutionPlan,
        node: NodeInstanceId,
        port: &str,
    ) -> ResourceId {
        for step in plan.steps() {
            if step.node == node {
                for &(name, id) in &step.outputs {
                    if name == port {
                        return id;
                    }
                }
            }
        }
        panic!("no output `{port}` on node {node:?}");
    }

    /// Build a minimal graph: cycle_table_row(1×5 table) → nested_cubes_geometry,
    /// run one frame, read back the rendered texture as `u16` pixels.
    fn run_geometry(w: u32, h: u32, angles: [f32; 5]) -> Vec<u16> {
        let device = manifold_gpu::testkit::test_device();
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let cycler = g.add_node(Box::new(CycleTableRow::new()));
        let table = std::sync::Arc::new(
            TableData::new(vec![vec![angles[0], angles[1], angles[2], angles[3], angles[4]]])
                .expect("1×5 table"),
        );
        g.set_param(cycler, "table", ParamValue::Table(table)).unwrap();

        let geom = g.add_node(Box::new(NestedCubesGeometry::new()));
        g.connect((cycler, "row"), (geom, "target_angles")).unwrap();
        // FinalOutput sink: as of d84ae560 the planner skips outputs
        // with no downstream consumer, so wire `geom.out` to something
        // that keeps the resource alive for readback.
        let sink = g.add_node(Box::new(FinalOutput::new()));
        g.connect((geom, "out"), (sink, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let r_out = output_resource(&plan, geom, "out");
        let r_row = output_resource(&plan, cycler, "row");

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        let out_target = RenderTarget::new(&device, w, h, format, "nested-cubes-geometry-out");
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);
        // Pre-allocate the intermediate Array<f32> wire (cycler → geom).
        // Mirror of JsonGraphGenerator::pre_allocate_array_buffers but
        // local to this test (the generator path runs that walk
        // automatically; the bare Graph + Executor path here doesn't).
        let row_buf = device.create_buffer_shared((5 * std::mem::size_of::<f32>()) as u64);
        backend.pre_bind_array(r_row, row_buf);

        let mut native_enc = device.create_encoder("nested-cubes-geometry-test");
        let mut exec = Executor::new(Box::new(backend));
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let out_tex = exec
            .backend()
            .texture_2d(out_slot)
            .expect("output texture retained");
        let bytes_per_row = w * 8;
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("nested-cubes-geometry-readback");
        readback_enc.copy_texture_to_buffer(out_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] = unsafe {
            std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize)
        };
        halves.to_vec()
    }

    /// Sanity: at the canonical initial angles + default params, the
    /// centre of the frame should be non-black — at least one of the
    /// white edge lines crosses the centre region of the isometric
    /// camera. If the whole frame is black, the dispatch is broken
    /// (depth state, vertex emission, transform composition, etc.).
    #[test]
    fn renders_non_black_output_at_initial_pose() {
        let w = 128;
        let h = 128;
        // First pose from POSES table — same as legacy initial angles.
        let pixels = run_geometry(w, h, [0.0, 90.0, 180.0, 270.0, 360.0]);
        let mut nonzero = 0usize;
        for chunk in pixels.chunks_exact(4) {
            // Any of R/G/B > 0 (fp16 0x0000) counts as a lit pixel.
            if chunk[0] != 0 || chunk[1] != 0 || chunk[2] != 0 {
                nonzero += 1;
            }
        }
        assert!(
            nonzero > 100,
            "expected >100 non-black pixels at the initial pose, got {nonzero}"
        );
    }

    /// Determinism: same input → same output. Confirms the dispatch is
    /// not picking up uninitialised state or time-dependent jitter when
    /// time = 0 / no trigger.
    #[test]
    fn deterministic_across_runs_with_same_input() {
        let w = 64;
        let h = 64;
        let angles = [0.0, 45.0, 90.0, 135.0, 180.0];
        let a = run_geometry(w, h, angles);
        let b = run_geometry(w, h, angles);
        assert_eq!(a.len(), b.len());
        for (i, (&n, &m)) in a.iter().zip(b.iter()).enumerate() {
            if n != m {
                panic!("pixel {i} diverged: {n:#06x} vs {m:#06x}");
            }
        }
    }
}
