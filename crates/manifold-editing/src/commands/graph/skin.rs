//! Scene-object layer-skin commands (SCENE_FX_DESIGN.md section 3.3 / P4b).
//!
//! A skin is a `node.layer_source` wired into a `node.scene_object`'s
//! `emissive_map` or `base_color_map` port. The panel presents one Skin row
//! per object with two dropdowns: source layer and target map. These commands
//! splice, move, and remove that wire, mirroring the modifier-stack command
//! shape: one undoable composite that snapshots the whole object level before
//! mutating and restores it verbatim on undo.

use std::collections::BTreeMap;

use manifold_core::GraphTarget;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue,
};
use manifold_core::project::Project;

use crate::command::Command;

use super::{
    descend_level, refresh_target_manifest, scene_build_node, scene_build_wire,
    with_existing_target_graph_mut, with_target_graph_mut,
};

/// Which material map port the skin drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinTargetMap {
    Emissive,
    BaseColor,
}

impl SkinTargetMap {
    fn port_name(self) -> &'static str {
        match self {
            SkinTargetMap::Emissive => "emissive_map",
            SkinTargetMap::BaseColor => "base_color_map",
        }
    }

    fn label(self) -> &'static str {
        match self {
            SkinTargetMap::Emissive => "Emissive",
            SkinTargetMap::BaseColor => "Base Color",
        }
    }
}

/// Set (or clear) a scene object's layer-skin source layer. `source: None`
/// removes the skin from `target_map`; if the source node is no longer wired
/// to any map it is deleted. `source: Some(layer_id)` ensures a
/// `node.layer_source` exists wired to `target_map` and sets its `layer` param.
#[derive(Debug)]
pub struct SetSceneObjectSkinSourceCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    scene_object_id: u32,
    source_node_id: Option<u32>,
    target_map: SkinTargetMap,
    source: Option<String>,
    catalog_default: EffectGraphDef,
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
}

impl SetSceneObjectSkinSourceCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        scene_object_id: u32,
        source_node_id: Option<u32>,
        target_map: SkinTargetMap,
        source: Option<String>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            scene_object_id,
            source_node_id,
            target_map,
            source,
            catalog_default,
            prev: None,
        }
    }
}

/// Set which material map port the existing skin (or a newly-created one with
/// an empty source) wires into.
#[derive(Debug)]
pub struct SetSceneObjectSkinTargetMapCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    scene_object_id: u32,
    source_node_id: Option<u32>,
    target_map: SkinTargetMap,
    catalog_default: EffectGraphDef,
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
}

impl SetSceneObjectSkinTargetMapCommand {
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        scene_object_id: u32,
        source_node_id: Option<u32>,
        target_map: SkinTargetMap,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            scene_object_id,
            source_node_id,
            target_map,
            catalog_default,
            prev: None,
        }
    }
}

/// True if `node_id` is currently wired to `scene_object_id`'s `target_map` port.
fn feeds_map(
    wires: &[EffectGraphWire],
    node_id: u32,
    scene_object_id: u32,
    target_map: SkinTargetMap,
) -> bool {
    wires
        .iter()
        .any(|w| w.from_node == node_id && w.to_node == scene_object_id && w.to_port == target_map.port_name())
}

/// Locate the existing `node.layer_source` that feeds `scene_object_id`'s
/// `target_map`, or return the supplied `source_node_id` hint if it exists in
/// the level and is a layer_source. Returns `None` when no layer_source node
/// is wired to the target map.
fn find_layer_source_for_map(
    nodes: &[EffectGraphNode],
    wires: &[EffectGraphWire],
    scene_object_id: u32,
    target_map: SkinTargetMap,
    source_node_id: Option<u32>,
) -> Option<u32> {
    if let Some(id) = source_node_id
        && nodes.iter().any(|n| n.id == id && n.type_id == "node.layer_source")
        && feeds_map(wires, id, scene_object_id, target_map)
    {
        return Some(id);
    }
    wires
        .iter()
        .find(|w| w.to_node == scene_object_id && w.to_port == target_map.port_name())
        .and_then(|w| nodes.iter().find(|n| n.id == w.from_node && n.type_id == "node.layer_source"))
        .map(|n| n.id)
}

/// While a skin is bound to a map port it is that port's SOLE producer — the
/// port's previous producer (typically the glTF import's baked texture) is
/// displaced. This mirrors P4a's runtime semantics: a bound-but-missing layer
/// renders transparent black, NOT the baked texture underneath, so the
/// texture must not keep feeding the port beside the skin.
fn displace_other_producers(
    wires: &mut Vec<EffectGraphWire>,
    scene_object_id: u32,
    target_map: SkinTargetMap,
    keep: u32,
) {
    wires.retain(|w| {
        !(w.to_node == scene_object_id
            && w.to_port == target_map.port_name()
            && w.from_node != keep)
    });
}

/// Apply the desired skin state: ensure the layer_source node exists at the
/// right map port with the right layer id, or remove it. Records the previous
/// level state in `prev` (always the PRE-edit state — undo of a value edit
/// needs it as much as undo of a splice). Returns whether the topology
/// changed (vs. a pure value edit).
fn apply_skin_source(
    nodes: &mut Vec<EffectGraphNode>,
    wires: &mut Vec<EffectGraphWire>,
    scene_object_id: u32,
    target_map: SkinTargetMap,
    source: &Option<String>,
    source_node_id: Option<u32>,
    prev: &mut Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>)>,
) -> bool {
    prev.replace((nodes.clone(), wires.clone()));

    let existing_id = find_layer_source_for_map(nodes, wires, scene_object_id, target_map, source_node_id);

    match source {
        None => {
            // Remove the wire to this map. If the node no longer feeds
            // anything, delete it.
            wires.retain(|w| !(w.to_node == scene_object_id && w.to_port == target_map.port_name()));
            if let Some(id) = existing_id {
                let still_used = wires.iter().any(|w| w.from_node == id);
                if !still_used {
                    nodes.retain(|n| n.id != id);
                    wires.retain(|w| w.from_node != id);
                }
            }
            true
        }
        Some(layer_id) => {
            if let Some(id) = existing_id {
                // Value-only edit: update the layer param. Topology unchanged
                // (the pre-edit snapshot taken above is the undo state).
                if let Some(node) = nodes.iter_mut().find(|n| n.id == id) {
                    node.params.insert(
                        "layer".to_string(),
                        SerializedParamValue::String { value: layer_id.clone() },
                    );
                }
                false
            } else {
                // Need to create or repurpose a layer_source node. Prefer the
                // hint id if it still names a layer_source somewhere in this
                // level, even if currently wired to the other map.
                let mut reused_id = source_node_id.filter(|&id| {
                    nodes.iter().any(|n| n.id == id && n.type_id == "node.layer_source")
                });

                // If the reused node is wired to the other map, adding a second
                // wire is fine (one source can feed both maps). If it doesn't
                // exist, mint one.
                if reused_id.is_none() {
                    let new_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
                    let mut params = BTreeMap::new();
                    params.insert(
                        "layer".to_string(),
                        SerializedParamValue::String { value: layer_id.clone() },
                    );
                    nodes.push(scene_build_node(new_id, "node.layer_source", Some("Skin".to_string()), params));
                    reused_id = Some(new_id);
                } else if let Some(id) = reused_id {
                    // Update the existing node's layer param too.
                    if let Some(node) = nodes.iter_mut().find(|n| n.id == id) {
                        node.params.insert(
                            "layer".to_string(),
                            SerializedParamValue::String { value: layer_id.clone() },
                        );
                    }
                }

                let id = reused_id.unwrap();
                // Ownership: displace the port's previous producer (a baked
                // glTF texture), then wire the skin if not already.
                displace_other_producers(wires, scene_object_id, target_map, id);
                if !feeds_map(wires, id, scene_object_id, target_map) {
                    wires.push(scene_build_wire(id, "out", scene_object_id, target_map.port_name()));
                }
                true
            }
        }
    }
}

impl Command for SetSceneObjectSkinSourceCommand {
    fn execute(&mut self, project: &mut Project) {
        let source = self.source.clone();
        let target_map = self.target_map;
        let scene_object_id = self.scene_object_id;
        let source_node_id = self.source_node_id;
        let scope = self.scope_path.clone();
        let structural = with_target_graph_mut(
            project,
            &self.target,
            &self.catalog_default,
            true,
            |def| {
                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let mut local_prev = None;
                let topology_changed = apply_skin_source(
                    nodes,
                    wires,
                    scene_object_id,
                    target_map,
                    &source,
                    source_node_id,
                    &mut local_prev,
                );
                self.prev = local_prev;
                Some(topology_changed)
            },
        )
        .flatten()
        .unwrap_or(true);
        if !structural {
            // Value-only edit: bump only the snapshot version, not structure.
            project.with_preset_graph_mut(&self.target, |host| host.bump_graph_version());
        }
        refresh_target_manifest(project, &self.target);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
        refresh_target_manifest(project, &self.target);
    }

    fn description(&self) -> &str {
        match (&self.source, self.target_map) {
            (Some(_), SkinTargetMap::Emissive) => "Set Skin Source (Emissive)",
            (Some(_), SkinTargetMap::BaseColor) => "Set Skin Source (Base Color)",
            (None, _) => "Clear Skin",
        }
    }
}

impl Command for SetSceneObjectSkinTargetMapCommand {
    fn execute(&mut self, project: &mut Project) {
        let target_map = self.target_map;
        let scene_object_id = self.scene_object_id;
        let source_node_id = self.source_node_id;
        let scope = self.scope_path.clone();
        let _ = with_target_graph_mut(
            project,
            &self.target,
            &self.catalog_default,
            true,
            |def| {
                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                self.prev = Some((nodes.clone(), wires.clone()));

                let existing_id =
                    find_layer_source_for_map(nodes, wires, scene_object_id, target_map, source_node_id)
                        .or_else(|| {
                            source_node_id.filter(|&id| {
                                nodes.iter().any(|n| n.id == id && n.type_id == "node.layer_source")
                            })
                        })
                        .or_else(|| {
                            wires
                                .iter()
                                .find(|w| {
                                    w.to_node == scene_object_id
                                        && (w.to_port == SkinTargetMap::Emissive.port_name()
                                            || w.to_port == SkinTargetMap::BaseColor.port_name())
                                })
                                .and_then(|w| {
                                    nodes
                                        .iter()
                                        .find(|n| n.id == w.from_node && n.type_id == "node.layer_source")
                                        .map(|n| n.id)
                                })
                        });

                if let Some(id) = existing_id {
                    // The skin leaves its old port: remove only THIS node's
                    // wire into the other map (the port's own producers —
                    // e.g. a baked texture the skin displaced on arrival —
                    // stay displaced per the ownership rule; anything that
                    // was never the skin's stays put). Then take the
                    // requested port as its sole producer.
                    let other = match target_map {
                        SkinTargetMap::Emissive => SkinTargetMap::BaseColor,
                        SkinTargetMap::BaseColor => SkinTargetMap::Emissive,
                    };
                    wires.retain(|w| {
                        !(w.from_node == id && w.to_node == scene_object_id && w.to_port == other.port_name())
                    });
                    displace_other_producers(wires, scene_object_id, target_map, id);
                    if !feeds_map(wires, id, scene_object_id, target_map) {
                        wires.push(scene_build_wire(id, "out", scene_object_id, target_map.port_name()));
                    }
                } else {
                    // No existing skin: create one with an empty source layer
                    // so the panel shows "None" / missing chip until the user
                    // picks a source.
                    let new_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
                    let mut params = BTreeMap::new();
                    params.insert(
                        "layer".to_string(),
                        SerializedParamValue::String { value: String::new() },
                    );
                    nodes.push(scene_build_node(
                        new_id,
                        "node.layer_source",
                        Some(format!("Skin {}", target_map.label())),
                        params,
                    ));
                    wires.push(scene_build_wire(new_id, "out", scene_object_id, target_map.port_name()));
                }
                Some(())
            },
        );
        refresh_target_manifest(project, &self.target);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
        refresh_target_manifest(project, &self.target);
    }

    fn description(&self) -> &str {
        match self.target_map {
            SkinTargetMap::Emissive => "Set Skin Target (Emissive)",
            SkinTargetMap::BaseColor => "Set Skin Target (Base Color)",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{mirror_catalog_default, project_with_one_generator_layer};
    use super::*;
    use manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION;

    /// `render_scene(0) ← scene_object(1)` at root scope, mirroring the
    /// scene_vm D3 trace shape the Skin commands address.
    fn object_graph() -> EffectGraphDef {
        let mut render = EffectGraphNode {
            id: 0,
            node_id: manifold_core::NodeId::new("render"),
            type_id: "node.render_scene".to_string(),
            handle: Some("render".to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };
        render.params.insert(
            "objects".to_string(),
            SerializedParamValue::Float { value: 1.0 },
        );
        let object = EffectGraphNode {
            id: 1,
            node_id: manifold_core::NodeId::new("obj"),
            type_id: "node.scene_object".to_string(),
            handle: Some("Statue".to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render, object],
            wires: vec![EffectGraphWire {
                from_node: 1,
                from_port: "object".to_string(),
                to_node: 0,
                to_port: "object_0".to_string(),
            }],
        }
    }

    fn generator_project() -> (Project, manifold_core::LayerId) {
        let (mut project, lid) = project_with_one_generator_layer();
        let target = GraphTarget::Generator(lid.clone());
        with_target_graph_mut(&mut project, &target, &object_graph(), true, |_| Some(())).unwrap();
        (project, lid)
    }

    fn dump_level(project: &Project, lid: &manifold_core::LayerId) {
        let def = project
            .timeline
            .find_layer_by_id(lid)
            .unwrap()
            .1
            .gen_params()
            .unwrap()
            .graph
            .as_ref()
            .unwrap();
        for n in &def.nodes {
            println!(
                "node {} type={} layer={:?}",
                n.id,
                n.type_id,
                n.params.get("layer")
            );
        }
        for w in &def.wires {
            println!("wire {}:{} -> {}:{}", w.from_node, w.from_port, w.to_node, w.to_port);
        }
    }

    #[test]
    fn set_then_move_target_map_keeps_binding() {
        let (mut project, lid) = generator_project();
        let target = GraphTarget::Generator(lid.clone());

        let mut set = SetSceneObjectSkinSourceCommand::new(
            target.clone(),
            vec![],
            1,
            None,
            SkinTargetMap::Emissive,
            Some("layer-a".to_string()),
            mirror_catalog_default(),
        );
        set.execute(&mut project);
        println!("-- after set source --");
        dump_level(&project, &lid);

        let mut map = SetSceneObjectSkinTargetMapCommand::new(
            target.clone(),
            vec![],
            1,
            Some(2),
            SkinTargetMap::BaseColor,
            mirror_catalog_default(),
        );
        map.execute(&mut project);
        println!("-- after set target map --");
        dump_level(&project, &lid);

        let def = project
            .timeline
            .find_layer_by_id(&lid)
            .unwrap()
            .1
            .gen_params()
            .unwrap()
            .graph
            .as_ref()
            .unwrap()
            .clone();
        let skin_wires: Vec<&EffectGraphWire> = def
            .wires
            .iter()
            .filter(|w| w.to_node == 1 && (w.to_port == "emissive_map" || w.to_port == "base_color_map"))
            .collect();
        println!("skin wires: {skin_wires:?}");
        assert_eq!(skin_wires.len(), 1, "exactly one map wire after the move");
        assert_eq!(skin_wires[0].to_port, "base_color_map");
        let src = def.nodes.iter().find(|n| n.type_id == "node.layer_source").unwrap();
        assert_eq!(
            src.params.get("layer"),
            Some(&SerializedParamValue::String { value: "layer-a".to_string() }),
            "moving the target map must keep the layer binding"
        );
        assert_eq!(def.nodes.iter().filter(|n| n.type_id == "node.layer_source").count(), 1);
    }

    #[test]
    fn set_then_move_target_map_in_group_scope_keeps_binding() {
        use super::super::AddSceneObjectCommand;
        use manifold_core::effect_graph_def::GROUP_TYPE_ID;

        let (mut project, lid) = project_with_one_generator_layer();
        let target = GraphTarget::Generator(lid.clone());
        // A bare render_scene at root…
        let mut render_graph = object_graph();
        render_graph.nodes.truncate(1);
        render_graph.wires.clear();
        render_graph.nodes[0]
            .params
            .insert("objects".to_string(), SerializedParamValue::Float { value: 0.0 });
        with_target_graph_mut(&mut project, &target, &render_graph, true, |_| Some(()))
            .unwrap();
        // …then a real AddSceneObjectCommand so the object sits inside a
        // GROUP node's nested level, the topology the panel addresses.
        let mut add = AddSceneObjectCommand::new(
            target.clone(),
            vec![],
            0,
            0,
            (0.0, 0.0),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            mirror_catalog_default(),
        );
        add.execute(&mut project);

        let def = |project: &Project| {
            project
                .timeline
                .find_layer_by_id(&lid)
                .unwrap()
                .1
                .gen_params()
                .unwrap()
                .graph
                .clone()
                .unwrap()
        };
        let d = def(&project);
        let group = d.nodes.iter().find(|n| n.type_id == GROUP_TYPE_ID).unwrap();
        let group_id = group.id;
        let inner = group.group.as_ref().unwrap();
        let object_id = inner
            .nodes
            .iter()
            .find(|n| n.type_id == "node.scene_object")
            .unwrap()
            .id;
        println!("group_id={group_id} object_id={object_id}");

        let mut set = SetSceneObjectSkinSourceCommand::new(
            target.clone(),
            vec![group_id],
            object_id,
            None,
            SkinTargetMap::Emissive,
            Some("layer-a".to_string()),
            mirror_catalog_default(),
        );
        set.execute(&mut project);
        let d = def(&project);
        let inner = d.nodes.iter().find(|n| n.id == group_id).unwrap().group.clone().unwrap();
        let minted = inner
            .nodes
            .iter()
            .find(|n| n.type_id == "node.layer_source")
            .expect("source node minted into the group level")
            .id;
        println!("-- after set source (group scope) --");
        for n in &inner.nodes {
            println!("node {} type={} layer={:?}", n.id, n.type_id, n.params.get("layer"));
        }
        for w in &inner.wires {
            println!("wire {}:{} -> {}:{}", w.from_node, w.from_port, w.to_node, w.to_port);
        }

        // Simulate the glTF import's baked texture on base_color_map (the
        // occupancy the real fixture shows): the move must displace it, not
        // share the port. Root level (no descend): the group node itself is
        // what holds the inner wires.
        with_target_graph_mut(&mut project, &target, &mirror_catalog_default(), true, |def| {
            let group = def.nodes.iter_mut().find(|n| n.id == group_id)?;
            group
                .group
                .as_mut()?
                .wires
                .push(scene_build_wire(99, "out", object_id, "base_color_map"));
            Some(())
        })
        .unwrap();

        let mut map = SetSceneObjectSkinTargetMapCommand::new(
            target.clone(),
            vec![group_id],
            object_id,
            Some(minted),
            SkinTargetMap::BaseColor,
            mirror_catalog_default(),
        );
        map.execute(&mut project);
        let d = def(&project);
        let inner = d.nodes.iter().find(|n| n.id == group_id).unwrap().group.clone().unwrap();
        println!("-- after set target map (group scope) --");
        for n in &inner.nodes {
            println!("node {} type={} layer={:?}", n.id, n.type_id, n.params.get("layer"));
        }
        for w in &inner.wires {
            println!("wire {}:{} -> {}:{}", w.from_node, w.from_port, w.to_node, w.to_port);
        }

        let map_wires: Vec<&EffectGraphWire> = inner
            .wires
            .iter()
            .filter(|w| {
                w.to_node == object_id
                    && (w.to_port == "emissive_map" || w.to_port == "base_color_map")
            })
            .collect();
        assert_eq!(map_wires.len(), 1, "exactly one map wire after the move");
        assert_eq!(map_wires[0].to_port, "base_color_map");
        assert!(
            !inner.wires.iter().any(|w| w.from_node == 99),
            "the baked texture wire is displaced, not shared with"
        );
        let sources: Vec<_> = inner
            .nodes
            .iter()
            .filter(|n| n.type_id == "node.layer_source")
            .collect();
        assert_eq!(sources.len(), 1, "no second layer_source minted");
        assert_eq!(
            sources[0].params.get("layer"),
            Some(&SerializedParamValue::String { value: "layer-a".to_string() }),
            "moving the target map must keep the layer binding"
        );
    }

    /// A source change on an EXISTING skin is a value-only edit; its undo
    /// must restore the previous layer id (the pre-edit level snapshot, not a
    /// post-edit one).
    #[test]
    fn value_edit_undo_restores_previous_layer() {
        let (mut project, lid) = generator_project();
        let target = GraphTarget::Generator(lid.clone());

        let mut set_a = SetSceneObjectSkinSourceCommand::new(
            target.clone(),
            vec![],
            1,
            None,
            SkinTargetMap::Emissive,
            Some("layer-a".to_string()),
            mirror_catalog_default(),
        );
        set_a.execute(&mut project);
        let mut set_b = SetSceneObjectSkinSourceCommand::new(
            target.clone(),
            vec![],
            1,
            Some(2),
            SkinTargetMap::Emissive,
            Some("layer-b".to_string()),
            mirror_catalog_default(),
        );
        set_b.execute(&mut project);
        set_b.undo(&mut project);

        let def = project
            .timeline
            .find_layer_by_id(&lid)
            .unwrap()
            .1
            .gen_params()
            .unwrap()
            .graph
            .as_ref()
            .unwrap();
        assert_eq!(
            def.nodes.iter().find(|n| n.id == 2).unwrap().params.get("layer"),
            Some(&SerializedParamValue::String { value: "layer-a".to_string() }),
            "undo of a value-only source change restores the previous layer id"
        );
    }
}
