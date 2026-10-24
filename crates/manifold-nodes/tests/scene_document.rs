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
        // A modifier frame's rest centre, and the Scene Loop's rest box of
        // each object it measures, are placed by the compiler, not a row.
        let frame_centre = (role.starts_with("centre:") || role.starts_with("box:"))
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
            FieldControl::OnOff => vec![manifold_core::scene_schema::is_on(false), manifold_core::scene_schema::is_on(true)],
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

    // Placing Math View BEFORE a deformer in the modifier chain still captures the deformed mesh.
    let mut doc_before = SceneDoc::base();
    let mut mv = ItemFields::default_for(ItemKind::Modifier);
    if let ItemFields::Modifier(fields) = &mut mv {
        *fields = ModifierFields { modifier: ModifierType::MathView, on: true, targets: ModifierTargets::Scene, ..fields.clone() };
    }
    let mut deform = ItemFields::default_for(ItemKind::Modifier);
    if let ItemFields::Modifier(fields) = &mut deform {
        *fields = ModifierFields { modifier: ModifierType::ElasticSculpture, on: true, targets: ModifierTargets::Scene, ..fields.clone() };
    }
    for (id, fields) in [("mv_first", mv), ("deform_after", deform)] {
        let end = doc_before.items.len();
        doc_before.insert(None, end, Item::new(ItemId::new(id), id, fields)).unwrap();
    }
    let def_before = scene_layer(doc_before).gen_params().unwrap().graph.clone().unwrap();
    let prep_before = prepare_scene_modifiers(&def_before, &PrimitiveRegistry::with_builtin()).unwrap().def;
    let feed_before = |to: u32, port: &str| prep_before.wires.iter().find(|w| w.to_node == to && w.to_port == port).map(|w| (w.from_node, w.from_port.clone()));
    let cube_obj = prep_before.nodes.iter().find(|n| n.type_id == "node.scene_object" && n.node_id.as_str() == "cube:object").unwrap().id;
    let diag = prep_before.nodes.iter().find(|n| n.type_id == "node.render_mesh_diagram").unwrap().id;
    assert_eq!(
        feed_before(diag, "current_mesh"),
        feed_before(cube_obj, "vertices"),
        "Math View placed before a deformer still reads the final deformed mesh"
    );
    assert_ne!(
        feed_before(diag, "current_mesh"),
        feed_before(diag, "reference_mesh"),
        "current_mesh differs from the undeformed reference_mesh"
    );
}

/// Copy Blend on copies placed On Surface lays its second arrangement out in
/// the frame those copies draw in: the surface's frame, then the item's own
/// transform. No copies body lays them out, so the arrangement reads no
/// copies frame and centres on the item where it sits on the surface, never
/// on the world origin.
#[test]
fn copy_blend_on_surface_copies_centres_on_the_item() {
    use manifold_core::effect_graph_def::SerializedParamValue;
    use manifold_core::scene_doc::{ModifierFields, ModifierTargets, ModifierType, Shape};
    use manifold_node_engine::load::expand::prepare_scene_modifiers;
    let mut doc = SceneDoc::base();
    let mut plane = ItemFields::default_for(ItemKind::Object);
    if let ItemFields::Object(object) = &mut plane {
        object.shape = Shape::Plane;
    }
    let mut card = ItemFields::default_for(ItemKind::Modifier);
    if let ItemFields::Modifier(fields) = &mut card {
        *fields = ModifierFields {
            modifier: ModifierType::CopyBlend,
            on: true,
            targets: ModifierTargets::Items(vec![ItemId::new("cube")]),
            ..fields.clone()
        };
    }
    for (id, fields) in [("plane", plane), ("blend", card)] {
        let end = doc.items.len();
        doc.insert(None, end, Item::new(ItemId::new(id), id, fields)).unwrap();
    }
    if let Some(Item { fields: ItemFields::Object(object), .. }) = doc.find_mut(&ItemId::new("cube")) {
        object.copies = true;
        object.copies_surface = Some(ItemId::new("plane"));
    }
    let def = scene_layer(doc).gen_params().unwrap().graph.clone().unwrap();
    let prepared = prepare_scene_modifiers(&def, &PrimitiveRegistry::with_builtin()).unwrap().def;
    let feed = |to: u32, port: &str| {
        prepared.wires.iter().find(|w| w.to_node == to && w.to_port == port).map(|w| {
            let from = prepared.nodes.iter().find(|n| n.id == w.from_node).unwrap();
            (from.node_id.as_str().to_owned(), w.from_port.clone())
        })
    };
    let arrange: Vec<_> = prepared.nodes.iter().filter(|n| n.type_id == "node.arrange_copies").collect();
    assert_eq!(arrange.len(), 1, "one Copy Blend arrangement");
    for port in ["copy_frame", "copy_box", "copy_turn", "copy_grid", "active_count"] {
        assert_eq!(feed(arrange[0].id, port), None, "no copies body lays surface copies out, so {port} is unwired");
    }
    let object = prepared.nodes.iter().find(|n| n.type_id == "node.scene_object" && n.node_id.as_str() == "cube:object").expect("the cube draws");
    assert_eq!(object.params.get("instances_space"), Some(&SerializedParamValue::Enum { value: 1 }), "each copy draws inside its instance");
    assert_eq!(feed(object.id, "parent_transform"), Some(("cube:emit_frame".into(), "transform".into())), "instances sit in the surface's frame");
    assert_eq!(feed(object.id, "transform"), Some(("cube:transform".into(), "transform".into())), "then the item's own transform");
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

/// Two swarms draw one item, each with its own particles: the item draws
/// once per swarm, and a group with Copies on nests its one layout under
/// each swarm's particles.
#[test]
fn two_swarms_draw_one_item() {
    use manifold_core::scene_doc::DrawAs;
    let swarm = |id: &str, draw: &str| {
        let mut fields = ItemFields::default_for(ItemKind::Swarm);
        if let ItemFields::Swarm(s) = &mut fields {
            s.draw_as = DrawAs::Item(ItemId::new(draw));
        }
        Item::new(ItemId::new(id), id, fields)
    };
    let source = |def: &EffectGraphDef, to: u32, port: &str| {
        let from = def.wires.iter().find(|w| w.to_node == to && w.to_port == port)?.from_node;
        def.nodes.iter().find(|n| n.id == from).map(|n| n.node_id.as_str().to_owned())
    };

    let mut doc = SceneDoc::base();
    let end = doc.items.len();
    doc.insert(None, end, swarm("bees", "cube")).unwrap();
    doc.insert(None, end + 1, swarm("wasps", "cube")).unwrap();
    let def = flat(scene_layer(doc).gen_params().unwrap());
    let mut drawn_by: Vec<String> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.scene_object" && n.node_id.as_str().contains("cube:object"))
        .map(|n| source(&def, n.id, "instances").expect("drawn at particles"))
        .collect();
    drawn_by.sort();
    assert_eq!(drawn_by.len(), 2, "{drawn_by:?}");
    assert!(drawn_by[0].ends_with("bees:copies") && drawn_by[1].ends_with("wasps:copies"), "{drawn_by:?}");

    let mut doc = SceneDoc::base();
    let mut group = ItemFields::default_for(ItemKind::Group);
    if let ItemFields::Group(fields) = &mut group {
        fields.copies = true;
    }
    let end = doc.items.len();
    doc.insert(None, end, Item::new(ItemId::new("rack"), "Rack", group)).unwrap();
    doc.reparent(&ItemId::new("cube"), Some(&ItemId::new("rack")), 0).unwrap();
    doc.insert(None, end + 1, swarm("bees", "rack")).unwrap();
    doc.insert(None, end + 2, swarm("wasps", "rack")).unwrap();
    let def = flat(scene_layer(doc).gen_params().unwrap());
    let nests: Vec<&EffectGraphNode> = def.nodes.iter().filter(|n| n.type_id == "node.nest_copies").collect();
    assert_eq!(nests.len(), 2);
    let layouts: Vec<Option<String>> = nests.iter().map(|nest| source(&def, nest.id, "inner")).collect();
    assert!(layouts[0].is_some() && layouts[0] == layouts[1], "both nest the rack's one layout: {layouts:?}");
    let objects = def.nodes.iter().filter(|n| n.type_id == "node.scene_object" && n.node_id.as_str().contains("cube:object"));
    let mut drawn_by: Vec<String> = objects.map(|n| source(&def, n.id, "instances").expect("drawn at particles")).collect();
    drawn_by.sort();
    assert!(drawn_by.len() == 2 && drawn_by[0].ends_with("bees:nest") && drawn_by[1].ends_with("wasps:nest"), "{drawn_by:?}");
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

/// The rows the Scene Loop card hides until a Loop camera exists are exactly
/// the rows it shares with the Loop cameras that its corridor recipe lacks.
#[test]
fn the_rows_waiting_for_a_loop_camera_are_those_the_corridor_does_not_read() {
    let recipe: serde_json::Value =
        serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/scene-modifier-presets/SceneLoop.json"))).unwrap();
    let corridor: Vec<&str> = recipe["presetMetadata"]["params"].as_array().unwrap().iter().filter_map(|param| param["id"].as_str()).collect();
    let camera_only: Vec<&str> = manifold_core::scene_schema::LOOP_CAMERA_ROWS.iter().map(|(property, _)| *property).filter(|property| !corridor.contains(property)).collect();
    assert_eq!(camera_only, manifold_core::scene_schema::LOOP_CAMERA_ONLY);
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
    // Spacing, the gap between tiles, starts at the depth of what is drawn:
    // the swarm's 6 m spread, so its tiles start a spread apart.
    let spacing = |doc: SceneDoc| values(scene_layer(doc).gen_params().unwrap()).into_iter().find(|(id, ..)| id == "loop.spacing").unwrap().2;
    assert!((spacing(doc.clone()) - 6.0).abs() < 1e-4, "the swarm's spread sizes the gap");
    let mut no_swarm = doc.clone();
    no_swarm.remove(&ItemId::new("swarm")).unwrap();
    // A Copies row at its defaults lies across the travel axis: one cube deep.
    assert!((spacing(no_swarm) - 1.0).abs() < 1e-4, "the copies' row sizes the gap");
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
        let (cells, _) = source(nest.id, "outer");
        assert_eq!(cells.type_id, "node.scene_array", "one copy set per corridor cell");
        assert_eq!(source(cells.id, "cell_size").0.type_id, "node.loop_cell", "the cells are as long as the live measure");
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

thread_local! {
    static CELL: Cell<Option<f32>> = const { Cell::new(None) };
}

/// Reads the Scene Loop's live cell length.
struct CellProbe(EffectNodeType);

impl EffectNode for CellProbe {
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
        static INPUTS: [NodeInput; 1] = [NodePort {
            name: Cow::Borrowed("cell"),
            ty: PortType::Scalar(manifold_node_engine::ports::ScalarType::F32),
            kind: PortKind::Input,
            required: true,
        }];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        CELL.set(match ctx.inputs.scalar("cell") {
            Some(ParamValue::Float(value)) => Some(value),
            _ => None,
        });
    }
}

/// The Scene Loop's cell follows the live rows on the frame they change, as
/// Blender's Array follows its object: Copies Columns and Count, Copies
/// Spacing and an Object's scale all move the tiles at once. Bodies physics
/// moves never move them: the cell measures where things start
/// (SCENE_REFINEMENT_DESIGN.md P3b).
#[test]
fn the_loop_cell_follows_live_rows_and_never_a_moving_body() {
    use manifold_core::scene_doc::{ModifierFields, ModifierTargets, ModifierType, ObjectFields, Shape};
    let mut doc = SceneDoc::base();
    if let Some(Item { fields: ItemFields::Object(cube), .. }) = doc.find_mut(&ItemId::new("cube")) {
        cube.copies = true;
        cube.physics = PhysicsSetting::Body;
    }
    let ball = ObjectFields { shape: Shape::Cube, physics: PhysicsSetting::Body, ..ObjectFields::default() };
    let end = doc.items.len();
    doc.insert(None, end, Item::new(ItemId::new("ball"), "Ball", ItemFields::Object(ball))).unwrap();
    let mut card = ItemFields::default_for(ItemKind::Modifier);
    if let ItemFields::Modifier(fields) = &mut card {
        *fields = ModifierFields { modifier: ModifierType::SceneLoop, on: true, targets: ModifierTargets::Scene, ..fields.clone() };
    }
    doc.insert(None, end + 1, Item::new(ItemId::new("loop"), "loop", card)).unwrap();
    let mut layer = scene_layer(doc);
    let fx = layer.gen_params_mut().unwrap();
    // The ball starts 3 m back along the corridor and gravity pulls along it, so a
    // measure that read moving bodies would change every frame.
    for (id, value) in [("ball.pos_z", -3.0), ("world.gravity_y", 0.0), ("world.gravity_z", -9.81)] {
        assert!(fx.set_base_param(id, value), "the scene has {id}");
    }

    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register("test.loop_cell", || Box::new(CellProbe(EffectNodeType::new("test.loop_cell"))));
    registry.register("test.scene_pose", || Box::new(PoseProbe(EffectNodeType::new("test.scene_pose"))));
    // The load pass runs on the sealed layer graph, as the runtime loads it.
    let prepared = manifold_node_engine::load::expand::prepare_scene_modifiers(fx.graph.as_ref().unwrap(), &registry).unwrap();
    let mut def = manifold_core::flatten::flatten_groups(&prepared.def).unwrap();
    let id = |def: &EffectGraphDef, node_id: &str| def.nodes.iter().find(|n| n.node_id.as_str().ends_with(node_id)).unwrap_or_else(|| panic!("no {node_id}")).id;
    let (cell, pose) = (id(&def, "scene:loop_cell"), id(&def, "ball:pose_frame"));
    let next = def.nodes.iter().map(|n| n.id).max().unwrap() + 1;
    for (offset, node_id, type_id) in [(0, "cell_probe", "test.loop_cell"), (1, "pose_probe", "test.scene_pose")] {
        def.nodes.push(serde_json::from_value(serde_json::json!({"id": next + offset, "nodeId": node_id, "typeId": type_id})).unwrap());
    }
    for (from_node, from_port, to_node, to_port) in [(cell, "cell_size", next, "cell"), (pose, "transform", next + 1, "pose")] {
        def.wires.push(EffectGraphWire { from_node, from_port: from_port.into(), to_node, to_port: to_port.into() });
    }
    let mut runtime = PresetRuntime::from_def_for_render(def, &registry, Some(&fx.params), false).unwrap();
    runtime.apply_param_values(&fx.params);

    let mut frame_count = 0;
    let mut step = |runtime: &mut PresetRuntime| {
        CELL.set(None);
        POSE.set(None);
        let seconds = f64::from(frame_count) / 30.0;
        runtime.execute_frame(FrameTime { seconds: Seconds(seconds), beats: Beats(seconds * 2.0), delta: Seconds(1.0 / 30.0), frame_count: i64::from(frame_count) });
        frame_count += 1;
        (CELL.get().expect("the cell measure ran"), POSE.get().expect("the ball draws at its pose"))
    };
    let write = |fx: &mut PresetInstance, runtime: &mut PresetRuntime, id: &str, value: f32| {
        assert!(fx.set_base_param(id, value), "the scene has {id}");
        runtime.apply_param_values(&fx.params);
    };
    let close = |cell: f32, want: f32, what: &str| assert!((cell - want).abs() < 1e-4, "{what}: cell {cell}, want {want}");

    // Four copies in one row across the corridor, one cube deep (z from -0.5
    // to 0.5), and the ball from -3.5 to -2.5: 4 m of scene. Spacing starts
    // at the 1 m the compiler saw.
    close(step(&mut runtime).0, 4.0 + 1.0, "at the start");
    // Nine copies in three rows of three, growing along +Z from the first:
    // rows at 0, 1.1 and 2.2 m, copies to 2.7.
    write(fx, &mut runtime, "cube.columns", 3.0);
    write(fx, &mut runtime, "cube.count", 9.0);
    close(step(&mut runtime).0, 6.2 + 1.0, "Columns and Count");
    // Spacing 2 sets the rows 2 m apart: copies to 4.5.
    write(fx, &mut runtime, "cube.spacing", 2.0);
    close(step(&mut runtime).0, 8.0 + 1.0, "Copies Spacing");
    // The group's box rides its frame: across the corridor it changes
    // nothing, a metre along it carries the copies to 5.5.
    write(fx, &mut runtime, "cube.pos_x", 4.0);
    close(step(&mut runtime).0, 8.0 + 1.0, "the group moved across");
    write(fx, &mut runtime, "cube.pos_z", 1.0);
    close(step(&mut runtime).0, 9.0 + 1.0, "the group moved along");
    write(fx, &mut runtime, "cube.pos_z", 0.0);
    // The ball three times as deep: from -4.5 to -1.5.
    write(fx, &mut runtime, "ball.scale_z", 3.0);
    let (scaled, start) = step(&mut runtime);
    close(scaled, 9.0 + 1.0, "the ball's scale");
    // The Scene Loop's Spacing is the gap on top.
    write(fx, &mut runtime, "loop.spacing", 0.5);
    close(step(&mut runtime).0, 9.0 + 0.5, "the gap");

    let mut moved = start;
    for _ in 0..20 {
        let (cell, pose) = step(&mut runtime);
        close(cell, 9.5, "a moving body");
        moved = pose;
    }
    let (from, to) = (start.world()[3][2], moved.world()[3][2]);
    assert!(to < from - 0.5, "the ball falls down the corridor: {from} then {to}");
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

/// A Bend aimed at an import's Group bends its parts as one body: parts the
/// file turns and scales (one nested in the other, under a non-uniform
/// scale) land on the same world-space vertices as the same file imported
/// with Combine Meshes, whose parts sit flat in the file's space.
#[test]
fn a_group_bend_bends_turned_and_scaled_parts_as_one_body() {
    use manifold_core::scene_doc::transform::{mat4_mul, trs_matrix};
    use manifold_core::scene_doc::{ModifierFields, ModifierTargets, ModifierType};
    use manifold_node_engine::load::expand::prepare_scene_modifiers;
    use manifold_nodes_scene::node_graph::gltf_import::compile_model_scene_with;

    // Four triangles spanning the bend's axis, drawn by both parts.
    let raw: [[f32; 3]; 12] = [
        [-1.0, 0.0, -0.5], [1.0, 0.0, -0.5], [1.0, 2.0, 0.5],
        [-1.0, 0.0, -0.5], [1.0, 2.0, 0.5], [-1.0, 2.0, 0.5],
        [-1.0, 0.0, 0.5], [1.0, 1.0, 0.5], [0.0, 2.0, -0.5],
        [0.0, 0.0, 0.0], [1.0, 1.0, -0.5], [-1.0, 1.5, 0.5],
    ];
    let (turn_a, turn_b) = (0.6f32, 0.9f32);
    let (a_at, a_scale) = ([1.0, 0.0, 0.0], [2.0, 1.0, 0.5]);
    let (b_at, b_scale) = ([0.0, 1.5, 0.3], [0.5, 1.5, 1.0]);
    let half = |angle: f32| ((angle / 2.0).sin(), (angle / 2.0).cos());
    let ((ya, wa), (xb, wb)) = (half(turn_a), half(turn_b));
    let nodes = serde_json::json!([
        { "name": "rig", "children": [1] },
        { "name": "a", "mesh": 0, "translation": a_at, "rotation": [0.0, ya, 0.0, wa], "scale": a_scale, "children": [2] },
        { "name": "b", "mesh": 0, "translation": b_at, "rotation": [xb, 0.0, 0.0, wb], "scale": b_scale }
    ]);
    let mut bin = Vec::new();
    for value in raw.iter().flatten() {
        bin.extend_from_slice(&value.to_le_bytes());
    }
    let gltf = serde_json::json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": nodes,
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
        "accessors": [{
            "bufferView": 0, "componentType": 5126, "count": raw.len(), "type": "VEC3",
            "min": [-1.0, 0.0, -0.5], "max": [1.0, 2.0, 0.5],
        }],
        "bufferViews": [{ "buffer": 0, "byteOffset": 0, "byteLength": bin.len() }],
        "buffers": [{ "byteLength": bin.len() }],
    });
    let mut json = serde_json::to_vec(&gltf).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut glb = Vec::new();
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&((12 + 8 + json.len() + 8 + bin.len()) as u32).to_le_bytes());
    for (chunk, kind) in [(&json, b"JSON"), (&bin, b"BIN\0")] {
        glb.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        glb.extend_from_slice(kind);
        glb.extend_from_slice(chunk);
    }
    let path = std::env::temp_dir().join(format!("manifold_group_bend_{}.glb", std::process::id()));
    std::fs::write(&path, glb).unwrap();

    // Each part's mesh where the file puts it, for a Combine Meshes source.
    let a_world = trs_matrix(a_at, [0.0, turn_a, 0.0], a_scale);
    let b_world = mat4_mul(a_world, trs_matrix(b_at, [turn_b, 0.0, 0.0], b_scale));
    let file_world = |node_path: &str| match node_path.rsplit('/').next() {
        Some("a") => a_world,
        Some("b") => b_world,
        other => panic!("no part {other:?}"),
    };
    // The import with one card aimed at its Group, prepared as the loader runs it.
    let prepared = |combine: bool, modifier: ModifierType| {
        let (def, _) = compile_model_scene_with(&path, combine, |doc, import| {
            let mut card = ItemFields::default_for(ItemKind::Modifier);
            if let ItemFields::Modifier(fields) = &mut card {
                let targets = ModifierTargets::Items(vec![import.clone()]);
                *fields = ModifierFields { modifier, on: true, targets, ..fields.clone() };
            }
            let end = doc.items.len();
            doc.insert(None, end, Item::new(ItemId::new("card"), "Card", card)).unwrap();
        })
        .unwrap();
        prepare_scene_modifiers(&def, &PrimitiveRegistry::with_builtin()).unwrap().def
    };
    let bent = |combine: bool| {
        let prepared = prepared(combine, ModifierType::Bend);
        let eval = CpuMeshes { def: &prepared, raw: &raw, file_world: &file_world };
        let mut parts: Vec<(String, Vec<[f32; 3]>)> = prepared
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.scene_object")
            .filter_map(|object| eval.world(object.id))
            .collect();
        parts.sort_by(|x, y| x.0.cmp(&y.0));
        parts
    };
    let (carried, combined) = (bent(false), bent(true));

    // A fragment cut carried into the frame and back still draws only the
    // cut's live records.
    let cut = prepared(false, ModifierType::VortexFragments);
    std::fs::remove_file(&path).unwrap();
    let feed = |to: u32, port: &str| cut.wires.iter().find(|w| w.to_node == to && w.to_port == port).map(|w| (w.from_node, w.from_port.clone()));
    let carried_out: Vec<u32> = cut
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.scene_object")
        .map(|n| n.id)
        .filter(|&object| feed(object, "vertices").is_some_and(|(from, _)| cut.nodes.iter().any(|n| n.id == from && n.type_id == "node.merge_meshes")))
        .collect();
    assert_eq!(carried_out.len(), 2, "both parts come back out of the frame");
    for object in carried_out {
        let (map, _) = feed(object, "topology").expect("the part draws the cut's triangles");
        assert_eq!(feed(object, "live_count"), Some((map, "live_count".to_owned())), "the part draws the cut's live records");
    }

    assert_eq!(carried.len(), 2, "both parts draw");
    assert_eq!(carried.iter().map(|p| &p.0).collect::<Vec<_>>(), combined.iter().map(|p| &p.0).collect::<Vec<_>>());
    let mut moved = 0.0f32;
    for ((part, got), (_, want)) in carried.iter().zip(&combined) {
        assert_eq!(got.len(), want.len(), "{part}");
        let flat = file_world(part);
        for (index, ((g, w), r)) in got.iter().zip(want).zip(&raw).enumerate() {
            assert!((0..3).all(|axis| (g[axis] - w[axis]).abs() < 1e-4), "{part} vertex {index}: {g:?}, flat in the file {w:?}");
            let unbent = transform_point(&flat, *r);
            moved = moved.max((0..3).map(|axis| (w[axis] - unbent[axis]).abs()).fold(0.0, f32::max));
        }
    }
    assert!(moved > 0.05, "the bend moved the model ({moved})");
}

/// Column-major `m` applied to the point `p`.
fn transform_point(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|row| m[0][row] * p[0] + m[1][row] * p[1] + m[2][row] * p[2] + m[3][row])
}

/// The inverse of an affine column-major matrix.
fn affine_inverse(m: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let a = |col: usize, row: usize| m[col][row];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(2, 1) * a(1, 2)) - a(1, 0) * (a(0, 1) * a(2, 2) - a(2, 1) * a(0, 2))
        + a(2, 0) * (a(0, 1) * a(1, 2) - a(1, 1) * a(0, 2));
    let linear: [[f32; 3]; 3] = std::array::from_fn(|col| {
        std::array::from_fn(|row| {
            let (r1, r2) = ((col + 1) % 3, (col + 2) % 3);
            let (c1, c2) = ((row + 1) % 3, (row + 2) % 3);
            (a(c1, r1) * a(c2, r2) - a(c2, r1) * a(c1, r2)) / det
        })
    });
    let moved: [f32; 3] = std::array::from_fn(|row| -(0..3).map(|col| linear[col][row] * m[3][col]).sum::<f32>());
    std::array::from_fn(|col| if col < 3 { [linear[col][0], linear[col][1], linear[col][2], 0.0] } else { [moved[0], moved[1], moved[2], 1.0] })
}

/// A CPU reading of the prepared mesh chain a Bend card builds on an
/// imported part: the file's mesh, the frame's carry in and out, the bend
/// itself (`bend_mesh_body.wgsl`, unmasked) and the transforms the part
/// draws with.
struct CpuMeshes<'a> {
    def: &'a EffectGraphDef,
    raw: &'a [[f32; 3]],
    file_world: &'a dyn Fn(&str) -> [[f32; 4]; 4],
}

impl CpuMeshes<'_> {
    fn node(&self, id: u32) -> &EffectGraphNode {
        self.def.nodes.iter().find(|node| node.id == id).unwrap()
    }

    fn feed(&self, id: u32, port: &str) -> Option<(u32, String)> {
        self.def.wires.iter().find(|w| w.to_node == id && w.to_port == port).map(|w| (w.from_node, w.from_port.clone()))
    }

    fn param(&self, id: u32, name: &str) -> f32 {
        use manifold_core::effect_graph_def::SerializedParamValue as V;
        match self.node(id).params.get(name) {
            Some(V::Float { value }) => *value,
            Some(V::Int { value }) => *value as f32,
            Some(V::Enum { value }) => *value as f32,
            Some(V::Bool { value }) => f32::from(u8::from(*value)),
            None => 0.0,
            other => panic!("{name}: {other:?}"),
        }
    }

    fn scalar(&self, id: u32, port: &str) -> f32 {
        let Some((from, from_port)) = self.feed(id, port) else {
            return self.param(id, port);
        };
        match self.node(from).type_id.as_str() {
            "node.value" => self.param(from, "value"),
            // A live place: the transform inside its parent, seen from the frame.
            "node.transform_components" => {
                let axis = ["pos_x", "pos_y", "pos_z"].iter().position(|p| *p == from_port).expect("a live offset reads a position");
                let placed = manifold_core::scene_doc::transform::mat4_mul(
                    self.transform(self.feed(from, "parent")),
                    self.transform(self.feed(from, "transform")),
                );
                let seen = manifold_core::scene_doc::transform::mat4_mul(affine_inverse(&self.transform(self.feed(from, "frame"))), placed);
                seen[3][axis]
            }
            "node.math" => {
                let (a, b) = (self.scalar(from, "a"), self.scalar(from, "b"));
                match self.param(from, "op") as u32 {
                    0 => a + b,
                    1 => a - b,
                    2 => a * b,
                    3 => a / b,
                    op => panic!("math op {op}"),
                }
            }
            other => panic!("scalar from {other}"),
        }
    }

    fn transform(&self, at: Option<(u32, String)>) -> [[f32; 4]; 4] {
        let Some((id, _)) = at else {
            return trs_identity();
        };
        assert_eq!(self.node(id).type_id, "node.transform_3d");
        let three = |names: [&str; 3]| names.map(|name| self.scalar(id, name));
        let own = manifold_core::scene_doc::transform::trs_matrix(
            three(["pos_x", "pos_y", "pos_z"]),
            three(["rot_x", "rot_y", "rot_z"]),
            [self.scale(id, "scale_x"), self.scale(id, "scale_y"), self.scale(id, "scale_z")],
        );
        manifold_core::scene_doc::transform::mat4_mul(self.transform(self.feed(id, "parent")), own)
    }

    fn scale(&self, id: u32, name: &str) -> f32 {
        if self.feed(id, name).is_some() || self.node(id).params.contains_key(name) { self.scalar(id, name) } else { 1.0 }
    }

    /// The mesh at `(id, port)` and the file node it is drawn from.
    fn mesh(&self, id: u32) -> (String, Vec<[f32; 3]>) {
        use manifold_core::effect_graph_def::SerializedParamValue as V;
        let node = self.node(id);
        match node.type_id.as_str() {
            "node.gltf_mesh_source" => {
                let Some(V::String { value: path }) = node.params.get("node_path") else { panic!("a source with no node path") };
                let own = matches!(node.params.get("own_space"), Some(V::Bool { value: true }));
                let place = if own { trs_identity() } else { (self.file_world)(path) };
                (path.clone(), self.raw.iter().map(|p| transform_point(&place, *p)).collect())
            }
            "node.merge_meshes" => {
                assert!(self.feed(id, "mesh_1").is_none(), "a carry holds one mesh");
                let placed = |port: &str| self.transform(self.feed(id, port));
                let to_frame = affine_inverse(&manifold_core::scene_doc::transform::mat4_mul(placed("frame_within"), placed("frame")));
                let m = manifold_core::scene_doc::transform::mat4_mul(to_frame, manifold_core::scene_doc::transform::mat4_mul(placed("within_0"), placed("place_0")));
                let (path, mesh) = self.mesh(self.feed(id, "mesh_0").unwrap().0);
                (path, mesh.iter().map(|p| transform_point(&m, *p)).collect())
            }
            "node.bend_mesh" => {
                if let Some((mask, _)) = self.feed(id, "weights") {
                    assert_eq!(self.param(mask, "amount"), 0.0, "an unmasked bend weighs every vertex 1");
                }
                let axis = self.param(id, "axis") as usize;
                let (angle, center) = (self.scalar(id, "angle"), self.scalar(id, "center"));
                let offset = ["source_offset_x", "source_offset_y", "source_offset_z"].map(|port| self.scalar(id, port));
                let (path, mesh) = self.mesh(self.feed(id, "in").unwrap().0);
                let companion = (axis + 1) % 3;
                let bent = mesh
                    .iter()
                    .map(|p| {
                        let mut q: [f32; 3] = std::array::from_fn(|i| p[i] + offset[i]);
                        let s = q[axis] - center;
                        let (sn, c) = (angle * s).sin_cos();
                        let along = q[companion];
                        q[axis] = center + s * c - along * sn;
                        q[companion] = s * sn + along * c;
                        std::array::from_fn(|i| q[i] - offset[i])
                    })
                    .collect();
                (path, bent)
            }
            other => panic!("unexpected mesh node {other}"),
        }
    }

    /// The world-space vertices `object` draws, by the file node it draws,
    /// or `None` when it draws no imported part.
    fn world(&self, object: u32) -> Option<(String, Vec<[f32; 3]>)> {
        let (vertices, _) = self.feed(object, "vertices")?;
        if !matches!(self.node(vertices).type_id.as_str(), "node.gltf_mesh_source" | "node.merge_meshes" | "node.bend_mesh") {
            return None;
        }
        let (path, mesh) = self.mesh(vertices);
        let world = manifold_core::scene_doc::transform::mat4_mul(
            self.transform(self.feed(object, "parent_transform")),
            self.transform(self.feed(object, "transform")),
        );
        Some((path, mesh.iter().map(|p| transform_point(&world, *p)).collect()))
    }
}

fn trs_identity() -> [[f32; 4]; 4] {
    manifold_core::scene_doc::transform::trs_matrix([0.0; 3], [0.0; 3], [1.0; 3])
}
