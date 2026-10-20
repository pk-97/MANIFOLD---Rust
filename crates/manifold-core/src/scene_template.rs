//! One data-only template and the one pure insertion every Add menu entry
//! shares (PHYSICS_ENGINE_BOUNDARY_DESIGN.md section 5.1).
//!
//! A template is a complete group node plus the card exposures of its own
//! nodes, all in template-local ids. Insertion gives every node, wire,
//! handle, stable id and exposure id fresh identity in the host graph once,
//! routes each declared output to the render scene's next free slot of its
//! port type, and merges the exposures. The result is one candidate graph:
//! nothing is written until the caller commits it.

use std::collections::{HashMap, HashSet};

use crate::NodeId;
use crate::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_TYPE_ID,
    PresetMetadata, RENDER_SCENE_TYPE_ID, SerializedParamValue,
};
use crate::scene_graph_edit::{SceneGraphEdit, SceneGraphEditError};
use crate::scene_modifier_preset::SceneNodeRef;

/// Where a template's handles and section titles take the inserted group's
/// numbered name ("Water 1").
pub const HANDLE_TOKEN: &str = "{handle}";

/// What a declared output feeds on the render scene.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    Object,
    Light,
}

impl SlotKind {
    fn of(port_type: &str) -> Option<Self> {
        match port_type {
            "SceneObject" | "Object" => Some(Self::Object),
            "Light" => Some(Self::Light),
            _ => None,
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Self::Object => "object",
            Self::Light => "light",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SceneTemplate {
    /// A complete ordinary group: body nodes and wires, every output
    /// declared on the interface. Handles may carry [`HANDLE_TOKEN`].
    pub group: EffectGraphNode,
    /// The exposures of the group's own nodes. `display_name` is the
    /// numbering prefix ("Water" gives "Water 1"); the other identity fields
    /// never replace the host's. Exposure ids and binding targets are in
    /// template-local ids.
    pub metadata: PresetMetadata,
}

fn fail(message: impl Into<String>) -> SceneGraphEditError {
    SceneGraphEditError { at: None, message: message.into() }
}

/// Node types that insert bare, with the output they route. A light stays a
/// plain node on the scene: a one-node group taxes every later edit.
fn bare_node_outputs(type_id: &str) -> Option<Vec<crate::effect_graph_def::InterfacePortDef>> {
    match type_id {
        "node.light" => Some(vec![crate::effect_graph_def::InterfacePortDef { name: "out".into(), port_type: "Light".into() }]),
        _ => None,
    }
}

fn max_doc_id(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|node| node.id.max(node.group.as_deref().map_or(0, |body| max_doc_id(&body.nodes))))
        .max()
        .unwrap_or(0)
}

fn collect_handles(nodes: &[EffectGraphNode], out: &mut HashSet<String>) {
    for node in nodes {
        if let Some(handle) = &node.handle {
            out.insert(handle.clone());
        }
        if let Some(body) = node.group.as_deref() {
            collect_handles(&body.nodes, out);
        }
    }
}

fn collect_stable_ids(nodes: &[EffectGraphNode], out: &mut HashSet<NodeId>) {
    for node in nodes {
        if !node.node_id.is_empty() {
            out.insert(node.node_id.clone());
        }
        if let Some(body) = node.group.as_deref() {
            collect_stable_ids(&body.nodes, out);
        }
    }
}

/// The level of `nodes`/`wires` a scene reference's scope names.
fn level_mut<'a>(
    nodes: &'a mut Vec<EffectGraphNode>,
    wires: &'a mut Vec<EffectGraphWire>,
    scope: &[NodeId],
) -> Option<(&'a mut Vec<EffectGraphNode>, &'a mut Vec<EffectGraphWire>)> {
    let Some((first, rest)) = scope.split_first() else {
        return Some((nodes, wires));
    };
    let group = nodes.iter_mut().find(|node| node.node_id == *first && node.type_id == GROUP_TYPE_ID)?;
    let body = group.group.as_deref_mut()?;
    level_mut(&mut body.nodes, &mut body.wires, rest)
}

fn whole_number(value: &SerializedParamValue) -> Option<u32> {
    let value = match value {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Int { value } => *value as f32,
        _ => return None,
    };
    (value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value < u32::MAX as f32).then_some(value as u32)
}

/// Identity assigned to every template node, one pass over the whole body.
struct Remap {
    docs: HashMap<u32, u32>,
    stable: HashMap<NodeId, NodeId>,
}

struct Allocator {
    next_doc: Option<u32>,
    taken_stable: HashSet<NodeId>,
}

impl Allocator {
    fn doc(&mut self) -> Result<u32, SceneGraphEditError> {
        let id = self.next_doc.ok_or_else(|| fail("Scene document id space is exhausted"))?;
        self.next_doc = id.checked_add(1);
        Ok(id)
    }

    fn stable(&mut self, prefix: &str, doc: u32) -> NodeId {
        let base = format!("{prefix}_{doc}");
        let mut candidate = NodeId::new(base.clone());
        let mut suffix = 2u32;
        while self.taken_stable.contains(&candidate) {
            candidate = NodeId::new(format!("{base}_{suffix}"));
            suffix += 1;
        }
        self.taken_stable.insert(candidate.clone());
        candidate
    }
}

/// Fresh identity for the template's group and everything under it. The
/// group takes its document id after its body (card controls keep the ids
/// authored flows address); inside a body a parent precedes its nested
/// nodes. Document ids are only unique within one body, so wires are
/// rewritten per body; `remap.docs` records the group and its own children,
/// the only nodes the template's exposure ids name.
fn renumber_group(
    group: &mut EffectGraphNode,
    alloc: &mut Allocator,
    remap: &mut Remap,
    handle: &str,
) -> Result<(), SceneGraphEditError> {
    let old = group.id;
    renumber_body(group, alloc, remap, handle, true)?;
    let fresh = alloc.doc()?;
    remap.docs.insert(old, fresh);
    restamp(group, fresh, alloc, remap, handle);
    Ok(())
}

fn restamp(node: &mut EffectGraphNode, fresh: u32, alloc: &mut Allocator, remap: &mut Remap, handle: &str) {
    let prefix = if node.node_id.is_empty() { node.type_id.replace('.', "_") } else { node.node_id.as_str().to_owned() };
    let stable = alloc.stable(&prefix, fresh);
    if !node.node_id.is_empty() {
        remap.stable.insert(node.node_id.clone(), stable.clone());
    }
    node.id = fresh;
    node.node_id = stable;
    node.handle = node.handle.take().map(|text| text.replace(HANDLE_TOKEN, handle));
}

/// Renumber every node in `node`'s body, rewrite the body's wires to match,
/// and descend.
fn renumber_body(
    node: &mut EffectGraphNode,
    alloc: &mut Allocator,
    remap: &mut Remap,
    handle: &str,
    record: bool,
) -> Result<(), SceneGraphEditError> {
    let Some(body) = node.group.as_deref_mut() else { return Ok(()) };
    let mut local: HashMap<u32, u32> = HashMap::new();
    for inner in &mut body.nodes {
        let fresh = alloc.doc()?;
        if local.insert(inner.id, fresh).is_some() {
            return Err(fail("Scene template reuses a document id within a group"));
        }
        if record {
            remap.docs.insert(inner.id, fresh);
        }
        restamp(inner, fresh, alloc, remap, handle);
        renumber_body(inner, alloc, remap, handle, false)?;
    }
    for wire in &mut body.wires {
        let (Some(from), Some(to)) = (local.get(&wire.from_node), local.get(&wire.to_node)) else {
            return Err(fail("Scene template wire leaves its group"));
        };
        wire.from_node = *from;
        wire.to_node = *to;
    }
    Ok(())
}

/// A template exposure id: `<node doc id>_<param>` follows its node to the
/// node's fresh document id; any other id is scoped under the group's.
fn remap_exposure_id(id: &str, remap: &Remap, group: u32) -> String {
    if let Some((head, rest)) = id.split_once('_')
        && let Ok(doc) = head.parse::<u32>()
        && let Some(fresh) = remap.docs.get(&doc)
    {
        return format!("{fresh}_{rest}");
    }
    format!("{group}_{id}")
}

fn remap_section(section: &str, handle: &str) -> String {
    section.replace(HANDLE_TOKEN, handle)
}

/// The first unused "<prefix> N".
fn numbered_handle(prefix: &str, taken: &HashSet<String>) -> String {
    (1u32..).map(|n| format!("{prefix} {n}")).find(|candidate| !taken.contains(candidate)).expect("unbounded range")
}

/// Insert `template` into the render scene `scene` names and route every
/// declared output to that scene's next free slot of its port type. Returns
/// one candidate graph, or the reason nothing could be built.
pub fn insert_scene_template(
    graph: &EffectGraphDef,
    scene: &SceneNodeRef,
    template: &SceneTemplate,
) -> Result<SceneGraphEdit, SceneGraphEditError> {
    let at = |message: &str| SceneGraphEditError { at: Some(scene.clone()), message: message.into() };
    // A bare node (a light) inserts as itself: no group, one declared output.
    let bare_outputs = bare_node_outputs(&template.group.type_id);
    if bare_outputs.is_none() && (template.group.type_id != GROUP_TYPE_ID || template.group.group.is_none()) {
        return Err(fail("Scene template is not a group"));
    }
    let mut candidate = graph.clone();
    let mut handles = HashSet::new();
    collect_handles(&candidate.nodes, &mut handles);
    let mut taken_stable = HashSet::new();
    collect_stable_ids(&candidate.nodes, &mut taken_stable);
    let mut alloc = Allocator { next_doc: max_doc_id(&candidate.nodes).checked_add(1), taken_stable };

    let numbering = if template.metadata.display_name.is_empty() { "Group" } else { template.metadata.display_name.as_str() };
    let handle = numbered_handle(numbering, &handles);

    // Identity first: the group, then every node under it.
    let mut group = template.group.clone();
    let mut remap = Remap { docs: HashMap::new(), stable: HashMap::new() };
    renumber_group(&mut group, &mut alloc, &mut remap, &handle)?;
    let group_doc = group.id;
    let domain = match crate::liquid_domain::liquid_domains_in(group.group.as_deref().map_or(&[][..], |body| body.nodes.as_slice())) {
        domains if domains.len() > 1 => return Err(fail("Scene template holds more than one liquid domain")),
        mut domains => domains.pop(),
    };
    if domain.is_some() && !scene.scope.is_empty() {
        return Err(at("A liquid inserts into the root scene"));
    }

    // Outputs, routed by port type.
    let outputs = match bare_outputs {
        Some(outputs) => outputs,
        None => group.group.as_deref().map(|body| body.interface.outputs.clone()).unwrap_or_default(),
    };
    let (nodes, wires) =
        level_mut(&mut candidate.nodes, &mut candidate.wires, &scene.scope).ok_or_else(|| at("Scene scope is unavailable"))?;
    let render = nodes
        .iter_mut()
        .find(|node| node.node_id == scene.node)
        .ok_or_else(|| at("Render scene is unavailable"))?;
    if render.type_id != RENDER_SCENE_TYPE_ID {
        return Err(at("Target is not a render scene"));
    }
    let render_doc = render.id;
    // Slot kinds in routing order: (port prefix, count param, next free slot).
    let mut slots: Vec<(&'static str, &'static str, u32)> = Vec::new();
    for (kind, (prefix, param)) in [(SlotKind::Object, ("object", "objects")), (SlotKind::Light, ("light", "lights"))] {
        let routed = outputs.iter().filter(|output| SlotKind::of(&output.port_type) == Some(kind)).count() as u32;
        if routed == 0 {
            continue;
        }
        let current = render.params.get(param).and_then(whole_number).ok_or_else(|| at("Render scene has an invalid slot count"))?;
        let total = current.checked_add(routed).ok_or_else(|| at("Render scene slot count is exhausted"))?;
        if let Some(limit) = crate::param_lookup::node_params(RENDER_SCENE_TYPE_ID)
            .iter()
            .find(|meta| meta.name == param)
            .map(|meta| meta.max)
            && total as f32 > limit
        {
            return Err(at("Render scene has no free slots"));
        }
        slots.push((prefix, param, current));
        render.params.insert(param.to_owned(), SerializedParamValue::Float { value: total as f32 });
    }
    for output in &outputs {
        let Some(kind) = SlotKind::of(&output.port_type) else { continue };
        let (prefix, _, next) = slots.iter_mut().find(|(prefix, ..)| *prefix == kind.prefix()).expect("counted above");
        let port = format!("{prefix}_{next}");
        if wires.iter().any(|wire| wire.to_node == render_doc && wire.to_port == port) {
            return Err(at("Render scene slot is occupied"));
        }
        wires.push(EffectGraphWire { from_node: group_doc, from_port: output.name.clone(), to_node: render_doc, to_port: port });
        *next += 1;
    }
    nodes.push(group);
    // A scene that had no Sun light gets its sky's sun from the first light.
    if slots.iter().any(|(prefix, ..)| *prefix == SlotKind::Light.prefix()) {
        crate::sky_sun::wire_sky_sun(nodes, wires);
    }

    // Exposures, merged by id.
    let host_meta = candidate.preset_metadata.as_mut();
    let Some(host_meta) = host_meta else {
        if template.metadata.params.is_empty() {
            return Ok(SceneGraphEdit { graph: candidate, removed_param_ids: Vec::new(), parameter_id_remaps: Vec::new() });
        }
        return Err(at("Scene graph has no card to expose controls on"));
    };
    let mut id_map: HashMap<&str, String> = HashMap::new();
    for spec in &template.metadata.params {
        id_map.insert(spec.id.as_str(), remap_exposure_id(&spec.id, &remap, group_doc));
    }
    for spec in &template.metadata.params {
        let mut spec = spec.clone();
        spec.id = id_map[spec.id.as_str()].clone();
        spec.section = spec.section.map(|section| remap_section(&section, &handle));
        if host_meta.params.iter().any(|existing| existing.id == spec.id) {
            return Err(fail(format!("Exposure {} already exists", spec.id)));
        }
        host_meta.params.push(spec);
    }
    for binding in &template.metadata.bindings {
        let mut binding = binding.clone();
        binding.id = id_map
            .get(binding.id.as_str())
            .cloned()
            .ok_or_else(|| fail(format!("Binding {} has no exposure", binding.id)))?;
        if let BindingTarget::Node { node_id, .. } = &mut binding.target {
            *node_id = remap
                .stable
                .get(node_id)
                .cloned()
                .ok_or_else(|| fail(format!("Binding targets {} outside the template", node_id.as_str())))?;
        }
        host_meta.bindings.push(binding);
    }
    for alias in &template.metadata.param_aliases {
        let mut alias = alias.clone();
        if let Some(fresh) = id_map.get(alias.old.as_str()) {
            alias.old = fresh.clone();
        }
        if let Some(new) = alias.new.as_mut()
            && let Some(fresh) = id_map.get(new.as_str())
        {
            *new = fresh.clone();
        }
        host_meta.param_aliases.push(alias);
    }
    for alias in &template.metadata.value_aliases {
        let mut alias = alias.clone();
        if let Some(fresh) = id_map.get(alias.param_id.as_str()) {
            alias.param_id = fresh.clone();
        }
        host_meta.value_aliases.push(alias);
    }
    for binding in &template.metadata.string_bindings {
        let mut binding = binding.clone();
        if let BindingTarget::Node { node_id, .. } = &mut binding.target {
            *node_id = remap.stable.get(node_id).cloned().ok_or_else(|| fail("String binding targets outside the template"))?;
        }
        host_meta.string_bindings.push(binding);
    }
    if let Some(domain) = domain {
        crate::scene_world_controls::share_world_controls(&mut candidate, group_doc, &domain).map_err(fail)?;
    }
    Ok(SceneGraphEdit { graph: candidate, removed_param_ids: Vec::new(), parameter_id_remaps: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 2,
            "nodes": [{"id": 1, "nodeId": "scene", "typeId": "node.render_scene", "params": {"objects": {"type": "Float", "value": 0.0}}}],
            "wires": [],
            "presetMetadata": {
                "id": "t", "displayName": "T", "category": "Test", "oscPrefix": "t",
                "params": [], "bindings": [], "stringParams": [], "stringBindings": []
            }
        }))
        .unwrap()
    }

    fn node(id: u32, node_id: &str, type_id: &str, handle: Option<&str>) -> EffectGraphNode {
        serde_json::from_value(serde_json::json!({
            "id": id, "nodeId": node_id, "typeId": type_id, "handle": handle
        }))
        .unwrap()
    }

    /// A group with a nested group inside it and `outputs` scene-object outputs.
    fn template(outputs: usize) -> SceneTemplate {
        let mut inner = node(12, "inner", GROUP_TYPE_ID, Some("{handle} Inner"));
        inner.group = Some(Box::new(crate::effect_graph_def::GroupDef {
            interface: crate::effect_graph_def::GroupInterface { inputs: vec![], outputs: vec![], params: vec![] },
            nodes: vec![node(13, "leaf", "node.transform_3d", Some("{handle} Leaf"))],
            wires: vec![],
            tint: None,
        }));
        let mut group = node(10, "fam", GROUP_TYPE_ID, Some("{handle}"));
        let mut nodes = vec![node(11, "obj", "node.scene_object", None), inner];
        let mut wires = Vec::new();
        let mut interface = crate::effect_graph_def::GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        for index in 0..outputs {
            let out = 20 + index as u32;
            nodes.push(node(out, &format!("out{index}"), crate::effect_graph_def::GROUP_OUTPUT_TYPE_ID, None));
            wires.push(EffectGraphWire { from_node: 11, from_port: "object".into(), to_node: out, to_port: "object".into() });
            interface.outputs.push(crate::effect_graph_def::InterfacePortDef { name: format!("o{index}"), port_type: "SceneObject".into() });
        }
        group.group = Some(Box::new(crate::effect_graph_def::GroupDef { interface, nodes, wires, tint: None }));
        let mut metadata: PresetMetadata = host().preset_metadata.unwrap();
        metadata.display_name = "Water".into();
        metadata.params = serde_json::from_value(serde_json::json!([
            {"id": "13_pos_x", "name": "X", "min": 0.0, "max": 1.0, "defaultValue": 0.0, "section": "{handle} Leaf"},
            {"id": "shared", "name": "Shared", "min": 0.0, "max": 1.0, "defaultValue": 0.0}
        ]))
        .unwrap();
        metadata.bindings = serde_json::from_value(serde_json::json!([
            {"id": "13_pos_x", "defaultValue": 0.0, "target": {"kind": "node", "nodeId": "leaf", "param": "pos_x"}},
            {"id": "shared", "defaultValue": 0.0, "target": {"kind": "node", "nodeId": "obj", "param": "x"}},
            {"id": "shared", "defaultValue": 0.0, "target": {"kind": "node", "nodeId": "leaf", "param": "pos_y"}}
        ]))
        .unwrap();
        SceneTemplate { group, metadata }
    }

    fn scene() -> SceneNodeRef {
        SceneNodeRef { scope: vec![], node: NodeId::new("scene") }
    }

    fn all_doc_ids(nodes: &[EffectGraphNode], out: &mut Vec<u32>) {
        for node in nodes {
            out.push(node.id);
            if let Some(body) = node.group.as_deref() {
                all_doc_ids(&body.nodes, out);
            }
        }
    }

    #[test]
    fn physics_boundary_insert_fan_out() {
        let edit = insert_scene_template(&host(), &scene(), &template(3)).unwrap();
        let scene_node = &edit.graph.nodes[0];
        assert_eq!(scene_node.params.get("objects"), Some(&SerializedParamValue::Float { value: 3.0 }));
        let group = edit.graph.nodes.iter().find(|node| node.type_id == GROUP_TYPE_ID).unwrap();
        let mut routed: Vec<_> = edit.graph.wires.iter().map(|wire| (wire.from_node, wire.to_port.clone())).collect();
        routed.sort();
        assert_eq!(routed, (0..3).map(|n| (group.id, format!("object_{n}"))).collect::<Vec<_>>());
    }

    #[test]
    fn physics_boundary_nested_remap() {
        let first = insert_scene_template(&host(), &scene(), &template(1)).unwrap();
        let second = insert_scene_template(&first.graph, &scene(), &template(1)).unwrap();
        let mut docs = Vec::new();
        all_doc_ids(&second.graph.nodes, &mut docs);
        let unique: HashSet<_> = docs.iter().collect();
        assert_eq!(docs.len(), unique.len(), "two inserts share no document id");
        let mut stable = HashSet::new();
        collect_stable_ids(&second.graph.nodes, &mut stable);
        assert_eq!(stable.len(), docs.len(), "two inserts share no stable id");
        let meta = second.graph.preset_metadata.as_ref().unwrap();
        let ids: HashSet<_> = meta.params.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(ids.len(), meta.params.len(), "exposure ids stay unique");
        for binding in &meta.bindings {
            let BindingTarget::Node { node_id, .. } = &binding.target else { panic!("node target") };
            assert!(stable.contains(node_id), "binding {} follows its node", binding.id);
            assert!(ids.contains(binding.id.as_str()));
        }
        let handles: Vec<_> = {
            let mut set = HashSet::new();
            collect_handles(&second.graph.nodes, &mut set);
            let mut handles: Vec<_> = set.into_iter().collect();
            handles.sort();
            handles
        };
        assert_eq!(handles, ["Water 1", "Water 1 Inner", "Water 1 Leaf", "Water 2", "Water 2 Inner", "Water 2 Leaf"]);
        assert!(meta.params.iter().any(|spec| spec.section.as_deref() == Some("Water 2 Leaf")));
    }

    #[test]
    fn physics_boundary_insert_atomic() {
        let graph = host();
        let before = graph.clone();
        let mut occupied = graph.clone();
        occupied.wires.push(EffectGraphWire { from_node: 1, from_port: "x".into(), to_node: 1, to_port: "object_0".into() });
        assert!(insert_scene_template(&occupied, &scene(), &template(1)).is_err());
        let missing = SceneNodeRef { scope: vec![], node: NodeId::new("nowhere") };
        assert!(insert_scene_template(&graph, &missing, &template(1)).is_err());
        let mut bad = template(1);
        bad.metadata.bindings[0].target = BindingTarget::Node { node_id: NodeId::new("outside"), param: "x".into() };
        assert!(insert_scene_template(&graph, &scene(), &bad).is_err());
        assert_eq!(graph, before, "a refused insert leaves the host untouched");
    }

    #[test]
    fn physics_boundary_insert_routes_lights_to_light_slots() {
        let mut host = host();
        host.nodes[0].params.insert("lights".into(), SerializedParamValue::Float { value: 2.0 });
        let mut template = template(0);
        let body = template.group.group.as_deref_mut().unwrap();
        body.interface.outputs.push(crate::effect_graph_def::InterfacePortDef { name: "out".into(), port_type: "Light".into() });
        let edit = insert_scene_template(&host, &scene(), &template).unwrap();
        let render = &edit.graph.nodes[0];
        assert_eq!(render.params.get("lights"), Some(&SerializedParamValue::Float { value: 3.0 }));
        assert_eq!(render.params.get("objects"), Some(&SerializedParamValue::Float { value: 0.0 }));
        assert_eq!(edit.graph.wires.len(), 1);
        assert_eq!(edit.graph.wires[0].to_port, "light_2");
    }
}
