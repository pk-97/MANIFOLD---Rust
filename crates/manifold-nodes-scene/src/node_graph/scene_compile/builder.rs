//! The compiler's working state: the nodes and wires it has emitted, which
//! node plays which role for which item, and the manifest the card rows make.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    ParamSpecDef, PresetMetadata, SerializedParamValue,
};
use manifold_core::scene_doc::{Item, SceneDoc, param_id};
use manifold_core::scene_exposure::NodeParamMetadata;
use manifold_core::scene_schema::{self, CardFacts, Exposure, PropertyRow, Reach};
use manifold_foundation::scene_surface::{ControlGroup, SurfaceRole};

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
    pub(crate) lights: Vec<u32>,
    /// Whether the camera emitter wired the render's output chain.
    pub(crate) output_wired: bool,
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
            lights: Vec::new(),
            output_wired: false,
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
        for row in scene_schema::applicable(&item.fields) {
            let targets: Vec<u32> = match row.reach {
                Reach::Own(role) => self.id(item.id.as_str(), role).into_iter().collect(),
                Reach::Scene(role) => self.id(SCENE, role).into_iter().collect(),
                Reach::EveryCameraLens => self.lenses.clone(),
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
        if !self.output_wired {
            let output = self.scene("output");
            self.wire(render, "color", output, "in");
        }
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION_WITH_METADATA,
            name: Some("Scene".into()),
            description: None,
            preset_metadata: Some(scene_metadata(self.params, self.bindings)),
            scene_modifiers: Vec::new(),
            nodes: self.nodes,
            wires: self.wires,
        };
        Ok(def)
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
    let widen = !meta.whole_numbers && !meta.is_toggle && !meta.is_trigger;
    let (min, max) = if widen { (meta.min.min(default), meta.max.max(default)) } else { (meta.min, meta.max) };
    ParamSpecDef {
        id,
        name: name.to_owned(),
        min,
        max,
        default_value: default,
        whole_numbers: meta.whole_numbers,
        is_toggle: meta.is_toggle,
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

