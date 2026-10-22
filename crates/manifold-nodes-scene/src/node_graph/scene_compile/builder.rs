//! The compiler's working state: the nodes and wires it has emitted, which
//! node plays which role for which item, and the manifest the card rows make.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EFFECT_GRAPH_VERSION_WITH_METADATA, EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS,
    EffectGraphDef, EffectGraphNode, EffectGraphWire,
    ParamSpecDef, PresetMetadata, SerializedParamValue,
};
use manifold_core::scene_doc::{Item, SceneDoc, param_id};
use manifold_foundation::ItemId;
use manifold_core::param_lookup::NodeParamMetadata;
use manifold_core::scene_modifier_preset::SceneModifierInstanceDef;
use manifold_core::scene_schema::{self, CardFacts, Exposure, PropertyRow, Reach};
use manifold_foundation::scene_surface::{ControlGroup, SurfaceRole};

use manifold_node_engine::load::expand::resolve_modifier_mesh_frames;

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
    /// The active camera's lens, for what draws behind the scene.
    pub(crate) active_lens: Option<u32>,
    /// Each Material's texture sources and the `node.scene_object` port
    /// each feeds, by material id.
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
    /// The scene modifiers (forces, mesh modifiers), in application order.
    modifiers: Vec<SceneModifierInstanceDef>,
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
            material_maps: BTreeMap::new(),
            clocks: Vec::new(),
            physics: super::physics::State::new(doc),
            copy_bodies: Vec::new(),
            copy_parts: BTreeMap::new(),
            group_frames: BTreeMap::new(),
            modifiers: Vec::new(),
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
        let Some(source) = self.node_mut(from).map(|node| (node.node_id.clone(), node.params.clone())) else {
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

    pub(crate) fn id(&self, owner: &str, role: &'static str) -> Option<u32> {
        self.roles.get(&(owner.to_owned(), role)).copied()
    }

    pub(crate) fn scene(&self, role: &'static str) -> u32 {
        self.id(SCENE, role).expect("scene-level node")
    }

    pub(crate) fn wire(&mut self, from: u32, from_port: &str, to: u32, to_port: &str) {
        self.wires.push(wire(from, from_port, to, to_port));
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
        self.node_mut(id).map(|node| node.type_id.clone()).unwrap_or_default()
    }

    pub(crate) fn set(&mut self, id: u32, param: &str, value: SerializedParamValue) {
        if let Some(node) = self.node_mut(id) {
            node.params.insert(param.to_owned(), value);
        }
    }

    /// Set a node param from a number, typed as the param declares it.
    pub(crate) fn set_number(&mut self, id: u32, param: &str, value: f32) {
        let type_id = self.type_of(id);
        let like = self.param_meta(&type_id, param).map(|meta| meta.default_value);
        let typed = match like {
            Some(SerializedParamValue::Int { .. }) => SerializedParamValue::Int { value: value.round() as i32 },
            Some(SerializedParamValue::Bool { .. }) => SerializedParamValue::Bool { value: value > 0.5 },
            Some(SerializedParamValue::Enum { .. }) => SerializedParamValue::Enum { value: value.max(0.0).round() as u32 },
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
        self.apply_rows(item, scene_schema::applicable(&item.fields).filter(|row| row.reach != Reach::Clocks));
    }

    /// Apply the item's rows that reach the animation clocks below it, once
    /// its parts have compiled.
    pub(crate) fn clock_rows(&mut self, item: &Item) {
        self.apply_rows(item, scene_schema::applicable(&item.fields).filter(|row| row.reach == Reach::Clocks));
    }

    /// Apply `rows` of the item's table, as [`Self::rows`] does: for a node
    /// emitted after the item's own rows.
    pub(crate) fn apply_rows<'r>(&mut self, item: &Item, rows: impl IntoIterator<Item = &'r PropertyRow>) {
        for row in rows {
            let targets: Vec<u32> = match row.reach {
                Reach::Own(role) => self.id(item.id.as_str(), role).into_iter().collect(),
                Reach::Scene(role) => self.id(SCENE, role).into_iter().collect(),
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
        self.clocks.iter().filter(|(part, _)| inside.contains(&part)).map(|(_, clock)| *clock).collect()
    }

    /// The number a node param was set to, if it was.
    pub(crate) fn started(&mut self, id: u32, param: &str) -> Option<f32> {
        self.node_mut(id).and_then(|node| node.params.get(param).map(number))
    }

    fn card(&mut self, item: &Item, row: &PropertyRow, facts: &CardFacts, targets: &[u32]) {
        let type_id = self.type_of(targets[0]);
        let Some(meta) = self.param_meta(&type_id, row.param) else {
            return;
        };
        let id = param_id(&item.id, row.property);
        // The kind's default, else what the emitter started the node at,
        // else the node's own default.
        let started = self.node_mut(targets[0]).and_then(|node| node.params.get(row.param).map(number));
        let default = row.default.or(started).unwrap_or_else(|| number(&meta.default_value));
        let mut spec = card_spec(id, row.name.unwrap_or(&meta.label), &meta, default, row.section, facts);
        spec.material_role = meta.material_role;
        self.declare(spec, &meta, targets, row.param);
    }

    /// A card param `spec` bound to `param` on every target node, each target
    /// set to the spec's default.
    pub(crate) fn declare(&mut self, spec: ParamSpecDef, meta: &NodeParamMetadata, targets: &[u32], param: &str) {
        for &target in targets {
            self.set_number(target, param, spec.default_value);
            let node_id = self.node_mut(target).map(|node| node.node_id.clone()).unwrap_or_default();
            self.bindings.push(BindingDef {
                id: spec.id.clone(),
                label: spec.name.clone(),
                default_value: spec.default_value,
                target: BindingTarget::Node { node_id, param: param.to_owned() },
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
        self.set(render, "objects", SerializedParamValue::Int { value: objects.len().max(1) as i32 });
        self.set(render, "lights", SerializedParamValue::Int { value: lights.len() as i32 });
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
        let mut def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: Some("Scene".into()),
            description: None,
            preset_metadata: Some(scene_metadata(self.params, self.bindings)),
            scene_modifiers: Vec::new(),
            nodes: self.nodes,
            wires: self.wires,
        };
        if !self.modifiers.is_empty() {
            def.version = EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS;
            // A recipe that reads the source mesh's placement captures it
            // once, from the scene it is attached to.
            for modifier in &mut self.modifiers {
                modifier.mesh_frames = resolve_modifier_mesh_frames(&def, modifier)
                    .map_err(|error| format!("{}: {error}", modifier.id))?;
            }
            def.scene_modifiers = self.modifiers;
        }
        Ok(def)
    }

    /// How many scene modifiers are attached so far.
    pub(crate) fn modifier_count(&self) -> usize {
        self.modifiers.len()
    }

    /// Attach a scene modifier instance with the host params and bindings
    /// that drive its recipe's params.
    pub(crate) fn push_modifier(
        &mut self,
        instance: SceneModifierInstanceDef,
        params: Vec<ParamSpecDef>,
        bindings: Vec<BindingDef>,
    ) {
        self.modifiers.push(instance);
        self.params.extend(params);
        self.bindings.extend(bindings);
    }
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

