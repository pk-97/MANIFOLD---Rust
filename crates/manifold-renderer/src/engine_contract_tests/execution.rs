/// BUG-216 (`docs/BUG_BACKLOG.md`, D6(b) of `docs/DEPTH_RELIGHT_DESIGN.md`):
/// a `node.feedback` loop whose blend output feeds `system.final_output`
/// DIRECTLY (the natural authoring wiring) used to freeze at one frame of
/// history — the boundary output's resource is pre-bound as a borrowed
/// target, `node.feedback`'s ping-pong swap refuses under that shadow, and
/// the executor's `late_capture` had no fallback, silently dropping the
/// frame's capture forever. Real-GPU regression: builds exactly that shape
/// (`node.mix` Add-blending a constant source against its own delayed
/// output, wired straight to `FinalOutput`) and proves the readback value
/// keeps compounding across frames instead of freezing after frame 1.
#[cfg(all(test, feature = "gpu-proofs"))]
mod bug_216_gpu_tests {
use manifold_core::{Beats, Seconds};
    use half::f16;
    
    use manifold_gpu::GpuTextureFormat;

    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use manifold_node_engine::{exec::execution_plan::ExecutionPlan, exec::execution::Executor, scene::boundary_nodes::FinalOutput, exec::effect_node::FrameTime, graph::Graph, exec::metal_backend::MetalBackend, exec::effect_node::NodeInstanceId, persistence::PrimitiveRegistry, exec::execution_plan::ResourceId, scene::boundary_nodes::Source, state_store::StateStore, exec::execution_plan::compile};
    use manifold_node_engine::gpu::render_target::RenderTarget;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn output_resource(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
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

    /// Reads back pixel (0,0) of `res`'s CURRENT texture as rgba16float.
    fn readback_pixel(
        device: &manifold_gpu::GpuDevice,
        exec: &Executor,
        res: ResourceId,
        w: u32,
        h: u32,
    ) -> [f32; 4] {
        let slot = exec
            .backend()
            .slot_for(res)
            .expect("resource must be bound to a slot");
        let tex = exec
            .backend()
            .texture_2d(slot)
            .expect("resource's texture must be retained");
        let bytes_per_row = w * 8; // rgba16float = 8 bytes/pixel
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("bug216-readback");
        readback_enc.copy_texture_to_buffer(tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();
        let ptr = readback_buf.mapped_ptr().expect("shared buffer pointer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        [
            f16::from_bits(halves[0]).to_f32(),
            f16::from_bits(halves[1]).to_f32(),
            f16::from_bits(halves[2]).to_f32(),
            f16::from_bits(halves[3]).to_f32(),
        ]
    }

    #[test]
    fn feedback_direct_to_final_output_accumulates_trails() {
        use manifold_node_engine::parameters::ParamValue;

        let device = manifold_gpu::testkit::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();

        // BUG-216 shape: mix(source, feedback.out) → feedback.in AND
        // mix.out → final_output DIRECTLY (no node sitting between the
        // blend and the boundary — the wiring the backlog entry calls
        // "the natural wiring", and the one that used to freeze).
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let mix = g.add_node(registry.construct("node.mix").expect("node.mix registered"));
        let fb = g
            .add_node(registry.construct("node.feedback").expect("node.feedback registered"));
        let out = g.add_node(Box::new(FinalOutput::new()));

        g.connect((src, "out"), (mix, "a")).unwrap();
        g.connect((fb, "out"), (mix, "b")).unwrap();
        g.connect((mix, "out"), (fb, "in")).unwrap();
        g.connect((mix, "out"), (out, "in")).unwrap();

        // Add mode, amount=1.0 (full blend, no crossfade) — every frame's
        // output is `source + previous frame's delayed output`, so a
        // WORKING loop compounds monotonically; a FROZEN loop (BUG-216)
        // reads the SAME delayed value forever and every frame after the
        // first renders identically to frame 1.
        {
            let inst = g.get_node_mut(mix).expect("mix node exists");
            inst.params
                .insert(std::borrow::Cow::Borrowed("mode"), ParamValue::Enum(2)); // Add
            inst.params
                .insert(std::borrow::Cow::Borrowed("amount"), ParamValue::Float(1.0));
        }

        let plan = compile(&g).unwrap();
        let source_res = output_resource(&plan, src, "out");
        let mix_out_res = output_resource(&plan, mix, "out");

        let source_target = RenderTarget::new(&device, w, h, format, "bug216-source");
        let canvas_target = RenderTarget::new(&device, w, h, format, "bug216-canvas");
        let mut native_enc = device.create_encoder("bug216-setup");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(&source_target.texture, 0.05, 0.05, 0.05, 1.0);
        }
        native_enc.commit_and_wait_completed();

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(source_res, source_target);
        // The exact BUG-216 condition: `mix.out` (== `feedback.in` ==
        // `final_output.in`, one shared ResourceId) carries a BORROWED
        // shadow via `replace_texture_2d` — the real mechanism
        // `PresetRuntime::install_target` uses to install the host's
        // canvas texture over `final_output.in` each frame
        // (`preset_runtime.rs:3091`), NOT `pre_bind_texture_2d` (which
        // installs an OWNED slot with no shadow, and would let the swap
        // succeed every frame — a plain `pre_bind` does not reproduce
        // this bug). `replace_texture_2d` requires the slot to already
        // own a `RenderTarget`, so allocate one first and bind it to
        // `mix_out_res`.
        let placeholder = RenderTarget::new(&device, w, h, format, "bug216-mix-out-placeholder");
        let mix_out_slot = backend.allocate_slot(placeholder);
        backend.bind_resource_to_slot(mix_out_res, mix_out_slot);
        assert!(
            backend.replace_texture_2d(mix_out_slot, canvas_target.texture.clone()),
            "replace_texture_2d requires an owned RenderTarget already at the slot"
        );

        let mut exec = Executor::new(Box::new(backend));
        let mut store = StateStore::new();
        let owner_key = 216;

        let mut pixels: Vec<[f32; 4]> = Vec::new();
        for _ in 0..4 {
            let mut native_enc = device.create_encoder("bug216-frame");
            {
                let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
                exec.execute_frame_with_state(
                    &mut g,
                    &plan,
                    frame_time(),
                    &mut gpu,
                    &mut store,
                    owner_key,
                );
            }
            native_enc.commit_and_wait_completed();
            pixels.push(readback_pixel(&device, &exec, mix_out_res, w, h));
        }

        assert_ne!(
            pixels[3][0], pixels[0][0],
            "BUG-216: frame 4's output must differ from frame 1's — a frozen \
             loop (the swap-refused-and-dropped bug) reproduces the SAME \
             value every frame after the first. Frames: {pixels:?}",
        );
        // Frame 1 == frame 2 is EXPECTED, not the bug under test:
        // `node.feedback`'s allocation frame seeds its state from `in` and
        // deliberately skips ITS OWN late_capture that same frame (else the
        // seed would be immediately clobbered — `temporal.rs`'s
        // `just_allocated` guard), so the delayed value first advances
        // starting frame 3. From frame 2 onward the loop is in steady
        // state; a frozen loop (BUG-216) would hold frame 2's value
        // forever, so frames 2→4 must strictly increase.
        assert!(
            pixels[2][0] > pixels[1][0] && pixels[3][0] > pixels[2][0],
            "trails must compound monotonically frame over frame under \
             Add-mode feedback once past the alloc-frame plateau — got {pixels:?}",
        );
    }
}

mod tests { mod mesh_revision_tests {
use manifold_core::Beats;
use manifold_core::Seconds;
use manifold_node_engine::testkit::mesh_revision::*;
use manifold_node_engine::graph::Graph;
use manifold_node_engine::exec::execution::*;
use manifold_node_engine::exec::execution_plan::*;
use manifold_node_engine::exec::effect_node::*;
use manifold_node_engine::scene::mesh_change::*;
use std::sync::{Arc, Mutex};

fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }
        /// P2 (BUG-e3p6.4, design §3.3) — fused/unfused parity. The chain is
        /// P2 (BUG-e3p6.4, design §3.3) — fused/unfused parity on a REAL
        /// fused mesh kernel. The chain is two coincident `ripple_mesh`
        /// deformers — the mesh-deformer shape that fuses today (every
        /// stock deformer declaring `Dependencies` rules, wave/morph
        /// included, also declares a `weights_len` derived uniform with no
        /// registered recompute, so the fail-closed gate in
        /// `fuse_canonical_def_masked` keeps those regions unfused; see the
        /// report and the composition unit proof in freeze/install.rs).
        /// Ripple carries no mesh-rule declaration, so both sides compile
        /// the conservative Written/Written class: the fused node's
        /// installed sidecar must select exactly that class, and driving
        /// both graphs through the executor must show the class's behavior
        /// on both paths — every actual write revises all three aspects.
        #[test]
        fn mesh_change_fused_rules_match_unfused() {
            use manifold_node_engine::freeze::install::{FusedDef, fuse_canonical_def};
            use manifold_node_engine::scene::mesh_change::{PreparedMeshRevisionRule, PreparedMeshRules};
            use manifold_node_engine::persistence::EffectGraphDefExt;
            use manifold_node_engine::persistence::PrimitiveRegistry;
            use manifold_core::NodeId;
            use manifold_core::effect_graph_def::EffectGraphDef;

            let json = r#"{
                "version": 1, "name": "p2_fused_parity",
                "nodes": [
                    { "id": 0, "typeId": "system.mesh_input", "nodeId": "mesh_in" },
                    { "id": 1, "typeId": "node.ripple_mesh", "nodeId": "r1" },
                    { "id": 2, "typeId": "node.ripple_mesh", "nodeId": "r2" },
                    { "id": 3, "typeId": "node.free_camera", "nodeId": "cam" },
                    { "id": 4, "typeId": "node.unlit_material", "nodeId": "mat" },
                    { "id": 5, "typeId": "node.render_mesh", "nodeId": "render" },
                    { "id": 6, "typeId": "system.final_output", "nodeId": "final" }
                ],
                "wires": [
                    { "fromNode": 0, "fromPort": "vertices", "toNode": 1, "toPort": "in" },
                    { "fromNode": 0, "fromPort": "weights", "toNode": 1, "toPort": "weights" },
                    { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                    { "fromNode": 0, "fromPort": "weights", "toNode": 2, "toPort": "weights" },
                    { "fromNode": 3, "fromPort": "out", "toNode": 5, "toPort": "camera" },
                    { "fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "material" },
                    { "fromNode": 2, "fromPort": "out", "toNode": 5, "toPort": "vertices" },
                    { "fromNode": 5, "fromPort": "color", "toNode": 6, "toPort": "in" }
                ]
            }"#;
            let def: EffectGraphDef = serde_json::from_str(json).unwrap();
            let registry = PrimitiveRegistry::with_builtin();

            // The scripted source replaces `system.mesh_input` (identical
            // ports) because MockBackend cannot run the real producers; its
            // no-op evaluate is an actual write every frame, matching the
            // MeshNode fixture contract.
            let swap_source =
                |graph: &mut Graph, rule: &Arc<Mutex<Option<MeshOutputRule<'static>>>>| {
                    let id = graph
                        .instance_by_node_id(&NodeId::new("mesh_in"))
                        .expect("mesh_input must instantiate");
                    graph.get_node_mut(id).unwrap().node =
                        Box::new(ScriptedMeshSource::new(Arc::clone(rule)));
                    id
                };

            // ── Path A: canonical def, empty sidecar (the unfused chain) ──
            let mut graph_a =
                def.clone().into_graph(&registry, &PreparedMeshRules::default()).unwrap();
            let _src_a = swap_source(&mut graph_a, &shared_rule(None));
            let r1_a = graph_a.instance_by_node_id(&NodeId::new("r1")).unwrap();
            let r2_a = graph_a.instance_by_node_id(&NodeId::new("r2")).unwrap();
            for id in [r1_a, r2_a] {
                let inner = std::mem::replace(
                    &mut graph_a.get_node_mut(id).unwrap().node,
                    Box::new(MeshNode::sink()),
                );
                graph_a.get_node_mut(id).unwrap().node = Box::new(DeclaredPrimitiveProbe::new(inner));
            }
            let plan_a = compile(&graph_a).unwrap();
            let res_r2 = out_res(&plan_a, r2_a);
            // No declaration on ripple: the conservative Written/Written class.
            let unfused_rule = plan_a.mesh_rule(res_r2).expect("r2 output compiles a mesh rule");
            assert!(
                matches!(unfused_rule.topology, CompiledMeshRevisionRule::Written)
                    && matches!(unfused_rule.positions, CompiledMeshRevisionRule::Written),
                "unfused ripple must compile to the conservative class, got {unfused_rule:?}"
            );

            // ── Path B: fuse the same def and install the sidecar ──
            let fused = fuse_canonical_def(&def, &registry)
                .expect("the ripple+ripple region must fuse");
            let fused_key = {
                let doc = fused
                    .def
                    .nodes
                    .iter()
                    .find(|n| n.type_id == "node.wgsl_compute")
                    .expect("the fused def carries the fused kernel node");
                if doc.node_id.is_empty() {
                    doc.handle.clone().expect("fused node carries an id or handle")
                } else {
                    doc.node_id.as_str().to_string()
                }
            };
            // The composed sidecar: Written/Written, matching the unfused
            // declarations — no silent class change from fusion.
            {
                let mut entries = fused.mesh_rules.values().flatten();
                let rule = entries.next().expect("the fused node carries a mesh-rule sidecar");
                assert!(
                    entries.next().is_none(),
                    "exactly one fused node carries mesh rules, got {:?}",
                    fused.mesh_rules
                );
                assert_eq!(rule.output, "dst", "single-output region emits dst, got {:?}", rule);
                assert!(
                    matches!(rule.topology, PreparedMeshRevisionRule::Written)
                        && matches!(rule.positions, PreparedMeshRevisionRule::Written),
                    "fused sidecar must compose to Written/Written, got {rule:?}"
                );
            }
            let FusedDef { def: fused_def, mesh_rules, .. } = fused;
            let mut graph_b = fused_def.into_graph(&registry, &mesh_rules).unwrap();
            let _src_b = swap_source(&mut graph_b, &shared_rule(None));
            let fused_rt = graph_b
                .instance_by_node_id(&NodeId::new(&fused_key))
                .expect("fused node must instantiate");
            {
                let inner = std::mem::replace(
                    &mut graph_b.get_node_mut(fused_rt).unwrap().node,
                    Box::new(MeshNode::sink()),
                );
                graph_b.get_node_mut(fused_rt).unwrap().node =
                    Box::new(DeclaredPrimitiveProbe::new(inner));
            }
            let plan_b = compile(&graph_b).unwrap();
            let res_fused = out_res(&plan_b, fused_rt);
            let fused_rule = plan_b
                .mesh_rule(res_fused)
                .expect("fused MeshVertex output compiles a mesh rule");
            assert!(
                matches!(fused_rule.topology, CompiledMeshRevisionRule::Written)
                    && matches!(fused_rule.positions, CompiledMeshRevisionRule::Written),
                "fused rule must match the unfused class, got {fused_rule:?}"
            );

            // ── Drive both graphs: same-class revision behavior every frame ──
            // Revision tokens come from a per-executor global counter, so
            // absolute values are not comparable across two executors (the
            // unfused graph has more mesh writers). The parity invariant is
            // behavioral: both sides advance ALL THREE aspects on EVERY
            // write — the conservative class's signature.
            fn run_frame_pair(
                graph_a: &mut Graph,
                plan_a: &ExecutionPlan,
                exec_a: &mut Executor,
                graph_b: &mut Graph,
                plan_b: &ExecutionPlan,
                exec_b: &mut Executor,
                res_unfused: ResourceId,
                res_fused: ResourceId,
            ) -> (MeshRevision, MeshRevision) {
                exec_a.execute_frame(graph_a, plan_a, frame_time());
                exec_b.execute_frame(graph_b, plan_b, frame_time());
                let a = exec_a.mesh_revision_of_res(res_unfused);
                let b = exec_b.mesh_revision_of_res(res_fused);
                (a, b)
            }
            let mut exec_a = Executor::with_mock();
            let mut exec_b = Executor::with_mock();
            let (first_a, first_b) = run_frame_pair(
                &mut graph_a, &plan_a, &mut exec_a,
                &mut graph_b, &plan_b, &mut exec_b,
                res_r2, res_fused,
            );
            assert!(
                first_a.topology > 0 && first_b.topology > 0,
                "the first write must issue a topology token on both paths, got {first_a:?} / {first_b:?}"
            );
            let (mut prev_a, mut prev_b) = (first_a, first_b);
            for _ in 1..4 {
                let (next_a, next_b) = run_frame_pair(
                    &mut graph_a, &plan_a, &mut exec_a,
                    &mut graph_b, &plan_b, &mut exec_b,
                    res_r2, res_fused,
                );
                for (side, next, prev) in [
                    ("unfused", next_a, prev_a),
                    ("fused", next_b, prev_b),
                ] {
                    assert!(
                        next.topology > prev.topology
                            && next.positions > prev.positions
                            && next.content > prev.content,
                        "{side}: the conservative class must revise all aspects on every \
                         write, got {prev:?} then {next:?}"
                    );
                }
                (prev_a, prev_b) = (next_a, next_b);
            }
        }




} }
