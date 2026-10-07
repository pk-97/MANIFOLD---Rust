
        use crate::node_graph::*;
use crate::node_graph::execution_plan::CompiledMeshRevisionRule;
use crate::node_graph::mesh_change::MeshAspect;
        
        use crate::node_graph::mesh_change::{MeshOutputRule, MeshRevisionRule};
        

use crate::testkit::mesh_revision::*;
        /// Wraps a real stock primitive so its DECLARED
        /// `mesh_output_rule` is compiled into the plan and driven
        /// through the executor on `MockBackend`. `evaluate` is a
        /// deliberate no-op write: the mock binds no GPU encoder, so the
        /// primitive's real `run()` (a compute dispatch) cannot execute
        /// here — the contract under test is the executor's revision
        /// commit, which the compiled rule drives, not the kernel.
        /// P2c: the stock deformer declaration on `node.normal_wave_mesh`
        /// (topology = `Dependencies([in.Topology])`, positions =
        /// `Written`) makes the deformer's output topology revision
        /// follow the INPUT's topology revision — held while the source
        /// topology is stable, and revising again the moment the source
        /// topology starts changing — while positions and content
        /// advance on every write. The real `NormalWaveMesh` declaration
        /// is exercised through [`DeclaredPrimitiveProbe`] because the
        /// mock backend cannot run its compute dispatch (see the probe's
        /// doc).
        #[test]
        fn mesh_change_declared_deformer_tracks_input_topology() {
            use crate::node_graph::mesh_change::MeshAspect;
            use crate::node_graph::primitives::NormalWaveMesh;

            let (src, (_unchanged, _pending, src_rule)) = MeshNode::producer(Some(fixed_rule()));
            let mut g = Graph::new();
            let a = g.add_node(Box::new(src));
            let probe = g.add_node(Box::new(DeclaredPrimitiveProbe {
                inner: Box::new(NormalWaveMesh::new()),
            }));
            let sink = g.add_node(Box::new(MeshNode::sink()));
            g.connect((a, "out"), (probe, "in")).unwrap();
            g.connect((probe, "out"), (sink, "in")).unwrap();
            let plan = compile(&g).unwrap();
            let (res_src, res_out) = (out_res(&plan, a), out_res(&plan, probe));

            // The plan must have compiled the REAL declaration off the
            // stock primitive: topology depends on the wired input's
            // Topology aspect, positions are Written.
            let compiled = plan
                .mesh_rule(res_out)
                .expect("MeshVertex output must compile a mesh rule");
            match &compiled.topology {
                CompiledMeshRevisionRule::Dependencies(deps) => {
                    assert_eq!(
                        deps,
                        &[(res_src, MeshAspect::Topology)],
                        "declared deformer topology must watch the wired input's Topology"
                    );
                }
                other => panic!(
                    "declared deformer topology must be Dependencies([in.Topology]), got {other:?}"
                ),
            }
            assert!(
                matches!(compiled.positions, CompiledMeshRevisionRule::Written),
                "declared deformer positions must be Written, got {:?}",
                compiled.positions
            );

            let mut exec = Executor::with_mock();
            exec.execute_frame(&mut g, &plan, frame_time());
            let src_rev = exec.mesh_revision_of_res(res_src);
            let out_rev = exec.mesh_revision_of_res(res_out);
            // The source topology rule is Fixed, so its topology revision
            // retains 0 — content still advances on the write.
            assert!(src_rev.content > 0, "source write must issue a content token");
            assert!(out_rev.topology > 0, "deformer write must issue a topology token");

            // Phase 1: the source writes every frame (content advances)
            // with a Fixed topology rule — the declared deformer must
            // hold its topology revision while positions/content advance.
            exec.execute_frame(&mut g, &plan, frame_time());
            let src2 = exec.mesh_revision_of_res(res_src);
            let out2 = exec.mesh_revision_of_res(res_out);
            assert_eq!(src2.topology, src_rev.topology, "source topology is Fixed");
            assert_eq!(
                out2.topology, out_rev.topology,
                "declared deformer topology must track the input: unchanged while input topology is unchanged"
            );
            assert!(
                out2.positions > out_rev.positions && out2.content > out_rev.content,
                "positions/content advance on every write, got {out_rev:?} then {out2:?}"
            );

            exec.execute_frame(&mut g, &plan, frame_time());
            let out3 = exec.mesh_revision_of_res(res_out);
            assert_eq!(
                out3.topology, out_rev.topology,
                "declared deformer topology must keep tracking the still-stable input"
            );
            assert!(out3.positions > out2.positions && out3.content > out2.content);

            // Phase 2: the source topology starts changing (rule flips to
            // Written at plan recompile; revision state persists because
            // the plan shape is unchanged). The dependency must follow.
            *src_rule.lock().unwrap() = Some(MeshOutputRule {
                topology: MeshRevisionRule::Written,
                positions: MeshRevisionRule::Fixed,
            });
            let plan = compile(&g).unwrap();
            let (res_src, res_out) = (out_res(&plan, a), out_res(&plan, probe));
            let before = exec.mesh_revision_of_res(res_out);

            exec.execute_frame(&mut g, &plan, frame_time());
            let after1 = exec.mesh_revision_of_res(res_out);
            assert!(
                exec.mesh_revision_of_res(res_src).topology > src_rev.topology,
                "flipped source rule must revise its own topology"
            );
            assert!(
                after1.topology > before.topology,
                "input topology now changes every write — the declared dependency must follow, got {before:?} then {after1:?}"
            );
            assert!(after1.positions > before.positions);

            exec.execute_frame(&mut g, &plan, frame_time());
            let after2 = exec.mesh_revision_of_res(res_out);
            assert!(
                after2.topology > after1.topology,
                "tracking must persist frame over frame, got {after1:?} then {after2:?}"
            );
        }
        /// P2 acceptance (BUG-e3p6.4, design §7): the stock Surface Waves
        /// modifiers must select the refit-eligible update class — topology
        /// driven by Topology-only input dependencies, positions Written —
        /// so a fused path (where it exists) can never degrade below the
        /// unfused class. Two parts:
        ///
        /// 1. The unfused oracle: the bundled preset's own member atoms
        ///    (`normal_wave_mesh`, `morph_mesh`) declare the class plan
        ///    compilation reads straight off the node.
        /// 2. The real preset graph, embedded VERBATIM (bundled group JSON)
        ///    in a production-shaped host def (mesh inputs + scalar values +
        ///    render tail — the shape a scene render view gives it). The
        ///    host MUST fuse now: every weights-carrying deformer has a
        ///    registered `weights_len` recompute whose marker carries the
        ///    member→fused-port mapping, and buffer regions admit the mask's
        ///    unwired optional coincident `weights` (BUG-7wwy + BUG-jwyh).
        ///    The composed §3.3 sidecar must keep the refit-eligible class.
        ///
        /// The fused-path executor parity on the fusing chain is proven on
        /// GPU in `tests/gpu_proofs/rt_dynamic_fusion.rs`.
        #[test]
        fn mesh_change_surface_waves_fused_sidecar_is_refit_eligible() {
            use crate::node_graph::bundled_presets::bundled_preset_json;
            use crate::node_graph::freeze::install::fuse_canonical_def;
            use crate::node_graph::mesh_change::{
                PreparedMeshOutputRule, PreparedMeshRevisionRule,
            };
            use crate::node_graph::persistence::EffectGraphDefExt;
            use crate::node_graph::primitive::Primitive;
            use crate::node_graph::PrimitiveRegistry;
            use manifold_core::PresetTypeId;
            use manifold_core::effect_graph_def::EffectGraphDef;

            let json = bundled_preset_json(&PresetTypeId::new("SurfaceWaves"))
                .expect("SurfaceWaves is a bundled scene-modifier preset");
            let registry = PrimitiveRegistry::with_builtin();

            // Part 1 — the unfused class, straight off the stock declarations
            // the preset's graph compiles today.
            let wave_node = crate::node_graph::primitives::NormalWaveMesh::new();
            let wave = Primitive::mesh_output_rule(&wave_node, "out");
            match wave.topology {
                MeshRevisionRule::Dependencies(deps) => {
                    assert_eq!(deps.len(), 1);
                    assert_eq!(deps[0].aspect, MeshAspect::Topology);
                }
                other => panic!("wave topology must be Dependencies([in.Topology]), got {other:?}"),
            }
            assert!(matches!(wave.positions, MeshRevisionRule::Written));
            let morph_node = crate::node_graph::primitives::MorphMesh::new();
            let morph = Primitive::mesh_output_rule(&morph_node, "out");
            match morph.topology {
                MeshRevisionRule::Dependencies(deps) => {
                    assert_eq!(deps.len(), 2);
                    assert!(deps.iter().all(|d| d.aspect == MeshAspect::Topology));
                }
                other => panic!(
                    "morph topology must be Dependencies([in.Topology, b.Topology]), got {other:?}"
                ),
            }
            assert!(matches!(morph.positions, MeshRevisionRule::Written));

            // Part 2 — the verbatim bundled group in a production-shaped
            // host. (Standalone the bundled JSON cannot fuse at all: fusion
            // liveness seeds from system.final_output, which only a render
            // host provides.)
            let preset: serde_json::Value = serde_json::from_str(&json).unwrap();
            let mut group = preset["nodes"][0].clone();
            group["id"] = serde_json::json!(1);
            let host = serde_json::json!({
                "version": 1,
                "name": "surface_waves_host",
                "nodes": [
                    { "id": 0, "typeId": "system.mesh_input", "nodeId": "mesh_in" },
                    group,
                    { "id": 2, "typeId": "system.mesh_input", "nodeId": "mesh_ref" },
                    { "id": 3, "typeId": "node.value", "nodeId": "radius",
                      "params": { "value": { "type": "Float", "value": 1.0 } } },
                    { "id": 4, "typeId": "node.value", "nodeId": "off_x",
                      "params": { "value": { "type": "Float", "value": 0.0 } } },
                    { "id": 5, "typeId": "node.value", "nodeId": "off_y",
                      "params": { "value": { "type": "Float", "value": 0.0 } } },
                    { "id": 6, "typeId": "node.value", "nodeId": "off_z",
                      "params": { "value": { "type": "Float", "value": 0.0 } } },
                    { "id": 9, "typeId": "node.free_camera", "nodeId": "cam" },
                    { "id": 10, "typeId": "node.unlit_material", "nodeId": "mat" },
                    { "id": 11, "typeId": "node.render_mesh", "nodeId": "render" },
                    { "id": 12, "typeId": "system.final_output", "nodeId": "final" }
                ],
                "wires": [
                    { "fromNode": 0, "fromPort": "vertices", "toNode": 1, "toPort": "current" },
                    { "fromNode": 2, "fromPort": "vertices", "toNode": 1, "toPort": "reference" },
                    { "fromNode": 3, "fromPort": "out", "toNode": 1, "toPort": "sourceRadius" },
                    { "fromNode": 4, "fromPort": "out", "toNode": 1, "toPort": "sourceOffsetX" },
                    { "fromNode": 5, "fromPort": "out", "toNode": 1, "toPort": "sourceOffsetY" },
                    { "fromNode": 6, "fromPort": "out", "toNode": 1, "toPort": "sourceOffsetZ" },
                    { "fromNode": 9, "fromPort": "out", "toNode": 11, "toPort": "camera" },
                    { "fromNode": 10, "fromPort": "out", "toNode": 11, "toPort": "material" },
                    { "fromNode": 1, "fromPort": "vertices", "toNode": 11, "toPort": "vertices" },
                    { "fromNode": 11, "fromPort": "color", "toNode": 12, "toPort": "in" }
                ]
            });
            let host_def: EffectGraphDef = serde_json::from_value(host).unwrap();

            // The mask fusion gap is closed (BUG-7wwy + BUG-jwyh): the host
            // must fuse, and the composed sidecar must keep the
            // refit-eligible class (Topology-only Dependencies, Written
            // positions), same as the unfused declarations above.
            let fused = fuse_canonical_def(&host_def, &registry).expect(
                "the production-shaped Surface Waves host must fuse: every \
                 weights-carrying deformer has a registered weights_len \
                 recompute and buffer regions admit the mask's unwired \
                 optional coincident weights (BUG-7wwy, BUG-jwyh)",
            );
            {
                let rules: Vec<&PreparedMeshOutputRule> =
                    fused.mesh_rules.values().flatten().collect();
                assert!(
                    !rules.is_empty(),
                    "fused Surface Waves must carry a mesh-rule sidecar for its mesh output"
                );
                for rule in &rules {
                    match &rule.topology {
                        PreparedMeshRevisionRule::Dependencies(deps) => {
                            assert!(
                                !deps.is_empty()
                                    && deps.iter().all(|d| d.aspect == MeshAspect::Topology),
                                "every composed leaf must be a Topology aspect, got {deps:?}"
                            );
                        }
                        other => panic!(
                            "the fused mesh output must stay refit-eligible (Dependencies), got {other:?}"
                        ),
                    }
                    assert!(
                        matches!(rule.positions, PreparedMeshRevisionRule::Written),
                        "morph positions stay Written under fusion, got {:?}",
                        rule.positions
                    );
                }
                let graph = fused.def.into_graph(&registry, &fused.mesh_rules).unwrap();
                let plan = compile(&graph).unwrap();
                let compiled: Vec<&crate::node_graph::execution_plan::CompiledMeshOutputRule> = plan
                    .steps()
                    .iter()
                    .flat_map(|s| s.outputs.iter())
                    .filter_map(|&(_, res)| plan.mesh_rule(res))
                    .collect();
                assert!(
                    compiled.iter().any(|r| matches!(
                        r.topology,
                        CompiledMeshRevisionRule::Dependencies(_)
                    )),
                    "the compiled fused plan must carry a Dependencies mesh rule, got {compiled:?}"
                );
            }
        }

use manifold_core::{Beats, Seconds};
    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }
