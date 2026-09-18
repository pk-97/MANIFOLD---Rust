//! Shared control vocabulary for the standalone Math View scene modifier, plus
//! detection and migration helpers for legacy per-modifier Math View sections.
//!
//! Math View is one scene modifier (`MathView` recipe) that visualises the
//! combined deformation of all preceding modifiers in its scene. Legacy
//! projects embedded the same controls in qualified carrier recipes (Vortex
//! Fragments); load-time migration strips those and moves the host bindings
//! onto a standalone instance (see `project_io::migrate_project_scene_graphs`).

use crate::effect_graph_def::{BindingTarget, EffectGraphDef};
use crate::effects::ParamConvert;
use crate::scene_modifier_preset::SceneNodeRef;
use crate::NodeId;

/// Bundled recipe id of the standalone Math View modifier.
pub const MATH_VIEW_RECIPE_ID: &str = "MathView";

pub const CONTROL_PREFIX: &str = "math_view_";
pub const CONTROLS: &[(&str, &str, f32, f32, f32)] = &[
    ("mode", "Mode", 0.0, 0.0, 2.0),
    ("occlusion", "Occlusion", 0.0, 0.0, 1.0),
    ("grid", "Grid", 1.0, 0.0, 1.0),
    ("fragments", "Fragments", 1.0, 0.0, 1.0),
    ("ghosts", "Ghosts", 1.0, 0.0, 1.0),
    ("vectors", "Vectors", 1.0, 0.0, 1.0),
    ("axes", "Axes", 1.0, 0.0, 1.0),
    ("trails", "Motion trails", 1.0, 0.0, 1.0),
    ("grid_brightness", "Grid Brightness", 1.0, 0.0, 2.0),
    (
        "fragments_brightness",
        "Fragments Brightness",
        1.0,
        0.0,
        2.0,
    ),
    ("ghosts_brightness", "Ghosts Brightness", 1.0, 0.0, 2.0),
    ("vectors_brightness", "Vectors Brightness", 1.0, 0.0, 2.0),
    ("trails_brightness", "Trails Brightness", 1.0, 0.0, 2.0),
    ("pulse", "Pulse", 0.0, 0.0, 1.0),
    ("pulse_strength", "Pulse Strength", 1.0, -1.0, 3.0),
    ("pulse_target", "Pulse Target", 0.0, 0.0, 5.0),
    ("pulse_beats", "Pulse Beats", 1.0, 0.0625, 32.0),
    ("pulse_trigger", "Pulse Trigger", 0.0, 0.0, 1.0),
    ("scan_amount", "Scan Amount", 0.0, 0.0, 1.0),
    ("scan_progress", "Scan Progress", 0.0, 0.0, 1.0),
    ("scan_width", "Scan Width", 0.2, 0.01, 2.0),
    ("scan_direction", "Scan Direction", 2.0, 0.0, 5.0),
    ("scan_mode", "Scan Mode", 0.0, 0.0, 1.0),
    ("scan_target", "Scan Target", 0.0, 0.0, 5.0),
    ("scan_beats", "Scan Beats", 4.0, 0.0625, 32.0),
    ("scan_trigger", "Scan Trigger", 0.0, 0.0, 1.0),
    ("connect_mesh", "Connect to Mesh", 0.0, 0.0, 1.0),
    ("density", "Density", 3.0, 2.0, 8.0),
    ("line_width", "Line Width", 1.0, 0.5, 4.0),
    ("geometry_hue", "Geometry Hue", 0.52, 0.0, 1.0),
    ("path_hue", "Path Hue", 0.13, 0.0, 1.0),
];

/// Whether this recipe graph is the standalone Math View modifier.
pub fn is_math_view_recipe(graph: &EffectGraphDef) -> bool {
    graph
        .preset_metadata
        .as_ref()
        .is_some_and(|metadata| metadata.id.as_str() == MATH_VIEW_RECIPE_ID)
}

fn control_node_id(suffix: &str) -> NodeId {
    NodeId::new(format!("__math_view_{suffix}"))
}

fn visit(nodes: &[crate::effect_graph_def::EffectGraphNode], f: &mut impl FnMut(&crate::effect_graph_def::EffectGraphNode)) {
    for node in nodes {
        f(node);
        if let Some(group) = &node.group {
            visit(&group.nodes, f);
        }
    }
}

/// Legacy carrier qualification: the Vortex Fragments patch recipe with its
/// orbit binding reaching a patch transform. Only used to recognise saved
/// projects for migration; nothing new qualifies.
fn legacy_eligible(graph: &EffectGraphDef) -> bool {
    let Some(metadata) = &graph.preset_metadata else {
        return false;
    };
    if metadata.id.as_str() != "VortexFragments" {
        return false;
    }
    metadata.bindings.iter().any(|binding| {
        let BindingTarget::Node { node_id, param } = &binding.target else {
            return false;
        };
        if binding.id != "orbit" || param != "orbit" {
            return false;
        }
        let mut found = false;
        visit(&graph.nodes, &mut |node| {
            found |= &node.node_id == node_id && node.type_id == "node.transform_mesh_patches";
        });
        found
    })
}

fn control_present(graph: &EffectGraphDef, suffix: &str) -> bool {
    let Some(metadata) = &graph.preset_metadata else {
        return false;
    };
    let id = format!("{CONTROL_PREFIX}{suffix}");
    let nid = control_node_id(suffix);
    let mut matches = 0;
    visit(&graph.nodes, &mut |node| {
        matches += usize::from(node.node_id == nid)
    });
    metadata.params.iter().filter(|param| param.id == id).count() == 1
        && metadata.bindings.iter().filter(|binding| binding.id == id).count() == 1
        && metadata.bindings.iter().any(|binding| binding.id == id
            && binding.convert == ParamConvert::Float
            && matches!(&binding.target, BindingTarget::Node { node_id, param } if node_id == &nid && param == "value"))
        && matches == 1
        && graph.nodes.iter().any(|node| node.node_id == nid && node.type_id == "node.value"
            && matches!(node.params.get("value"), Some(crate::effect_graph_def::SerializedParamValue::Float { .. })))
}

/// Legacy carrier detection: a qualified recipe that still embeds the
/// per-modifier Math View section. Standalone instances never match.
pub fn has_legacy_math_view_controls(graph: &EffectGraphDef) -> bool {
    if is_math_view_recipe(graph) {
        return false;
    }
    legacy_eligible(graph)
        && CONTROLS
            .iter()
            .all(|(suffix, ..)| control_present(graph, suffix))
}

/// Ids of modifiers whose graphs still carry legacy embedded controls.
pub fn legacy_math_view_carriers(owner: &EffectGraphDef) -> Vec<NodeId> {
    owner
        .scene_modifiers
        .iter()
        .filter(|instance| has_legacy_math_view_controls(&instance.graph))
        .map(|instance| instance.id.clone())
        .collect()
}

/// Remove the embedded Math View section from a legacy carrier recipe:
/// `math_view_*` params and bindings plus the `__math_view_*` control nodes.
/// Returns whether anything changed. Host-side bindings are the caller's
/// responsibility (see `retarget_math_view_bindings` / `drop_math_view_bindings`).
pub fn strip_legacy_math_view_controls(graph: &mut EffectGraphDef) -> bool {
    if !has_legacy_math_view_controls(graph) {
        return false;
    }
    let is_control_param = |id: &str| id.starts_with(CONTROL_PREFIX);
    let is_control_node = |id: &NodeId| id.as_str().starts_with("__math_view_");
    if let Some(metadata) = graph.preset_metadata.as_mut() {
        metadata.params.retain(|param| !is_control_param(&param.id));
        metadata.bindings.retain(|binding| !is_control_param(&binding.id));
    }
    graph.nodes.retain(|node| !is_control_node(&node.node_id));
    true
}

/// Move one carrier's host Math View bindings onto the standalone instance.
/// Only bindings for params the view recipe declares are moved (legacy
/// `math_view_scope` has no standalone equivalent and stays for
/// `drop_math_view_bindings`). Host binding ids are left unchanged so
/// host-side values, animation and modulation keyed by id survive; only the
/// target changes. Returns the number of retargeted bindings.
pub fn retarget_math_view_bindings(
    owner: &mut EffectGraphDef,
    from: &NodeId,
    to: &NodeId,
) -> usize {
    let declared: std::collections::HashSet<&str> = owner
        .scene_modifiers
        .iter()
        .find(|instance| &instance.id == to)
        .and_then(|instance| instance.graph.preset_metadata.as_ref())
        .map(|metadata| {
            metadata
                .params
                .iter()
                .map(|param| param.id.as_str())
                .collect()
        })
        .unwrap_or_default();
    let Some(metadata) = owner.preset_metadata.as_mut() else {
        return 0;
    };
    let mut moved = 0;
    for binding in &mut metadata.bindings {
        let BindingTarget::SceneModifier { modifier_id, param_id } = &mut binding.target else {
            continue;
        };
        if modifier_id == from
            && param_id.starts_with(CONTROL_PREFIX)
            && declared.contains(param_id.as_str())
        {
            *modifier_id = to.clone();
            moved += 1;
        }
    }
    moved
}

/// Drop a secondary carrier's host Math View bindings and their orphaned host
/// params (the standalone instance already received the donor's). Returns the
/// removed host param ids so the caller can prune live instance state.
pub fn drop_math_view_bindings(owner: &mut EffectGraphDef, carrier: &NodeId) -> Vec<String> {
    let Some(metadata) = owner.preset_metadata.as_mut() else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    metadata.bindings.retain(|binding| {
        let drop = matches!(&binding.target, BindingTarget::SceneModifier { modifier_id, param_id }
            if modifier_id == carrier && param_id.starts_with(CONTROL_PREFIX));
        if drop {
            removed.push(binding.id.clone());
        }
        !drop
    });
    metadata.params.retain(|param| !removed.contains(&param.id));
    removed
}

/// The scenes that contain at least one legacy carrier, in chain order.
pub fn legacy_math_view_scenes(owner: &EffectGraphDef) -> Vec<SceneNodeRef> {
    let mut scenes: Vec<SceneNodeRef> = Vec::new();
    for instance in &owner.scene_modifiers {
        if has_legacy_math_view_controls(&instance.graph) && !scenes.contains(&instance.scene) {
            scenes.push(instance.scene.clone());
        }
    }
    scenes
}

/// Static Connect to Mesh support for a standalone Math View instance:
/// exactly one preceding modifier in the same scene may carry a reference
/// patch transform, and it must cover every object the view samples. The
/// compiler enforces the same rule at preparation (all-or-nothing); this is
/// the card's projection of it, so an unsupported chain shows the reason
/// instead of silently doing nothing.
pub fn math_view_connect_support(owner: &EffectGraphDef, view_id: &NodeId) -> Result<(), String> {
    let Some(position) = owner.scene_modifiers.iter().position(|m| &m.id == view_id) else {
        return Err("Math View modifier is not part of this chain".into());
    };
    let view = &owner.scene_modifiers[position];
    if view.mesh_frames.is_empty() {
        return Err("Math View has no sampled objects".into());
    }
    let qualified: Vec<&crate::scene_modifier_preset::SceneModifierInstanceDef> = owner.scene_modifiers
        [..position]
        .iter()
        .filter(|m| m.scene == view.scene)
        .filter(|m| {
            let mut found = false;
            visit(&m.graph.nodes, &mut |node| {
                found |= node.type_id == "node.transform_mesh_patches";
            });
            found
        })
        .collect();
    match qualified.len() {
        0 => Err("Connect to Mesh needs a patch-based modifier (like Vortex Fragments) earlier in the chain".into()),
        1 => {
            let carrier = qualified[0];
            let covered = view
                .mesh_frames
                .iter()
                .all(|frame| carrier.mesh_frames.iter().any(|f| f.target == frame.target));
            if covered {
                Ok(())
            } else {
                Err("The patch-based modifier does not cover every object Math View samples".into())
            }
        }
        _ => Err("Connect to Mesh is ambiguous with several patch-based modifiers in the chain".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_graph_def::{BindingDef, ParamSpecDef, SerializedParamValue};
    use crate::effects::ParamConvert;
    use crate::scene_modifier_preset::SceneModifierInstanceDef;

    fn control_entry(suffix: &str) -> (ParamSpecDef, BindingDef, crate::effect_graph_def::EffectGraphNode) {
        let id = format!("{CONTROL_PREFIX}{suffix}");
        let nid = control_node_id(suffix);
        (
            ParamSpecDef {
                id: id.clone(),
                name: suffix.into(),
                min: 0.0,
                max: 1.0,
                default_value: 0.0,
                section: Some("Math View".into()),
                ..Default::default()
            },
            BindingDef {
                id: id.clone(),
                label: suffix.into(),
                default_value: 0.0,
                target: BindingTarget::Node {
                    node_id: nid.clone(),
                    param: "value".into(),
                },
                convert: ParamConvert::Float,
                user_added: false,
                scale: 1.0,
                offset: 0.0,
                default_mirrors_node_param: false,
            },
            crate::effect_graph_def::EffectGraphNode {
                id: 0,
                node_id: nid,
                type_id: "node.value".into(),
                handle: Some(id),
                params: [("value".into(), SerializedParamValue::Float { value: 0.0 })].into(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: Default::default(),
                output_canvas_scales: Default::default(),
                group: None,
            },
        )
    }

    /// A minimal legacy carrier: qualified Vortex recipe plus every control.
    fn legacy_carrier() -> EffectGraphDef {
        let mut graph: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version":3,
            "presetMetadata":{"id":"VortexFragments","displayName":"Vortex","category":"Geometry","oscPrefix":"vortex","params":[],"bindings":[{"id":"orbit","label":"Orbit","defaultValue":1,"target":{"kind":"node","nodeId":"patch","param":"orbit"}}]},
            "nodes":[{"id":1,"nodeId":"stage","typeId":"group","group":{"interface":{"inputs":[],"outputs":[]},"nodes":[{"id":2,"nodeId":"patch","typeId":"node.transform_mesh_patches","params":{"orbit":{"type":"Float","value":1}}}],"wires":[]}}],"wires":[]
        }))
        .unwrap();
        let mut next = 10;
        for (suffix, ..) in CONTROLS {
            let (param, binding, mut node) = control_entry(suffix);
            node.id = next;
            next += 1;
            let metadata = graph.preset_metadata.as_mut().unwrap();
            metadata.params.push(param);
            metadata.bindings.push(binding);
            graph.nodes.push(node);
        }
        graph
    }

    fn owner_with_carrier() -> EffectGraphDef {
        let mut owner: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version":3,
            "presetMetadata":{"id":"Host","displayName":"Host","category":"Geometry","oscPrefix":"host","params":[],"bindings":[]},
            "nodes":[],"wires":[]
        }))
        .unwrap();
        owner.scene_modifiers.push(SceneModifierInstanceDef {
            id: NodeId::new("vortex_a"),
            scene: SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") },
            targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
            mesh_frames: Vec::new(),
            graph: Box::new(legacy_carrier()),
        });
        owner
    }

    #[test]
    fn legacy_carrier_detected_and_stripped() {
        let carrier = legacy_carrier();
        assert!(has_legacy_math_view_controls(&carrier));
        assert!(!is_math_view_recipe(&carrier));

        let mut stripped = carrier.clone();
        assert!(strip_legacy_math_view_controls(&mut stripped));
        assert!(!has_legacy_math_view_controls(&stripped));
        // Strip is idempotent and keeps the authored deformation intact.
        assert!(!strip_legacy_math_view_controls(&mut stripped));
        let metadata = stripped.preset_metadata.as_ref().unwrap();
        assert!(metadata.bindings.iter().any(|binding| binding.id == "orbit"));
        assert!(metadata.params.is_empty());
        assert!(stripped.nodes.iter().any(|node| node.node_id == "stage"));
        assert!(!stripped.nodes.iter().any(|node| node.node_id.as_str().starts_with("__math_view_")));
    }

    #[test]
    fn standalone_recipe_is_never_a_legacy_carrier() {
        let mut recipe: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version":3,
            "presetMetadata":{"id":"MathView","displayName":"Math View","category":"Geometry","oscPrefix":"mathview","params":[],"bindings":[],"sceneModifier":{"schemaVersion":1,"singleton":true,"enabledParam":"enabled"}},
            "nodes":[],"wires":[]
        }))
        .unwrap();
        assert!(is_math_view_recipe(&recipe));
        assert!(!has_legacy_math_view_controls(&recipe));
        // Even with control-shaped nodes present, the standalone recipe is not stripped.
        let (param, binding, node) = control_entry("mode");
        {
            let metadata = recipe.preset_metadata.as_mut().unwrap();
            metadata.params.push(param);
            metadata.bindings.push(binding);
            recipe.nodes.push(node);
        }
        assert!(!strip_legacy_math_view_controls(&mut recipe));
    }

    #[test]
    fn carriers_and_scenes_listed_in_chain_order() {
        let mut owner = owner_with_carrier();
        owner.scene_modifiers.push(SceneModifierInstanceDef {
            id: NodeId::new("plain"),
            scene: SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") },
            targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
            mesh_frames: Vec::new(),
            graph: Box::new(serde_json::from_value(serde_json::json!({
                "version":3,
                "presetMetadata":{"id":"Other","displayName":"Other","category":"Geometry","oscPrefix":"other","params":[],"bindings":[]},
                "nodes":[],"wires":[]
            })).unwrap()),
        });
        owner.scene_modifiers.push(SceneModifierInstanceDef {
            id: NodeId::new("vortex_b"),
            scene: SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") },
            targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
            mesh_frames: Vec::new(),
            graph: Box::new(legacy_carrier()),
        });
        assert_eq!(
            legacy_math_view_carriers(&owner),
            vec![NodeId::new("vortex_a"), NodeId::new("vortex_b")]
        );
        assert_eq!(legacy_math_view_scenes(&owner).len(), 1);
    }

    #[test]
    fn retarget_moves_declared_control_bindings_and_drop_removes_rest() {
        let mut owner = owner_with_carrier();
        let view = NodeId::new("math_view");
        // The standalone instance declares every control except the dropped scope.
        let mut view_graph: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version":3,
            "presetMetadata":{"id":"MathView","displayName":"Math View","category":"Geometry","oscPrefix":"mathview","params":[],"bindings":[],"sceneModifier":{"schemaVersion":1,"singleton":true,"enabledParam":"enabled"}},
            "nodes":[],"wires":[]
        }))
        .unwrap();
        for (suffix, ..) in CONTROLS {
            view_graph
                .preset_metadata
                .as_mut()
                .unwrap()
                .params
                .push(ParamSpecDef {
                    id: format!("{CONTROL_PREFIX}{suffix}"),
                    name: (*suffix).into(),
                    ..Default::default()
                });
        }
        owner.scene_modifiers.push(SceneModifierInstanceDef {
            id: view.clone(),
            scene: SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") },
            targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
            mesh_frames: Vec::new(),
            graph: Box::new(view_graph),
        });
        {
            let metadata = owner.preset_metadata.as_mut().unwrap();
            for suffix in CONTROLS.iter().map(|(suffix, ..)| *suffix).chain(["scope"]) {
                let param_id = format!("{CONTROL_PREFIX}{suffix}");
                metadata.params.push(ParamSpecDef {
                    id: format!("sceneModifier:[\"vortex_a\",\"{param_id}\"]"),
                    name: (*suffix).into(),
                    ..Default::default()
                });
                metadata.bindings.push(BindingDef {
                    id: format!("sceneModifier:[\"vortex_a\",\"{param_id}\"]"),
                    label: (*suffix).into(),
                    default_value: 0.0,
                    target: BindingTarget::SceneModifier {
                        modifier_id: NodeId::new("vortex_a"),
                        param_id: param_id.clone(),
                    },
                    convert: ParamConvert::Float,
                    user_added: false,
                    scale: 1.0,
                    offset: 0.0,
                    default_mirrors_node_param: false,
                });
            }
        }
        let moved = retarget_math_view_bindings(&mut owner, &NodeId::new("vortex_a"), &view);
        assert_eq!(moved, CONTROLS.len(), "scope stays behind for dropping");
        let removed = drop_math_view_bindings(&mut owner, &NodeId::new("vortex_a"));
        assert_eq!(
            removed,
            vec!["sceneModifier:[\"vortex_a\",\"math_view_scope\"]".to_string()]
        );
        let metadata = owner.preset_metadata.as_ref().unwrap();
        assert!(metadata.bindings.iter().all(|binding| matches!(&binding.target,
            BindingTarget::SceneModifier { modifier_id, .. } if modifier_id == &view)));
        assert!(!metadata.params.iter().any(|p| p.id == removed[0]));

        // A second carrier's bindings drop with their host params.
        let metadata = owner.preset_metadata.as_mut().unwrap();
        metadata.params.push(ParamSpecDef {
            id: "sceneModifier:[\"vortex_b\",\"math_view_mode\"]".into(),
            name: "Mode".into(),
            ..Default::default()
        });
        metadata.bindings.push(BindingDef {
            id: "sceneModifier:[\"vortex_b\",\"math_view_mode\"]".into(),
            label: "Mode".into(),
            default_value: 0.0,
            target: BindingTarget::SceneModifier {
                modifier_id: NodeId::new("vortex_b"),
                param_id: "math_view_mode".into(),
            },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        });
        let removed = drop_math_view_bindings(&mut owner, &NodeId::new("vortex_b"));
        assert_eq!(removed, vec!["sceneModifier:[\"vortex_b\",\"math_view_mode\"]".to_string()]);
        let metadata = owner.preset_metadata.as_ref().unwrap();
        assert!(!metadata.params.iter().any(|p| p.id == removed[0]));
        assert!(!metadata.bindings.iter().any(|b| b.id == removed[0]));
    }

    #[test]
    fn connect_support_tracks_the_preceding_chain() {
        use crate::scene_modifier_preset::SceneMeshReferenceFrame;
        let frame = |target: &str| SceneMeshReferenceFrame {
            target: SceneNodeRef { scope: Vec::new(), node: NodeId::new(target) },
            source: SceneNodeRef { scope: Vec::new(), node: NodeId::new("src") },
            source_definition_hash: "hash".into(),
            source_offset: [0.0; 3],
            scene_radius: 1.0,
        };
        let view_graph: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version":3,
            "presetMetadata":{"id":"MathView","displayName":"Math View","category":"Geometry","oscPrefix":"mathview","params":[],"bindings":[],"sceneModifier":{"schemaVersion":1,"singleton":true,"enabledParam":"enabled"}},
            "nodes":[],"wires":[]
        }))
        .unwrap();
        let view = SceneModifierInstanceDef {
            id: NodeId::new("math_view"),
            scene: SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") },
            targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
            mesh_frames: vec![frame("object")],
            graph: Box::new(view_graph),
        };
        let mut owner = owner_with_carrier();
        // Carrier stripped of controls keeps its patch transform.
        let carrier_graph = {
            let mut graph = legacy_carrier();
            assert!(strip_legacy_math_view_controls(&mut graph));
            graph
        };
        *owner.scene_modifiers[0].graph = carrier_graph.clone();
        owner.scene_modifiers[0].mesh_frames = vec![frame("object")];

        // No view yet.
        assert!(math_view_connect_support(&owner, &NodeId::new("math_view")).is_err());
        owner.scene_modifiers.push(view.clone());
        // One qualified preceding carrier covering the sampled object.
        assert!(math_view_connect_support(&owner, &NodeId::new("math_view")).is_ok());

        // A second patch carrier makes connection ambiguous.
        owner.scene_modifiers.insert(1, SceneModifierInstanceDef {
            id: NodeId::new("vortex_b"),
            scene: SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") },
            targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
            mesh_frames: vec![frame("object")],
            graph: Box::new(carrier_graph.clone()),
        });
        let reason = math_view_connect_support(&owner, &NodeId::new("math_view")).unwrap_err();
        assert!(reason.contains("ambiguous"), "{reason}");
        owner.scene_modifiers.remove(1);

        // Carrier without coverage of the sampled object is unsupported.
        owner.scene_modifiers[0].mesh_frames = vec![frame("other_object")];
        let reason = math_view_connect_support(&owner, &NodeId::new("math_view")).unwrap_err();
        assert!(reason.contains("does not cover"), "{reason}");
        owner.scene_modifiers[0].mesh_frames = vec![frame("object")];

        // An instances-only carrier (SpatialEchoes shape: echo nodes, no patch
        // transform) is not a qualified carrier; connect stays locked with the
        // reason instead of silently partially connecting (BUG-uvts).
        let echo_only: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version":3,
            "presetMetadata":{"id":"SpatialEchoes","displayName":"Spatial Echoes","category":"Geometry","oscPrefix":"spatialechoes","params":[],"bindings":[],"sceneModifier":{"schemaVersion":1,"singleton":false,"enabledParam":"enabled"}},
            "nodes":[{"id":1,"nodeId":"echo","typeId":"node.analytic_echo_instances"}],
            "wires":[]
        }))
        .unwrap();
        let original_carrier = (*owner.scene_modifiers[0].graph).clone();
        *owner.scene_modifiers[0].graph = echo_only.clone();
        let reason = math_view_connect_support(&owner, &NodeId::new("math_view")).unwrap_err();
        assert!(reason.contains("patch-based modifier"), "{reason}");
        // Echoes mixed after a real patch carrier neither enable nor break
        // connect: exactly one qualified carrier still resolves.
        *owner.scene_modifiers[0].graph = original_carrier;
        owner.scene_modifiers.insert(1, SceneModifierInstanceDef {
            id: NodeId::new("spatial_echoes"),
            scene: SceneNodeRef { scope: Vec::new(), node: NodeId::new("scene") },
            targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
            mesh_frames: vec![frame("object")],
            graph: Box::new(echo_only),
        });
        assert!(math_view_connect_support(&owner, &NodeId::new("math_view")).is_ok());
        owner.scene_modifiers.remove(1);

        // A view ahead of the carrier sees no qualified preceding modifier.
        let mut reordered = owner.clone();
        let view = reordered.scene_modifiers.pop().unwrap();
        reordered.scene_modifiers.insert(0, view);
        let reason = math_view_connect_support(&reordered, &NodeId::new("math_view")).unwrap_err();
        assert!(reason.contains("earlier in the chain"), "{reason}");
    }
}
