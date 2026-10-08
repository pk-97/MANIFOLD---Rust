//! Structural and CPU preparation coverage for the stock force scene modifiers.

use manifold_renderer as _;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
use manifold_core::scene_modifier_edit::insert_scene_modifier;
use manifold_core::scene_modifier_preset::{
    SceneEndpoint, SceneStageScope, SceneStageSource, SceneTargetSelection,
    validate_scene_modifier_schema,
};
use manifold_core::{Beats, Seconds};
use manifold_physics::interaction::VectorField;
use manifold_node_engine::persistence::EffectGraphDefExt;
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier;
use manifold_node_engine::load::expand::prepare_scene_modifiers;
use manifold_node_engine::{exec::effect_node::EffectNode, exec::effect_node::EffectNodeContext, exec::effect_node::EffectNodeType, exec::execution::Executor, exec::effect_node::FrameTime, parameters::ParamValue, persistence::PrimitiveRegistry, exec::execution_plan::compile};

const PHYSICS_SOLIDS: &str = include_str!("../assets/generator-presets/PhysicsSolids.json");

const RECIPES: [(&str, &str, &str); 3] = [
    ("UniformForce", "node.uniform_vector_field", "Vector X"),
    ("RadialForce", "node.radial_vector_field", "Center X"),
    ("VortexForce", "node.vortex_vector_field", "Axis X"),
];

fn recipe(name: &str) -> EffectGraphDef {
    let source = match name {
        "UniformForce" => include_str!("../assets/scene-modifier-presets/UniformForce.json"),
        "RadialForce" => include_str!("../assets/scene-modifier-presets/RadialForce.json"),
        "VortexForce" => include_str!("../assets/scene-modifier-presets/VortexForce.json"),
        _ => panic!("unknown force recipe {name}"),
    };
    serde_json::from_str(source).expect("force recipe parses")
}

fn node<'a>(nodes: &'a [EffectGraphNode], node_id: &str) -> &'a EffectGraphNode {
    nodes
        .iter()
        .find(|node| node.node_id.as_str() == node_id)
        .unwrap_or_else(|| panic!("node {node_id} is present"))
}

fn inner_node<'a>(group: &'a EffectGraphNode, node_id: &str) -> &'a EffectGraphNode {
    group
        .group
        .as_ref()
        .expect("stage group body")
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == node_id)
        .unwrap_or_else(|| panic!("inner node {node_id} is present"))
}

fn inner_wires(group: &EffectGraphNode) -> &[EffectGraphWire] {
    &group.group.as_ref().expect("stage group body").wires
}

fn has_wire(group: &EffectGraphNode, from: &str, from_port: &str, to: &str, to_port: &str) -> bool {
    let body = group.group.as_ref().expect("stage group body");
    let from_id = body
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == from)
        .map(|node| node.id)
        .unwrap_or_else(|| panic!("inner source {from} is present"));
    let to_id = body
        .nodes
        .iter()
        .find(|node| node.node_id.as_str() == to)
        .map(|node| node.id)
        .unwrap_or_else(|| panic!("inner target {to} is present"));
    body.wires.iter().any(|wire| {
        wire.from_node == from_id
            && wire.from_port == from_port
            && wire.to_node == to_id
            && wire.to_port == to_port
    })
}

#[test]
fn force_recipes_validate_and_expose_finite_controls() {
    for (name, field_type, label) in RECIPES {
        let def = recipe(name);
        validate_scene_modifier_schema(&def)
            .unwrap_or_else(|error| panic!("{name} schema is valid: {error}"));
        let metadata = def.preset_metadata.as_ref().expect("preset metadata");
        assert_eq!(metadata.category, "Forces");
        assert_eq!(metadata.params[0].id, "enabled");
        assert!(metadata.params[0].is_toggle);
        assert!(metadata.params.iter().all(|param| {
            param.min.is_finite()
                && param.max.is_finite()
                && param.default_value.is_finite()
                && param.min <= param.default_value
                && param.default_value <= param.max
        }));
        let source = node(&def.nodes, "force_source_stage");
        assert_eq!(inner_node(source, "force_field").type_id, field_type);
        assert!(
            metadata.params.iter().any(|param| param.name == label),
            "{name} keeps its field-specific control label"
        );
        let strength = metadata
            .params
            .iter()
            .find(|param| param.id == "strength")
            .expect("signed strength control");
        assert!(strength.min < 0.0 && strength.max > 0.0);
        let impulse_strength = metadata
            .params
            .iter()
            .find(|param| param.id == "impulse_strength")
            .expect("signed impulse strength control");
        assert_eq!(
            (impulse_strength.min, impulse_strength.max, impulse_strength.default_value),
            (-20.0, 20.0, 2.0)
        );
        let fire = metadata
            .params
            .iter()
            .find(|param| param.id == "fire")
            .expect("Fire trigger control");
        assert!(fire.is_trigger);
        assert_eq!((fire.min, fire.max, fire.default_value), (0.0, 16777216.0, 0.0));
        let binding_ids: std::collections::BTreeSet<_> = metadata
            .bindings
            .iter()
            .map(|binding| binding.id.as_str())
            .collect();
        let param_ids: std::collections::BTreeSet<_> = metadata
            .params
            .iter()
            .map(|param| param.id.as_str())
            .collect();
        assert!(binding_ids.contains("impulse_strength"));
        assert!(!binding_ids.contains("fire"));
        assert_eq!(binding_ids.len() + 1, param_ids.len(), "{name} leaves only Fire unbound");
        let impulse_binding = metadata
            .bindings
            .iter()
            .find(|binding| binding.id == "impulse_strength")
            .expect("impulse strength binding");
        assert_eq!(impulse_binding.label, "Impulse (m/s)");
        assert!(matches!(
            &impulse_binding.target,
            manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
                if node_id.as_str() == "force_impulse_strength" && param == "strength"
        ));
        let modifier = metadata.scene_modifier.as_ref().expect("scene modifier recipe");
        assert_eq!(modifier.impulses.len(), 1);
        let impulse = &modifier.impulses[0];
        assert_eq!(impulse.param_id, "fire");
        assert_eq!(impulse.field.scope, vec![NodeId::new("force_source_stage")]);
        assert_eq!(impulse.field.node.as_str(), "force_impulse_gate");
        assert_eq!(impulse.port, "out");
    }
}

#[test]
fn force_sources_scale_strength_then_enabled_and_apply_additively() {
    for (name, _, _) in RECIPES {
        let def = recipe(name);
        let source = node(&def.nodes, "force_source_stage");
        let apply = node(&def.nodes, "force_apply_stage");
        let source_body = source.group.as_ref().expect("source body");
        assert_eq!(source_body.interface.outputs[0].name, "field");
        assert!(source_body.interface.inputs.is_empty());
        assert!(source_body.interface.outputs[0].port_type == "VectorField");
        assert!(has_wire(
            source,
            "force_field",
            "out",
            "force_strength",
            "field"
        ));
        assert!(has_wire(
            source,
            "force_strength",
            "out",
            "force_gate",
            "field"
        ));
        assert!(has_wire(
            source,
            "force_enabled",
            "out",
            "force_gate",
            "strength"
        ));
        assert!(has_wire(
            source,
            "force_field",
            "out",
            "force_impulse_strength",
            "field"
        ));
        assert!(has_wire(
            source,
            "force_impulse_strength",
            "out",
            "force_impulse_gate",
            "field"
        ));
        assert!(has_wire(
            source,
            "force_enabled",
            "out",
            "force_impulse_gate",
            "strength"
        ));
        assert!(has_wire(
            source,
            "force_gate",
            "out",
            "force_source_output",
            "field"
        ));
        assert!(!has_wire(
            source,
            "force_impulse_gate",
            "out",
            "force_source_output",
            "field"
        ));
        assert!(has_wire(
            apply,
            "group_previous",
            "previous",
            "force_add",
            "a"
        ));
        assert!(has_wire(apply, "group_field", "field", "force_add", "b"));
        assert!(has_wire(
            apply,
            "force_add",
            "out",
            "force_apply_output",
            "acceleration"
        ));
        assert!(
            inner_wires(apply).len() == 3,
            "{name} has only the previous-plus-force composition"
        );
        let recipe = def
            .preset_metadata
            .as_ref()
            .unwrap()
            .scene_modifier
            .as_ref()
            .unwrap();
        assert_eq!(recipe.stages.len(), 2);
        assert_eq!(recipe.stages[0].scope, SceneStageScope::Scene);
        assert!(recipe.stages[0].outputs.is_empty());
        assert_eq!(recipe.stages[1].scope, SceneStageScope::EachObject);
        assert_eq!(
            recipe.stages[1].outputs[0].endpoint,
            SceneEndpoint::Acceleration
        );
        assert!(recipe.stages[1].inputs.iter().any(|input| {
            input.port == "previous"
                && input.source
                    == SceneStageSource::Previous {
                        endpoint: SceneEndpoint::Acceleration,
                    }
        }));
        assert!(recipe.stages[1].inputs.iter().any(|input| {
            input.port == "field"
                && matches!(
                    &input.source,
                    SceneStageSource::StageOutput { stage, port }
                        if stage.as_str() == "force_source_stage" && port == "field"
                )
        }));
    }

    let host: EffectGraphDef = serde_json::from_str(PHYSICS_SOLIDS).expect("host parses");
    let scene = host
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .map(|node| manifold_core::scene_modifier_preset::SceneNodeRef {
            scope: Vec::new(),
            node: node.node_id.clone(),
        })
        .expect("physics host has render_scene");
    for name in ["UniformForce", "RadialForce", "VortexForce"] {
        let instance = prepare_new_scene_modifier(
            &host,
            &recipe(name),
            NodeId::new(format!("cpu-{name}")),
            scene.clone(),
            SceneTargetSelection::Explicit {
                objects: vec![manifold_core::scene_modifier_preset::SceneNodeRef {
                    scope: Vec::new(),
                    node: NodeId::new("physics_demo_114"),
                }],
            },
        )
        .unwrap_or_else(|error| panic!("{name} attaches to a physics recipient: {error}"));
        let attached = insert_scene_modifier(&host, 0, instance)
            .unwrap_or_else(|error| panic!("{name} attaches: {error}"))
            .graph;
        prepare_scene_modifiers(&attached, &PrimitiveRegistry::with_builtin())
            .unwrap_or_else(|error| panic!("{name} prepares: {error}"));
    }

    let previous = [1.0, 2.0, 3.0];
    let disabled = evaluate_force_graph(&["UniformForce"], &[-2.0], &[0.0]);
    assert_eq!(
        disabled, previous,
        "Enabled=0 preserves previous acceleration"
    );
    let active = evaluate_force_graph(&["UniformForce"], &[-2.0], &[1.0]);
    assert_eq!(active, [1.0, 0.0, 3.0], "signed Strength scales the field");
    let radial = evaluate_force_graph(&["RadialForce"], &[0.5], &[1.0]);
    let two_forces =
        evaluate_force_graph(&["UniformForce", "RadialForce"], &[-2.0, 0.5], &[1.0, 1.0]);
    for (actual, expected) in two_forces.iter().zip(
        active
            .iter()
            .zip(radial.iter())
            .zip(previous.iter())
            .map(|((a, b), previous)| a + b - previous),
    ) {
        assert!((actual - expected).abs() < 1.0e-6, "two force fields sum");
    }
}

#[test]
fn force_impulses_are_independent_enabled_gated_and_serializable() {
    let disabled = evaluate_impulse_graph(&["UniformForce"], &[0.0], &[0.0]);
    assert_eq!(disabled, [0.0, 0.0, 0.0]);
    let active = evaluate_impulse_graph(&["UniformForce"], &[0.0], &[1.0]);
    assert_eq!(active, [0.0, 2.0, 0.0]);
    let continuous_strength_changed = evaluate_impulse_graph(&["UniformForce"], &[-20.0], &[1.0]);
    assert_eq!(continuous_strength_changed, active);

    for name in ["UniformForce", "RadialForce", "VortexForce"] {
        let def = recipe(name);
        let json = serde_json::to_string(&def).expect("force recipe serializes");
        let round_trip: EffectGraphDef =
            serde_json::from_str(&json).expect("serialized force recipe parses");
        let modifier = round_trip
            .preset_metadata
            .as_ref()
            .and_then(|metadata| metadata.scene_modifier.as_ref())
            .expect("serialized scene modifier recipe");
        assert_eq!(modifier.impulses.len(), 1);
        assert_eq!(modifier.impulses[0].param_id, "fire");
        assert_eq!(modifier.impulses[0].field.node.as_str(), "force_impulse_gate");
    }
}

struct FieldObserver {
    type_id: EffectNodeType,
}

impl EffectNode for FieldObserver {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }

    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }

    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 1] = [NodePort {
            name: std::borrow::Cow::Borrowed("field"),
            ty: PortType::VectorField,
            kind: PortKind::Input,
            required: true,
        }];
        &INPUTS
    }

    fn outputs(&self) -> &[NodeOutput] {
        static OUTPUTS: [NodeOutput; 3] = [
            NodePort {
                name: std::borrow::Cow::Borrowed("x"),
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Output,
                required: false,
            },
            NodePort {
                name: std::borrow::Cow::Borrowed("y"),
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Output,
                required: false,
            },
            NodePort {
                name: std::borrow::Cow::Borrowed("z"),
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Output,
                required: false,
            },
        ];
        &OUTPUTS
    }

    fn parameters(&self) -> &[manifold_node_engine::parameters::ParamDef] {
        &[]
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(field) = ctx.inputs.vector_field("field") else {
            ctx.mark_outputs_pending();
            return;
        };
        let value = field.sample([0.25, -0.5, 0.75]);
        for (port, component) in [("x", value[0]), ("y", value[1]), ("z", value[2])] {
            ctx.outputs.set_scalar(port, ParamValue::Float(component));
        }
    }
}

struct ScalarSink {
    type_id: EffectNodeType,
}

impl EffectNode for ScalarSink {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }

    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }

    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 3] = [
            NodePort {
                name: std::borrow::Cow::Borrowed("x"),
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            },
            NodePort {
                name: std::borrow::Cow::Borrowed("y"),
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            },
            NodePort {
                name: std::borrow::Cow::Borrowed("z"),
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            },
        ];
        &INPUTS
    }

    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }

    fn parameters(&self) -> &[manifold_node_engine::parameters::ParamDef] {
        &[]
    }

    fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

fn frame_time() -> FrameTime {
    FrameTime {
        beats: Beats(0.0),
        seconds: Seconds(0.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    }
}

fn evaluate_force_graph(names: &[&str], strengths: &[f32], enabled: &[f32]) -> [f32; 3] {
    evaluate_force_output(names, strengths, enabled, None)
}

fn evaluate_impulse_graph(names: &[&str], strengths: &[f32], enabled: &[f32]) -> [f32; 3] {
    evaluate_force_output(names, strengths, enabled, Some(2.0))
}

fn evaluate_force_output(
    names: &[&str],
    strengths: &[f32],
    enabled: &[f32],
    impulse_strength: Option<f32>,
) -> [f32; 3] {
    assert_eq!(names.len(), strengths.len());
    assert_eq!(names.len(), enabled.len());
    if impulse_strength.is_some() {
        assert_eq!(names.len(), 1, "impulse proof uses one source branch");
    }
    let registry = PrimitiveRegistry::with_builtin();
    let mut host: EffectGraphDef = serde_json::from_str(PHYSICS_SOLIDS).unwrap();
    let scene = host
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .unwrap()
        .node_id
        .clone();
    host.nodes.push(serde_json::from_value(serde_json::json!({
        "id": 900, "nodeId": "previous", "typeId": "node.uniform_vector_field",
        "params": {"x":{"type":"Float","value":1.0},"y":{"type":"Float","value":2.0},"z":{"type":"Float","value":3.0}}
    })).unwrap());
    host.wires.push(EffectGraphWire {
        from_node: 900,
        from_port: "out".into(),
        to_node: 40,
        to_port: "body_acceleration_1".into(),
    });
    for (index, ((name, strength), enabled)) in names.iter().zip(strengths).zip(enabled).enumerate()
    {
        let mut recipe = recipe(name);
        let metadata = recipe.preset_metadata.as_mut().unwrap();
        for (id, value) in [("strength", *strength), ("enabled", *enabled)] {
            metadata
                .params
                .iter_mut()
                .find(|param| param.id == id)
                .unwrap()
                .default_value = value;
            for binding in metadata
                .bindings
                .iter_mut()
                .filter(|binding| binding.id == id)
            {
                binding.default_value = value;
            }
        }
        if let Some(value) = impulse_strength {
            metadata
                .params
                .iter_mut()
                .find(|param| param.id == "impulse_strength")
                .unwrap()
                .default_value = value;
            metadata
                .bindings
                .iter_mut()
                .find(|binding| binding.id == "impulse_strength")
                .unwrap()
                .default_value = value;
        }
        let instance = prepare_new_scene_modifier(
            &host,
            &recipe,
            NodeId::new(format!("force-{index}")),
            manifold_core::scene_modifier_preset::SceneNodeRef {
                scope: vec![],
                node: scene.clone(),
            },
            SceneTargetSelection::Explicit {
                objects: vec![manifold_core::scene_modifier_preset::SceneNodeRef {
                    scope: vec![],
                    node: NodeId::new("physics_demo_114"),
                }],
            },
        )
        .unwrap();
        host = insert_scene_modifier(&host, index, instance).unwrap().graph;
    }
    let prepared = prepare_scene_modifiers(&host, &registry).unwrap();
    let routes = prepared.routes;
    let mut def = prepared.def;
    let (producer, root) = if impulse_strength.is_some() {
        let route = routes
            .iter()
            .find(|route| {
                route.modifier_id.as_str() == "force-0"
                    && route.local.node.as_str() == "force_impulse_gate"
            })
            .expect("impulse route resolves the source gate");
        let producer = route
            .copies
            .first()
            .expect("scene impulse has one source copy")
            .node_id
            .clone();
        let root = def
            .nodes
            .iter()
            .find(|node| node.node_id == producer)
            .expect("prepared impulse producer is present")
            .id;
        (producer, root)
    } else {
        let world = def
            .nodes
            .iter()
            .find(|node| node.node_id.as_str() == "physics_demo_40")
            .unwrap()
            .id;
        let target = def
            .wires
            .iter()
            .find(|wire| wire.to_node == world && wire.to_port == "body_acceleration_1")
            .unwrap()
            .clone();
        let producer = def
            .nodes
            .iter()
            .find(|node| node.id == target.from_node)
            .unwrap()
            .node_id
            .clone();
        (producer, target.from_node)
    };
    // Execute the actual prepared acceleration ancestry. Removing only its
    // consumers keeps this CPU proof independent of native worlds/GPU meshes.
    let mut keep = std::collections::BTreeSet::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        if keep.insert(id) {
            pending.extend(
                def.wires
                    .iter()
                    .filter(|wire| wire.to_node == id)
                    .map(|wire| wire.from_node),
            );
        }
    }
    def.nodes.retain(|node| keep.contains(&node.id));
    def.wires
        .retain(|wire| keep.contains(&wire.to_node) && keep.contains(&wire.from_node));
    def.preset_metadata = None;
    let mut graph = def.into_graph(&registry, &Default::default()).unwrap();
    let accumulated = graph.instance_by_node_id(&producer).unwrap();
    let observer = graph.add_node(Box::new(FieldObserver {
        type_id: EffectNodeType::new("test.force_field_observer"),
    }));
    let sink = graph.add_node(Box::new(ScalarSink {
        type_id: EffectNodeType::new("test.force_scalar_sink"),
    }));
    graph
        .connect((accumulated, "out"), (observer, "field"))
        .unwrap();
    for port in ["x", "y", "z"] {
        graph.connect((observer, port), (sink, port)).unwrap();
    }
    let plan = compile(&graph).expect("CPU force graph compiles");
    let mut executor = Executor::with_mock();
    executor.set_preview_target(Some(observer));
    executor.execute_frame(&mut graph, &plan, frame_time());
    let mut result = [0.0; 3];
    for (port, value) in executor.preview_scalar_outputs() {
        result[match port.as_str() {
            "x" => 0,
            "y" => 1,
            "z" => 2,
            other => panic!("unexpected observer output {other}"),
        }] = *value;
    }
    assert_eq!(executor.preview_scalar_outputs().len(), 3);
    assert!(result.iter().all(|value| value.is_finite()));
    result
}

#[test]
fn force_recipes_prepare_with_zero_selected_bodies() {
    let host: EffectGraphDef = serde_json::from_str(PHYSICS_SOLIDS).expect("host parses");
    let scene = host
        .nodes
        .iter()
        .find(|node| node.type_id == "node.render_scene")
        .map(|node| manifold_core::scene_modifier_preset::SceneNodeRef {
            scope: Vec::new(),
            node: node.node_id.clone(),
        })
        .expect("physics host has render_scene");
    for name in ["UniformForce", "RadialForce", "VortexForce"] {
        let instance = prepare_new_scene_modifier(
            &host,
            &recipe(name),
            NodeId::new(format!("zero-{name}")),
            scene.clone(),
            SceneTargetSelection::Explicit {
                objects: Vec::new(),
            },
        )
        .unwrap_or_else(|error| panic!("{name} accepts zero selected bodies: {error}"));
        let attached = insert_scene_modifier(&host, 0, instance)
            .unwrap_or_else(|error| panic!("{name} attaches: {error}"))
            .graph;
        prepare_scene_modifiers(&attached, &PrimitiveRegistry::with_builtin())
            .unwrap_or_else(|error| panic!("{name} prepares with zero selected bodies: {error}"));
    }
}
