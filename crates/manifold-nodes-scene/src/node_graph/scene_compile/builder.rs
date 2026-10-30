//! The compiler's working state: the nodes and wires it has emitted, which
//! node plays which role for which item, and the manifest the card rows make.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EFFECT_GRAPH_VERSION_WITH_METADATA,
    EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    ParamSpecDef, PresetMetadata, SerializedParamValue,
};
use manifold_core::param_lookup::NodeParamMetadata;
use manifold_core::scene_doc::{Item, SceneDoc, param_id};
use manifold_core::scene_modifier_preset::SceneModifierInstanceDef;
use manifold_core::scene_schema::{self, CardFacts, Exposure, PropertyRow, Reach};
use manifold_foundation::ItemId;
use manifold_foundation::scene_surface::{ControlGroup, SurfaceRole};

use manifold_core::NodeId;
use manifold_node_engine::load::expand::{
    TargetPlacement, capture_mesh_frames, resolve_modifier_mesh_frames,
};

use crate::node_graph::scene_graph::{plain_node, wire};

/// The owner of the nodes a scene has once, outside any item. Item ids are
/// never this word.
pub(crate) const SCENE: &str = "scene";

pub(crate) struct Builder<'d> {
    pub(crate) doc: &'d SceneDoc,
    nodes: Vec<EffectGraphNode>,
    wires: Vec<EffectGraphWire>,
    params: Vec<ParamSpecDef>,
    bindings: Vec<BindingDef>,
    next_id: u32,
    roles: BTreeMap<(String, &'static str), u32>,
    meta: BTreeMap<String, Vec<NodeParamMetadata>>,
    /// Every camera's lens, in tree order.
    pub(crate) lenses: Vec<u32>,
    /// The draws and lights the render node takes, in tree order.
    pub(crate) objects: Vec<u32>,
    /// The item each entry of `objects` draws.
    pub(crate) object_items: Vec<ItemId>,
    pub(crate) lights: Vec<u32>,
    /// The node whose `out` is the finished image: the cinematic tail the
    /// camera emitter built, or what the Environment put after it. `None`
    /// sends the render straight out.
    pub(crate) tail: Option<u32>,
    /// The cut's camera (the camera switch), for what draws behind the scene.
    pub(crate) active_lens: Option<u32>,
    /// Each Material's texture sources and the `node.scene_object` port
    /// each feeds, by material id.
    /// The material each item draws with (`SceneDoc::effective_materials`).
    pub(crate) effective_materials: std::collections::HashMap<ItemId, ItemId>,
    pub(crate) material_maps: BTreeMap<String, Vec<(u32, &'static str)>>,
    /// Every imported part's animation clocks: (part id, clock node).
    pub(crate) clocks: Vec<(ItemId, u32)>,
    pub(crate) physics: super::physics::State,
    /// The copies bodies, in copy-group order.
    pub(crate) copy_bodies: Vec<u32>,
    /// Each group copies body's parts, (mesh node, transform node), by body.
    pub(crate) copy_parts: BTreeMap<u32, Vec<(u32, u32)>>,
    /// The transform each Group's lights and cameras sit in, by group.
    pub(crate) group_frames: BTreeMap<ItemId, u32>,
    /// The node whose `out` is each Group's visibility, by group.
    pub(crate) group_visible: BTreeMap<ItemId, u32>,
    /// The Swarms, in tree order.
    pub(crate) swarms: Vec<ItemId>,
    /// The swarm copies node that draws each Object or Group a Swarm draws as:
    /// the first swarm's in tree order.
    pub(crate) swarm_draws: BTreeMap<ItemId, u32>,
    /// Every later swarm drawing an item the first already draws: (item,
    /// swarm, its copies node).
    pub(crate) swarm_extra: Vec<(ItemId, ItemId, u32)>,
    /// Each scene object a later swarm draws: (the first swarm's object it
    /// copies, the copy, the swarm).
    pub(crate) alike: Vec<(u32, u32, ItemId)>,
    /// The emitter transform each swarm copies node is placed in, and the
    /// node whose `out` is the swarm's live Count.
    pub(crate) swarm_frames: BTreeMap<u32, (u32, u32)>,
    /// Each swarm's `node.nest_copies` (a drawn Group with Copies on), with
    /// the group's copies body once it is laid out.
    pub(crate) nests: BTreeMap<u32, Option<u32>>,
    /// The scene modifiers (forces, then modifier cards), in application order.
    modifiers: Vec<SceneModifierInstanceDef>,
    /// Each modifier instance's rest-pose frame, by instance: its targets'
    /// source offsets and the frame's radius.
    frames: BTreeMap<NodeId, FramePlan>,
    /// Every drawn object a modifier can measure, in draw order.
    pub(crate) drawn: Vec<Drawn>,
    /// The Objects a smoothly bending Modifier that is on reaches: their built-in
    /// shapes get mesh detail.
    pub(crate) smoothed: BTreeSet<ItemId>,
    /// Objects whose imported meshes draw as flat triangle lists
    /// ([`super::modifiers::flat_mesh_items`]).
    pub(crate) flat_meshes: BTreeSet<ItemId>,
}

/// A mesh's rest pose (SCENE_DOCUMENT_DESIGN.md ruling 20): every shape
/// supplies one, and every modifier recipe reads it the same way.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rest {
    /// The mesh's bounds about its own origin.
    pub lo: [f32; 3],
    pub hi: [f32; 3],
    /// Its box where it sits at rest in the scene, when the compiler knows
    /// it (an imported part: where the file puts it); else `lo` and `hi`
    /// about the scene's origin.
    pub placed: Option<([f32; 3], [f32; 3])>,
    /// Whether the mesh draws in its file node's own space, its Transform
    /// carrying the rotation and scale the file gave it: a frame it shares
    /// with other parts carries it whole, so it bends as one body with them.
    pub own_space: bool,
    pub vertices: u32,
}

/// One drawn object whose mesh has a rest pose.
#[derive(Debug, Clone)]
pub(crate) struct Drawn {
    /// The `node.scene_object`.
    pub object: u32,
    /// The Object item it draws.
    pub item: ItemId,
    pub rest: Rest,
}

/// One modifier instance's frame: where each target sits in it, and the
/// frame's radius.
pub(crate) struct FramePlan {
    pub placements: Vec<TargetPlacement>,
    pub radius: f64,
}

impl<'d> Builder<'d> {
    pub(crate) fn new(doc: &'d SceneDoc) -> Self {
        let mut b = Self {
            doc,
            nodes: Vec::new(),
            wires: Vec::new(),
            params: Vec::new(),
            bindings: Vec::new(),
            next_id: 1,
            roles: BTreeMap::new(),
            meta: BTreeMap::new(),
            lenses: Vec::new(),
            objects: Vec::new(),
            object_items: Vec::new(),
            lights: Vec::new(),
            tail: None,
            active_lens: None,
            effective_materials: doc.effective_materials(),
            material_maps: BTreeMap::new(),
            clocks: Vec::new(),
            physics: super::physics::State::new(doc),
            copy_bodies: Vec::new(),
            copy_parts: BTreeMap::new(),
            group_frames: BTreeMap::new(),
            group_visible: BTreeMap::new(),
            swarms: Vec::new(),
            swarm_draws: BTreeMap::new(),
            swarm_extra: Vec::new(),
            alike: Vec::new(),
            swarm_frames: BTreeMap::new(),
            nests: BTreeMap::new(),
            modifiers: Vec::new(),
            frames: BTreeMap::new(),
            drawn: Vec::new(),
            smoothed: super::modifiers::smoothed_items(doc),
            flat_meshes: super::modifiers::flat_mesh_items(doc),
        };
        b.add(SCENE, "input", "system.generator_input");
        b.add(SCENE, "render", "node.render_scene");
        b.add(SCENE, "output", "system.final_output");
        b
    }

    /// A fresh doc id.
    pub(crate) fn fresh(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Emit a node of `type_id` playing `role` for `owner`.
    pub(crate) fn add(&mut self, owner: &str, role: &'static str, type_id: &str) -> u32 {
        let id = self.fresh();
        let node_id = format!("{owner}:{role}");
        self.nodes.push(plain_node(id, &node_id, type_id, &node_id));
        self.roles.insert((owner.to_owned(), role), id);
        id
    }

    /// Emit a node of `type_id` under the node id `node_id`, for a node one
    /// owner makes many of (a modifier card's per-part nodes).
    pub(crate) fn add_named(&mut self, node_id: NodeId, type_id: &str) -> u32 {
        let id = self.fresh();
        self.nodes
            .push(plain_node(id, node_id.as_str(), type_id, node_id.as_str()));
        id
    }

    /// A copy of node `original` under `node_id`: its params, every wire into
    /// it but `except`'s port with each source `remap` names swapped, and
    /// every binding that drives it, so the copy follows the same rows.
    pub(crate) fn copy_node(
        &mut self,
        original: u32,
        node_id: NodeId,
        remap: &[(u32, u32)],
        except: Option<&str>,
    ) -> Option<u32> {
        let mut node = self.node_mut(original)?.clone();
        let original_id = std::mem::replace(&mut node.node_id, node_id.clone());
        let id = self.fresh();
        node.id = id;
        node.handle = Some(node_id.as_str().to_owned());
        self.nodes.push(node);
        self.copy_inputs(original, id, remap, except);
        let driven: Vec<BindingDef> = self
            .bindings
            .iter()
            .filter(|binding| matches!(&binding.target, BindingTarget::Node { node_id, .. } if *node_id == original_id))
            .cloned()
            .collect();
        for mut binding in driven {
            if let BindingTarget::Node {
                node_id: target, ..
            } = &mut binding.target
            {
                target.clone_from(&node_id);
            }
            self.bindings.push(binding);
        }
        Some(id)
    }

    /// Wire into `to` what feeds `from`, but `except`'s port, with each
    /// source `remap` names swapped.
    pub(crate) fn copy_inputs(
        &mut self,
        from: u32,
        to: u32,
        remap: &[(u32, u32)],
        except: Option<&str>,
    ) {
        let inputs: Vec<EffectGraphWire> = self
            .wires
            .iter()
            .filter(|wire| wire.to_node == from && Some(wire.to_port.as_str()) != except)
            .cloned()
            .collect();
        for wire in inputs {
            let source = remap
                .iter()
                .find(|(old, _)| *old == wire.from_node)
                .map_or(wire.from_node, |(_, new)| *new);
            self.wire(source, &wire.from_port, to, &wire.to_port);
        }
    }

    /// The copies later swarms draw of scene object `original`.
    pub(crate) fn alike_of(&self, original: u32) -> impl Iterator<Item = u32> + '_ {
        self.alike
            .iter()
            .filter(move |(of, _, _)| *of == original)
            .map(|(_, copy, _)| *copy)
    }

    /// Add a node built elsewhere (a group with its body). Its node ids are
    /// the caller's.
    pub(crate) fn push_node(&mut self, node: EffectGraphNode) {
        self.nodes.push(node);
    }

    /// Record that `id` (already emitted, perhaps inside a group) plays
    /// `role` for `owner`.
    pub(crate) fn adopt(&mut self, owner: &str, role: &'static str, id: u32) {
        self.roles.insert((owner.to_owned(), role), id);
    }

    /// Bind `to` to every card row bound to `from`, starting at `from`'s
    /// values: a second node that follows the same rows.
    pub(crate) fn mirror(&mut self, from: u32, to: u32) {
        let Some(source) = self
            .node_mut(from)
            .map(|node| (node.node_id.clone(), node.params.clone()))
        else {
            return;
        };
        let Some(target) = self.node_mut(to) else {
            return;
        };
        target.params = source.1;
        let target_id = target.node_id.clone();
        let mirrored: Vec<BindingDef> = self
            .bindings
            .iter()
            .filter(|binding| matches!(&binding.target, BindingTarget::Node { node_id, .. } if *node_id == source.0))
            .map(|binding| {
                let mut binding = binding.clone();
                if let BindingTarget::Node { node_id, .. } = &mut binding.target {
                    node_id.clone_from(&target_id);
                }
                binding
            })
            .collect();
        self.bindings.extend(mirrored);
    }

    /// Drive `to`'s `to_param` from every card row bound to `from`'s
    /// `from_param`, starting at its value: a second node that follows the
    /// same control.
    pub(crate) fn follow(&mut self, from: u32, from_param: &str, to: u32, to_param: &str) {
        self.follow_offset(from, from_param, to, to_param, 0.0);
    }

    /// [`Self::follow`], with `to_param` at the control's value plus `offset`.
    pub(crate) fn follow_offset(
        &mut self,
        from: u32,
        from_param: &str,
        to: u32,
        to_param: &str,
        offset: f32,
    ) {
        let Some(source) = self
            .node_mut(from)
            .map(|node| (node.node_id.clone(), node.params.get(from_param).cloned()))
        else {
            return;
        };
        let Some((target_id, target_type)) = self
            .node_mut(to)
            .map(|node| (node.node_id.clone(), node.type_id.clone()))
        else {
            return;
        };
        if let Some(value) = source.1 {
            self.set_number(to, to_param, number(&value) + offset);
        }
        // The target converts the row's number (plus any shift) as its own
        // param is typed, not the source's.
        let convert = self
            .param_meta(&target_type, to_param)
            .map(|meta| meta.convert);
        let followed: Vec<BindingDef> = self
            .bindings
            .iter()
            .filter(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param } if *node_id == source.0 && param == from_param))
            .map(|binding| {
                let mut binding = binding.clone();
                binding.target = BindingTarget::Node { node_id: target_id.clone(), param: to_param.to_owned() };
                binding.offset += offset;
                if let Some(convert) = convert {
                    binding.convert = convert;
                }
                binding
            })
            .collect();
        self.bindings.extend(followed);
    }

    /// Scale node `id`'s `param`, and every card binding that writes it, by
    /// `factor`: the row's value stays the user's, the node gets it times
    /// `factor`, live edits included.
    pub(crate) fn scale_param(&mut self, id: u32, param: &str, factor: f32) {
        let Some((node_id, value)) = self
            .node_mut(id)
            .map(|node| (node.node_id.clone(), node.params.get(param).cloned()))
        else {
            return;
        };
        if let Some(value) = value {
            self.set_number(id, param, number(&value) * factor);
        }
        for binding in &mut self.bindings {
            if matches!(&binding.target, BindingTarget::Node { node_id: n, param: p } if *n == node_id && p == param)
            {
                binding.scale *= factor;
                binding.offset *= factor;
            }
        }
    }

    /// Make the card param `id` a whole number from 0 to `last` with no
    /// labels of its own: the panel names its values from the document.
    pub(crate) fn pick_range(&mut self, id: &str, last: f32) {
        let Some(spec) = self.params.iter_mut().find(|spec| spec.id == id) else {
            return;
        };
        spec.min = 0.0;
        spec.max = last;
        spec.whole_numbers = true;
        spec.default_value = spec.default_value.clamp(0.0, last);
        spec.value_labels.clear();
    }

    pub(crate) fn id(&self, owner: &str, role: &'static str) -> Option<u32> {
        self.roles.get(&(owner.to_owned(), role)).copied()
    }

    /// The node and port of an Object's undeformed mesh vertices: its mesh
    /// node's, or a curve shape's swept triangles (a curve's `mesh` is split
    /// into the drawn triangles and the CPU solid). An Ocean has none.
    pub(crate) fn mesh_vertices(&self, owner: &str) -> Option<(u32, &'static str)> {
        if let Some(mesh) = self.id(owner, "mesh") {
            return Some((mesh, "vertices"));
        }
        self.id(owner, scene_schema::SOLID)
            .and_then(|_| self.id(owner, scene_schema::TRIANGLES))
            .map(|triangles| (triangles, "out"))
    }

    pub(crate) fn scene(&self, role: &'static str) -> u32 {
        self.id(SCENE, role).expect("scene-level node")
    }

    pub(crate) fn wire(&mut self, from: u32, from_port: &str, to: u32, to_port: &str) {
        self.wires.push(wire(from, from_port, to, to_port));
    }

    /// Whether something already feeds `to`'s `to_port`.
    pub(crate) fn wired_into(&self, to: u32, to_port: &str) -> bool {
        self.wires
            .iter()
            .any(|wire| wire.to_node == to && wire.to_port == to_port)
    }

    /// Put a pass after `from`'s `port`: every wire that read that port now
    /// reads `to`'s `to_port`. Wire the pass's own input from `from` after.
    pub(crate) fn splice(&mut self, from: u32, port: &str, to: u32, to_port: &str) {
        for wire in &mut self.wires {
            if wire.from_node == from && wire.from_port == port {
                wire.from_node = to;
                to_port.clone_into(&mut wire.from_port);
            }
        }
    }

    fn node_mut(&mut self, id: u32) -> Option<&mut EffectGraphNode> {
        fn find(nodes: &mut [EffectGraphNode], id: u32) -> Option<&mut EffectGraphNode> {
            for node in nodes {
                if node.id == id {
                    return Some(node);
                }
                if let Some(body) = node.group.as_deref_mut()
                    && let Some(found) = find(&mut body.nodes, id)
                {
                    return Some(found);
                }
            }
            None
        }
        find(&mut self.nodes, id)
    }

    fn type_of(&mut self, id: u32) -> String {
        self.node_mut(id)
            .map(|node| node.type_id.clone())
            .unwrap_or_default()
    }

    pub(crate) fn set(&mut self, id: u32, param: &str, value: SerializedParamValue) {
        if let Some(node) = self.node_mut(id) {
            node.params.insert(param.to_owned(), value);
        }
    }

    /// Set a node param from a number, typed as the param declares it.
    pub(crate) fn set_number(&mut self, id: u32, param: &str, value: f32) {
        let type_id = self.type_of(id);
        let like = self
            .param_meta(&type_id, param)
            .map(|meta| meta.default_value);
        let typed = match like {
            Some(SerializedParamValue::Int { .. }) => SerializedParamValue::Int {
                value: value.round() as i32,
            },
            Some(SerializedParamValue::Bool { .. }) => {
                SerializedParamValue::Bool { value: value > 0.5 }
            }
            Some(SerializedParamValue::Enum { .. }) => SerializedParamValue::Enum {
                value: value.max(0.0).round() as u32,
            },
            _ => SerializedParamValue::Float { value },
        };
        self.set(id, param, typed);
    }

    pub(crate) fn param_meta(&mut self, type_id: &str, param: &str) -> Option<NodeParamMetadata> {
        self.meta
            .entry(type_id.to_owned())
            .or_insert_with(|| manifold_core::param_lookup::node_params(type_id))
            .iter()
            .find(|meta| meta.name == param)
            .cloned()
    }

    /// Every param `type_id` declares.
    pub(crate) fn type_params(&mut self, type_id: &str) -> Vec<NodeParamMetadata> {
        self.meta
            .entry(type_id.to_owned())
            .or_insert_with(|| manifold_core::param_lookup::node_params(type_id))
            .clone()
    }

    /// Apply the item's (kind, property) rows to the nodes emitted for it:
    /// card rows become manifest params bound to their nodes, internal rows
    /// with a kind default write it. A row whose node was not emitted is
    /// skipped. Automatic rows were set by the emitter.
    pub(crate) fn rows(&mut self, item: &Item) {
        self.apply_rows(
            item,
            scene_schema::applicable(&item.fields).filter(|row| row.reach != Reach::Clocks),
        );
    }

    /// Apply the item's rows that reach the animation clocks below it, once
    /// its parts have compiled.
    pub(crate) fn clock_rows(&mut self, item: &Item) {
        self.apply_rows(
            item,
            scene_schema::applicable(&item.fields).filter(|row| row.reach == Reach::Clocks),
        );
    }

    /// Apply `rows` of the item's table, as [`Self::rows`] does: for a node
    /// emitted after the item's own rows.
    pub(crate) fn apply_rows<'r>(
        &mut self,
        item: &Item,
        rows: impl IntoIterator<Item = &'r PropertyRow>,
    ) {
        for row in rows {
            let targets: Vec<u32> = match row.reach {
                Reach::Own(role) => self.id(item.id.as_str(), role).into_iter().collect(),
                Reach::Scene(role) => self.id(SCENE, role).into_iter().collect(),
                Reach::OwnAndScene(own, scene) => self
                    .id(item.id.as_str(), own)
                    .into_iter()
                    .chain(self.id(SCENE, scene))
                    .collect(),
                Reach::EveryCameraLens => self.lenses.clone(),
                Reach::Clocks => self.clocks_below(item),
                // A scene modifier's params are declared with the modifier.
                Reach::Modifier => Vec::new(),
            };
            if targets.is_empty() {
                continue;
            }
            match row.exposure {
                Exposure::Card(facts) => self.card(item, row, &facts, &targets),
                Exposure::Internal(_) => {
                    if let Some(value) = row.default {
                        for &target in &targets {
                            self.set_number(target, row.param, value);
                        }
                    }
                }
                Exposure::Automatic(_) => {}
            }
        }
    }

    /// The clock nodes of every part inside `item`.
    fn clocks_below(&self, item: &Item) -> Vec<u32> {
        let mut inside = Vec::new();
        fn visit<'a>(item: &'a Item, out: &mut Vec<&'a ItemId>) {
            for child in &item.children {
                out.push(&child.id);
                visit(child, out);
            }
        }
        visit(item, &mut inside);
        self.clocks
            .iter()
            .filter(|(part, _)| inside.contains(&part))
            .map(|(_, clock)| *clock)
            .collect()
    }

    /// The number a node param was set to, if it was.
    pub(crate) fn started(&mut self, id: u32, param: &str) -> Option<f32> {
        self.node_mut(id)
            .and_then(|node| node.params.get(param).map(number))
    }

    fn card(&mut self, item: &Item, row: &PropertyRow, facts: &CardFacts, targets: &[u32]) {
        let type_id = self.type_of(targets[0]);
        let Some(meta) = self.param_meta(&type_id, row.param) else {
            return;
        };
        let id = param_id(&item.id, row.property);
        // The kind's default, else what the emitter started the node at,
        // else the node's own default.
        let started = self
            .node_mut(targets[0])
            .and_then(|node| node.params.get(row.param).map(number));
        let default = row
            .default
            .or(started)
            .unwrap_or_else(|| number(&meta.default_value));
        let mut spec = card_spec(
            id,
            row.name.unwrap_or(&meta.label),
            &meta,
            default,
            row.section,
            facts,
        );
        spec.material_role = meta.material_role;
        self.declare(spec, &meta, targets, row.param);
    }

    /// A card param `spec` bound to `param` on every target node, each target
    /// set to the spec's default.
    pub(crate) fn declare(
        &mut self,
        spec: ParamSpecDef,
        meta: &NodeParamMetadata,
        targets: &[u32],
        param: &str,
    ) {
        for &target in targets {
            self.set_number(target, param, spec.default_value);
            let node_id = self
                .node_mut(target)
                .map(|node| node.node_id.clone())
                .unwrap_or_default();
            self.bindings.push(BindingDef {
                id: spec.id.clone(),
                label: spec.name.clone(),
                default_value: spec.default_value,
                target: BindingTarget::Node {
                    node_id,
                    param: param.to_owned(),
                },
                convert: meta.convert,
                user_added: false,
                scale: 1.0,
                offset: 0.0,
                default_mirrors_node_param: true,
            });
        }
        self.params.push(spec);
    }

    pub(crate) fn finish(mut self) -> Result<EffectGraphDef, String> {
        let render = self.scene("render");
        let objects = self.objects.clone();
        let lights = self.lights.clone();
        self.set(
            render,
            "objects",
            SerializedParamValue::Int {
                value: objects.len().max(1) as i32,
            },
        );
        self.set(
            render,
            "lights",
            SerializedParamValue::Int {
                value: lights.len() as i32,
            },
        );
        for (k, object) in objects.into_iter().enumerate() {
            self.wire(object, "object", render, &format!("object_{k}"));
        }
        for (k, light) in lights.into_iter().enumerate() {
            self.wire(light, "out", render, &format!("light_{k}"));
        }
        let output = self.scene("output");
        match self.tail {
            Some(tail) => self.wire(tail, "out", output, "in"),
            None => self.wire(render, "color", output, "in"),
        }
        let bounds = self.scene_bounds();
        let mut metadata = scene_metadata(self.params, self.bindings);
        metadata.scene_bounds = bounds;
        let mut def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: Some("Scene".into()),
            description: None,
            preset_metadata: Some(metadata),
            scene_modifiers: Vec::new(),
            nodes: self.nodes,
            wires: self.wires,
        };
        if !self.modifiers.is_empty() {
            def.version = EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS;
            // A recipe that reads the source mesh's placement takes it from
            // the rest pose its targets compiled with.
            for modifier in &mut self.modifiers {
                let frames = match self.frames.get(&modifier.id) {
                    Some(plan) => {
                        capture_mesh_frames(&def, modifier, &plan.placements, plan.radius)
                    }
                    None => resolve_modifier_mesh_frames(&def, modifier),
                };
                modifier.mesh_frames =
                    frames.map_err(|error| format!("{}: {error}", modifier.id))?;
            }
            def.scene_modifiers = self.modifiers;
        }
        Ok(def)
    }

    /// The rest-pose box of everything drawn: the whole scene's bounds.
    fn scene_bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        rest_box(self.drawn.iter().map(|drawn| &drawn.rest))
    }

    /// Attach a scene modifier instance with the host params and bindings
    /// that drive its recipe's params, and its rest-pose frame when its
    /// owner measured one.
    pub(crate) fn push_modifier(
        &mut self,
        instance: SceneModifierInstanceDef,
        params: Vec<ParamSpecDef>,
        bindings: Vec<BindingDef>,
        frame: Option<FramePlan>,
    ) {
        if let Some(frame) = frame {
            self.frames.insert(instance.id.clone(), frame);
        }
        self.modifiers.push(instance);
        self.params.extend(params);
        self.bindings.extend(bindings);
    }

    /// Declare manifest params that drive no node yet (a modifier card with
    /// nothing to act on keeps its values).
    pub(crate) fn push_params(&mut self, params: impl IntoIterator<Item = ParamSpecDef>) {
        self.params.extend(params);
    }

    /// The node and port wired into `to`'s `port`, if any.
    pub(crate) fn input(&self, to: u32, port: &str) -> Option<(u32, String)> {
        self.wires
            .iter()
            .find(|wire| wire.to_node == to && wire.to_port == port)
            .map(|wire| (wire.from_node, wire.from_port.clone()))
    }

    /// The type of node `id`.
    pub(crate) fn node_type(&self, id: u32) -> Option<&str> {
        fn find(nodes: &[EffectGraphNode], id: u32) -> Option<&EffectGraphNode> {
            nodes.iter().find_map(|node| {
                if node.id == id {
                    Some(node)
                } else {
                    node.group.as_deref().and_then(|body| find(&body.nodes, id))
                }
            })
        }
        find(&self.nodes, id).map(|node| node.type_id.as_str())
    }

    /// The stable id of node `id`.
    pub(crate) fn node_id_of(&mut self, id: u32) -> Option<NodeId> {
        self.node_mut(id).map(|node| node.node_id.clone())
    }
}

/// The box holding every rest pose in `rests`, each where it is placed.
pub(crate) fn rest_box<'r>(
    rests: impl IntoIterator<Item = &'r Rest>,
) -> Option<([f32; 3], [f32; 3])> {
    let mut found: Option<([f32; 3], [f32; 3])> = None;
    for rest in rests {
        let (lo, hi) = rest.placed.unwrap_or((rest.lo, rest.hi));
        found = Some(match found {
            None => (lo, hi),
            Some((min, max)) => (
                std::array::from_fn(|axis| min[axis].min(lo[axis])),
                std::array::from_fn(|axis| max[axis].max(hi[axis])),
            ),
        });
    }
    found.filter(|(min, max)| min.iter().chain(max.iter()).all(|value| value.is_finite()))
}

/// A param's default as a number.
pub(crate) fn number(value: &SerializedParamValue) -> f32 {
    match value {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Int { value } => *value as f32,
        SerializedParamValue::Bool { value } => f32::from(u8::from(*value)),
        SerializedParamValue::Enum { value } => *value as f32,
        _ => 0.0,
    }
}

/// A card row's description: the node's own range and flags, the table's
/// placement, unit and limit. A default outside the node's slider range
/// widens it, for plain numbers only.
pub(crate) fn card_spec(
    id: String,
    name: &str,
    meta: &NodeParamMetadata,
    default: f32,
    section: ControlGroup,
    facts: &CardFacts,
) -> ParamSpecDef {
    let toggle = meta.is_toggle || facts.toggle;
    let widen = !meta.whole_numbers && !toggle && !meta.is_trigger && facts.range.is_none();
    let (min, max) = match facts.range {
        Some(range) => range,
        None if facts.toggle => (0.0, 1.0),
        None if widen => (meta.min.min(default), meta.max.max(default)),
        None => (meta.min, meta.max),
    };
    ParamSpecDef {
        id,
        name: name.to_owned(),
        min,
        max,
        default_value: default,
        whole_numbers: meta.whole_numbers || facts.toggle,
        is_toggle: toggle,
        is_trigger: meta.is_trigger,
        value_labels: meta.value_labels.clone(),
        is_angle: meta.is_angle,
        wraps: meta.wraps,
        card_visible: facts.role == SurfaceRole::Performance,
        limit: facts.limit,
        unit: facts.unit,
        scene: Some(facts.desc(section)),
        tooltip: facts.tip.map(str::to_owned),
        ..ParamSpecDef::default()
    }
}

fn scene_metadata(params: Vec<ParamSpecDef>, bindings: Vec<BindingDef>) -> PresetMetadata {
    PresetMetadata {
        shape: None,
        id: manifold_core::PresetTypeId::from_string("Scene".to_owned()),
        display_name: "Scene".into(),
        category: "Geometry".into(),
        osc_prefix: "scene".into(),
        legacy_discriminant: None,
        available: true,
        is_line_based: false,
        layer_types: None,
        params,
        bindings,
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: Vec::new(),
        string_bindings: Vec::new(),
        scene_modifier: None,
        scene_bounds: None,
    }
}
