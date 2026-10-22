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

/// Controls that existed in every shipped legacy snapshot, from the initial
/// native Math View through the last pre-standalone recipe. Later controls
/// (brightness levels, pulse, scan, connect, axes, occlusion) are optional
/// for detection: the standalone instance carries their current defaults and
/// `reconcile_scene_modifier_parameters` mints the missing host bindings.
const CORE_CONTROLS: &[&str] = &[
    "mode",
    "scope",
    "grid",
    "fragments",
    "ghosts",
    "vectors",
    "trails",
    "density",
    "line_width",
    "geometry_hue",
    "path_hue",
];

/// Legacy carrier detection: a qualified recipe that still embeds the
/// per-modifier Math View section. Standalone instances never match. The
/// required core subset keeps historical projects (saved before later
/// controls existed) recognisable; strip and retarget are prefix-based and
/// handle any partial set.
pub fn has_legacy_math_view_controls(graph: &EffectGraphDef) -> bool {
    if is_math_view_recipe(graph) {
        return false;
    }
    legacy_eligible(graph)
        && CORE_CONTROLS
            .iter()
            .all(|suffix| control_present(graph, suffix))
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
    strip_controls(graph, |_| true);
    true
}

/// Controls a saved Math View may still carry that no longer exist: Pulse,
/// Scan and Connect to Mesh (removed 2026-10-13) and the legacy Scope.
pub const REMOVED_CONTROLS: &[&str] = &[
    "pulse",
    "pulse_strength",
    "pulse_target",
    "pulse_beats",
    "pulse_trigger",
    "scan_amount",
    "scan_progress",
    "scan_width",
    "scan_direction",
    "scan_mode",
    "scan_target",
    "scan_beats",
    "scan_trigger",
    "connect_mesh",
    "scope",
];

fn is_removed_control(suffix: &str) -> bool {
    REMOVED_CONTROLS.contains(&suffix)
}

/// Remove `math_view_<suffix>` params and bindings and `__math_view_<suffix>`
/// nodes (and any wire touching them) for every suffix `strip` accepts.
/// Returns whether anything was removed.
fn strip_controls(graph: &mut EffectGraphDef, strip: impl Fn(&str) -> bool) -> bool {
    let param = |id: &str| id.strip_prefix(CONTROL_PREFIX).is_some_and(&strip);
    let node = |id: &NodeId| id.as_str().strip_prefix("__math_view_").is_some_and(&strip);
    let before = (graph.nodes.len(), graph.wires.len());
    let mut changed = false;
    if let Some(metadata) = graph.preset_metadata.as_mut() {
        let counts = (metadata.params.len(), metadata.bindings.len());
        metadata.params.retain(|spec| !param(&spec.id));
        metadata.bindings.retain(|binding| !param(&binding.id));
        changed |= counts != (metadata.params.len(), metadata.bindings.len());
    }
    let removed: std::collections::HashSet<u32> = graph
        .nodes
        .iter()
        .filter(|n| node(&n.node_id))
        .map(|n| n.id)
        .collect();
    graph.nodes.retain(|n| !removed.contains(&n.id));
    graph
        .wires
        .retain(|w| !removed.contains(&w.from_node) && !removed.contains(&w.to_node));
    changed || before != (graph.nodes.len(), graph.wires.len())
}

/// Load-time upgrade of a saved standalone Math View instance: drop the
/// removed controls and put the params in the bundled card order (Enabled,
/// then `CONTROLS`), so Display is one contiguous section. Idempotent;
/// returns whether anything changed. Host bindings are the caller's job
/// (`drop_removed_math_view_bindings`).
pub fn upgrade_math_view_instance(graph: &mut EffectGraphDef) -> bool {
    if !is_math_view_recipe(graph) {
        return false;
    }
    let mut changed = strip_controls(graph, is_removed_control);
    let Some(metadata) = graph.preset_metadata.as_mut() else {
        return changed;
    };
    let rank = |id: &str| -> usize {
        id.strip_prefix(CONTROL_PREFIX)
            .and_then(|suffix| CONTROLS.iter().position(|(s, ..)| *s == suffix))
            .map_or(if id.starts_with(CONTROL_PREFIX) { CONTROLS.len() + 1 } else { 0 }, |at| at + 1)
    };
    let order: Vec<String> = metadata.params.iter().map(|p| p.id.clone()).collect();
    metadata.params.sort_by_key(|spec| rank(&spec.id));
    changed |= metadata.params.iter().map(|p| &p.id).ne(order.iter());
    changed
}
/// Move one carrier's host Math View bindings onto the standalone instance.
/// Only bindings for params the view recipe declares are moved. Host binding
/// ids are left unchanged so host-side values, animation and modulation keyed
/// by id survive; only the target changes. Returns the number moved.
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
    drop_host_bindings(owner, carrier, |_| true)
}

/// Drop a view's host bindings and host params for the removed controls.
/// Returns the removed host param ids so the caller can prune animation and
/// modulation keyed by them.
pub fn drop_removed_math_view_bindings(owner: &mut EffectGraphDef, view: &NodeId) -> Vec<String> {
    drop_host_bindings(owner, view, is_removed_control)
}

fn drop_host_bindings(
    owner: &mut EffectGraphDef,
    modifier: &NodeId,
    strip: impl Fn(&str) -> bool,
) -> Vec<String> {
    let Some(metadata) = owner.preset_metadata.as_mut() else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    metadata.bindings.retain(|binding| {
        let drop = matches!(&binding.target, BindingTarget::SceneModifier { modifier_id, param_id }
            if modifier_id == modifier && param_id.strip_prefix(CONTROL_PREFIX).is_some_and(&strip));
        if drop {
            removed.push(binding.id.clone());
        }
        !drop
    });
    metadata.params.retain(|param| !removed.contains(&param.id));
    removed
}

/// The scenes that contain at least one legacy carrier, in chain order.
pub fn legacy_math_view_scenes(owner: &EffectGraphDef) -> Vec<NodeId> {
    let mut scenes: Vec<NodeId> = Vec::new();
    for instance in &owner.scene_modifiers {
        if has_legacy_math_view_controls(&instance.graph) && !scenes.contains(&instance.scene) {
            scenes.push(instance.scene.clone());
        }
    }
    scenes
}

/// Whether a legacy carrier carries authored Math View content that merits its
/// own standalone instance: an embedded control value off its default, host
/// base values off their binding defaults, or host-side animation or
/// modulation (drivers, envelopes, Ableton mappings, audio mods, automation
/// lanes) touching its `math_view_*` bindings. Carriers failing every check
/// have default, inactive content and are stripped cleanly by load migration
/// instead — an enabled-but-untouched modifier is not authored.
pub fn carrier_has_authored_math_view_content(
    host: &crate::effects::PresetInstance,
    carrier_id: &NodeId,
) -> bool {
    let Some(graph) = host.graph.as_ref() else {
        return false;
    };
    let Some(instance) = graph
        .scene_modifiers
        .iter()
        .find(|modifier| &modifier.id == carrier_id)
    else {
        return false;
    };
    // Embedded node values off the control defaults (covers saves whose host
    // bindings were never minted for every control).
    let defaults: std::collections::HashMap<&str, f32> = CONTROLS
        .iter()
        .map(|(suffix, _, default, ..)| (*suffix, *default))
        .chain([("scope", 1.0)])
        .collect();
    let mut embedded_authored = false;
    visit(&instance.graph.nodes, &mut |node| {
        let Some(suffix) = node.node_id.as_str().strip_prefix("__math_view_") else {
            return;
        };
        let Some(default) = defaults.get(suffix) else {
            return;
        };
        if let Some(crate::effect_graph_def::SerializedParamValue::Float { value }) =
            node.params.get("value")
        {
            embedded_authored |= value != default;
        }
    });
    if embedded_authored {
        return true;
    }
    let Some(metadata) = graph.preset_metadata.as_ref() else {
        return false;
    };
    let control_ids: std::collections::HashSet<&str> = metadata
        .bindings
        .iter()
        .filter(|binding| {
            matches!(
                &binding.target,
                BindingTarget::SceneModifier { modifier_id, param_id }
                    if modifier_id == carrier_id && param_id.starts_with(CONTROL_PREFIX)
            )
        })
        .map(|binding| binding.id.as_str())
        .collect();
    // Host-side animation or modulation keyed by those binding ids.
    macro_rules! motion_references {
        ($field:ident) => {
            if let Some(entries) = host.$field.as_ref() {
                if entries
                    .iter()
                    .any(|entry| control_ids.contains(entry.param_id.as_ref()))
                {
                    return true;
                }
            }
        };
    }
    motion_references!(drivers);
    motion_references!(envelopes);
    motion_references!(ableton_mappings);
    motion_references!(audio_mods);
    motion_references!(automation_lanes);
    // Host base values off their binding defaults.
    for binding in metadata
        .bindings
        .iter()
        .filter(|binding| control_ids.contains(binding.id.as_str()))
    {
        if host.get_base_param(&binding.id) != binding.default_value {
            return true;
        }
    }
    false
}

/// The carrier's embedded `__math_view_*` control node values, suffix → value.
/// These are the values a save carried when no host binding was minted for a
/// control; load migration copies them onto the standalone view so they are
/// not lost (host bindings still win where they exist).
pub fn legacy_embedded_control_values(graph: &EffectGraphDef) -> std::collections::BTreeMap<String, f32> {
    let mut values = std::collections::BTreeMap::new();
    visit(&graph.nodes, &mut |node| {
        let Some(suffix) = node.node_id.as_str().strip_prefix("__math_view_") else {
            return;
        };
        if let Some(crate::effect_graph_def::SerializedParamValue::Float { value }) =
            node.params.get("value")
        {
            values.insert(suffix.to_string(), *value);
        }
    });
    values
}

/// Suffixes of `math_view_*` controls that hold a host binding on this
/// carrier. Only those suffixes have host-side values, animation or
/// modulation; every other embedded value is carried by the node alone.
pub fn legacy_bound_control_suffixes(
    owner: &EffectGraphDef,
    carrier: &NodeId,
) -> std::collections::HashSet<String> {
    owner
        .preset_metadata
        .as_ref()
        .map(|metadata| {
            metadata
                .bindings
                .iter()
                .filter(|binding| {
                    matches!(
                        &binding.target,
                        BindingTarget::SceneModifier { modifier_id, param_id }
                            if modifier_id == carrier && param_id.starts_with(CONTROL_PREFIX)
                    )
                })
                .filter_map(|binding| {
                    match &binding.target {
                        BindingTarget::SceneModifier { param_id, .. } => param_id
                            .strip_prefix(CONTROL_PREFIX)
                            .map(str::to_string),
                        _ => None,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether an existing standalone view may absorb a legacy carrier's section
/// instead of the migration appending a fresh view: the view must be the
/// Math View recipe, sit immediately after the carrier in the same-scene
/// chain, sample the same objects (targets and mesh frames), carry only
/// default Math View content, and hold no host `math_view_*` bindings of its
/// own (retargeted carrier bindings would otherwise target the same params
/// twice; with no bindings, no animation or modulation can reference the view
/// either). Anything else appends a fresh view so an authored view is never
/// clobbered and the carrier's chain position is never changed.
pub fn reusable_math_view_for_carrier(
    owner: &EffectGraphDef,
    carrier: &NodeId,
    view: &NodeId,
) -> bool {
    let Some(view_instance) = owner.scene_modifiers.iter().find(|m| &m.id == view) else {
        return false;
    };
    if !is_math_view_recipe(&view_instance.graph) {
        return false;
    }
    // Same-scene chain order: the view must immediately follow the carrier.
    let chain: Vec<&NodeId> = owner
        .scene_modifiers
        .iter()
        .filter(|m| m.scene == view_instance.scene)
        .map(|m| &m.id)
        .collect();
    let carrier_at = chain.iter().position(|id| *id == carrier);
    if !matches!(carrier_at, Some(at) if chain.get(at + 1) == Some(&view)) {
        return false;
    }
    // The view must sample the same objects the carrier deforms: a default
    // view over different frames would absorb the carrier's bindings while
    // its connected coverage fails on the wrong geometry — the silent
    // connection loss this migration exists to prevent.
    let Some(carrier_instance) = owner.scene_modifiers.iter().find(|m| &m.id == carrier) else {
        return false;
    };
    if view_instance.mesh_frames != carrier_instance.mesh_frames
        || view_instance.targets != carrier_instance.targets
    {
        return false;
    }
    // Only default content: every embedded control node at its default.
    let defaults: std::collections::HashMap<&str, f32> = CONTROLS
        .iter()
        .map(|(suffix, _, default, ..)| (*suffix, *default))
        .collect();
    let mut default_content = true;
    visit(&view_instance.graph.nodes, &mut |node| {
        let Some(suffix) = node.node_id.as_str().strip_prefix("__math_view_") else {
            return;
        };
        if let (Some(default), Some(crate::effect_graph_def::SerializedParamValue::Float { value })) =
            (defaults.get(suffix), node.params.get("value"))
        {
            default_content &= value == default;
        }
    });
    if !default_content {
        return false;
    }
    // No host Math View bindings of its own: a retargeted carrier binding
    // would target the same (view, param) a second time.
    if !legacy_bound_control_suffixes(owner, view).is_empty() {
        return false;
    }
    true
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
    fn legacy_carrier() -> EffectGraphDef {        let mut graph: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version":3,
            "presetMetadata":{"id":"VortexFragments","displayName":"Vortex","category":"Geometry","oscPrefix":"vortex","params":[],"bindings":[{"id":"orbit","label":"Orbit","defaultValue":1,"target":{"kind":"node","nodeId":"patch","param":"orbit"}}]},
            "nodes":[{"id":1,"nodeId":"stage","typeId":"group","group":{"interface":{"inputs":[],"outputs":[]},"nodes":[{"id":2,"nodeId":"patch","typeId":"node.transform_mesh_patches","params":{"orbit":{"type":"Float","value":1}}}],"wires":[]}}],"wires":[]
        }))
        .unwrap();
        let mut next = 10;
        for suffix in CONTROLS
            .iter()
            .map(|(suffix, ..)| *suffix)
            .chain(["scope"])
        {
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
            scene: NodeId::new("scene"),
            targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
            mesh_frames: Vec::new(),
            legacy_math_view_carrier: None,
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
            scene: NodeId::new("scene"),
            targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
            mesh_frames: Vec::new(),
            legacy_math_view_carrier: None,
            graph: Box::new(serde_json::from_value(serde_json::json!({
                "version":3,
                "presetMetadata":{"id":"Other","displayName":"Other","category":"Geometry","oscPrefix":"other","params":[],"bindings":[]},
                "nodes":[],"wires":[]
            })).unwrap()),
        });
        owner.scene_modifiers.push(SceneModifierInstanceDef {
            id: NodeId::new("vortex_b"),
            scene: NodeId::new("scene"),
            targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
            mesh_frames: Vec::new(),
            legacy_math_view_carrier: None,
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
            scene: NodeId::new("scene"),
            targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
            mesh_frames: Vec::new(),
            legacy_math_view_carrier: None,
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

    // Real bundled snapshots from every legacy era: the initial native Math
    // View, the connected-events era, and the depth-occlusion era. Each lacks
    // controls that did not exist yet, so they only detect via the core
    // subset.
    #[test]
    fn historical_partial_control_sets_are_detected() {
        let cases: &[(&str, &str, &[&str])] = &[
            (
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/math-view-legacy/vortex-fragments-initial-ce78a59d0.json"
                ),
                "initial",
                &["occlusion", "axes", "connect_mesh", "pulse", "scan_amount"],
            ),
            (
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/math-view-legacy/vortex-fragments-events-96c78f522.json"
                ),
                "events",
                &["axes"],
            ),
            (
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/math-view-legacy/vortex-fragments-occlusion-9f453beb0.json"
                ),
                "occlusion",
                &["axes"],
            ),
        ];
        for (path, era, absent) in cases {
            let json = std::fs::read_to_string(path).expect("historical fixture readable");
            let graph: EffectGraphDef =
                serde_json::from_str(&json).expect("historical fixture parses");
            assert!(
                has_legacy_math_view_controls(&graph),
                "{era} snapshot must detect as a legacy carrier"
            );
            assert!(
                !is_math_view_recipe(&graph),
                "{era} snapshot is not the standalone recipe"
            );
            for suffix in *absent {
                assert!(
                    !control_present(&graph, suffix),
                    "{era} snapshot predates math_view_{suffix}"
                );
            }
            // Migration strips whatever embedded section exists, partial or not.
            let mut stripped = graph.clone();
            assert!(
                strip_legacy_math_view_controls(&mut stripped),
                "{era} snapshot strips"
            );
            assert!(!has_legacy_math_view_controls(&stripped));
            assert!(
                !strip_legacy_math_view_controls(&mut stripped),
                "{era} strip is idempotent"
            );
        }
    }

    /// Carrier with embedded control nodes sitting at their defaults.
    fn legacy_carrier_at_defaults() -> EffectGraphDef {
        let mut graph: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version":3,
            "presetMetadata":{"id":"VortexFragments","displayName":"Vortex","category":"Geometry","oscPrefix":"vortex","params":[],"bindings":[{"id":"orbit","label":"Orbit","defaultValue":1,"target":{"kind":"node","nodeId":"patch","param":"orbit"}}]},
            "nodes":[{"id":1,"nodeId":"stage","typeId":"group","group":{"interface":{"inputs":[],"outputs":[]},"nodes":[{"id":2,"nodeId":"patch","typeId":"node.transform_mesh_patches","params":{"orbit":{"type":"Float","value":1}}}],"wires":[]}}],"wires":[]
        }))
        .unwrap();
        let mut next = 10;
        for (suffix, default) in CONTROLS
            .iter()
            .map(|(suffix, _, default, ..)| (*suffix, *default))
            .chain([("scope", 1.0)])
        {
            let (param, binding, mut node) = control_entry(suffix);
            node.id = next;
            next += 1;
            node.params.insert(
                "value".into(),
                SerializedParamValue::Float { value: default },
            );
            let metadata = graph.preset_metadata.as_mut().unwrap();
            metadata.params.push(param);
            metadata.bindings.push(binding);
            graph.nodes.push(node);
        }
        graph
    }

    fn host_with_carrier(carrier: EffectGraphDef) -> crate::effects::PresetInstance {
        let mut owner: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version":3,
            "presetMetadata":{"id":"Host","displayName":"Host","category":"Geometry","oscPrefix":"host","params":[],"bindings":[]},
            "nodes":[],"wires":[]
        }))
        .unwrap();
        owner.scene_modifiers.push(SceneModifierInstanceDef {
            id: NodeId::new("vortex_a"),
            scene: NodeId::new("scene"),
            targets: crate::scene_modifier_preset::SceneTargetSelection::AllObjects,
            mesh_frames: Vec::new(),
            legacy_math_view_carrier: None,
            graph: Box::new(carrier),
        });
        let mut host = crate::effects::PresetInstance::new(crate::PresetTypeId::new("Host"));
        host.graph = Some(owner);
        host.refresh_manifest_from_graph();
        host
    }

    fn push_host_binding(
        host: &mut crate::effects::PresetInstance,
        local: &str,
        default: f32,
    ) -> String {
        let macro_id = format!(
            "sceneModifier:{}",
            serde_json::to_string(&("vortex_a", local)).unwrap()
        );
        let graph = host.graph.as_mut().unwrap();
        let metadata = graph.preset_metadata.as_mut().unwrap();
        metadata.params.push(ParamSpecDef {
            id: macro_id.clone(),
            name: local.into(),
            default_value: default,
            ..Default::default()
        });
        metadata.bindings.push(BindingDef {
            id: macro_id.clone(),
            label: local.into(),
            default_value: default,
            target: BindingTarget::SceneModifier {
                modifier_id: NodeId::new("vortex_a"),
                param_id: local.into(),
            },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        });
        host.params.push(crate::params::Param::user_added(ParamSpecDef {
            id: macro_id.clone(),
            name: local.into(),
            default_value: default,
            ..Default::default()
        }));
        host.refresh_manifest_from_graph();
        macro_id
    }

    #[test]
    fn authored_content_detection() {
        use crate::types::{BeatDivision, DriverWaveform};

        // All defaults and no enabled binding: nothing authored.
        let host = host_with_carrier(legacy_carrier_at_defaults());
        assert!(!carrier_has_authored_math_view_content(
            &host,
            &NodeId::new("vortex_a")
        ));

        // An embedded node value off its default is authored even without any
        // host bindings.
        let mut carrier = legacy_carrier_at_defaults();
        let density = carrier
            .nodes
            .iter_mut()
            .find(|node| node.node_id == "__math_view_density")
            .expect("density control node");
        density.params.insert(
            "value".into(),
            SerializedParamValue::Float { value: 5.0 },
        );
        let host = host_with_carrier(carrier);
        assert!(carrier_has_authored_math_view_content(
            &host,
            &NodeId::new("vortex_a")
        ));

        // The modifier's own enabled state is not Math View authorship: an
        // enabled-but-untouched carrier (enabled at its default) and a
        // disabled one both strip cleanly.
        let mut host = host_with_carrier(legacy_carrier_at_defaults());
        let enabled = push_host_binding(&mut host, "enabled", 1.0);
        host.set_base_param(&enabled, 1.0);
        assert!(!carrier_has_authored_math_view_content(
            &host,
            &NodeId::new("vortex_a")
        ));
        host.set_base_param(&enabled, 0.0);
        assert!(!carrier_has_authored_math_view_content(
            &host,
            &NodeId::new("vortex_a")
        ));

        // A host base value off its binding default is authored.
        let mut host = host_with_carrier(legacy_carrier_at_defaults());
        let mode = push_host_binding(&mut host, "math_view_mode", 0.0);
        host.set_base_param(&mode, 1.0);
        assert!(carrier_has_authored_math_view_content(
            &host,
            &NodeId::new("vortex_a")
        ));
        host.set_base_param(&mode, 0.0);
        assert!(!carrier_has_authored_math_view_content(
            &host,
            &NodeId::new("vortex_a")
        ));

        // Host-side animation or modulation touching a math_view binding is
        // authored even at the default value.
        let mut host = host_with_carrier(legacy_carrier_at_defaults());
        let mode = push_host_binding(&mut host, "math_view_mode", 0.0);
        host.drivers = Some(vec![crate::effects::ParameterDriver::new(
            crate::effects::ParamId::from(mode.clone()),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        )]);
        assert!(carrier_has_authored_math_view_content(
            &host,
            &NodeId::new("vortex_a")
        ));
        host.drivers = None;
        host.envelopes = Some(vec![crate::effects::ParamEnvelope::new(
            crate::effects::ParamId::from(mode),
        )]);
        assert!(carrier_has_authored_math_view_content(
            &host,
            &NodeId::new("vortex_a")
        ));
    }
}
