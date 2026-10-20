//! Shared test fixtures for the `commands/graph/` command modules.
//!
//! Distributed here in P2-G/S7 (pure move) from the flat `mod tests` that
//! lived in `graph.rs`. Every fixture is imported by two or more sibling test
//! mods via `use super::super::test_support::*`; single-consumer fixtures stay
//! beside their module's tests. Bodies are verbatim; the only edits are the
//! module dedent and a `pub(super)` on each fixture so sibling test mods reach
//! it (`pub(super)` = visible within `graph` and all its descendants).

use super::*;
use manifold_core::EffectId;
use manifold_core::LayerId;
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::{
    EFFECT_GRAPH_VERSION, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef, PresetMetadata,
};
use manifold_core::effects::PresetInstance;
use manifold_core::layer::Layer;
use manifold_core::types::LayerType;
pub(super) fn modifier_draft_fixture() -> (Project, GraphTarget, EffectGraphDef) {
    use manifold_core::scene_modifier_preset::{
        SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
    };
    let local: EffectGraphDef = serde_json::from_value(serde_json::json!({
        "version":3, "presetMetadata":{"id":"recipe","displayName":"Recipe","category":"Geometry",
            "oscPrefix":"recipe","params":[],"bindings":[]},
        "nodes":[{"id":1,"nodeId":"value","typeId":"node.value","handle":"value",
            "params":{"value":{"type":"Float","value":0.2}}}],"wires":[]
    }))
    .unwrap();
    let mut graph: EffectGraphDef = serde_json::from_value(serde_json::json!({
        "version":3,"presetMetadata":{"id":"host","displayName":"Host","category":"Geometry",
            "oscPrefix":"host","params":[],"bindings":[]},"nodes":[],"wires":[]
    }))
    .unwrap();
    for id in ["a", "b"] {
        graph.scene_modifiers.push(SceneModifierInstanceDef {
            id: NodeId::new(id),
            scene: SceneNodeRef {
                scope: vec![],
                node: NodeId::new("scene"),
            },
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: vec![],
            legacy_math_view_carrier: None,
            graph: Box::new(local.clone()),
        });
    }
    let mut layer = Layer::new_generator("Scene".into(), PresetTypeId::new("host"), 0);
    layer.layer_id = LayerId::new("layer");
    let host = layer.gen_params_mut().unwrap();
    host.graph = Some(graph.clone());
    host.refresh_manifest_from_graph();
    let mut project = Project::default();
    project.timeline.layers.push(layer);
    let target = GraphTarget::SceneModifier {
        owner: Box::new(GraphTarget::Generator(LayerId::new("layer"))),
        modifier_id: NodeId::new("a"),
    };
    (project, target, graph)
}

pub(super) fn modifier_exposure_command(
    target: GraphTarget,
    graph: EffectGraphDef,
    expose: bool,
) -> ToggleNodeParamExposeCommand {
    ToggleNodeParamExposeCommand::new(
        target,
        NodeId::new("value"),
        1,
        "value".into(),
        "value".into(),
        expose,
        graph,
        "Value".into(),
        0.0,
        1.0,
        0.2,
        manifold_core::effects::ParamConvert::Float,
        false,
        vec![],
    )
}

pub(super) fn slot(id: &str, value: f32, exposed: bool) -> manifold_core::params::Param {
    let mut p =
        manifold_core::params::Param::bundled(manifold_core::effect_graph_def::ParamSpecDef {
            tooltip: None,
            id: id.into(),
            name: id.into(),
            min: 0.0,
            max: 1.0,
            default_value: value,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: vec![],
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: None,
            card_visible: true,
            material_role: None,
            limit: Default::default(),
            unit: None,
            scene: None,
        });
    p.value = value;
    p.base = value;
    p.exposed = exposed;
    p
}

pub(super) fn abc_graph() -> EffectGraphDef {
    let mk = |id: u32, handle: &str, ty: &str| EffectGraphNode {
        id,
        node_id: manifold_core::NodeId::new(handle),
        type_id: ty.to_string(),
        handle: Some(handle.to_string()),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    };
    let w = |fln: u32, fp: &str, tn: u32, tp: &str| EffectGraphWire {
        from_node: fln,
        from_port: fp.to_string(),
        to_node: tn,
        to_port: tp.to_string(),
    };
    EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: vec![
            mk(0, "a", "system.source"),
            mk(1, "b", "node.transform"),
            mk(2, "c", "system.final_output"),
        ],
        wires: vec![w(0, "out", 1, "in"), w(1, "out", 2, "in")],
    }
}

/// A command prepared against the project when made, as the app sends it.
/// A preparation refusal becomes a command that changes nothing and reports
/// why, so a test reads a refusal at preparation and one at execute alike.
#[derive(Debug)]
pub(super) enum Prepared<C> {
    Ready(C),
    Refused(String),
}

impl<C> From<Result<C, manifold_core::scene_graph_edit::SceneGraphEditError>> for Prepared<C> {
    fn from(result: Result<C, manifold_core::scene_graph_edit::SceneGraphEditError>) -> Self {
        match result {
            Ok(command) => Self::Ready(command),
            Err(error) => Self::Refused(error.message),
        }
    }
}

impl<C: crate::command::Command> crate::command::Command for Prepared<C> {
    fn touches(&self) -> manifold_core::change::ChangeSet {
        match self {
            Self::Ready(command) => command.touches(),
            Self::Refused(_) => manifold_core::change::ChangeSet::default(),
        }
    }
    fn do_execute(&mut self, project: &mut Project) {
        if let Self::Ready(command) = self {
            command.do_execute(project);
        }
    }
    fn do_undo(&mut self, project: &mut Project) {
        if let Self::Ready(command) = self {
            command.do_undo(project);
        }
    }
    fn description(&self) -> &str {
        match self {
            Self::Ready(command) => command.description(),
            Self::Refused(_) => "Refused",
        }
    }
    fn was_applied(&self) -> bool {
        matches!(self, Self::Ready(command) if command.was_applied())
    }
    fn rejection_reason(&self) -> Option<&str> {
        match self {
            Self::Ready(command) => command.rejection_reason(),
            Self::Refused(reason) => Some(reason),
        }
    }
}

/// Enable Physics on render slot `index` of scene `render`, prepared now.
pub(super) fn enable_physics(
    project: &Project,
    target: GraphTarget,
    render: u32,
    index: u32,
    body: Vec<manifold_core::scene_exposure::NodeParamMetadata>,
    default: EffectGraphDef,
) -> Prepared<EnableSceneObjectPhysicsCommand> {
    EnableSceneObjectPhysicsCommand::new(project, target, render, index, &body, &[], &default).into()
}

/// One copy of render slot `index` of scene `render`, prepared now.
pub(super) fn duplicate_object(
    project: &Project,
    target: GraphTarget,
    scope: Vec<u32>,
    render: u32,
    index: u32,
    default: EffectGraphDef,
) -> Prepared<DuplicateSceneObjectCommand> {
    DuplicateSceneObjectCommand::new(project, target, scope, render, index, 1, &default).into()
}

pub(super) fn project_with_graph(def: EffectGraphDef) -> (Project, EffectId) {
    let mut project = Project::default();
    let effect_id = EffectId::new("test-group-fx");
    let mut fx = PresetInstance::new(PresetTypeId::new("test.fx"));
    fx.id = effect_id.clone();
    fx.graph = Some(def);
    project.settings.master_effects.push(fx);
    (project, effect_id)
}

pub(super) fn graph_of<'a>(project: &'a Project, id: &EffectId) -> &'a EffectGraphDef {
    project
        .find_effect_by_id(id)
        .unwrap()
        .graph
        .as_ref()
        .unwrap()
}

/// Catalog default for a Mirror-like graph: source → uv_transform
/// → mix → final_output, four nodes plus four wires. Mirrors the
/// shape the runtime `build_mirror` produces.
pub(super) fn mirror_catalog_default() -> EffectGraphDef {
    let mut def = EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: vec![
            EffectGraphNode {
                id: 0,
                node_id: manifold_core::NodeId::default(),
                type_id: "system.source".to_string(),
                handle: Some("source".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
            EffectGraphNode {
                id: 1,
                node_id: manifold_core::NodeId::default(),
                type_id: "node.transform".to_string(),
                handle: Some("uv_transform".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
            EffectGraphNode {
                id: 2,
                node_id: manifold_core::NodeId::default(),
                type_id: "node.mix".to_string(),
                handle: Some("mix".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
            EffectGraphNode {
                id: 3,
                node_id: manifold_core::NodeId::default(),
                type_id: "system.final_output".to_string(),
                handle: Some("final_output".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
        ],
        wires: vec![
            EffectGraphWire {
                from_node: 0,
                from_port: "out".to_string(),
                to_node: 1,
                to_port: "source".to_string(),
            },
            EffectGraphWire {
                from_node: 0,
                from_port: "out".to_string(),
                to_node: 2,
                to_port: "a".to_string(),
            },
            EffectGraphWire {
                from_node: 1,
                from_port: "out".to_string(),
                to_node: 2,
                to_port: "b".to_string(),
            },
            EffectGraphWire {
                from_node: 2,
                from_port: "out".to_string(),
                to_node: 3,
                to_port: "in".to_string(),
            },
        ],
    };
    // Stamp node ids == handle, matching the bundled-preset convention
    // (a node's stable id is its authoring handle).
    for n in &mut def.nodes {
        if let Some(h) = n.handle.clone() {
            n.node_id = manifold_core::NodeId::new(h);
        }
    }
    def
}

fn template_node(id: u32, type_id: &str, handle: Option<&str>) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: manifold_core::NodeId::new(type_id.rsplit('.').next().unwrap_or(type_id)),
        type_id: type_id.to_string(),
        handle: handle.map(str::to_string),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn template_metadata(display_name: &str) -> PresetMetadata {
    PresetMetadata {
        shape: None,
        id: PresetTypeId::from_string(format!("__template_{display_name}")),
        display_name: display_name.to_string(),
        category: "Geometry".to_string(),
        osc_prefix: "scene".to_string(),
        legacy_discriminant: None,
        available: true,
        is_line_based: false,
        layer_types: None,
        params: Vec::new(),
        bindings: Vec::new(),
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: Vec::new(),
        string_bindings: Vec::new(),
        scene_modifier: None,
        scene_bounds: None,
    }
}

/// Add Object's shape, without card rows: a cube, a material and a
/// transform bound by one scene object, grouped behind one Object output.
/// `manifold-nodes-scene` builds the shipped one; this crate cannot see it.
pub(super) fn test_object_template(centroid: (f32, f32)) -> manifold_core::scene_template::SceneTemplate {
    use manifold_core::scene_template::HANDLE_TOKEN;
    let wire = |from_node, from_port: &str, to_node, to_port: &str| EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    };
    let mut group = template_node(6, GROUP_TYPE_ID, Some(HANDLE_TOKEN));
    group.editor_pos = Some(centroid);
    group.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: Vec::new(),
            outputs: vec![InterfacePortDef { name: "object".to_string(), port_type: "Object".to_string() }],
            params: Vec::new(),
        },
        nodes: vec![
            template_node(1, "node.cube_mesh", Some("mesh")),
            template_node(2, "node.pbr_material", Some("material")),
            template_node(3, "node.transform_3d", Some("transform")),
            template_node(4, "node.scene_object", Some(HANDLE_TOKEN)),
            template_node(5, GROUP_OUTPUT_TYPE_ID, None),
        ],
        wires: vec![
            wire(1, "vertices", 4, "vertices"),
            wire(2, "out", 4, "material"),
            wire(3, "transform", 4, "transform"),
            wire(4, "object", 5, "object"),
        ],
        tint: None,
    }));
    manifold_core::scene_template::SceneTemplate { group, metadata: template_metadata("Object") }
}

/// Add Light's shape, without card rows: a bare white Sun light casting
/// shadows.
pub(super) fn test_light_template(position: (f32, f32)) -> manifold_core::scene_template::SceneTemplate {
    let mut light = template_node(1, "node.light", Some(manifold_core::scene_template::HANDLE_TOKEN));
    light.editor_pos = Some(position);
    light.params = BTreeMap::from([
        ("mode".to_string(), SerializedParamValue::Enum { value: 0 }),
        ("color_r".to_string(), SerializedParamValue::Float { value: 1.0 }),
        ("color_g".to_string(), SerializedParamValue::Float { value: 1.0 }),
        ("color_b".to_string(), SerializedParamValue::Float { value: 1.0 }),
        ("intensity".to_string(), SerializedParamValue::Float { value: 1.0 }),
        ("cast_shadows".to_string(), SerializedParamValue::Float { value: 1.0 }),
    ]);
    manifold_core::scene_template::SceneTemplate { group: light, metadata: template_metadata("Light") }
}

/// Stamp `rows` as card rows of the template node with template-local id
/// `node` (the template's own node or one of its group's children), under
/// `section` (which may carry the handle token).
pub(super) fn stamp_template_rows(
    template: &mut manifold_core::scene_template::SceneTemplate,
    node: u32,
    section: &str,
    rows: &[manifold_core::scene_exposure::NodeParamMetadata],
) {
    let found = std::iter::once(&template.group)
        .chain(template.group.group.as_deref().into_iter().flat_map(|body| body.nodes.iter()))
        .find(|candidate| candidate.id == node)
        .expect("template node");
    manifold_core::scene_exposure::stamp_scene_node_exposures_into(
        &mut template.metadata.params,
        &mut template.metadata.bindings,
        found.id,
        &found.node_id,
        section,
        rows,
        &found.params,
    );
}

/// Insert `template` into the root-level render scene with document id
/// `render`, prepared against `target`'s current graph.
pub(super) fn insert_template(
    project: &Project,
    target: &GraphTarget,
    render: u32,
    template: manifold_core::scene_template::SceneTemplate,
) -> Result<crate::scene_template::InsertSceneTemplateCommand, String> {
    let default = mirror_catalog_default();
    let node = project
        .graph_for_target(target, Some(&default))
        .and_then(|def| def.nodes.iter().find(|node| node.id == render))
        .map(|node| node.node_id.clone())
        .ok_or("no render scene")?;
    let scene = manifold_core::scene_modifier_preset::SceneNodeRef { scope: Vec::new(), node };
    crate::scene_template::InsertSceneTemplateCommand::new(project, target.clone(), scene, None, template, &default)
        .map_err(|error| error.message)
}

/// Project with one timeline layer, no generator override.
pub(super) fn project_with_one_generator_layer() -> (Project, LayerId) {
    let mut project = Project::default();
    let layer = Layer::new("Test Layer".to_string(), LayerType::Generator, 0);
    let lid = layer.layer_id.clone();
    project.timeline.layers.push(layer);
    (project, lid)
}

/// A single-param `NodeParamMetadata` fixture — stands in for what
/// `manifold_nodes::node_graph::scene_exposure::metadata_for_node_type`
/// would compute from a real primitive's `ParamDef` (this crate can't
/// depend on the renderer, so the app-side caller is the real source —
/// see the cross-crate constraint note in
/// `docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` P1).
pub(super) fn scene_param_meta(
    name: &str,
    label: &str,
) -> manifold_core::scene_exposure::NodeParamMetadata {
    manifold_core::scene_exposure::NodeParamMetadata {
        name: name.to_string(),
        label: label.to_string(),
        min: 0.0,
        max: 1.0,
        default_value: SerializedParamValue::Float { value: 0.5 },
        is_angle: false,
        wraps: false,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        convert: manifold_core::effects::ParamConvert::Float,
        material_role: None,
        scene: None,
        unit: None,
        limit: Default::default(),
    }
}
