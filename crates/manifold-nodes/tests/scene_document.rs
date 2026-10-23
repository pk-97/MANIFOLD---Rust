//! A scene document through save, load and a running world
//! (SCENE_DOCUMENT_DESIGN.md section 7 (Commands and the compiler)): a saved
//! scene loads back as the same scene, adding a light keeps a falling body
//! falling, a Speed past its range clamps instead of freezing the world, and
//! a param write or a camera cut reaches the next frame with no rebuild.

use std::borrow::Cow;
use std::cell::Cell;

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, EffectGraphWire};
use manifold_core::effects::{ParameterDriver, PresetInstance};
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::scene_doc::{Item, ItemFields, ItemId, ItemKind, PhysicsSetting, SceneDoc};
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::{Beats, PresetTypeId, Seconds};
use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, FrameTime};
use manifold_node_engine::parameters::{ParamDef, ParamValue};
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::scene::camera::Camera;
use manifold_node_engine::scene::transform::Transform;

fn scene_layer(doc: SceneDoc) -> Layer {
    let mut layer = Layer::new_generator("Scene".into(), PresetTypeId::from_string("Scene".into()), 0);
    layer.gen_params_mut().unwrap().install_scene_document(doc).unwrap();
    layer
}

/// Every param's id, value and base.
fn values(fx: &PresetInstance) -> Vec<(String, f32, f32)> {
    fx.params.iter().map(|p| (p.id().to_owned(), p.value, p.base)).collect()
}

/// The layer graph as the loader runs it: the Scene node's body inlined.
fn flat(fx: &PresetInstance) -> EffectGraphDef {
    manifold_core::flatten::flatten_groups(fx.graph.as_ref().unwrap()).unwrap()
}

/// A blur the user wired between the Scene node and the layer output.
fn with_post_blur(fx: &mut PresetInstance) {
    use manifold_core::scene_node::scene_node;
    let graph = fx.graph.as_mut().unwrap();
    let scene = scene_node(graph).unwrap().id;
    let output = graph.nodes.iter().find(|n| n.type_id == "system.final_output").unwrap().id;
    let blur = graph.nodes.iter().map(|n| n.id).max().unwrap() + 1;
    graph.nodes.push(serde_json::from_value(serde_json::json!({"id": blur, "nodeId": "post_blur", "typeId": "node.blur"})).unwrap());
    graph.wires.retain(|w| w.to_node != output);
    for (from_node, from_port, to_node, to_port) in [(scene, "color", blur, "source"), (blur, "out", output, "in")] {
        graph.wires.push(EffectGraphWire { from_node, from_port: from_port.into(), to_node, to_port: to_port.into() });
    }
}

#[test]
fn a_saved_scene_loads_back_as_the_same_scene() {
    let mut doc = SceneDoc::base();
    doc.insert(None, 0, Item::new(ItemId::new("rig"), "Rig", ItemFields::default_for(ItemKind::Group))).unwrap();
    doc.reparent(&ItemId::new("cube"), Some(&ItemId::new("rig")), 0).unwrap();
    let mut layer = scene_layer(doc.clone());
    let fx = layer.gen_params_mut().unwrap();
    // Post-processing wired after the Scene node survives a Scene panel edit,
    // which recompiles only what is inside the Scene node.
    with_post_blur(fx);
    // The blur's radius goes on the layer card with an LFO on it.
    let radius = "user.post_blur.radius.1";
    fx.append_user_binding(manifold_core::effects::UserParamBinding {
        id: radius.into(),
        label: "Blur".into(),
        node_id: manifold_core::NodeId::new("post_blur"),
        legacy_node_handle: None,
        inner_param: "radius".into(),
        min: 0.0,
        max: 20.0,
        default_value: 4.0,
        convert: Default::default(),
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
        value_labels: Vec::new(),
        section: None,
    });
    assert!(fx.set_base_param(radius, 9.0));
    let post = fx.graph.as_ref().unwrap().wires.clone();
    doc.insert(None, 0, Item::new(ItemId::new("lamp"), "Lamp", ItemFields::default_for(ItemKind::Light))).unwrap();
    // A second camera, then the first one gone: the cut numbers and the
    // numbers already handed out come back with the document.
    doc.insert(None, 0, Item::new(ItemId::new("closeup"), "Closeup", ItemFields::default_for(ItemKind::Camera))).unwrap();
    doc.remove(&ItemId::new("camera")).unwrap();
    fx.install_scene_document(doc.clone()).unwrap();
    assert_eq!(fx.graph.as_ref().unwrap().wires, post, "a recompile rewired the user's post-processing");
    assert_eq!(fx.get_base_param(radius), 9.0, "a recompile dropped the blur's card control");
    assert!(flat(fx).nodes.iter().any(|n| n.node_id.as_str() == "lamp:light"), "the new light is inside the Scene node");
    assert!(fx.set_base_param("cube.pos_y", 2.5), "the cube has a Position Y");
    fx.drivers = Some(vec![
        ParameterDriver::new("cube.pos_y", BeatDivision::Quarter, DriverWaveform::Sine),
        ParameterDriver::new(radius, BeatDivision::Quarter, DriverWaveform::Sine),
    ]);
    let (before, graph) = (values(fx), fx.graph.clone());
    let mut project = Project::default();
    project.timeline.layers.push(layer);

    let saved = serde_json::to_string(&project).unwrap();
    let mut loaded: Project = serde_json::from_str(&saved).unwrap();
    loaded.on_after_deserialize();
    loaded.reconcile_param_manifests();

    let fx = loaded.timeline.layers[0].gen_params().unwrap();
    assert_eq!(fx.scene.as_ref(), Some(&doc), "the document comes back");
    let cut = |doc: &SceneDoc| match doc.find(&ItemId::new("closeup")).map(|item| &item.fields) {
        Some(ItemFields::Camera(camera)) => Some(camera.cut),
        _ => None,
    };
    let loaded_doc = fx.scene.as_ref().unwrap();
    assert_eq!((cut(loaded_doc), loaded_doc.next_cut), (Some(1), 2), "the closeup keeps cut 1 and the deleted camera's 0 stays spent");
    assert!(graph.is_some() && fx.graph == graph, "and compiles to the same graph, post-processing included");
    PresetRuntime::from_def_for_render(fx.graph.clone().unwrap(), &PrimitiveRegistry::with_builtin(), Some(&fx.params), false)
        .expect("the loaded layer graph builds, Scene node and post-processing together");
    assert_eq!(values(fx), before, "every value comes back under its id");
    assert_eq!(fx.drivers.as_ref().unwrap()[0].param_id, "cube.pos_y", "the LFO still points at the cube");
    assert!(fx.params.get(radius).is_some_and(|p| p.exposed), "the blur's control is back on the card");
    assert_eq!(fx.drivers.as_ref().unwrap()[1].param_id, radius, "and its LFO with it");

    // The format is closed inside an enum variant too.
    fn open_sky(value: &mut serde_json::Value) -> bool {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::Object(sky)) = map.get_mut("sky") {
                    sky.insert("bogus".into(), serde_json::json!(1));
                    return true;
                }
                map.values_mut().any(open_sky)
            }
            serde_json::Value::Array(items) => items.iter_mut().any(open_sky),
            _ => false,
        }
    }
    let mut json = serde_json::to_value(&doc).unwrap();
    assert!(open_sky(&mut json), "the base scene's environment has a Sky source");
    assert!(serde_json::from_value::<SceneDoc>(json).is_err(), "an unknown key inside a variant is refused");
}

thread_local! {
    static POSE: Cell<Option<Transform>> = const { Cell::new(None) };
}

struct PoseProbe(EffectNodeType);

impl EffectNode for PoseProbe {
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn runs_with_pending_inputs(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 1] =
            [NodePort { name: Cow::Borrowed("pose"), ty: PortType::Transform, kind: PortKind::Input, required: true }];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        POSE.set(ctx.inputs.transform("pose"));
    }
}

/// The compiled scene with the cube's body joining the world: the part a
/// later lane emits, stood in for by the rigid test body fed from the cube's own
/// transform, and a probe reading its pose.
fn with_cube_body(mut def: EffectGraphDef) -> EffectGraphDef {
    let id = |def: &EffectGraphDef, node_id: &str| def.nodes.iter().find(|n| n.node_id.as_str() == node_id).unwrap().id;
    let (transform, world) = (id(&def, "cube:transform"), id(&def, "world:physics"));
    let next = def.nodes.iter().map(|n| n.id).max().unwrap() + 1;
    for (offset, node_id, type_id) in [(0, "cube:body", "test.physics_body"), (1, "probe", "test.scene_pose")] {
        let node: EffectGraphNode =
            serde_json::from_value(serde_json::json!({"id": next + offset, "nodeId": node_id, "typeId": type_id})).unwrap();
        def.nodes.push(node);
    }
    for (from_node, from_port, to_node, to_port) in
        [(transform, "transform", next, "transform"), (next, "body", world, "body_0"), (world, "pose_0", next + 1, "pose")]
    {
        def.wires.push(EffectGraphWire { from_node, from_port: from_port.into(), to_node, to_port: to_port.into() });
    }
    def
}

fn runtime(fx: &PresetInstance) -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_builtin();
    manifold_water_rigid::testkit::physics_fixtures::register(&mut registry);
    registry.register("test.scene_pose", || Box::new(PoseProbe(EffectNodeType::new("test.scene_pose"))));
    let def = with_cube_body(flat(fx));
    let mut runtime = PresetRuntime::from_def_for_render(def, &registry, Some(&fx.params), false).unwrap();
    runtime.apply_param_values(&fx.params);
    runtime
}

fn frame(runtime: &mut PresetRuntime, seconds: f64) -> Transform {
    POSE.set(None);
    runtime.execute_frame(FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds(1.0 / 30.0),
        frame_count: (seconds * 30.0) as i64,
    });
    POSE.get().expect("the world publishes the cube's pose")
}

/// Adding a light is a structural edit, so the scene recompiles and the
/// generator rebuilds. The world carries over: the falling cube keeps its
/// trajectory. A Speed typed or modulated past 4 lands on 4, so the world
/// keeps stepping instead of holding.
#[test]
fn adding_a_light_keeps_physics_running_and_speed_clamps() {
    let mut doc = SceneDoc::base();
    let ItemFields::Object(cube) = &mut doc.find_mut(&ItemId::new("cube")).unwrap().fields else { panic!("cube") };
    cube.physics = PhysicsSetting::Body;
    let layer = scene_layer(doc.clone());
    let fx = layer.gen_params().unwrap();
    let mut reference = runtime(fx);
    let mut edited = runtime(fx);
    for time in [0.0, 0.033, 0.12, 0.24] {
        assert_eq!(frame(&mut reference, time), frame(&mut edited, time));
    }
    let paused = frame(&mut edited, 0.24);
    assert!(paused.pos[1] < 0.0, "the cube must be falling");

    doc.insert(None, 0, Item::new(ItemId::new("lamp"), "Lamp", ItemFields::default_for(ItemKind::Light))).unwrap();
    let lit = scene_layer(doc);
    let lit = lit.gen_params().unwrap();
    assert_ne!(lit.graph, fx.graph, "the light is in the graph");
    let mut rebuilt = runtime(lit);
    rebuilt.carry_generator_state_from(&mut edited);
    assert_eq!(frame(&mut rebuilt, 0.24), paused, "adding a light reset or stepped the cube");
    for time in [0.273, 0.35] {
        assert_eq!(frame(&mut reference, time), frame(&mut rebuilt, time), "trajectory changed at {time}");
    }

    let mut fast = lit.clone();
    let speed = fast.params.get_mut("world.speed").expect("the world has a Speed");
    assert_eq!(speed.spec.clamp_input(9.0), 4.0, "a typed Speed stops at 4");
    assert_eq!(speed.spec.clamp_input(-1.0), 0.0, "and at 0");
    speed.value = speed.driven_value(9.0);
    assert_eq!(speed.value, 4.0, "a modulated Speed stops at 4");
    rebuilt.apply_param_values(&fast.params);
    let before = frame(&mut rebuilt, 0.4);
    let after = frame(&mut rebuilt, 0.5);
    assert!(after.pos[1] < before.pos[1], "the world froze at Speed 4: {before:?} then {after:?}");
}

thread_local! {
    static SHOWN: Cell<Option<Camera>> = const { Cell::new(None) };
}

/// Reads the camera the scene's camera switch hands the render.
struct CameraProbe(EffectNodeType);

impl EffectNode for CameraProbe {
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 1] =
            [NodePort { name: Cow::Borrowed("camera"), ty: PortType::Camera, kind: PortKind::Input, required: true }];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        SHOWN.set(ctx.inputs.camera("camera"));
    }
}

/// The camera on air at `seconds`.
fn shown(runtime: &mut PresetRuntime, seconds: f64) -> Camera {
    SHOWN.set(None);
    runtime.execute_frame(FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds(1.0 / 30.0),
        frame_count: (seconds * 30.0) as i64,
    });
    SHOWN.get().expect("the camera switch hands the render a camera")
}

/// The layer's graph running with a probe on the camera switch's output.
fn cut_runtime(fx: &PresetInstance) -> PresetRuntime {
    let mut def = flat(fx);
    let switch = def.nodes.iter().find(|n| n.node_id.as_str() == "world:camera_switch").expect("the cut").id;
    let probe = def.nodes.iter().map(|n| n.id).max().unwrap() + 1;
    def.nodes.push(serde_json::from_value(serde_json::json!({"id": probe, "nodeId": "probe", "typeId": "test.scene_camera"})).unwrap());
    def.wires.push(EffectGraphWire { from_node: switch, from_port: "out".into(), to_node: probe, to_port: "camera".into() });
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("test.scene_camera", || Box::new(CameraProbe(EffectNodeType::new("test.scene_camera"))));
    let mut runtime = PresetRuntime::from_def_for_render(def, &registry, Some(&fx.params), false).unwrap();
    runtime.apply_param_values(&fx.params);
    runtime
}

/// Promise 9 (SCENE_DOCUMENT_DESIGN.md section 10 (Testing)): a knob, MIDI or
/// OSC change is a param write that reaches the item on the next frame, and a
/// camera cut lands on its frame, both on the running graph with no rebuild.
#[test]
fn a_param_write_reaches_the_next_frame_and_a_cut_lands_on_its_frame() {
    let mut doc = SceneDoc::base();
    doc.insert(None, 1, Item::new(ItemId::new("closeup"), "Closeup", ItemFields::default_for(ItemKind::Camera))).unwrap();
    let mut layer = scene_layer(doc);
    let fx = layer.gen_params_mut().unwrap();
    let graph = fx.graph.clone();
    let mut runtime = cut_runtime(fx);
    let frame = 1.0 / 30.0;
    let wide = shown(&mut runtime, 0.0);

    let write = |fx: &mut PresetInstance, runtime: &mut PresetRuntime, id: &str, value: f32| {
        assert!(fx.set_base_param(id, value), "the scene has {id}");
        runtime.apply_param_values(&fx.params);
    };
    let distance = |camera: Camera, look_y: f32| {
        let [x, y, z] = camera.pos;
        (x * x + (y - look_y) * (y - look_y) + z * z).sqrt()
    };
    let look_y = fx.get_base_param("closeup.look_y");
    write(fx, &mut runtime, "closeup.distance", 3.0);
    assert_eq!(shown(&mut runtime, frame), wide, "a camera off air moved the picture");

    write(fx, &mut runtime, "world.camera", 1.0);
    let cut = shown(&mut runtime, 2.0 * frame);
    assert!((distance(cut, look_y) - 3.0).abs() < 1e-4, "the cut did not land on its frame: {cut:?}");
    let cut_image = runtime.graph.instance_by_node_id(&manifold_core::NodeId::new("world:camera_cut")).unwrap();
    let selector = runtime.graph.get_node(cut_image).unwrap().params.get("selector").cloned();
    assert_eq!(selector, Some(ParamValue::Float(1.0)), "the closeup's depth of field and motion blur follow the cut");

    write(fx, &mut runtime, "closeup.distance", 5.0);
    let moved = shown(&mut runtime, 3.0 * frame);
    assert!((distance(moved, look_y) - 5.0).abs() < 1e-4, "the knob did not reach the next frame: {moved:?}");
    assert_eq!(fx.graph, graph, "a param write rebuilt the scene");
}

/// Every param of every node the compiler emits is in the scene table, for
/// the base scene plus one item of every Add menu entry. A new node, or a new
/// param on an existing node, with no row fails here instead of reaching the
/// screen unplaced. A row that does not apply to an item's fields (a cone
/// angle on a Sun) still counts: it names the param and why it is left alone.
mod every_param_has_a_row {
    use manifold_core::effect_graph_def::EffectGraphNode;
    use manifold_core::scene_doc::{Item, ItemId, ItemKind, SceneDoc};
    use manifold_core::scene_schema::{self, ADD_ENTRIES, MATERIAL_FAMILY, Reach, SCENE_LEVEL_ROWS};
    use manifold_nodes_scene::node_graph::node_param_source::declared_params_for_node;
    use manifold_nodes_scene::node_graph::scene_compile::compile;

    pub(super) fn doc_with_every_kind() -> SceneDoc {
        let mut doc = SceneDoc::base();
        let mut water = None;
        for (k, entry) in ADD_ENTRIES.iter().enumerate() {
            // The model file entry imports a file; the Object it makes is below.
            let Some(fields) = entry.fields else { continue };
            let item = Item::new(ItemId::new(format!("added{k}")), entry.name, fields());
            if entry.kind == ItemKind::Water {
                water = Some(item.id.clone());
            }
            // A Water Source sits in the Water added before it.
            let parent = (entry.kind == ItemKind::WaterSource).then(|| water.clone().expect("Water comes before its sources"));
            let at = parent.as_ref().and_then(|id| doc.find(id)).map_or(doc.items.len(), |p| p.children.len());
            doc.insert(parent.as_ref(), at, item).unwrap();
        }
        // Each material type compiles its own node, so each needs its rows.
        for (name, material_type) in [("unlit", manifold_core::scene_doc::MaterialType::Unlit), ("cel", manifold_core::scene_doc::MaterialType::Cel)] {
            let fields = manifold_core::scene_doc::MaterialFields { material_type, ..Default::default() };
            let at = doc.items.len();
            doc.insert(None, at, Item::new(ItemId::new(name), name, manifold_core::scene_doc::ItemFields::Material(fields))).unwrap();
        }
        let mesh = manifold_core::scene_doc::Shape::Mesh {
            file: String::new(),
            node_path: String::new(),
            vertex_count: 0,
            primitives: Default::default(),
        };
        let fields = manifold_core::scene_doc::ObjectFields { shape: mesh, ..Default::default() };
        let at = doc.items.len();
        doc.insert(None, at, Item::new(ItemId::new("mesh"), "Mesh", manifold_core::scene_doc::ItemFields::Object(fields))).unwrap();
        // The Ocean surface's chain, and the Environment under a Sea Horizon.
        let fields = manifold_core::scene_doc::ObjectFields { shape: manifold_core::scene_doc::Shape::Ocean, ..Default::default() };
        let at = doc.items.len();
        doc.insert(None, at, Item::new(ItemId::new("ocean"), "Ocean", manifold_core::scene_doc::ItemFields::Object(fields))).unwrap();
        for item in doc.items.iter_mut() {
            if let manifold_core::scene_doc::ItemFields::Light(light) = &mut item.fields
                && let manifold_core::scene_doc::LightType::Environment { source } = &mut light.light
            {
                *source = manifold_core::scene_doc::EnvironmentSource::SeaHorizon { file: "sky.exr".into() };
            }
        }
        // Every curve shape with every curve.
        use manifold_core::scene_doc::{Curve, Shape};
        let curves = [Curve::Circle, Curve::Polygon, Curve::Star, Curve::Spiral, Curve::Helix, Curve::Wave, Curve::Lissajous];
        for (k, curve) in curves.into_iter().enumerate() {
            for (s, shape) in [Shape::Extrude { curve, caps: true }, Shape::Revolve { curve }, Shape::Tube { curve, caps: true }].into_iter().enumerate() {
                let fields = manifold_core::scene_doc::ObjectFields { shape, ..Default::default() };
                let at = doc.items.len();
                let id = ItemId::new(format!("curve{k}_{s}"));
                doc.insert(None, at, Item::new(id, "Curve", manifold_core::scene_doc::ItemFields::Object(fields))).unwrap();
            }
        }
        doc
    }

    fn flatten<'a>(nodes: &'a [EffectGraphNode], into: &mut Vec<&'a EffectGraphNode>) {
        for node in nodes {
            into.push(node);
            if let Some(body) = node.group.as_deref() {
                flatten(&body.nodes, into);
            }
        }
    }

    /// Whether some row accounts for `param` on the node `node_id` (`<owner>:<role>`).
    fn has_row(doc: &SceneDoc, node_id: &str, param: &str) -> bool {
        let Some((owner, role)) = node_id.split_once(':') else {
            return false;
        };
        let mut found = false;
        doc.walk(|item, _| {
            for row in scene_schema::declared_rows(item.kind()) {
                found |= row.param == param
                    && match row.reach {
                        Reach::Own(r) => item.id.as_str() == owner && r == role,
                        Reach::Scene(r) => owner == "scene" && r == role,
                        Reach::OwnAndScene(own, scene) => {
                            (item.id.as_str() == owner && own == role) || (owner == "scene" && scene == role)
                        }
                        Reach::EveryCameraLens => role == "lens",
                        Reach::Clocks => role == "pose",
                        // A scene modifier's params live on its instance, not a compiled node.
                        Reach::Modifier => false,
                    };
            }
        });
        let family = doc.find(&ItemId::new(owner)).is_some_and(|item| item.kind() == ItemKind::Material)
            && role == MATERIAL_FAMILY.node;
        // The water template's params are a family too (`scene_schema::water`).
        let water = doc.find(&ItemId::new(owner)).is_some_and(|item| match &item.fields {
            manifold_core::scene_doc::ItemFields::Water(fields) => {
                scene_schema::water_family(fields.solver, fields.draw).is_some_and(|template| template.holds(role))
            }
            _ => false,
        });
        let plain_material = owner == "scene" && role == "default_material";
        // A modifier frame's rest centre is placed by the compiler, not a row.
        let frame_centre = role.starts_with("centre:")
            && doc.find(&ItemId::new(owner)).is_some_and(|item| item.kind() == ItemKind::Modifier);
        let scene_level = SCENE_LEVEL_ROWS.iter().any(|row| matches!(row.reach, Reach::Scene(r) if r == role) && row.param == param);
        found || family || water || plain_material || frame_centre || (owner == "scene" && scene_level)
    }

    #[test]
    fn every_param_of_every_compiled_node_has_a_table_row() {
        let doc = doc_with_every_kind();
        let graph = compile(&doc).unwrap();
        let mut nodes = Vec::new();
        flatten(&graph.nodes, &mut nodes);
        let mut gaps = Vec::new();
        for node in nodes {
            // Groups and their ports declare no params of their own.
            let Some(params) = declared_params_for_node(node) else {
                continue;
            };
            for meta in params {
                if !has_row(&doc, node.node_id.as_str(), &meta.name) {
                    gaps.push(format!("{} ({}).{}", node.node_id, node.type_id, meta.name));
                }
            }
        }
        assert!(gaps.is_empty(), "params with no table row:\n{}", gaps.join("\n"));
    }
}

/// Promise 12 (SCENE_DOCUMENT_DESIGN.md section 10 (Testing)): no kind and no
/// field choice the panel offers crashes, hangs or loses the user's work.
/// Every Add menu entry, and every pick of every field row of the item it adds,
/// is applied to the base scene as the panel applies it. Each case compiles and
/// installs, ticks 120 frames, saves and loads back as the same scene, then
/// undoes to the base exactly. A choice the panel offers enabled that is then
/// refused, by the row's write or at compile or install, fails here: to the
/// user it is a dead button. Only a choice drawn disabled with its reason is
/// exempt. Entries and choices come from the tables, so a new kind or choice
/// is covered without editing this test. A hang is the nextest
/// `terminate-after` on this test (`.config/nextest.toml`).
mod every_kind_and_choice_survives {
    use super::*;
    use manifold_core::scene_schema::{
        ADD_ENTRIES, FieldControl, FieldValue, add_refused, applicable_fields, choice_disabled, set_field, unique_name,
    };
    use manifold_editing::command::{Command, CompositeCommand};
    use manifold_editing::commands::scene_doc::{
        AddSceneItemCommand, SetSceneItemFieldsCommand, add_modifier, add_object_commands, new_material, new_modifier,
    };

    struct Case {
        label: String,
        /// The panel's commands, or why the row's write refused an enabled pick.
        commands: Result<Vec<Box<dyn Command>>, String>,
    }

    fn doc_of(project: &Project) -> SceneDoc {
        project.timeline.layers[0].gen_params().unwrap().scene.clone().unwrap()
    }

    /// The commands that add `entry` as the panel does. A Water Source needs the
    /// Water it sits in first.
    fn add_commands(project: &Project, key: &str) -> Option<(Vec<Box<dyn Command>>, ItemId)> {
        let layer = project.timeline.layers[0].layer_id.clone();
        let doc = doc_of(project);
        let entry = manifold_core::scene_schema::add_entry(key)?;
        let fields = entry.fields?;
        if add_refused(&doc, &fields()).is_some() {
            return None;
        }
        let names: Vec<&str> = doc.ids().iter().filter_map(|id| doc.find(id)).map(|item| item.name.as_str()).collect();
        let name = unique_name(entry.name, &names);
        let id = ItemId::new(format!("case-{key}"));
        let mut commands: Vec<Box<dyn Command>> = Vec::new();
        let mut parent = None;
        if entry.kind == ItemKind::WaterSource {
            let water = ItemId::new("case-water");
            commands.push(Box::new(AddSceneItemCommand::new(layer.clone(), ItemKind::Water, None, doc.items.len(), None, water.clone())));
            parent = Some(water);
        }
        let index = if parent.is_some() { 0 } else { doc.items.len() };
        if let ItemFields::Modifier(card) = fields() {
            let mut item = new_modifier(&doc, id.clone(), card.modifier, &[]);
            item.name = name;
            commands.push(Box::new(add_modifier(&layer, &doc, item)));
        } else if entry.kind == ItemKind::Object {
            let material = new_material(ItemId::new(format!("case-{key}-material")), "Material");
            commands.extend(add_object_commands(&layer, None, index, Item::new(id.clone(), name, fields()), material));
        } else {
            commands.push(Box::new(AddSceneItemCommand::new(layer.clone(), entry.kind, parent, index, None, id.clone())));
            commands.push(Box::new(SetSceneItemFieldsCommand::new(layer, id.clone(), fields())));
        }
        Some((commands, id))
    }

    /// Every value a pick of `row` can send for `item`, as the panel lists
    /// them: an item never offers itself.
    fn picks(doc: &SceneDoc, item: &Item, control: &FieldControl) -> Vec<FieldValue> {
        let candidates = |passes: fn(&Item) -> bool| {
            let mut found = Vec::new();
            doc.walk(|other, _| {
                if other.id != item.id && passes(other) {
                    found.push(other.id.clone());
                }
            });
            found
        };
        match control {
            FieldControl::Closed(choices) | FieldControl::ClosedOrLayer(choices) => {
                choices.iter().map(|c| FieldValue::Choice(c.key.to_owned())).collect()
            }
            FieldControl::ClosedOrItem { closed, items } => closed
                .iter()
                .map(|c| FieldValue::Choice(c.key.to_owned()))
                .chain(candidates(*items).into_iter().map(|id| FieldValue::Item(Some(id))))
                .collect(),
            FieldControl::Item { items, .. } => {
                std::iter::once(FieldValue::Item(None)).chain(candidates(*items).into_iter().map(|id| FieldValue::Item(Some(id)))).collect()
            }
            FieldControl::ItemSet { items, .. } => {
                std::iter::once(FieldValue::Everything).chain(candidates(*items).into_iter().map(|id| FieldValue::Items(vec![id]))).collect()
            }
            FieldControl::File { .. } | FieldControl::Listed { .. } => Vec::new(),
        }
    }

    /// The base scene on a layer with a fixed id, so commands built against
    /// one base project apply to another.
    fn base_project() -> Project {
        let mut project = Project::default();
        let mut layer = scene_layer(SceneDoc::base());
        layer.layer_id = manifold_core::LayerId::new("scene");
        project.timeline.layers.push(layer);
        project
    }

    /// Run the panel's commands in order. A command the editor refuses is a
    /// failure: the user clicked an enabled choice and nothing happened.
    fn apply(project: &mut Project, commands: &mut [Box<dyn Command>]) -> Result<(), String> {
        for command in commands.iter_mut() {
            command.execute(project);
            if !command.was_applied() {
                let reason = command.rejection_reason().unwrap_or("no reason given");
                return Err(format!("{} was refused: {reason}", command.description()));
            }
        }
        Ok(())
    }

    type Snapshot = (Option<SceneDoc>, Option<manifold_node_engine::persistence::GraphDocument>, Vec<(String, f32, f32)>);

    /// Everything the undo has to bring back.
    fn snapshot(project: &Project) -> Snapshot {
        let fx = project.timeline.layers[0].gen_params().unwrap();
        (fx.scene.clone(), fx.graph.clone(), values(fx))
    }

    fn cases() -> Vec<Case> {
        let base = base_project();
        let mut cases = Vec::new();
        for entry in ADD_ENTRIES.iter() {
            let Some((commands, id)) = add_commands(&base, entry.key) else { continue };
            cases.push(Case { label: entry.key.to_owned(), commands: Ok(commands) });
            // The picks of this item's field rows, on top of the add. A
            // refused add is already a failing case above.
            let mut with_item = base_project();
            let (mut setup, _) = add_commands(&with_item, entry.key).expect("added above");
            if apply(&mut with_item, &mut setup).is_err() {
                continue;
            }
            let doc = doc_of(&with_item);
            let Some(item) = doc.find(&id) else { continue };
            for row in applicable_fields(&item.fields) {
                if (row.disabled)(&doc, item).is_some() {
                    continue;
                }
                for value in picks(&doc, item, &row.control) {
                    if let FieldValue::Choice(key) = &value
                        && choice_disabled(&doc, item, row.id, key).is_some()
                    {
                        continue;
                    }
                    let commands = set_field(&doc, item, row, &value).map(|fields| {
                        let (mut commands, _) = add_commands(&base, entry.key).expect("added above");
                        let layer = base.timeline.layers[0].layer_id.clone();
                        commands.push(Box::new(SetSceneItemFieldsCommand::new(layer, id.clone(), fields)) as Box<dyn Command>);
                        commands
                    });
                    cases.push(Case { label: format!("{} / {} = {value:?}", entry.key, row.id), commands });
                }
            }
        }
        cases
    }

    /// What a CPU test cannot run: a GPU node asks for the encoder.
    const NEEDS_GPU: &str = "EffectNodeContext::gpu_encoder called without a GpuEncoder bound";

    /// Build the runtime from the layer graph as the app does, then tick 120
    /// frames. A scene with a GPU node (Water) builds but stops ticking at
    /// its first frame: this test has no device.
    fn tick_120_frames(fx: &PresetInstance) {
        let registry = PrimitiveRegistry::with_builtin();
        let def = fx.graph.clone().expect("a scene layer has a graph");
        let mut runtime =
            PresetRuntime::from_def_for_render(def, &registry, Some(&fx.params), false).expect("the compiled scene builds");
        runtime.apply_param_values(&fx.params);
        let ticked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for k in 0..120 {
                let seconds = k as f64 / 30.0;
                runtime.execute_frame(FrameTime { seconds: Seconds(seconds), beats: Beats(seconds * 2.0), delta: Seconds(1.0 / 30.0), frame_count: k });
            }
        }));
        if let Err(panic) = ticked
            && panic.downcast_ref::<&str>().is_none_or(|message| *message != NEEDS_GPU)
            && panic.downcast_ref::<String>().is_none_or(|message| message != NEEDS_GPU)
        {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn every_add_entry_and_field_choice() {
        let mut failures = Vec::new();
        let all = cases();
        assert!(all.len() > ADD_ENTRIES.len(), "the table gave {} cases", all.len());
        for Case { label, commands } in all {
            let mut commands = match commands {
                Ok(commands) => commands,
                Err(reason) => {
                    failures.push(format!("{label}: the row refused an enabled pick: {reason}"));
                    continue;
                }
            };
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut project = base_project();
                let base = snapshot(&project);
                if let Err(refused) = apply(&mut project, &mut commands) {
                    panic!("{refused}");
                }
                let fx = project.timeline.layers[0].gen_params().unwrap();
                tick_120_frames(fx);

                let saved = serde_json::to_string(&project).unwrap();
                let mut loaded: Project = serde_json::from_str(&saved).unwrap();
                loaded.on_after_deserialize();
                loaded.reconcile_param_manifests();
                let after = snapshot(&project);
                let reloaded = snapshot(&loaded);
                assert_eq!(reloaded.0, after.0, "the loaded document differs from the saved one");
                assert_eq!(reloaded.1, after.1, "the loaded graph differs from the saved one");
                assert_eq!(reloaded.2, after.2, "the loaded values differ from the saved ones");

                let mut all = CompositeCommand::new(std::mem::take(&mut commands), label.clone());
                all.undo(&mut project);
                assert!(snapshot(&project) == base, "undo did not return the base scene exactly");
            }));
            if let Err(panic) = outcome {
                let reason = panic.downcast_ref::<String>().cloned().or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()));
                failures.push(format!("{label}: {}", reason.unwrap_or_else(|| "panicked".into())));
            }
        }
        assert!(failures.is_empty(), "{} cases failed:\n{}", failures.len(), failures.join("\n"));
    }
}

/// Group, Ungroup and a move out of a turned, scaled Group keep every item
/// where it is on screen (SCENE_DOCUMENT_DESIGN.md section 7.2), and undo
/// puts the item's rows back.
#[test]
fn moving_an_item_between_groups_keeps_it_in_place() {
    use manifold_core::scene_doc::param_id;
    use manifold_core::scene_doc::transform::{Mat4, frame_of, mat4_mul, trs_matrix};
    use manifold_editing::command::CompositeCommand;
    use manifold_editing::commands::scene_doc::{ReparentSceneItemCommand, group_commands, ungroup_commands};
    use manifold_editing::undo::UndoRedoManager;

    let (cube, rig, outer) = (ItemId::new("cube"), ItemId::new("rig"), ItemId::new("outer"));
    let mut doc = SceneDoc::base();
    doc.insert(None, 0, Item::new(outer.clone(), "Outer", ItemFields::default_for(ItemKind::Group))).unwrap();
    doc.insert(Some(&outer), 0, Item::new(rig.clone(), "Rig", ItemFields::default_for(ItemKind::Group))).unwrap();
    doc.reparent(&cube, Some(&rig), 0).unwrap();
    let mut layer = scene_layer(doc);
    layer.layer_id = manifold_core::LayerId::new("scene");
    let layer_id = layer.layer_id.clone();
    {
        let fx = layer.gen_params_mut().unwrap();
        for (item, values) in [
            (&outer, [3.0, -1.0, 2.0, 0.2, 0.0, -0.4, 2.0, 2.0, 2.0]),
            (&rig, [1.0, 2.0, -3.0, 0.3, -0.7, 1.1, 0.5, 0.5, 0.5]),
            (&cube, [0.5, -1.0, 0.25, -0.2, 0.4, 0.9, 1.5, 1.5, 1.5]),
        ] {
            for (name, value) in ["pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z"].into_iter().zip(values) {
                assert!(fx.set_base_param(&param_id(item, name), value), "{item} has {name}");
            }
        }
    }
    let mut project = Project::default();
    project.timeline.layers.push(layer);
    let world = |project: &Project| -> Mat4 {
        let fx = project.timeline.layers[0].gen_params().unwrap();
        let doc = fx.scene.as_ref().unwrap();
        let values = |id: &str| fx.params.contains(id).then(|| fx.get_base_param(id));
        let read = |names: [&str; 3]| names.map(|name| fx.get_base_param(&param_id(&cube, name)));
        let parent = doc.locate(&cube).unwrap().0;
        mat4_mul(
            frame_of(doc, parent.as_ref(), &values),
            trs_matrix(read(["pos_x", "pos_y", "pos_z"]), read(["rot_x", "rot_y", "rot_z"]), read(["scale_x", "scale_y", "scale_z"])),
        )
    };
    let same = |a: Mat4, b: Mat4| a.iter().flatten().zip(b.iter().flatten()).all(|(x, y)| (x - y).abs() < 1e-3);
    let start = world(&project);
    let doc_now = |project: &Project| project.timeline.layers[0].gen_params().unwrap().scene.clone().unwrap();
    let mut undo = UndoRedoManager::new();

    let out = ReparentSceneItemCommand::new(layer_id.clone(), cube.clone(), None, 0);
    assert!(undo.execute(Box::new(out), &mut project));
    assert!(same(world(&project), start), "a move to the root keeps the cube in place");
    assert!(undo.undo(&mut project));
    assert!(same(world(&project), start) && doc_now(&project).locate(&cube).unwrap().0 == Some(rig.clone()), "undo puts it back");

    let ungroup = CompositeCommand::new(ungroup_commands(&layer_id, &doc_now(&project), &rig), "Ungroup".into());
    assert!(undo.execute(Box::new(ungroup), &mut project));
    assert!(same(world(&project), start), "ungrouping keeps the cube in place");

    let group = CompositeCommand::new(group_commands(&layer_id, &doc_now(&project), std::slice::from_ref(&cube), ItemId::new("new")), "Group".into());
    assert!(undo.execute(Box::new(group), &mut project));
    assert!(same(world(&project), start), "grouping keeps the cube in place");
}

/// A copied Object draws at its prototype's per-axis scale: the copies
/// body's `copy_scale` goes into the draw, applied inside each copy (Group
/// space), so a 10 x 0.16 x 10 slab draws as one.
#[test]
fn a_copied_object_draws_its_per_axis_scale_inside_each_copy() {
    use manifold_core::effect_graph_def::SerializedParamValue;
    let mut doc = SceneDoc::base();
    if let Some(Item { fields: ItemFields::Object(object), .. }) = doc.find_mut(&ItemId::new("cube")) {
        object.copies = true;
    }
    let def = flat(scene_layer(doc).gen_params().unwrap());
    let object = def.nodes.iter().find(|n| n.node_id.as_str().ends_with("cube:object")).expect("the cube draws");
    let wire = def.wires.iter().find(|w| w.to_node == object.id && w.to_port == "copy_scale").expect("the draw takes the copies' scale");
    let body = def.nodes.iter().find(|n| n.id == wire.from_node).unwrap();
    assert_eq!((body.type_id.as_str(), wire.from_port.as_str()), ("node.rigid_body", "copy_scale"));
    assert_eq!(object.params.get("instances_space"), Some(&SerializedParamValue::Enum { value: 1 }), "inside each copy");
}

/// Math View sizes each item's marks from that item's own rest size, never
/// from the scene's: a plane beside a cube gets the plane's radius. A copied
/// item's diagram reads its copies' per-axis scale, as the draw does.
#[test]
fn math_view_marks_follow_each_item_and_its_copies_scale() {
    use manifold_core::effect_graph_def::SerializedParamValue;
    use manifold_core::scene_doc::{ModifierFields, ModifierTargets, ModifierType, Shape};
    use manifold_node_engine::load::expand::prepare_scene_modifiers;
    let mut doc = SceneDoc::base();
    if let Some(Item { fields: ItemFields::Object(object), .. }) = doc.find_mut(&ItemId::new("cube")) {
        object.copies = true;
    }
    let mut plane = ItemFields::default_for(ItemKind::Object);
    if let ItemFields::Object(object) = &mut plane {
        object.shape = Shape::Plane;
    }
    let mut card = ItemFields::default_for(ItemKind::Modifier);
    if let ItemFields::Modifier(fields) = &mut card {
        *fields = ModifierFields { modifier: ModifierType::MathView, on: true, targets: ModifierTargets::Scene, ..fields.clone() };
    }
    for (id, fields) in [("plane", plane), ("card", card)] {
        let end = doc.items.len();
        doc.insert(None, end, Item::new(ItemId::new(id), id, fields)).unwrap();
    }
    let def = scene_layer(doc).gen_params().unwrap().graph.clone().unwrap();
    let prepared = prepare_scene_modifiers(&def, &PrimitiveRegistry::with_builtin()).unwrap().def;
    let feed = |to: u32, port: &str| prepared.wires.iter().find(|w| w.to_node == to && w.to_port == port).map(|w| (w.from_node, w.from_port.clone()));
    let object = |item: &str| {
        let id = format!("{item}:object");
        prepared.nodes.iter().find(|n| n.type_id == "node.scene_object" && n.node_id.as_str() == id).unwrap_or_else(|| panic!("{item} draws")).id
    };
    // A diagram samples its item's own mesh.
    let diagram = |item: &str| {
        let mesh = feed(object(item), "vertices");
        prepared
            .nodes
            .iter()
            .find(|n| n.type_id == "node.render_mesh_diagram" && feed(n.id, "current_mesh") == mesh)
            .unwrap_or_else(|| panic!("{item} has a diagram"))
    };
    let radius = |item: &str| match diagram(item).params.get("radius") {
        Some(SerializedParamValue::Float { value }) => *value,
        other => panic!("{item}'s diagram radius: {other:?}"),
    };
    let (cube, plane) = (radius("cube"), radius("plane"));
    assert!((cube - 0.75f32.sqrt()).abs() < 1e-4, "a unit cube's own radius, got {cube}");
    assert!((plane - 0.5f32.sqrt()).abs() < 1e-4, "a unit plane's own radius, not the scene's, got {plane}");
    assert!(feed(object("cube"), "copy_scale").is_some(), "the cube's copies carry a scale");
    assert_eq!(feed(diagram("cube").id, "copy_scale"), feed(object("cube"), "copy_scale"), "the diagram reads the copies' scale");
    assert_eq!(feed(diagram("plane").id, "copy_scale"), None, "an item without copies has none");
}

/// A swarm drawing a group with Copies on nests the layout at its live
/// particles only: Count drives how many carry it, not the seed's capacity.
#[test]
fn a_swarm_nests_a_copied_group_at_its_live_count() {
    use manifold_core::effect_graph_def::BindingTarget;
    use manifold_core::scene_doc::{DrawAs, param_id};
    let (rack, swarm) = (ItemId::new("rack"), ItemId::new("swarm"));
    let mut doc = SceneDoc::base();
    let mut group = ItemFields::default_for(ItemKind::Group);
    if let ItemFields::Group(fields) = &mut group {
        fields.copies = true;
    }
    let mut fields = ItemFields::default_for(ItemKind::Swarm);
    if let ItemFields::Swarm(s) = &mut fields {
        s.draw_as = DrawAs::Item(rack.clone());
    }
    let end = doc.items.len();
    doc.insert(None, end, Item::new(rack.clone(), "Rack", group)).unwrap();
    doc.reparent(&ItemId::new("cube"), Some(&rack), 0).unwrap();
    doc.insert(None, end + 1, Item::new(swarm.clone(), "Swarm", fields)).unwrap();
    let layer = scene_layer(doc);
    let fx = layer.gen_params().unwrap();
    let def = flat(fx);
    let nest = def.nodes.iter().find(|n| n.type_id == "node.nest_copies").expect("the drawn group's layout is nested");
    let bindings = &fx.graph.as_ref().unwrap().preset_metadata.as_ref().unwrap().bindings;
    let count = param_id(&swarm, "count");
    assert!(
        bindings.iter().any(|binding| binding.id == count
            && matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id.as_str().ends_with(nest.node_id.as_str()) && param == "outer_count")),
        "Count drives how many particles carry the layout"
    );
}

/// Shatter breaks a built-in shape, not only an imported mesh: the load
/// pass cuts each piece from the shape the body collided with
/// (node.mesh_fragment), draws it and gives its body the same piece.
#[test]
fn shatter_breaks_a_built_in_cube() {
    use manifold_core::scene_doc::{ModifierFields, ModifierTargets, ModifierType};
    use manifold_node_engine::load::expand::prepare_scene_modifiers;
    use manifold_node_engine::persistence::EffectGraphDefExt;
    let cube = ItemId::new("cube");
    let mut doc = SceneDoc::base();
    if let Some(Item { fields: ItemFields::Object(object), .. }) = doc.find_mut(&cube) {
        object.physics = PhysicsSetting::Body;
    }
    let mut card = ItemFields::default_for(ItemKind::Modifier);
    if let ItemFields::Modifier(fields) = &mut card {
        *fields = ModifierFields { modifier: ModifierType::Shatter, on: true, targets: ModifierTargets::Items(vec![cube.clone()]), pieces: 24, ..fields.clone() };
    }
    let end = doc.items.len();
    doc.insert(None, end, Item::new(ItemId::new("break"), "Break", card)).unwrap();
    let layer = scene_layer(doc);
    // The load pass runs on the sealed layer graph, as the runtime loads it.
    let def = layer.gen_params().unwrap().graph.clone().unwrap();
    let mut registry = PrimitiveRegistry::with_builtin();
    manifold_water_rigid::testkit::physics_fixtures::register(&mut registry);
    let prepared = prepare_scene_modifiers(&def, &registry).unwrap();
    let cuts: Vec<_> = prepared.def.nodes.iter().filter(|n| n.type_id == "node.mesh_fragment").collect();
    assert_eq!(cuts.len(), 24, "one cut per piece");
    for cut in &cuts {
        let feeds = |port: &str| prepared.def.wires.iter().filter(|w| w.from_node == cut.id && w.from_port == port).count();
        assert_eq!((feeds("vertices"), feeds("source")), (1, 1), "the piece draws it and its body collides with it");
    }
    prepared.def.clone().into_graph(&registry, &Default::default()).unwrap();
}

/// A project with one scene layer holding `doc`, its layer id fixed.
fn project_with(doc: SceneDoc) -> Project {
    let mut layer = scene_layer(doc);
    layer.layer_id = manifold_core::LayerId::new("scene");
    let mut project = Project::default();
    project.timeline.layers.push(layer);
    project
}

fn run(project: &mut Project, mut command: impl manifold_editing::command::Command) {
    command.execute(project);
    assert!(command.was_applied(), "{} was refused: {:?}", command.description(), command.rejection_reason());
}

/// A name and a place among siblings are display only: renaming an item of
/// every kind and reversing every sibling list (the modifier chain aside,
/// whose order is its meaning) compiles to the same graph and manifest, so
/// the structure version holds and nothing restarts (GPU water restarts on a
/// structure version change). A camera's place is not its cut number either.
#[test]
fn renaming_or_reordering_items_recompiles_nothing() {
    use manifold_editing::commands::scene_doc::{RenameSceneItemCommand, ReparentSceneItemCommand};
    let doc = every_param_has_a_row::doc_with_every_kind();
    let mut project = project_with(doc.clone());
    let layer = project.timeline.layers[0].layer_id.clone();
    let fx = project.timeline.layers[0].gen_params().unwrap();
    let graph = serde_json::to_string(&fx.graph).unwrap();
    let manifest = format!("{:?}", fx.params.iter().map(|p| &p.spec).collect::<Vec<_>>());
    let version = fx.graph_structure_version;

    // One item of every kind.
    let mut kinds = Vec::new();
    let mut picked = Vec::new();
    doc.walk(|item, _| {
        if !kinds.contains(&item.kind()) {
            kinds.push(item.kind());
            picked.push(item.id.clone());
        }
    });
    assert_eq!(kinds.len(), 11, "the scene holds every kind: {kinds:?}");
    for id in picked {
        run(&mut project, RenameSceneItemCommand::new(layer.clone(), id.clone(), format!("renamed {id}")));
    }
    // Reverse every sibling list: each item in turn to the front of its parent.
    let mut moves = Vec::new();
    let top = doc.items.iter().map(|item| (None, item));
    let inner = doc.ids().into_iter().filter_map(|id| doc.find(&id)).flat_map(|parent| {
        parent.children.iter().map(move |child| (Some(parent.id.clone()), child))
    });
    for (parent, item) in top.chain(inner.collect::<Vec<_>>()) {
        if item.kind() != ItemKind::Modifier {
            moves.push((item.id.clone(), parent));
        }
    }
    for (id, parent) in moves {
        run(&mut project, ReparentSceneItemCommand::new(layer.clone(), id, parent, 0));
    }

    let fx = project.timeline.layers[0].gen_params().unwrap();
    let edited = fx.scene.as_ref().unwrap();
    assert!(edited.find(&doc.cameras()[0].clone()).is_some_and(|item| item.name.starts_with("renamed")), "the renames landed");
    assert_ne!(edited.cameras(), doc.cameras(), "the cameras moved");
    assert_ne!(edited.ids(), doc.ids(), "the items moved");
    let chain = |doc: &SceneDoc| doc.items.iter().filter(|item| item.kind() == ItemKind::Modifier).map(|item| item.id.clone()).collect::<Vec<_>>();
    assert_eq!(chain(edited), chain(&doc), "the modifier chain kept its order");
    assert_eq!(serde_json::to_string(&fx.graph).unwrap(), graph, "a rename or a reorder changed the compiled graph");
    assert_eq!(format!("{:?}", fx.params.iter().map(|p| &p.spec).collect::<Vec<_>>()), manifest, "and the manifest");
    assert_eq!(fx.graph_structure_version, version, "and bumped the structure version, which restarts water");
}

/// A recorded cut names a camera by its cut number, not its place: after a
/// duplicate, an add and a reorder in front of it, the base value and the
/// automation point still cut to the same camera, on the running graph.
#[test]
fn a_recorded_cut_keeps_its_camera_through_duplicate_add_and_reorder() {
    use manifold_core::effects::{AutomationLane, AutomationPoint, SegmentShape};
    use manifold_editing::commands::scene_doc::{AddSceneItemCommand, ReparentSceneItemCommand};
    let mut doc = SceneDoc::base();
    doc.insert(None, 1, Item::new(ItemId::new("closeup"), "Closeup", ItemFields::default_for(ItemKind::Camera))).unwrap();
    let mut project = project_with(doc);
    let layer = project.timeline.layers[0].layer_id.clone();
    let fx = project.timeline.layers[0].gen_params_mut().unwrap();
    assert!(fx.set_base_param("closeup.distance", 3.0));
    assert!(fx.set_base_param("world.camera", 1.0), "cut to the closeup");
    let point = AutomationPoint { beat: Beats(4.0), value: 1.0, shape: SegmentShape::Hold };
    fx.automation_lanes = Some(vec![AutomationLane { param_id: "world.camera".into(), enabled: true, points: vec![point] }]);

    // Duplicate the first camera (the copy lands right after it), add one at
    // the top, and move the closeup to the end.
    let camera = ItemId::new("camera");
    run(&mut project, AddSceneItemCommand::new(layer.clone(), ItemKind::Camera, None, 1, Some(camera), ItemId::new("copy")));
    run(&mut project, AddSceneItemCommand::new(layer.clone(), ItemKind::Camera, None, 0, None, ItemId::new("added")));
    run(&mut project, ReparentSceneItemCommand::new(layer.clone(), ItemId::new("closeup"), None, usize::MAX));

    let fx = project.timeline.layers[0].gen_params().unwrap();
    let doc = fx.scene.as_ref().unwrap();
    let cuts: Vec<(u32, &str)> = doc.cuts().into_iter().map(|(cut, id)| (cut, id.as_str())).collect();
    assert_eq!(cuts, [(0, "camera"), (1, "closeup"), (2, "copy"), (3, "added")], "new cameras take new numbers");
    assert_eq!(fx.get_base_param("world.camera"), 1.0, "the base value was rewritten");
    assert_eq!(fx.automation_lanes.as_ref().unwrap()[0].points, [point], "the recorded point was rewritten");
    assert_eq!(doc.picked_camera(1.0).map(ItemId::as_str), Some("closeup"));

    let look_y = fx.get_base_param("closeup.look_y");
    let shown_camera = shown(&mut cut_runtime(fx), 0.0);
    let [x, y, z] = shown_camera.pos;
    let distance = (x * x + (y - look_y) * (y - look_y) + z * z).sqrt();
    assert!((distance - 3.0).abs() < 1e-4, "the cut no longer shows the closeup: {shown_camera:?}");

    // Deleting the cameras on the top numbers shrinks the row to the live
    // cameras, and a value recorded on a deleted number shows the first
    // camera, not the last.
    let fx = project.timeline.layers[0].gen_params_mut().unwrap();
    let mut doc = fx.scene.clone().unwrap();
    doc.remove(&ItemId::new("added")).unwrap();
    doc.remove(&ItemId::new("copy")).unwrap();
    fx.install_scene_document(doc).unwrap();
    let row = fx.params.get("world.camera").unwrap();
    assert_eq!(row.spec.max, 1.0, "the Camera row still spans the deleted cameras' numbers");
    assert_eq!(row.driven_value(3.0), 3.0, "a recorded deleted number is pulled onto the last camera");
    assert!(fx.set_base_param("world.camera", 0.0));
    let first = shown(&mut cut_runtime(fx), 0.0);
    assert!(fx.set_base_param("world.camera", 3.0));
    let deleted = shown(&mut cut_runtime(fx), 0.0);
    assert_eq!(deleted.pos, first.pos, "a deleted camera's number no longer shows the first camera");
}

/// Scene Loop tiles objects that draw many copies: a cube with Copies and a
/// swarm's particles get every live copy in every corridor cell
/// (node.nest_copies over the cells), after Scene Mirror shaped one tile, and
/// the draw reads the live product as its count.
#[test]
fn the_loop_tiles_copies_and_swarms_with_every_live_copy() {
    use manifold_core::scene_doc::{ModifierFields, ModifierTargets, ModifierType};
    use manifold_node_engine::load::expand::prepare_scene_modifiers;
    use manifold_node_engine::persistence::EffectGraphDefExt;
    let mut doc = SceneDoc::base();
    if let Some(Item { fields: ItemFields::Object(object), .. }) = doc.find_mut(&ItemId::new("cube")) {
        object.copies = true;
    }
    let end = doc.items.len();
    doc.insert(None, end, Item::new(ItemId::new("swarm"), "Swarm", ItemFields::default_for(ItemKind::Swarm))).unwrap();
    for (k, (id, modifier)) in [("loop", ModifierType::SceneLoop), ("mirror", ModifierType::SceneMirror)].into_iter().enumerate() {
        let mut card = ItemFields::default_for(ItemKind::Modifier);
        if let ItemFields::Modifier(fields) = &mut card {
            *fields = ModifierFields { modifier, on: true, targets: ModifierTargets::Scene, ..fields.clone() };
        }
        doc.insert(None, end + 1 + k, Item::new(ItemId::new(id), id, card)).unwrap();
    }
    // The default cell holds what is drawn: the swarm's 6 m spread, so its
    // tiles start apart (twice its depth, one of swarm and one of air).
    let spacing = |doc: SceneDoc| values(scene_layer(doc).gen_params().unwrap()).into_iter().find(|(id, ..)| id == "loop.cell_size").unwrap().2;
    assert!((spacing(doc.clone()) - 12.0).abs() < 1e-4, "the swarm's spread sizes the cell");
    let mut no_swarm = doc.clone();
    no_swarm.remove(&ItemId::new("swarm")).unwrap();
    // A Copies row at its defaults lies across the travel axis: one cube deep.
    assert!((spacing(no_swarm) - 2.0).abs() < 1e-4, "the copies' row sizes the cell");
    // A Water's particle draws (foam, spray, bubbles, Physics View dots)
    // have no live count and stay out of the corridor.
    let end = doc.items.len();
    doc.insert(None, end, Item::new(ItemId::new("water"), "Water", ItemFields::default_for(ItemKind::Water))).unwrap();
    let layer = scene_layer(doc);
    let def = layer.gen_params().unwrap().graph.clone().unwrap();
    let mut registry = PrimitiveRegistry::with_builtin();
    manifold_water_rigid::testkit::physics_fixtures::register(&mut registry);
    let prepared = prepare_scene_modifiers(&def, &registry).unwrap();
    let graph = manifold_core::flatten::flatten_groups(&prepared.def).unwrap();
    let node = |id: u32| graph.nodes.iter().find(|n| n.id == id).unwrap();
    let source = |to: u32, port: &str| {
        let wire = graph.wires.iter().find(|w| w.to_node == to && w.to_port == port).unwrap_or_else(|| panic!("{} has no {port}", node(to).node_id));
        (node(wire.from_node), wire.from_port.as_str())
    };
    let drawn: Vec<&EffectGraphNode> = graph
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.scene_object" && (n.node_id.as_str().contains("cube:") || n.node_id.as_str().contains("swarm:")))
        .collect();
    assert_eq!(drawn.len(), 2, "the copied cube and the swarm draw");
    let nests = graph.nodes.iter().filter(|n| n.type_id == "node.nest_copies").count();
    assert_eq!(nests, 2, "only the copied cube and the swarm are tiled, not the water's particles");
    for object in drawn {
        let (switch, _) = source(object.id, "instances");
        let (nest, port) = source(switch.id, "in_1");
        assert_eq!((nest.type_id.as_str(), port), ("node.nest_copies", "copies"), "{}: the tiles carry the copies", object.node_id);
        assert_eq!(source(nest.id, "outer").0.type_id, "node.scene_array", "one copy set per corridor cell");
        assert_eq!(source(nest.id, "inner").0.type_id, "node.reflect_array", "the mirror shaped one tile before the tiling");
        let (count, _) = source(object.id, "instance_count");
        assert_eq!(source(count.id, "in_1"), (nest, "count"), "{}: the draw reads the live product", object.node_id);
        // Before the tiling, the tile's count is the live copies: Copies
        // Count for the cube, the swarm's Count for its particles.
        let (mirror_count, _) = source(nest.id, "inner_count");
        let (live, port) = source(mirror_count.id, "a");
        let want = if object.node_id.as_str().contains("swarm:") { ("node.value", "out") } else { ("node.rigid_body", "active_count") };
        assert_eq!((live.type_id.as_str(), port), want, "{}: the live count", object.node_id);
    }
    prepared.def.clone().into_graph(&registry, &Default::default()).unwrap();
}

/// Only smooth bends split a built-in shape; a fragment cut keeps the cube
/// as it is and the cube draws only the cut's live records.
#[test]
fn bends_add_detail_and_a_fragment_cut_draws_its_live_records() {
    use manifold_core::scene_doc::{ModifierFields, ModifierTargets, ModifierType};
    use manifold_node_engine::load::expand::prepare_scene_modifiers;
    let prepared = |modifier| {
        let mut doc = SceneDoc::base();
        let mut card = ItemFields::default_for(ItemKind::Modifier);
        if let ItemFields::Modifier(fields) = &mut card {
            *fields = ModifierFields { modifier, on: true, targets: ModifierTargets::Scene, ..fields.clone() };
        }
        let end = doc.items.len();
        doc.insert(None, end, Item::new(ItemId::new("card"), "Card", card)).unwrap();
        let def = scene_layer(doc).gen_params().unwrap().graph.clone().unwrap();
        prepare_scene_modifiers(&def, &PrimitiveRegistry::with_builtin()).unwrap().def
    };
    let split = |def: &EffectGraphDef| def.nodes.iter().any(|n| n.type_id == "node.subdivide_mesh");
    assert!(split(&prepared(ModifierType::Bend)), "a bend splits the cube so it curves");
    let cut = prepared(ModifierType::VortexFragments);
    assert!(!split(&cut), "fragments cut the cube as it is");
    let feed = |object: u32, port: &str| cut.wires.iter().find(|w| w.to_node == object && w.to_port == port).map(|w| (w.from_node, w.from_port.clone()));
    let objects: Vec<u32> = cut.nodes.iter().filter(|n| n.type_id == "node.scene_object").map(|n| n.id).filter(|&id| feed(id, "topology").is_some()).collect();
    assert!(!objects.is_empty(), "the cut reaches an object");
    for object in objects {
        let (map, _) = feed(object, "topology").unwrap();
        assert_eq!(feed(object, "live_count"), Some((map, "live_count".to_owned())), "the object draws the cut's live records");
    }
}
