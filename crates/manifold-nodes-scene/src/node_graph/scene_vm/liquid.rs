//! Where a scene's liquid domains sit: the authored Water family, the domain
//! behind a render slot, and the graph location of any stable node reference.

use std::collections::{HashMap, HashSet};

use manifold_core::effect_graph_def::{EffectGraphNode, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID};
use manifold_core::liquid_domain::{is_liquid_domain, liquid_domain_of};
use manifold_core::scene_index::FlatSceneIndex;
use manifold_core::{NodeId, SceneNodeRef};

use super::shapes::NodeAt;
use super::{Level, MODIFIER_TYPE_IDS, SWITCH_MESH_TYPE_ID, find_scene_object_in_group};

/// The liquid domain behind render slot `slot`, found by the core walk and
/// located in the authored graph.
pub(super) struct LiquidDomainAt<'a> {
    pub(super) level: Level<'a>,
    /// Group doc ids from the root down to `level`.
    pub(super) scope: Vec<u32>,
    pub(super) node: &'a EffectGraphNode,
    pub(super) stable: SceneNodeRef,
    /// The level holding the group `level` is the body of, and that group's
    /// doc id, so a domain input wired in from outside its group resolves.
    pub(super) parent: Option<(Level<'a>, u32)>,
    /// Whether the object's mesh walk should contribute display controls to
    /// the Water row. Particle View's display mesh is family dressing, not a
    /// simulation control.
    pub(super) surface_controls: bool,
}

pub(super) struct WaterFamilyInfo {
    pub(super) water_object_id: u32,
    pub(super) domain_node_id: u32,
    /// Child object ids in the family-output order: Foam, Spray, Bubbles.
    pub(super) output_order: HashMap<u32, usize>,
}

/// Recognise the authored Water family from its resolved group outputs. A
/// generic imported compound remains on the legacy path unless it has all
/// four family outputs and exactly one liquid-domain simulation owner in its
/// group body.
/// Keep aligned with manifold-editing commands::graph::scene::is_water_family_parent:
/// four distinct object outputs (water, foam, spray, bubbles), with water
/// reaching the group's sole liquid domain. Renderer has no production editing
/// dependency; app's water_family_recognizers_agree test checks both.
pub(super) fn discover_water_families(
    level: &Level<'_>,
    scene_node: &EffectGraphNode,
    objects: usize,
) -> HashMap<u32, WaterFamilyInfo> {
    let output_roles = ["object", "object_1", "object_2", "object_3"];
    let mut candidates: HashMap<u32, HashMap<&str, u32>> = HashMap::new();
    for slot in 0..objects {
        let Some((group_id, output_port)) = level.producer(scene_node.id, &format!("object_{slot}")) else {
            continue;
        };
        let Some(group_node) = level.node(group_id).filter(|node| node.type_id == GROUP_TYPE_ID) else {
            continue;
        };
        let Some((object, _)) = group_node
            .group
            .as_deref()
            .and_then(|group| find_scene_object_in_group(group, output_port))
        else {
            continue;
        };
        candidates.entry(group_id).or_default().insert(output_port, object.id);
    }

    candidates
        .into_iter()
        .filter_map(|(group_id, members)| {
            if output_roles.iter().any(|role| !members.contains_key(role)) {
                return None;
            }
            let mut objects_seen = HashSet::new();
            if !output_roles.iter().all(|role| objects_seen.insert(members[role])) {
                return None;
            }
            let group = level.node(group_id)?.group.as_deref()?;
            let mut domain_nodes = group.nodes.iter().filter(|node| is_liquid_domain(&node.type_id));
            let domain_node_id = domain_nodes.next()?.id;
            if domain_nodes.next().is_some() {
                return None;
            }
            let water_object_id = members["object"];
            let group_node = level.node(group_id)?;
            // Particle View's water vertices come from a platonic mesh, so
            // liquid_domain_of cannot see the solver through vertices. The
            // object still has a structural path to the sole domain through
            // frame → copies → instances; require that path instead of
            // attributing an unrelated domain to the family by coincidence.
            if !family_owner_reaches_domain(group_node, water_object_id, domain_node_id) {
                return None;
            }
            // Further scene objects the family outputs (Physics View's dots)
            // are child rows too, after the looks; they never decide
            // recognition.
            let mut extras: Vec<_> = members
                .iter()
                .filter(|(port, object)| !output_roles.contains(port) && !objects_seen.contains(object))
                .collect();
            extras.sort_unstable();
            let output_order = output_roles
                .iter()
                .skip(1)
                .map(|role| members[*role])
                .chain(extras.into_iter().map(|(_, object)| *object))
                .enumerate()
                .map(|(order, object)| (object, order + 1))
                .collect();
            Some((
                group_id,
                WaterFamilyInfo {
                    water_object_id,
                    domain_node_id,
                    output_order,
                },
            ))
        })
        .collect()
}

fn family_owner_reaches_domain(
    group_node: &EffectGraphNode,
    object_id: u32,
    domain_node_id: u32,
) -> bool {
    let Some(group) = group_node.group.as_deref() else { return false; };
    let mut pending = vec![object_id];
    let mut seen = HashSet::new();
    while let Some(target) = pending.pop() {
        if !seen.insert(target) { continue; }
        for wire in group.wires.iter().filter(|wire| wire.to_node == target) {
            if wire.from_node == domain_node_id { return true; }
            pending.push(wire.from_node);
        }
    }
    false
}

pub(super) fn family_liquid_domain<'a>(
    root: &Level<'a>,
    group_id: u32,
    domain_node_id: u32,
) -> Option<LiquidDomainAt<'a>> {
    let group_node = root.node(group_id)?;
    let group = group_node.group.as_deref()?;
    let level = Level { nodes: &group.nodes, wires: &group.wires };
    let node = level.node(domain_node_id)?;
    Some(LiquidDomainAt {
        level,
        scope: vec![group_id],
        node,
        stable: SceneNodeRef {
            scope: vec![group_node.node_id.clone()],
            node: node.node_id.clone(),
        },
        parent: Some((Level { nodes: root.nodes, wires: root.wires }, group_id)),
        surface_controls: false,
    })
}

/// Resolve a Water look's mesh source inside the family group. Child looks
/// are intentionally simple platonic-mesh → scene-object chains, but tolerate
/// a known mesh modifier or transparent nested group so the row stays honest
/// when a preset is hand-edited.
pub(super) fn family_look_mesh(group_node: &EffectGraphNode, object_id: u32) -> Option<NodeId> {
    let group = group_node.group.as_deref()?;
    let mut level = Level { nodes: &group.nodes, wires: &group.wires };
    let mut cursor = level.producer(object_id, "vertices");
    let mut guard = 0;
    while let Some((node_id, port)) = cursor {
        guard += 1;
        if guard > 64 { return None; }
        let node = level.node(node_id)?;
        if node.type_id == "node.platonic_solid_mesh" {
            return Some(node.node_id.clone());
        }
        if node.type_id == GROUP_TYPE_ID {
            let nested = node.group.as_deref()?;
            let inner = Level { nodes: &nested.nodes, wires: &nested.wires };
            let output = inner.nodes.iter().find(|candidate| candidate.type_id == GROUP_OUTPUT_TYPE_ID)?;
            let (inner_id, inner_port) = inner.producer(output.id, port)?;
            level = inner;
            cursor = Some((inner_id, inner_port));
        } else if MODIFIER_TYPE_IDS.contains(&node.type_id.as_str()) {
            cursor = level.producer(node.id, "in");
        } else if node.type_id == SWITCH_MESH_TYPE_ID {
            // Physics View's dots reach their mesh through a switch.
            cursor = ["in_0", "in_1"].into_iter().find_map(|input| level.producer(node.id, input));
        } else {
            return None;
        }
    }
    None
}

pub(super) fn slot_liquid_domain<'a>(
    root: &Level<'a>,
    index: Option<&FlatSceneIndex>,
    scene_node: &EffectGraphNode,
    slot: usize,
) -> Option<LiquidDomainAt<'a>> {
    let index = index?;
    let scene = SceneNodeRef { scope: Vec::new(), node: scene_node.node_id.clone() };
    let object = index.scene_object_at(&scene, slot as u32).ok()??;
    let domain = liquid_domain_of(index, &object).ok()??;
    let NodeAt { level, scope, node, parent } = locate_node(root, &domain)?;
    Some(LiquidDomainAt { level, scope, node, stable: domain, parent, surface_controls: true })
}

/// Find `reference` in the authored graph from the root.
pub(super) fn locate_node<'a>(root: &Level<'a>, reference: &SceneNodeRef) -> Option<NodeAt<'a>> {
    let mut level = Level { nodes: root.nodes, wires: root.wires };
    let mut parent = None;
    let mut scope = Vec::with_capacity(reference.scope.len());
    for group_id in &reference.scope {
        let group = level.nodes.iter().find(|node| &node.node_id == group_id)?;
        let body = group.group.as_deref()?;
        scope.push(group.id);
        parent = Some((Level { nodes: level.nodes, wires: level.wires }, group.id));
        level = Level { nodes: &body.nodes, wires: &body.wires };
    }
    let node = level.nodes.iter().find(|node| node.node_id == reference.node)?;
    Some(NodeAt { level, scope, node, parent })
}
