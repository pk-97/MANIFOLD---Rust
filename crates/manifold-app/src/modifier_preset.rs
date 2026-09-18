//! Export a bare local recipe with the current public control calibration.
use manifold_core::{
    GraphTarget,
    effect_graph_def::{BindingTarget, EffectGraphDef},
    project::Project,
};

/// Compare local authoring with the same scene-adjusted recipe used at attach.
/// This runs during structural projection, never live parameter synchronization.
pub(crate) fn has_graph_mod(owner: &EffectGraphDef, local: &EffectGraphDef) -> bool {
    match library_baseline(owner, local) {
        Ok(Some(baseline)) => local.diverges_ignoring_layout(&baseline),
        Ok(None) => true,
        Err(error) => {
            log::error!("[preset] modifier baseline unavailable: {error}");
            true
        }
    }
}

pub(crate) fn library_baseline(
    owner: &EffectGraphDef,
    local: &EffectGraphDef,
) -> Result<Option<EffectGraphDef>, String> {
    let Some(metadata) = &local.preset_metadata else {
        return Ok(None);
    };
    let Some(recipe) = manifold_renderer::node_graph::bundled_preset_def(&metadata.id) else {
        return Ok(None);
    };
    manifold_renderer::node_graph::scene_modifier_authoring::initialize_scene_modifier_graph(
        owner, recipe,
    )
    .map(Some)
    .map_err(|error| error.to_string())
}

/// Export owns its metadata, while the installed instance retains its original
/// macro addresses and affine transforms. Fork-in-place uses the raw local
/// snapshot instead, so those transforms are never applied twice.
pub(crate) fn export_def(project: &Project, target: &GraphTarget) -> Option<EffectGraphDef> {
    let GraphTarget::SceneModifier { modifier_id, .. } = target else {
        return None;
    };
    let owner = project.graph_target_owner(target)?;
    let outer = crate::graph_target::resolve(project, target.host_target()?)?
        .preset_metadata
        .as_ref()?;
    let mut def = crate::graph_target::resolve(project, target)?.clone();
    let local = def.preset_metadata.as_mut()?;
    for spec in &mut local.params {
        let mut sources = outer.bindings.iter().filter(|binding| matches!(&binding.target,
            BindingTarget::SceneModifier { modifier_id: mid, param_id } if mid == modifier_id && param_id == &spec.id));
        let Some(source) = sources.next() else {
            continue;
        };
        if sources.next().is_some() {
            log::error!(
                "[preset] cannot export modifier control {} with multiple host sources",
                spec.id
            );
            return None;
        }
        let live = owner.params.get(&source.id)?;
        let id = spec.id.clone();
        *spec = live.spec.clone();
        spec.id = id;
        spec.default_value = live.base;
        for binding in local
            .bindings
            .iter_mut()
            .filter(|binding| binding.id == spec.id)
        {
            binding.default_value = live.base;
            binding.offset += source.offset * binding.scale;
            binding.scale *= source.scale;
        }
    }
    // Initialization belongs to fresh application. An exported customized
    // control must keep its fixed value rather than recapture a stock default.
    if let Some(recipe) = &mut local.scene_modifier {
        let captured = |id: &str| {
            outer.bindings.iter().any(|binding|
            matches!(&binding.target, BindingTarget::SceneModifier { modifier_id: mid, param_id }
                if mid == modifier_id && param_id == id))
        };
        recipe
            .calibrations
            .retain(|calibration| !captured(&calibration.param_id));
        recipe.initializers.retain(|initializer| {
            !local.bindings.iter().any(|binding| {
                captured(&binding.id)
                    && matches!(&binding.target, BindingTarget::Node { node_id, param }
                if node_id == &initializer.target.node && param == &initializer.param)
            })
        });
    }
    Some(def)
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, EffectGraphNode, ParamSpecDef, PresetMetadata,
        SerializedParamValue,
    };
    use manifold_core::layer::Layer;
    use manifold_core::macro_bank::MacroCurve;
    use manifold_core::scene_modifier_preset::{
        SceneMeshReferenceFrame, SceneModifierInstanceDef, SceneModifierRecipe,
        SceneNodeInitializer, SceneNodeRef, SceneScalarExpr, SceneTargetSelection,
    };
    use manifold_core::{LayerId, NodeId, PresetTypeId};
    use std::collections::BTreeMap;

    #[test]
    fn modifier_badge_compares_scene_initialized_library_graph() {
        let mut owner = owner_graph();
        owner.preset_metadata.as_mut().unwrap().scene_bounds =
            Some(([-2.0, -3.0, -4.0], [2.0, 3.0, 4.0]));
        for id in [
            "SceneLoop",
            "SceneFog",
            "ElasticSculpture",
            "SurfacePeel",
            "SurfacePeelHit",
            "VortexFragments",
        ] {
            let recipe =
                manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::new(id)).unwrap();
            let local = manifold_renderer::node_graph::scene_modifier_authoring::initialize_scene_modifier_graph(&owner, recipe).unwrap();
            assert!(!has_graph_mod(&owner, &local), "fresh {id}");
            let mut edited = local.clone();
            edited.nodes[0].title = Some("Edited local graph".into());
            assert!(has_graph_mod(&owner, &edited), "edited {id}");
            assert_eq!(library_baseline(&owner, &edited).unwrap().unwrap(), local);
        }
        assert!(
            has_graph_mod(&owner, &local_recipe()),
            "missing library remains a local snapshot"
        );
    }

    #[test]
    fn modifier_editor_baseline_selects_local_recipe_and_ignores_host_controls() {
        let (mut project, target) = project();
        let host = project.graph_target_owner_mut(&target).unwrap();
        let graph = host.graph.as_mut().unwrap();
        let recipe =
            manifold_renderer::node_graph::bundled_preset_def(&PresetTypeId::new("SurfacePeel"))
                .unwrap();
        *graph.scene_modifiers[0].graph = recipe.clone();
        let baseline = crate::graph_target::catalog_default(&project, &target).unwrap();
        assert_eq!(target.graph_in(&baseline).unwrap(), recipe);
        let host = project.graph_target_owner_mut(&target).unwrap();
        host.params.get_mut("outer_a").unwrap().base = 0.1;
        let graph = host.graph.as_mut().unwrap();
        graph.scene_modifiers[0].graph.nodes[0].editor_pos = Some((40.0, 80.0));
        assert!(!has_graph_mod(graph, &graph.scene_modifiers[0].graph));
        assert!(
            !target
                .graph_in(graph)
                .unwrap()
                .diverges_ignoring_layout(target.graph_in(&baseline).unwrap())
        );
    }

    fn spec(id: &str, default: f32, curve: MacroCurve) -> ParamSpecDef {
        ParamSpecDef {
            id: id.into(),
            name: id.into(),
            min: 0.0,
            max: 1.0,
            default_value: default,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve,
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
            card_visible: true,
        }
    }

    fn node(id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id: 1,
            node_id: NodeId::new(id),
            type_id: "node.value".into(),
            handle: Some(id.into()),
            params: BTreeMap::from([("gain".into(), SerializedParamValue::Float { value: 0.25 })]),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }

    fn local_recipe() -> EffectGraphDef {
        let node_id = NodeId::new("local-node");
        let mut local = EffectGraphDef {
            version: 3,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::new("local-recipe"),
                display_name: "Local Recipe".into(),
                category: "Geometry".into(),
                osc_prefix: "local_recipe".into(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                layer_types: None,
                params: vec![
                    spec("gain", 0.25, MacroCurve::Linear),
                    spec("other", 0.4, MacroCurve::Linear),
                ],
                bindings: vec![BindingDef {
                    id: "gain".into(),
                    label: "Gain".into(),
                    default_value: 0.25,
                    target: BindingTarget::Node {
                        node_id: node_id.clone(),
                        param: "gain".into(),
                    },
                    convert: manifold_core::effects::ParamConvert::Float,
                    user_added: false,
                    scale: 0.5,
                    offset: 0.1,
                    default_mirrors_node_param: false,
                }],
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
                scene_bounds: None,
                scene_modifier: Some(SceneModifierRecipe {
                    schema_version: 1,
                    singleton: false,
                    enabled_param: "gain".into(),
                    preparation_params: Vec::new(),
                    stages: Vec::new(),
                    initializers: vec![
                        SceneNodeInitializer {
                            target: SceneNodeRef {
                                scope: Vec::new(),
                                node: node_id.clone(),
                            },
                            param: "gain".into(),
                            value: SceneScalarExpr::Constant { value: 0.25 },
                        },
                        SceneNodeInitializer {
                            target: SceneNodeRef {
                                scope: Vec::new(),
                                node: node_id,
                            },
                            param: "other".into(),
                            value: SceneScalarExpr::Constant { value: 0.4 },
                        },
                    ],
                    calibrations: vec![
                        manifold_core::scene_modifier_preset::SceneParamCalibration {
                            param_id: "gain".into(),
                            min: SceneScalarExpr::Constant { value: 0.0 },
                            max: SceneScalarExpr::Constant { value: 1.0 },
                            default_value: SceneScalarExpr::Constant { value: 0.25 },
                        },
                        manifold_core::scene_modifier_preset::SceneParamCalibration {
                            param_id: "other".into(),
                            min: SceneScalarExpr::Constant { value: 0.0 },
                            max: SceneScalarExpr::Constant { value: 1.0 },
                            default_value: SceneScalarExpr::Constant { value: 0.4 },
                        },
                    ],
                }),
            }),
            scene_modifiers: Vec::new(),
            nodes: vec![node("local-node")],
            wires: Vec::new(),
        };
        local.nodes[0]
            .params
            .insert("other".into(), SerializedParamValue::Float { value: 0.4 });
        local
    }

    fn owner_graph() -> EffectGraphDef {
        let local = local_recipe();
        let frame = SceneMeshReferenceFrame {
            target: SceneNodeRef {
                scope: Vec::new(),
                node: NodeId::new("object"),
            },
            source: SceneNodeRef {
                scope: Vec::new(),
                node: NodeId::new("mesh"),
            },
            source_definition_hash: "mesh-hash".into(),
            source_offset: [1.0, 2.0, 3.0],
            scene_radius: 4.0,
        };
        EffectGraphDef {
            version: 3,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::new("host"),
                display_name: "Host".into(),
                category: "Geometry".into(),
                osc_prefix: "host".into(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                layer_types: None,
                params: vec![
                    spec("outer_a", 0.2, MacroCurve::SCurve),
                    spec("outer_b", 0.3, MacroCurve::SCurve),
                ],
                bindings: vec![
                    BindingDef {
                        id: "outer_a".into(),
                        label: "A".into(),
                        default_value: 0.2,
                        target: BindingTarget::SceneModifier {
                            modifier_id: NodeId::new("a"),
                            param_id: "gain".into(),
                        },
                        convert: manifold_core::effects::ParamConvert::Float,
                        user_added: false,
                        scale: 1.4,
                        offset: 0.2,
                        default_mirrors_node_param: false,
                    },
                    BindingDef {
                        id: "outer_b".into(),
                        label: "B".into(),
                        default_value: 0.3,
                        target: BindingTarget::SceneModifier {
                            modifier_id: NodeId::new("b"),
                            param_id: "gain".into(),
                        },
                        convert: manifold_core::effects::ParamConvert::Float,
                        user_added: false,
                        scale: 0.8,
                        offset: -0.1,
                        default_mirrors_node_param: false,
                    },
                ],
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
                scene_bounds: None,
                scene_modifier: None,
            }),
            scene_modifiers: vec![
                SceneModifierInstanceDef {
                    id: NodeId::new("a"),
                    scene: SceneNodeRef {
                        scope: Vec::new(),
                        node: NodeId::new("scene"),
                    },
                    targets: SceneTargetSelection::AllObjects,
                    mesh_frames: vec![frame.clone()],
                    legacy_math_view_carrier: None,
                    graph: Box::new(local.clone()),
                },
                SceneModifierInstanceDef {
                    id: NodeId::new("b"),
                    scene: SceneNodeRef {
                        scope: Vec::new(),
                        node: NodeId::new("scene"),
                    },
                    targets: SceneTargetSelection::AllObjects,
                    mesh_frames: vec![frame],
                    legacy_math_view_carrier: None,
                    graph: Box::new(local),
                },
            ],
            nodes: Vec::new(),
            wires: Vec::new(),
        }
    }

    fn project() -> (Project, GraphTarget) {
        let graph = owner_graph();
        let mut layer = Layer::new_generator("Host".into(), PresetTypeId::new("host"), 0);
        let layer_id = LayerId::new("layer");
        layer.layer_id = layer_id.clone();
        let host = layer.gen_params_or_init();
        host.graph = Some(graph.clone());
        host.reseed_param_values_from_def(&graph);
        host.params.get_mut("outer_a").unwrap().base = 0.7;
        host.params.get_mut("outer_a").unwrap().value = 0.7;
        host.params.get_mut("outer_b").unwrap().base = 0.35;
        host.params.get_mut("outer_b").unwrap().value = 0.35;
        let mut project = Project::default();
        project.timeline.layers.push(layer);
        let target = GraphTarget::SceneModifier {
            owner: Box::new(GraphTarget::Generator(layer_id)),
            modifier_id: NodeId::new("a"),
        };
        (project, target)
    }

    #[test]
    fn scene_modifier_export_preserves_affine_calibration_and_isolates_instances() {
        let (project, target) = project();
        let before = project.graph_target_owner(&target).unwrap().graph.clone();
        let before_params = project.graph_target_owner(&target).unwrap().params.clone();
        let exported = export_def(&project, &target).expect("local modifier exports");
        let meta = exported.preset_metadata.as_ref().unwrap();
        let gain = meta.params.iter().find(|param| param.id == "gain").unwrap();
        assert_eq!(gain.default_value, 0.7);
        assert_eq!(
            gain.curve,
            MacroCurve::SCurve,
            "host nonlinear calibration is retained"
        );
        let binding = meta
            .bindings
            .iter()
            .find(|binding| binding.id == "gain")
            .unwrap();
        assert!((binding.scale - 0.7).abs() < f32::EPSILON);
        assert!((binding.offset - 0.2).abs() < f32::EPSILON);
        let slider_value = 0.6;
        let calibrated_value = MacroCurve::SCurve.apply(slider_value);
        let expected_before = 0.1 + 0.5 * (0.2 + 1.4 * calibrated_value);
        let after = binding.offset + binding.scale * calibrated_value;
        assert!(
            (expected_before - after).abs() < f32::EPSILON,
            "exported affine binding preserves forward reshape"
        );
        assert!(
            exported.scene_modifiers.is_empty(),
            "host modifier instances never leak into a bare export"
        );
        let recipe = meta.scene_modifier.as_ref().unwrap();
        assert_eq!(recipe.calibrations.len(), 1);
        assert_eq!(recipe.calibrations[0].param_id, "other");
        assert_eq!(recipe.initializers.len(), 1);
        assert_eq!(recipe.initializers[0].param, "other");
        assert_eq!(
            project.graph_target_owner(&target).unwrap().graph,
            before,
            "export does not mutate the owner"
        );
        assert_eq!(
            project.graph_target_owner(&target).unwrap().params,
            before_params,
            "export does not mutate live host params"
        );

        let target_b = GraphTarget::SceneModifier {
            owner: Box::new(target.host_target().unwrap().clone()),
            modifier_id: NodeId::new("b"),
        };
        let exported_b =
            export_def(&project, &target_b).expect("second identical modifier exports");
        let binding_b = exported_b
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .iter()
            .find(|binding| binding.id == "gain")
            .unwrap();
        assert!((binding_b.scale - 0.4).abs() < f32::EPSILON);
        assert!((binding_b.offset - 0.05).abs() < f32::EPSILON);
        let gain_b = exported_b
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .find(|param| param.id == "gain")
            .unwrap();
        assert_eq!(gain_b.default_value, 0.35);
        assert_eq!(gain_b.curve, MacroCurve::SCurve);
        assert_eq!(
            project.graph_target_owner(&target).unwrap().graph,
            before,
            "second export also leaves the owner unchanged"
        );
        assert_eq!(
            project.graph_target_owner(&target).unwrap().params,
            before_params,
            "second export also leaves live host params unchanged"
        );
    }
}
