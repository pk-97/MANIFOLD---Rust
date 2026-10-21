    //! An imported scan with Shatter on it keeps its fixed parent fixed once the
    //! card bindings have applied (BUG-1a94z). The card owns the parent body's
    //! Motion: a build writes the card value onto the node, so a Fixed parent is
    //! one whose Motion card says Fixed. A def edit that only changes the node
    //! is overwritten at the first apply and the parent falls away from its
    //! prepared fragments. CPU only: no `GpuDevice`, no `PresetRuntime`.
    use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, SerializedParamValue};
    use manifold_core::params::{Param, ParamManifest};
    use manifold_core::scene_modifier_edit::insert_scene_modifier;
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    use manifold_core::types::LayerType;
    use manifold_core::{Beats, GraphTarget, NodeId};
    use manifold_editing::command::Command;
    use manifold_editing::commands::graph::EnableSceneObjectPhysicsCommand;
    use manifold_node_engine::exec::bound_graph::BoundGraph;
    use manifold_node_engine::load::expand::prepare_scene_modifiers;
    use manifold_node_engine::param_binding::{BindingSource, ResolvedBinding, ResolvedTarget};
    use manifold_node_engine::parameters::ParamValue;
    use manifold_node_engine::persistence::{EffectGraphDefExt, PrimitiveRegistry};
    use manifold_nodes_scene::node_graph::gltf_import::assemble_import_graph;
    use manifold_nodes_scene::node_graph::scene_modifier_authoring::{
        prepare_new_scene_modifier, scene_modifier_objects,
    };

    const MOTION_FIXED: u32 = 0;

    /// The tiger lily with physics on its one object and Shatter attached.
    fn shattered_flower(fix_parent_on_card: bool) -> EffectGraphDef {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/cc0__tiger_lily.glb");
        let (imported, _report) = assemble_import_graph(&fixture).expect("flower imports");
        let render = imported
            .nodes
            .iter()
            .find(|node| node.type_id == "node.render_scene")
            .expect("render scene");
        let render_ref = SceneNodeRef { scope: Vec::new(), node: render.node_id.clone() };
        let mut project = manifold_core::project::Project::default();
        let preset_id = imported.preset_metadata.as_ref().unwrap().id.clone();
        let layer_index = project.timeline.add_layer("Flower", LayerType::Generator, preset_id);
        project.timeline.layers[layer_index].gen_params_mut().unwrap().graph = Some(imported.clone());
        project.timeline.layers[layer_index]
            .clips
            .push(manifold_core::clip::TimelineClip::new_generator(Beats(0.0), Beats(16.0)));
        let target = GraphTarget::Generator(project.timeline.layers[layer_index].layer_id.clone());
        let mut enable = EnableSceneObjectPhysicsCommand::new(
            &project,
            target,
            render.id,
            0,
            &manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type("node.rigid_body"),
            &manifold_nodes_scene::node_graph::scene_exposure::metadata_for_node_type("node.physics_world"),
            &imported,
        )
        .expect("the import takes physics");
        enable.execute(&mut project);
        assert!(enable.was_applied(), "{:?}", enable.rejection_reason());
        let mut enabled = project.timeline.layers[layer_index].generator_graph().unwrap().clone();

        let body_ids: Vec<NodeId> = enabled
            .nodes
            .iter_mut()
            .filter_map(|n| n.group.as_mut())
            .flat_map(|group| group.nodes.iter_mut())
            .filter(|n| n.type_id == "node.rigid_body")
            .map(|body| {
                body.params.insert("motion".into(), SerializedParamValue::Enum { value: MOTION_FIXED });
                body.node_id.clone()
            })
            .collect();
        if fix_parent_on_card {
            let metadata = enabled.preset_metadata.as_mut().expect("metadata");
            for binding in &mut metadata.bindings {
                let on_motion = matches!(
                    &binding.target,
                    BindingTarget::Node { node_id, param } if param == "motion" && body_ids.contains(node_id)
                );
                if on_motion {
                    binding.default_value = MOTION_FIXED as f32;
                    let spec = metadata.params.iter_mut().find(|p| p.id == binding.id).expect("card spec");
                    spec.default_value = MOTION_FIXED as f32;
                }
            }
        }

        let primary = scene_modifier_objects(&enabled, &render_ref).expect("objects")[0].clone();
        let recipe: EffectGraphDef = serde_json::from_str(
            manifold_nodes::testkit::assets::ASSETS_SCENE_MODIFIER_PRESETS_SHATTER_JSON,
        )
        .expect("Shatter recipe parses");
        let instance = prepare_new_scene_modifier(
            &enabled,
            &recipe,
            NodeId::new("flower_shatter"),
            render_ref,
            SceneTargetSelection::Explicit { objects: vec![primary] },
        )
        .expect("Shatter captures the import");
        insert_scene_modifier(&enabled, enabled.scene_modifiers.len(), instance)
            .expect("attach Shatter")
            .graph
    }

    /// Every parent body's Motion after the build's first apply with the card
    /// at rest, the way the renderer builds it.
    fn parent_motion_after_first_apply(attached: &EffectGraphDef) -> Vec<ParamValue> {
        let registry = PrimitiveRegistry::with_builtin();
        let prepared = prepare_scene_modifiers(attached, &registry).expect("Shatter expands");
        let expanded = &prepared.def;
        let mut graph = expanded
            .clone()
            .into_graph(&registry, &manifold_node_engine::scene::mesh_change::PreparedMeshRules::default())
            .expect("expanded graph builds");
        let metadata = expanded.preset_metadata.as_ref().expect("metadata");
        let resolved: Vec<ResolvedBinding> = metadata
            .bindings
            .iter()
            .filter_map(|b| {
                let BindingTarget::Node { node_id, param } = &b.target else { return None };
                Some(ResolvedBinding::assemble(
                    b.id.clone().into(),
                    b.id.clone().into(),
                    b.default_value,
                    ResolvedTarget::Node {
                        node: graph.instance_by_node_id(node_id)?,
                        param: param.clone().into(),
                    },
                    b.convert,
                    BindingSource::Static,
                    b.id.clone().into(),
                    None,
                    false,
                    b.default_mirrors_node_param,
                ))
            })
            .collect();
        let mut bound = BoundGraph::new(resolved, &mut graph, Some(expanded));
        let manifest = ParamManifest::from_params(
            metadata.params.iter().cloned().map(Param::bundled).collect(),
        );
        bound.apply(&mut graph, &manifest);

        let parents: Vec<_> = expanded
            .nodes
            .iter()
            .filter(|n| n.type_id == "node.rigid_body" && !n.params.contains_key("fragment_parent"))
            .collect();
        assert!(!parents.is_empty(), "the flower keeps its authored body");
        parents
            .iter()
            .map(|n| {
                let inst = graph.instance_by_node_id(&n.node_id).expect("parent body built");
                graph.get_node(inst).unwrap().params.get("motion").cloned().expect("motion param")
            })
            .collect()
    }

    #[test]
    fn a_fixed_motion_card_keeps_the_imported_parent_fixed_under_shatter() {
        let motions = parent_motion_after_first_apply(&shattered_flower(true));
        assert!(
            motions.iter().all(|m| *m == ParamValue::Enum(MOTION_FIXED)),
            "the parent body fell off Fixed: {motions:?}"
        );
    }

    /// The card is the truth: Motion edited on the node alone does not survive
    /// the build, which is why the GPU proof sets the card.
    #[test]
    fn a_motion_edit_on_the_node_alone_is_overwritten_by_the_card() {
        let motions = parent_motion_after_first_apply(&shattered_flower(false));
        assert!(
            motions.iter().all(|m| *m != ParamValue::Enum(MOTION_FIXED)),
            "the card default should have won: {motions:?}"
        );
    }
