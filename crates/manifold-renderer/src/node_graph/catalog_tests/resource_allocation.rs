    use manifold_node_engine::exec::resource_allocation::*;
use manifold_node_engine::{graph::Graph, exec::execution_plan::ExecutionPlan, exec::effect_node::NodeInstanceId, exec::execution_plan::ResourceId};
use manifold_node_engine::load::graph_loader::PreAllocationError;
use ahash::AHashMap;
    use manifold_node_engine::exec::execution_plan::compile;

    use manifold_node_engine::ports::PortType;
    use {crate::node_graph::primitives::ArrayFeedback, crate::node_graph::primitives::ContainerBounds3D, crate::node_graph::primitives::GenerateCubeMesh, crate::node_graph::primitives::ResolveAccumulator, crate::node_graph::primitives::ScatterParticles, crate::node_graph::primitives::SceneObjectNode, crate::node_graph::primitives::SeedParticles, manifold_node_engine::primitives::value::Value, crate::node_graph::primitives::WaveShearMesh};
    use manifold_node_engine::mesh::MeshVertex;



    fn array_outputs(plan: &ExecutionPlan) -> Vec<(NodeInstanceId, ResourceId, u32)> {
        plan.steps()
            .iter()
            .flat_map(|step| {
                step.outputs.iter().filter_map(|(_, resource)| {
                    let PortType::Array(layout) = plan.resource_type(*resource)? else {
                        return None;
                    };
                    Some((step.node, *resource, layout.item_size))
                })
            })
            .collect()
    }

    fn wave_graph() -> (Graph, Vec<NodeInstanceId>) {
        let mut graph = Graph::new();
        let cube = graph.add_node(Box::new(GenerateCubeMesh::new()));
        let mut previous = cube;
        let mut previous_port = "vertices";
        let mut waves = Vec::new();
        for _ in 0..4 {
            let wave = graph.add_node(Box::new(WaveShearMesh::new()));
            graph.connect((previous, previous_port), (wave, "in")).unwrap();
            waves.push(wave);
            previous = wave;
            previous_port = "out";
        }
        let object = graph.add_node(Box::new(SceneObjectNode::new()));
        graph
            .connect((previous, previous_port), (object, "vertices"))
            .unwrap();
        (graph, waves)
    }

    #[test]
    fn array_allocation_plan_reuses_four_temporary_wave_stages() {
        let (graph, _) = wave_graph();
        let plan = compile(&graph).unwrap();
        let arrays = array_outputs(&plan);
        assert_eq!(arrays.len(), 5);
        let expected_bytes = 36_u64 * u64::from(arrays[0].2);
        let planned =
            plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).unwrap();
        let allocations: Vec<_> = planned
            .actions
            .iter()
            .filter_map(|action| match action {
                ArrayAllocationAction::Allocate(allocation) => Some(allocation),
                ArrayAllocationAction::Reuse { .. } | ArrayAllocationAction::Alias { .. } => None,
            })
            .collect();
        // The held cube and the final carried geometry stay dedicated; only
        // wave 3 reuses wave 1, after wave 2 has consumed it.
        assert_eq!(allocations.len(), 4);
        assert!(
            allocations
                .iter()
                .all(|allocation| allocation.bytes == expected_bytes)
        );
        assert_eq!(
            planned
                .actions
                .iter()
                .filter(|action| matches!(action, ArrayAllocationAction::Alias { .. }))
                .count(),
            1
        );
        for step in plan.steps() {
            for (_, output) in &step.outputs {
                let Some(ArrayAllocationAction::Alias { input, .. }) = planned
                    .actions
                    .iter()
                    .find(|action| matches!(action, ArrayAllocationAction::Alias { resource, .. } if resource == output))
                else {
                    continue;
                };
                assert!(
                    step.inputs.iter().all(|(_, input_resource)| {
                        planned.storage.get(input_resource).is_none_or(|storage| {
                            storage.root != planned.storage[output].root
                        })
                    }),
                    "temporary reuse must not alias a same-step input"
                );
                assert_eq!(plan.resource_type(*output), plan.resource_type(*input));
                assert_eq!(planned.storage[output].bytes, planned.storage[input].bytes);
            }
        }
        for &held in plan.held_resources() {
            assert_eq!(planned.storage[&held].root, held);
        }
    }

    #[test]
    fn temporary_reuse_preserves_prebound_roots_and_exact_capacity() {
        let (graph, waves) = wave_graph();
        let plan = compile(&graph).unwrap();
        let resource = |node| plan.steps().iter().find(|step| step.node == node)
            .unwrap().outputs[0].1;
        let first = resource(waves[0]);
        let third = resource(waves[2]);
        // An oversized borrowed buffer must not become an exact-size scratch
        // allocation even when its logical resource reaches its last reader.
        let prebound = AHashMap::from_iter([(first, ArrayStorage {
            root: first, bytes: 72 * std::mem::size_of::<MeshVertex>() as u64,
        })]);
        let planned = plan_array_allocations(&graph, &plan, (64, 64), &prebound).unwrap();
        assert_ne!(planned.storage[&third].root, first);
        assert_eq!(prebound[&first].bytes, 72 * std::mem::size_of::<MeshVertex>() as u64);
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn temporary_arrays_match_dedicated_storage_across_animated_and_repeat_frames() {
        use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
        use manifold_node_engine::exec::backend::Backend;
        use manifold_node_engine::load::graph_loader::pre_allocate_resources;
        use manifold_node_engine::parameters::ParamValue;
        use manifold_node_engine::exec::{execution::Executor, effect_node::FrameTime, metal_backend::MetalBackend};
        use manifold_core::{Beats, Seconds};
        use manifold_gpu::GpuTextureFormat;

        let device = manifold_gpu::testkit::test_device();
        let make_runtime = |dedicated| {
            let (mut graph, waves) = wave_graph();
            let plan = compile(&graph).unwrap();
            let planned = plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).unwrap();
            let final_resource = plan.steps().iter().find(|step| step.node == waves[3])
                .unwrap().outputs[0].1;
            let mut backend = MetalBackend::new(device.arc(), 64, 64, GpuTextureFormat::Rgba16Float);
            if dedicated {
                // Prebound dedicated storage disables planner reuse without
                // introducing a production toggle or a second planner.
                for (&resource, storage) in &planned.storage {
                    backend.pre_bind_array(resource, device.create_buffer_shared(storage.bytes));
                }
            }
            pre_allocate_resources(&mut graph, &plan, &device, &mut backend).unwrap();
            let unique: AHashSet<_> = planned.storage.keys()
                .map(|resource| backend.slot_for(*resource).unwrap()).collect();
            assert_eq!(unique.len(), if dedicated { 5 } else { 4 });
            (graph, waves, plan, Executor::new(Box::new(backend)), final_resource)
        };
        let mut shared = make_runtime(false);
        let mut dedicated = make_runtime(true);
        let mut prior = None;
        for (frame, phase) in [0.13, 0.13, 0.37, 0.62, 0.62].into_iter().enumerate() {
            let mut outputs = Vec::new();
            for (graph, waves, plan, executor, output) in [&mut shared, &mut dedicated] {
                for (index, node) in waves.iter().enumerate() {
                    graph.set_param(*node, "phase", ParamValue::Float(phase + index as f32 * 0.11)).unwrap();
                }
                let mut encoder = device.create_encoder("temporary-array-proof");
                {
                    let mut gpu = GpuEncoder::new(&mut encoder, &device);
                    executor.execute_frame_with_gpu(graph, plan, FrameTime {
                        beats: Beats(f64::from(phase)), seconds: Seconds(f64::from(phase)),
                        delta: Seconds(1.0 / 24.0), frame_count: frame as i64,
                    }, &mut gpu);
                }
                encoder.commit_and_wait_completed();
                let backend = executor.backend();
                let buffer = backend.array_buffer(backend.slot_for(*output).unwrap()).unwrap();
                let bytes = unsafe {
                    std::slice::from_raw_parts(buffer.mapped_ptr().unwrap(), buffer.size() as usize)
                }.to_vec();
                outputs.push(bytes);
            }
            assert_eq!(outputs[0], outputs[1], "shared output differs at frame {frame}");
            if frame == 2 {
                assert_ne!(prior.as_ref().unwrap(), &outputs[0], "animation must change geometry");
            }
            prior = Some(outputs.remove(0));
        }
    }

    #[test]
    fn array_allocation_plan_reuses_exact_live_storage() {
        let mut graph = Graph::new();
        let cube = graph.add_node(Box::new(GenerateCubeMesh::new()));
        let wave = graph.add_node(Box::new(WaveShearMesh::new()));
        graph.connect((cube, "vertices"), (wave, "in")).unwrap();
        let plan = compile(&graph).unwrap();
        let mut prebound = AHashMap::default();
        for (_, resource, item_size) in array_outputs(&plan) {
            prebound.insert(
                resource,
                ArrayStorage {
                    root: resource,
                    bytes: 36 * u64::from(item_size),
                },
            );
        }
        let planned = plan_array_allocations(&graph, &plan, (64, 64), &prebound).unwrap();
        assert!(planned.actions.iter().all(|action| matches!(
            action,
            ArrayAllocationAction::Reuse { .. } | ArrayAllocationAction::Alias { .. }
        )));
    }

    #[test]
    fn array_allocation_plan_alias_shares_physical_root() {
        let mut graph = Graph::new();
        let source = graph.add_node(Box::new(SeedParticles::new()));
        let alias = graph.add_node(Box::new(ContainerBounds3D::new()));
        graph.connect((source, "particles"), (alias, "in")).unwrap();
        let consumer = graph.add_node(Box::new(ContainerBounds3D::new()));
        graph.connect((alias, "out"), (consumer, "in")).unwrap();

        let plan = compile(&graph).unwrap();
        let arrays = array_outputs(&plan);
        assert_eq!(arrays.len(), 2);
        let planned =
            plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).unwrap();
        assert_eq!(planned.actions.len(), 2);
        let (resource, input) = match planned.actions[1] {
            ArrayAllocationAction::Alias { resource, input } => (resource, input),
            ArrayAllocationAction::Allocate(_) | ArrayAllocationAction::Reuse { .. } => {
                panic!("alias output allocated separately")
            }
        };
        assert_eq!(
            planned.storage[&resource].root,
            planned.storage[&input].root
        );
        assert!(planned
            .actions
            .iter()
            .all(|action| !matches!(action, ArrayAllocationAction::Allocate(allocation) if allocation.resource == resource)));
    }

    #[test]
    fn array_allocation_plan_prebound_input_supplies_capacity() {
        let mut graph = Graph::new();
        let feedback = graph.add_node(Box::new(ArrayFeedback::new()));
        let alias = graph.add_node(Box::new(ContainerBounds3D::new()));
        graph.connect((feedback, "out"), (alias, "in")).unwrap();
        graph.connect((alias, "out"), (feedback, "in")).unwrap();

        let plan = compile(&graph).unwrap();
        let arrays = array_outputs(&plan);
        let input_resource = arrays
            .iter()
            .find(|(node, _, _)| *node == feedback)
            .map(|(_, resource, _)| *resource)
            .expect("feedback output");
        let prebound_resource = arrays
            .iter()
            .find(|(node, _, _)| *node == alias)
            .map(|(_, resource, _)| *resource)
            .expect("alias output");
        let item_size = u64::from(arrays[0].2);
        let bytes = item_size * 7;
        let mut prebound = AHashMap::default();
        prebound.insert(
            prebound_resource,
            ArrayStorage {
                root: prebound_resource,
                bytes,
            },
        );

        let planned = plan_array_allocations(&graph, &plan, (64, 64), &prebound).unwrap();
        let allocation = planned
            .actions
            .iter()
            .find_map(|action| match action {
                ArrayAllocationAction::Allocate(allocation)
                    if allocation.resource == input_resource =>
                {
                    Some(allocation)
                }
                _ => None,
            })
            .expect("feedback allocation");
        assert_eq!(allocation.bytes, bytes);
        assert_eq!(prebound[&prebound_resource].bytes, bytes);
    }

    #[test]
    fn math_view_borrowed_arrays_keep_owner_storage_and_capacity() {
        let mut graph = Graph::new();
        let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
        let source = graph.add_node(registry.construct("system.mesh_input").unwrap());
        let mask = graph.add_node(Box::new(crate::node_graph::primitives::MeshSpatialMask::new()));
        graph.connect((source, "vertices"), (mask, "in")).unwrap();
        let output = graph.add_node(registry.construct("system.mesh_output").unwrap());
        graph.connect((source, "vertices"), (output, "vertices")).unwrap();
        graph.connect((mask, "weights"), (output, "weights")).unwrap();
        let plan = compile(&graph).unwrap();
        let outputs = &plan.steps().iter().find(|step| step.node == source).unwrap().outputs;
        let mut prebound = AHashMap::default();
        for (port, resource) in outputs {
            let bytes = if *port == "vertices" {
                1536 * std::mem::size_of::<MeshVertex>() as u64
            } else {
                4 * 9000
            };
            prebound.insert(*resource, ArrayStorage { root: *resource, bytes });
        }
        let planned = plan_array_allocations(&graph, &plan, (64, 64), &prebound).unwrap();
        for (resource, storage) in &prebound {
            assert_eq!(planned.storage[resource], *storage);
            assert!(planned.actions.iter().all(|action| !matches!(action,
                ArrayAllocationAction::Allocate(allocation) if allocation.resource == *resource)));
        }
        assert_eq!(planned.actions.len(), 1);
        assert!(matches!(planned.actions[0], ArrayAllocationAction::Allocate(allocation)
            if allocation.node == mask && allocation.bytes == 1536 * 4));
    }

    fn scatter_graph() -> (Graph, NodeInstanceId) {
        let mut graph = Graph::new();
        let seed = graph.add_node(Box::new(SeedParticles::new()));
        let width = graph.add_node(Box::new(Value::new()));
        let height = graph.add_node(Box::new(Value::new()));
        let scatter = graph.add_node(Box::new(ScatterParticles::new()));
        graph
            .connect((seed, "particles"), (scatter, "particles"))
            .unwrap();
        graph.connect((width, "out"), (scatter, "width")).unwrap();
        graph.connect((height, "out"), (scatter, "height")).unwrap();
        let resolve = graph.add_node(Box::new(ResolveAccumulator::new()));
        graph
            .connect((scatter, "accum"), (resolve, "accum"))
            .unwrap();
        (graph, scatter)
    }

    #[test]
    fn array_allocation_plan_canvas_atomic_output_is_zero_initialized() {
        let (graph, scatter) = scatter_graph();
        let plan = compile(&graph).unwrap();
        let accum = plan
            .steps()
            .iter()
            .find(|step| step.node == scatter)
            .and_then(|step| step.outputs.iter().find(|(name, _)| *name == "accum"))
            .map(|(_, resource)| *resource)
            .expect("scatter accumulator");
        let planned = plan_array_allocations(&graph, &plan, (8, 4), &AHashMap::default()).unwrap();
        let allocation = planned
            .actions
            .iter()
            .find_map(|action| match action {
                ArrayAllocationAction::Allocate(allocation) if allocation.resource == accum => {
                    Some(allocation)
                }
                _ => None,
            })
            .expect("canvas allocation");
        assert_eq!(allocation.bytes, 8 * 4 * 4);
        assert!(allocation.zero_init);
    }

    #[test]
    fn array_allocation_plan_zero_or_unsized_fails_without_mutation() {
        let (graph, scatter) = scatter_graph();
        let plan = compile(&graph).unwrap();
        let accum = plan
            .steps()
            .iter()
            .find(|step| step.node == scatter)
            .and_then(|step| step.outputs.iter().find(|(name, _)| *name == "accum"))
            .map(|(_, resource)| *resource)
            .unwrap();
        let mut prebound = AHashMap::default();
        prebound.insert(
            accum,
            ArrayStorage {
                root: accum,
                bytes: 16,
            },
        );
        let before = prebound.clone();
        let error = plan_array_allocations(&graph, &plan, (0, 4), &prebound).unwrap_err();
        assert!(matches!(
            error,
            PreAllocationError::UnboundArrayResource {
                cause: "canvas dimensions are zero",
                ..
            }
        ));
        assert_eq!(prebound, before);

        let mut unsized_graph = Graph::new();
        let feedback = unsized_graph.add_node(Box::new(ArrayFeedback::new()));
        unsized_graph
            .connect((feedback, "out"), (feedback, "in"))
            .unwrap();
        let unsized_plan = compile(&unsized_graph).unwrap();
        let error = plan_array_allocations(
            &unsized_graph,
            &unsized_plan,
            (64, 64),
            &AHashMap::default(),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PreAllocationError::UnsizedArrayOutput { .. }
        ));
    }

    /// Only arrays whose capacity follows the canvas change size on resize.
    /// The particles a scatter reads are ordinary temporaries.
    #[test]
    fn canvas_lineage_follows_capacity_not_wires() {
        let (graph, scatter) = scatter_graph();
        let plan = compile(&graph).unwrap();
        let canvas = graph.test_canvas_capacity_lineage(&plan);
        let step = plan.steps().iter().find(|step| step.node == scatter).unwrap();
        let port = |ports: &[(&str, ResourceId)], name| ports.iter().find(|(p, _)| *p == name).unwrap().1;
        assert!(canvas[port(&step.outputs, "accum").0 as usize]);
        assert!(!canvas[port(&step.inputs, "particles").0 as usize]);
    }

    /// Every bundled preset's plan at 1080p: no shared root is ever live for
    /// two arrays at once.
    #[test]
    fn bundled_presets_share_only_dead_roots() {
        use crate::node_graph::bundled_presets::{bundled_preset_def, bundled_preset_type_ids};
        use manifold_node_engine::persistence::{EffectGraphDefExt, PrimitiveRegistry};
        use manifold_core::preset_def::PresetKind;
        let registry = PrimitiveRegistry::with_builtin();
        let mut planned = 0;
        for kind in [PresetKind::Generator, PresetKind::Effect] {
            for id in bundled_preset_type_ids(kind) {
                let def = bundled_preset_def(&id).expect("bundled def").clone();
                let Ok(graph) = def.into_graph(&registry, &Default::default()) else { continue };
                let Ok(plan) = compile(&graph) else { continue };
                let Ok(allocation) = plan_array_allocations(&graph, &plan, (1920, 1080), &AHashMap::default()) else {
                    continue;
                };
                let reused = lifetimes::assert_shared_roots_never_overlap(&graph, &plan, &allocation);
                let fresh: Vec<u64> = allocation.actions.iter().filter_map(|action| match action {
                    ArrayAllocationAction::Allocate(a) => Some(a.bytes),
                    _ => None,
                }).collect();
                if allocation.storage.len() > 1 {
                    let array = |r: &ResourceId| matches!(plan.resource_type(*r), Some(PortType::Array(_)));
                    let frees = plan.steps().iter().flat_map(|s| &s.free_after).filter(|r| array(r)).count();
                    let in_regions = plan.substep_regions().iter().flat_map(|r| &r.held_resources).filter(|r| array(r)).count();
                    let growing = growing_array_resources(&graph, &plan).iter().filter(|g| **g).count();
                    println!(
                        "planner {}: {} arrays, {} fresh, {reused} reused, {:.2} MB; {frees} frees, {in_regions} region-held, {growing} growing",
                        id.as_str(), allocation.storage.len(), fresh.len(), fresh.iter().sum::<u64>() as f64 / 1e6
                    );
                }
                planned += 1;
            }
        }
        assert!(planned > 40, "only {planned} presets planned");
    }

#[cfg(feature = "gpu-proofs")]
use ahash::AHashSet;
