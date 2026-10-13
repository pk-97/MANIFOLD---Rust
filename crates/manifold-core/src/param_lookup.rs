//! The one live param lookup (SCENE_SURFACES_DESIGN.md D2 (one live lookup)).
//!
//! What the code declares about a control — name, slider range, hard limit,
//! unit, scene placement, value labels, angle/toggle/trigger flags — comes from
//! here, never from a copy saved in the project. The lookup is read in one
//! place: the manifest join in `effects::instance_serde` ([`join_control`]).
//! Every other reader reads the joined `Param.spec`.
//!
//! Three keys, each reading a source that exists already:
//! - `Template`: `PRESET_DEFINITIONS` (bundled presets, recipes, registered
//!   user presets).
//! - `Node`: the node param source registered by the scene crate from
//!   `PrimitiveRegistry` ([`NodeParamSource`]), with scene-scaled ranges from
//!   the saved `meta.scene_bounds`.
//! - `Modifier`: a scene-modifier recipe's template.

use ahash::AHashMap;

use crate::PresetTypeId;
use crate::effect_graph_def::{BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, ParamSpecDef};
use crate::params::{ParamChoices, SpecSource};
use crate::scene_exposure::{
    SceneParamMetadata, scene_radius_from_bounds, scene_scaled_range, serialized_default_as_f32,
};

/// One control's lookup key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamKey<'a> {
    Template { preset: &'a PresetTypeId, id: &'a str },
    Node { type_id: &'a str, param: &'a str, scene_radius: Option<f32> },
    Modifier { recipe: &'a PresetTypeId, param: &'a str },
}

/// The node half of the lookup: every param a node type declares, unfiltered.
/// Registered once at link time by the crate that owns `PrimitiveRegistry`
/// (`manifold-nodes-scene`); read-only, no lock.
pub struct NodeParamSource {
    pub params_for_type: fn(&str) -> Vec<SceneParamMetadata>,
}
inventory::collect!(NodeParamSource);

/// The template half's graph, so a template answers only the control it
/// was written for (a node id reused after a delete points elsewhere).
/// Registered by the crate that owns the bundled preset graphs.
pub struct TemplateGraphSource {
    pub graph_for: fn(&PresetTypeId) -> Option<std::sync::Arc<EffectGraphDef>>,
}
inventory::collect!(TemplateGraphSource);

fn node_source() -> Option<&'static NodeParamSource> {
    let mut sources = inventory::iter::<NodeParamSource>.into_iter();
    let source = sources.next()?;
    debug_assert!(sources.next().is_none(), "duplicate node param source");
    Some(source)
}

/// What the code declares about one control. Never a user choice.
pub fn describe(key: ParamKey<'_>) -> Option<ParamSpecDef> {
    Resolver::default().describe(key)
}

/// Per-join cache: a node type's params are built once per manifest build,
/// not once per control.
#[derive(Default)]
struct Resolver {
    node_params: AHashMap<String, Vec<SceneParamMetadata>>,
}

impl Resolver {
    fn describe(&mut self, key: ParamKey<'_>) -> Option<ParamSpecDef> {
        match key {
            ParamKey::Template { preset, id } => template_spec(preset, id),
            ParamKey::Modifier { recipe, param } => template_spec(recipe, param),
            ParamKey::Node { type_id, param, scene_radius } => {
                let source = node_source()?;
                let params = self
                    .node_params
                    .entry(type_id.to_string())
                    .or_insert_with(|| (source.params_for_type)(type_id));
                let meta = params.iter().find(|m| m.name == param)?;
                let (min, max) = scene_radius
                    .and_then(|r| scene_scaled_range(type_id, param, r))
                    .unwrap_or((meta.min, meta.max));
                Some(ParamSpecDef {
                    id: meta.name.clone(),
                    name: meta.label.clone(),
                    min,
                    max,
                    default_value: serialized_default_as_f32(&meta.default_value),
                    whole_numbers: meta.whole_numbers,
                    is_toggle: meta.is_toggle,
                    is_trigger: meta.is_trigger,
                    value_labels: meta.value_labels.clone(),
                    is_angle: meta.is_angle,
                    wraps: meta.wraps,
                    material_role: meta.material_role,
                    ..ParamSpecDef::default()
                })
            }
        }
    }
}

fn template_spec(preset: &PresetTypeId, id: &str) -> Option<ParamSpecDef> {
    let def = crate::preset_definition_registry::try_get(preset)?;
    def.param_defs.iter().find(|pd| pd.spec.id == id).map(|pd| pd.spec.clone())
}

/// Node and scene-modifier types in one graph, keyed by stable id, group
/// bodies included.
#[derive(Debug, Default)]
pub struct NodeTypes {
    nodes: AHashMap<String, String>,
    modifiers: AHashMap<String, PresetTypeId>,
}

impl NodeTypes {
    pub fn from_graph(def: &EffectGraphDef) -> Self {
        fn walk(nodes: &[EffectGraphNode], out: &mut AHashMap<String, String>) {
            for node in nodes {
                out.insert(node.node_id.as_str().to_string(), node.type_id.clone());
                if let Some(body) = node.group.as_deref() {
                    walk(&body.nodes, out);
                }
            }
        }
        let mut nodes = AHashMap::new();
        walk(&def.nodes, &mut nodes);
        let modifiers = def
            .scene_modifiers
            .iter()
            .filter_map(|m| {
                let recipe = m.graph.preset_metadata.as_ref()?.id.clone();
                Some((m.id.as_str().to_string(), recipe))
            })
            .collect();
        Self { nodes, modifiers }
    }
}

/// Template id first, then the binding's primary target. `Composite` → `None`
/// (authored).
pub fn key_for<'a>(
    preset: &'a PresetTypeId,
    binding: &'a BindingDef,
    nodes: &'a NodeTypes,
    scene_radius: Option<f32>,
) -> Option<ParamKey<'a>> {
    if template_answers(preset, binding, nodes) {
        return Some(ParamKey::Template { preset, id: &binding.id });
    }
    match &binding.target {
        BindingTarget::Node { node_id, param } => {
            let type_id = nodes.nodes.get(node_id.as_str())?;
            Some(ParamKey::Node { type_id, param, scene_radius })
        }
        BindingTarget::SceneModifier { modifier_id, param_id } => {
            let recipe = nodes.modifiers.get(modifier_id.as_str())?;
            Some(ParamKey::Modifier { recipe, param: param_id })
        }
        BindingTarget::Composite { .. } => None,
    }
}

/// The template answers a control only when it declares the id and, if its
/// graph is known, its binding for that id targets a node of the same type
/// as this graph's binding does.
fn template_answers(preset: &PresetTypeId, binding: &BindingDef, nodes: &NodeTypes) -> bool {
    if template_spec(preset, &binding.id).is_none() {
        return false;
    }
    let BindingTarget::Node { node_id, .. } = &binding.target else {
        return true;
    };
    let Some(template) = inventory::iter::<TemplateGraphSource>
        .into_iter()
        .find_map(|s| (s.graph_for)(preset))
    else {
        return true;
    };
    let template_type = template
        .preset_metadata
        .as_ref()
        .and_then(|m| m.bindings.iter().find(|b| b.id == binding.id))
        .and_then(|b| match &b.target {
            BindingTarget::Node { node_id, .. } => Some(node_id.as_str().to_string()),
            _ => None,
        })
        .and_then(|id| NodeTypes::from_graph(&template).nodes.get(&id).cloned());
    match template_type {
        Some(t) => nodes.nodes.get(node_id.as_str()) == Some(&t),
        None => true,
    }
}

/// The join for one graph: each saved bundled control of a per-instance graph
/// is joined with its live description.
pub(crate) struct Join<'a> {
    preset: &'a PresetTypeId,
    bindings: &'a [BindingDef],
    nodes: NodeTypes,
    scene_radius: Option<f32>,
    resolver: Resolver,
}

impl<'a> Join<'a> {
    pub(crate) fn new(preset: &'a PresetTypeId, def: &'a EffectGraphDef) -> Self {
        let meta = def.preset_metadata.as_ref();
        Self {
            preset,
            bindings: meta.map(|m| m.bindings.as_slice()).unwrap_or(&[]),
            nodes: NodeTypes::from_graph(def),
            scene_radius: meta.and_then(|m| m.scene_bounds).map(scene_radius_from_bounds),
            resolver: Resolver::default(),
        }
    }

    /// The joined spec and where it came from. `saved` is the graph's
    /// `meta.params` entry.
    /// The choices are the user's old in-copy edits on a template control,
    /// converted once (SCENE_SURFACES_DESIGN.md D7 (one saved choice set)).
    pub(crate) fn join_control(
        &mut self,
        saved: &ParamSpecDef,
    ) -> (ParamSpecDef, SpecSource, ParamChoices) {
        let none = ParamChoices::default();
        let mut matching = self.bindings.iter().filter(|b| b.id == saved.id);
        let single = match (matching.next(), matching.next()) {
            (Some(b), None) => Some(b),
            _ => None,
        };
        let template_ok = single.is_none_or(|b| template_answers(self.preset, b, &self.nodes));
        if template_ok && let Some(spec) = template_spec(self.preset, &saved.id) {
            let choices = ParamChoices {
                rename: (!saved.name.is_empty() && saved.name != spec.name)
                    .then(|| saved.name.clone()),
                section: (saved.section != spec.section).then(|| saved.section.clone()),
                pin: None,
            };
            return (spec, SpecSource::Lookup, choices);
        }
        // A fan-out (several bindings under one id) has no single primary
        // target, so its spec is authored.
        let Some(binding) = single else {
            return (saved.clone(), SpecSource::Authored, none);
        };
        let Some(key) = key_for(self.preset, binding, &self.nodes, self.scene_radius) else {
            return match binding.target {
                BindingTarget::Composite { .. } => (saved.clone(), SpecSource::Authored, none),
                _ => (saved.clone(), SpecSource::Unresolved, none),
            };
        };
        let Some(described) = self.resolver.describe(key) else {
            return (saved.clone(), SpecSource::Unresolved, none);
        };
        (overlay(saved, described, binding.default_value), SpecSource::Lookup, none)
    }
}

/// A node or modifier description laid over the saved control. The lookup
/// owns range and flags. Identity, default and the fields that become lookup
/// outputs or user choices in later phases (name, section, card visibility,
/// curve, invert, tooltip, periodicity) stay as saved for now. The name waits
/// for the label rules, so recipe and importer labels (a look's "Size") are
/// not renamed before then; an empty saved name takes the lookup's.
fn overlay(saved: &ParamSpecDef, d: ParamSpecDef, binding_default: f32) -> ParamSpecDef {
    // Keep a seeded value outside the declared band reachable, as the stamp
    // does (a 300-unit camera distance).
    let widen = !d.whole_numbers && !d.is_toggle && !d.is_trigger;
    let (min, max) = if widen {
        (d.min.min(binding_default), d.max.max(binding_default))
    } else {
        (d.min, d.max)
    };
    ParamSpecDef {
        name: if saved.name.is_empty() { d.name } else { saved.name.clone() },
        min,
        max,
        whole_numbers: d.whole_numbers,
        is_toggle: d.is_toggle,
        is_trigger: d.is_trigger,
        value_labels: d.value_labels,
        is_angle: d.is_angle,
        material_role: d.material_role,
        limit: d.limit,
        unit: d.unit,
        scene: d.scene,
        ..saved.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeId;
    use crate::effect_graph_def::{PresetMetadata, SerializedParamValue};
    use crate::effects::ParamConvert;

    fn test_params(type_id: &str) -> Vec<SceneParamMetadata> {
        if type_id != "node.test_lamp" {
            return Vec::new();
        }
        vec![SceneParamMetadata {
            name: "intensity".into(),
            label: "Intensity".into(),
            min: 0.0,
            max: 10.0,
            default_value: SerializedParamValue::Float { value: 1.0 },
            is_angle: false,
            wraps: false,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            convert: ParamConvert::Float,
            material_role: None,
        }]
    }

    inventory::submit! { NodeParamSource { params_for_type: test_params } }

    fn node(id: &str, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id: 1,
            node_id: NodeId::new(id),
            type_id: type_id.to_string(),
            handle: None,
            params: Default::default(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        }
    }

    fn binding(id: &str, target: BindingTarget, default_value: f32) -> BindingDef {
        BindingDef {
            id: id.into(),
            label: "Saved Label".into(),
            default_value,
            target,
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: true,
        }
    }

    fn saved(id: &str) -> ParamSpecDef {
        ParamSpecDef {
            id: id.into(),
            name: "Stale Copy".into(),
            min: 0.0,
            max: 1.0,
            default_value: 12.0,
            section: Some("Key Light".into()),
            card_visible: false,
            ..ParamSpecDef::default()
        }
    }

    fn graph(bindings: Vec<BindingDef>) -> EffectGraphDef {
        let preset = PresetTypeId::from_string("test.param_lookup".into());
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: preset,
                display_name: String::new(),
                category: String::new(),
                osc_prefix: String::new(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                layer_types: None,
                params: Vec::new(),
                bindings,
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
                scene_modifier: None,
                scene_bounds: None,
            }),
            scene_modifiers: Vec::new(),
            nodes: vec![node("lamp", "node.test_lamp"), node("ghost", "node.not_registered")],
            wires: Vec::new(),
        }
    }

    fn lamp(param: &str) -> BindingTarget {
        BindingTarget::Node { node_id: NodeId::new("lamp"), param: param.into() }
    }

    #[test]
    fn node_control_takes_range_from_the_lookup() {
        let def = graph(vec![binding("1_intensity", lamp("intensity"), 12.0)]);
        let preset = def.preset_metadata.as_ref().unwrap().id.clone();
        let mut join = Join::new(&preset, &def);
        let (spec, source, _) = join.join_control(&saved("1_intensity"));
        assert_eq!(source, SpecSource::Lookup);
        // The name stays as saved until the label rules land; an empty one
        // takes the lookup's.
        assert_eq!(spec.name, "Stale Copy");
        let unnamed = ParamSpecDef { name: String::new(), ..saved("1_intensity") };
        assert_eq!(join.join_control(&unnamed).0.name, "Intensity");
        // The declared band widens to keep the binding's seeded value on it.
        assert_eq!((spec.min, spec.max), (0.0, 12.0));
        // Identity, default and the fields later phases move stay as saved.
        assert_eq!(spec.id, "1_intensity");
        assert_eq!(spec.default_value, 12.0);
        assert_eq!(spec.section.as_deref(), Some("Key Light"));
        assert!(!spec.card_visible);
    }

    #[test]
    fn fan_out_and_composite_stay_authored() {
        let fan_out = graph(vec![
            binding("sun", lamp("intensity"), 1.0),
            binding("sun", lamp("intensity"), 1.0),
        ]);
        let preset = fan_out.preset_metadata.as_ref().unwrap().id.clone();
        let (spec, source, _) = Join::new(&preset, &fan_out).join_control(&saved("sun"));
        assert_eq!((source, spec.name.as_str()), (SpecSource::Authored, "Stale Copy"));

        let composite = graph(vec![binding(
            "mix",
            BindingTarget::Composite { outer_name: "mix".into() },
            1.0,
        )]);
        let (spec, source, _) = Join::new(&preset, &composite).join_control(&saved("mix"));
        assert_eq!((source, spec.name.as_str()), (SpecSource::Authored, "Stale Copy"));
    }

    #[test]
    fn unknown_type_or_param_keeps_the_saved_spec_unresolved() {
        let def = graph(vec![
            binding("1_x", BindingTarget::Node { node_id: NodeId::new("ghost"), param: "x".into() }, 1.0),
            binding("1_gone", lamp("gone"), 1.0),
            binding("1_dangling", BindingTarget::Node { node_id: NodeId::new("nowhere"), param: "x".into() }, 1.0),
        ]);
        let preset = def.preset_metadata.as_ref().unwrap().id.clone();
        let mut join = Join::new(&preset, &def);
        for id in ["1_x", "1_gone", "1_dangling"] {
            let (spec, source, _) = join.join_control(&saved(id));
            assert_eq!(source, SpecSource::Unresolved, "{id}");
            assert_eq!(spec, saved(id), "{id}");
        }
    }

    #[test]
    fn node_describe_reads_the_registered_source() {
        let key = ParamKey::Node { type_id: "node.test_lamp", param: "intensity", scene_radius: Some(5.0) };
        // node.test_lamp has no scene-scaled band, so the declared range holds.
        assert_eq!(describe(key).map(|s| (s.min, s.max)), Some((0.0, 10.0)));
        assert!(describe(ParamKey::Node { type_id: "node.test_lamp", param: "nope", scene_radius: None }).is_none());
    }

    #[test]
    fn old_scene_copies_are_ignored_then_dropped() {
        let def = graph(vec![binding("1_intensity", lamp("intensity"), 1.0)]);
        let preset = def.preset_metadata.as_ref().unwrap().id.clone();
        let mut join = Join::new(&preset, &def);
        let (joined, source, _) = join.join_control(&saved("1_intensity"));
        assert_eq!(source, SpecSource::Lookup);
        assert_eq!((joined.min, joined.max), (0.0, 10.0), "the stale saved range is ignored");
        // Saved as a stub: no range, no flags.
        let mut meta = def.preset_metadata.clone().unwrap();
        meta.params = vec![joined.lookup_stub()];
        let json = serde_json::to_value(&meta).unwrap();
        let entry = &json["params"][0];
        assert!(entry.get("min").is_none() && entry.get("max").is_none(), "{entry}");
        assert_eq!(entry["id"], "1_intensity");
        // The stub parses and joins back to the same spec.
        let parsed: PresetMetadata = serde_json::from_value(json).unwrap();
        assert_eq!(join.join_control(&parsed.params[0]).0, joined);
    }

    #[test]
    fn unresolved_control_round_trips_verbatim() {
        let mut meta = graph(Vec::new()).preset_metadata.unwrap();
        meta.params = vec![saved("1_x")];
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: PresetMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.params[0], saved("1_x"));
    }
}
