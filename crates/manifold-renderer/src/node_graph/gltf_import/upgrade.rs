//! Load-time repair for imported graphs written before material fidelity fields
//! were added. The graph is the authority: only absent importer fields and
//! values that still equal an old generated default are repaired.
use crate::node_graph::gltf_load::{self, GltfImportSummary};
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
    StringBindingDef,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
mod maps;
mod params;
pub(super) mod project;
#[cfg(test)]
mod tests;
/// Per-project-load cache. It deliberately owns no locks: graph migration is
/// a load-time CPU operation and one caller owns a cache for that load.
#[derive(Default)]
pub struct MaterialUpgradeCache {
    summaries: HashMap<PathBuf, Result<GltfImportSummary, String>>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialBindingUpdate {
    pub id: String,
    pub old_value: f32,
    pub new_value: f32,
}
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MaterialGraphUpgrade {
    pub changed: bool,
    pub notices: Vec<String>,
    pub binding_updates: Vec<MaterialBindingUpdate>,
}
#[derive(Clone)]
struct GeometrySource {
    node_id: u32,
    stable_id: NodeId,
    material_index: u32,
    path_binding: Option<StringBindingDef>,
    pending: bool,
}
pub fn upgrade_material_graph(
    def: &mut EffectGraphDef,
    cache: &mut MaterialUpgradeCache,
) -> MaterialGraphUpgrade {
    if !contains_geometry_source(&def.nodes) {
        return MaterialGraphUpgrade::default();
    }
    let Some(metadata) = def.preset_metadata.as_mut() else {
        return MaterialGraphUpgrade {
            notices: vec!["material upgrade skipped: graph has no preset metadata".to_string()],
            ..MaterialGraphUpgrade::default()
        };
    };
    let mut next_id = max_node_id(&def.nodes).saturating_add(1);
    let mut result = MaterialGraphUpgrade::default();
    upgrade_level(
        &mut def.nodes,
        &mut def.wires,
        metadata,
        cache,
        &mut next_id,
        &mut result,
    );
    result
}

fn contains_geometry_source(nodes: &[EffectGraphNode]) -> bool {
    nodes.iter().any(|node| {
        (matches!(
            node.type_id.as_str(),
            "node.gltf_mesh_source" | "node.gltf_skinned_mesh_source"
        ) && !node.params.contains_key("vertex_colors"))
            || node
                .group
                .as_ref()
                .is_some_and(|group| contains_geometry_source(&group.nodes))
    })
}
fn upgrade_level(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    metadata: &mut manifold_core::effect_graph_def::PresetMetadata,
    cache: &mut MaterialUpgradeCache,
    next_id: &mut u32,
    result: &mut MaterialGraphUpgrade,
) {
    for node in nodes.iter_mut() {
        if let Some(group) = node.group.as_mut() {
            upgrade_level(
                &mut group.nodes,
                &mut group.wires,
                metadata,
                cache,
                next_id,
                result,
            );
        }
    }
    let sources = nodes
        .iter()
        .filter(|node| {
            matches!(
                node.type_id.as_str(),
                "node.gltf_mesh_source" | "node.gltf_skinned_mesh_source"
            )
        })
        .filter_map(|node| {
            let material_index = match node.params.get("material_index") {
                Some(SerializedParamValue::Int { value }) => {
                    if *value == gltf_load::DEFAULT_MATERIAL_MESH_PARAM {
                        gltf_load::DEFAULT_MATERIAL_SENTINEL
                    } else if *value >= 0 {
                        *value as u32
                    } else {
                        return None;
                    }
                }
                _ => return None,
            };
            Some(GeometrySource {
                node_id: node.id,
                stable_id: node.node_id.clone(),
                material_index,
                path_binding: source_path_binding(metadata, &node.node_id),
                pending: !node.params.contains_key("vertex_colors"),
            })
        })
        .collect::<Vec<_>>();
    if !sources.iter().any(|source| source.pending) {
        return;
    }
    let mut attempted = HashSet::new();
    let scene_ids = nodes
        .iter()
        .filter(|node| node.type_id == "node.scene_object")
        .map(|node| node.id)
        .collect::<Vec<_>>();
    for scene_id in scene_ids {
        let Some(source_id) = resolve_geometry_source(scene_id, wires, &sources) else {
            continue;
        };
        let source = sources
            .iter()
            .find(|source| source.node_id == source_id)
            .expect("resolved source")
            .clone();
        if !source.pending {
            continue;
        }
        attempted.insert(source_id);
        let Some(material_id) = unique_wire_source(wires, scene_id, "material") else {
            result.notices.push(format!(
                "material upgrade skipped scene object {scene_id}: material input is ambiguous"
            ));
            continue;
        };
        let Some(material_pos) = nodes.iter().position(|node| node.id == material_id) else {
            result.notices.push(format!(
                "material upgrade skipped scene object {scene_id}: material node is missing"
            ));
            continue;
        };
        if !matches!(
            nodes[material_pos].type_id.as_str(),
            "node.pbr_material" | "node.unlit_material"
        ) {
            result.notices.push(format!(
                "material upgrade deferred for {}: custom material node {}",
                source.stable_id, nodes[material_pos].type_id
            ));
            continue;
        }
        let Some(path_binding) = source.path_binding.clone() else {
            result.notices.push(format!(
                "material upgrade deferred for {}: model source path binding is missing",
                source.stable_id
            ));
            continue;
        };
        let path = path_binding.default_value.clone();
        if path.trim().is_empty() {
            result.notices.push(format!(
                "material upgrade deferred for {}: model source path is empty",
                source.stable_id
            ));
            continue;
        }
        let summary = match cached_summary(cache, Path::new(&path)) {
            Ok(summary) => summary,
            Err(error) => {
                result
                    .notices
                    .push(format!("material upgrade deferred for {path}: {error}"));
                continue;
            }
        };
        let Some(material) = summary
            .materials
            .iter()
            .find(|material| material.material_index == source.material_index)
        else {
            result.notices.push(format!(
                "material upgrade deferred for {path}: material index {} was not found",
                source.material_index
            ));
            continue;
        };
        let material = material.clone();
        let incoming = incoming_ports(wires, material_id);
        let new_fields = params::repair_material_node(
            &mut nodes[material_pos],
            &material,
            &incoming,
            metadata,
            result,
        );
        if new_fields {
            result.changed = true;
        }
        if repair_vertex_color_flag(nodes, source_id, material.vertex_color_varies) {
            result.changed = true;
        }
        if nodes[material_pos].type_id == "node.pbr_material" {
            result.changed |= maps::add_missing_maps(
                nodes,
                wires,
                metadata,
                scene_id,
                &path_binding,
                &material,
                &summary.texture_dims,
                next_id,
            );
            result.changed |= maps::repair_gloss_source(nodes, wires, scene_id, &material);
        }
    }
    for source in sources
        .iter()
        .filter(|source| source.pending && !attempted.contains(&source.node_id))
    {
        result.notices.push(format!("material upgrade deferred for {}: material/geometry association crosses a group boundary or is ambiguous", source.stable_id));
    }
}
fn cached_summary<'a>(
    cache: &'a mut MaterialUpgradeCache,
    path: &Path,
) -> &'a Result<GltfImportSummary, String> {
    let key = path.to_path_buf();
    cache
        .summaries
        .entry(key)
        .or_insert_with(|| gltf_load::gltf_import_summary(path))
}
fn source_path_binding(
    metadata: &manifold_core::effect_graph_def::PresetMetadata,
    node_id: &NodeId,
) -> Option<StringBindingDef> {
    metadata.string_bindings.iter().find_map(|binding| {
        let BindingTarget::Node {
            node_id: target,
            param,
        } = &binding.target
        else {
            return None;
        };
        if target != node_id || param != "path" {
            return None;
        }
        let mut result = binding.clone();
        if let Some(spec) = metadata
            .string_params
            .iter()
            .find(|spec| spec.id == binding.id)
        {
            result.default_value = spec.default_value.clone();
        }
        Some(result)
    })
}
fn unique_wire_source(wires: &[EffectGraphWire], to_node: u32, port: &str) -> Option<u32> {
    let mut ids = wires
        .iter()
        .filter(|wire| wire.to_node == to_node && wire.to_port == port)
        .map(|wire| wire.from_node);
    let first = ids.next()?;
    ids.next().is_none().then_some(first)
}
fn resolve_geometry_source(
    scene_id: u32,
    wires: &[EffectGraphWire],
    sources: &[GeometrySource],
) -> Option<u32> {
    let mut stack = wires
        .iter()
        .filter(|wire| wire.to_node == scene_id && wire.to_port == "vertices")
        .map(|wire| wire.from_node)
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let mut matches = HashSet::new();
    while let Some(node_id) = stack.pop() {
        if !seen.insert(node_id) {
            continue;
        }
        if sources.iter().any(|source| source.node_id == node_id) {
            matches.insert(node_id);
            continue;
        }
        for wire in wires.iter().filter(|wire| wire.to_node == node_id) {
            stack.push(wire.from_node);
        }
    }
    (matches.len() == 1).then(|| *matches.iter().next().unwrap())
}
fn incoming_ports(wires: &[EffectGraphWire], node_id: u32) -> HashSet<String> {
    wires
        .iter()
        .filter(|wire| wire.to_node == node_id)
        .map(|wire| wire.to_port.clone())
        .collect()
}
fn repair_vertex_color_flag(nodes: &mut [EffectGraphNode], source_id: u32, varying: bool) -> bool {
    let Some(source) = nodes.iter_mut().find(|node| node.id == source_id) else {
        return false;
    };
    if source.params.contains_key("vertex_colors") {
        return false;
    }
    source.params.insert(
        "vertex_colors".to_string(),
        SerializedParamValue::Bool { value: varying },
    );
    true
}
fn max_node_id(nodes: &[EffectGraphNode]) -> u32 {
    nodes
        .iter()
        .map(|node| {
            node.id.max(
                node.group
                    .as_ref()
                    .map_or(0, |group| max_node_id(&group.nodes)),
            )
        })
        .max()
        .unwrap_or(0)
}
