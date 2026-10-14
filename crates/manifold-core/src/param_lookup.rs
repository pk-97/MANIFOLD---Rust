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
                ..ParamChoices::default()
            };
            // The default is the instance's own stamp (a material's authored
            // factor, re-seeded from the node), not the template's, as on the
            // node path.
            let (min, max) = widen_to(&spec, saved.default_value);
            let spec = ParamSpecDef { default_value: saved.default_value, min, max, ..spec };
            return (spec, SpecSource::Lookup, choices);
        }
        // A fan-out (several bindings under one id) is described by its first
        // binding's target, in authored order (D22).
        let Some(binding) = single.or_else(|| self.bindings.iter().find(|b| b.id == saved.id)) else {
            return (saved.clone(), SpecSource::Authored, none);
        };
        if single.is_none() && !matches!(binding.target, BindingTarget::Node { .. }) {
            return (saved.clone(), SpecSource::Authored, none);
        }
        let fan_out = single.is_none();
        let unresolved = if fan_out { SpecSource::Authored } else { SpecSource::Unresolved };
        let Some(key) = key_for(self.preset, binding, &self.nodes, self.scene_radius) else {
            return match binding.target {
                BindingTarget::Composite { .. } => (saved.clone(), SpecSource::Authored, none),
                _ => (saved.clone(), unresolved, none),
            };
        };
        let Some(described) = self.resolver.describe(key) else {
            return (saved.clone(), unresolved, none);
        };
        (overlay(saved, described, binding.default_value), SpecSource::Lookup, none)
    }

    /// A user-exposed control keeps its saved spec. A saved label that
    /// differs from what the lookup calls its target becomes a rename
    /// choice, and a saved range that differs from the lookup's becomes a
    /// slider-range choice: the one place an old file holds them (D2 (one
    /// live lookup), old projects).
    pub(crate) fn user_choices(&mut self, saved: &ParamSpecDef) -> ParamChoices {
        let mut matching = self.bindings.iter().filter(|b| b.id == saved.id);
        let (Some(binding), None) = (matching.next(), matching.next()) else {
            return ParamChoices::default();
        };
        let described = key_for(self.preset, binding, &self.nodes, self.scene_radius)
            .and_then(|key| self.resolver.describe(key));
        ParamChoices {
            rename: described
                .as_ref()
                .filter(|d| !saved.name.is_empty() && saved.name != d.name)
                .map(|_| saved.name.clone()),
            slider: described
                .as_ref()
                .filter(|d| (saved.min, saved.max) != (d.min, d.max))
                .map(|_| (saved.min, saved.max)),
            ..ParamChoices::default()
        }
    }
}

/// What each control of one graph declares, before any user choice: its
/// `meta.params` entry joined with the lookup, as the manifest build joins
/// it. User-added and authored controls keep their saved spec. Readers with
/// no manifest at hand (a standalone runtime build, editor routings) read
/// this, so they agree with a build that has one.
pub struct Declared<'a> {
    join: Join<'a>,
    params: &'a [ParamSpecDef],
}

impl<'a> Declared<'a> {
    pub fn new(def: &'a EffectGraphDef) -> Option<Self> {
        let meta = def.preset_metadata.as_ref()?;
        Some(Self { join: Join::new(&meta.id, def), params: &meta.params })
    }

    pub fn spec(&mut self, id: &str) -> Option<ParamSpecDef> {
        let saved = self.params.iter().find(|p| p.id == id)?;
        if self.join.bindings.iter().any(|b| b.user_added && b.id == id) {
            return Some(saved.clone());
        }
        Some(self.join.join_control(saved).0)
    }

    /// The control's display name, or empty when the graph has no entry.
    pub fn name(&mut self, id: &str) -> String {
        self.spec(id).map(|s| s.name).unwrap_or_default()
    }
}

/// The declared slider range, widened so a seeded default outside the band
/// stays reachable, as the stamp does (a 300-unit camera distance).
fn widen_to(d: &ParamSpecDef, default: f32) -> (f32, f32) {
    if d.whole_numbers || d.is_toggle || d.is_trigger {
        return (d.min, d.max);
    }
    (d.min.min(default), d.max.max(default))
}

/// A node or modifier description laid over the saved control. The lookup
/// owns range and flags. Identity, default and the fields that become lookup
/// outputs or user choices in later phases (name, section, card visibility,
/// curve, invert, tooltip, periodicity) stay as saved for now. The name waits
/// for the label rules, so recipe and importer labels (a look's "Size") are
/// not renamed before then; an empty saved name takes the lookup's.
fn overlay(saved: &ParamSpecDef, d: ParamSpecDef, binding_default: f32) -> ParamSpecDef {
    let (min, max) = widen_to(&d, binding_default);
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
    fn fan_out_reads_its_first_target_and_composite_stays_authored() {
        let fan_out = graph(vec![
            binding("sun", lamp("intensity"), 1.0),
            binding("sun", lamp("intensity"), 1.0),
        ]);
        let preset = fan_out.preset_metadata.as_ref().unwrap().id.clone();
        let (spec, source, _) = Join::new(&preset, &fan_out).join_control(&saved("sun"));
        // D22: the first binding's target describes range and flags; the rest stays authored.
        assert_eq!((source, spec.name.as_str()), (SpecSource::Lookup, "Stale Copy"));
        assert_eq!((spec.min, spec.max), (0.0, 10.0));
        let ghost = graph(vec![
            binding("sun", BindingTarget::Node { node_id: NodeId::new("ghost"), param: "x".into() }, 1.0),
            binding("sun", lamp("intensity"), 1.0),
        ]);
        let (spec, source, _) = Join::new(&preset, &ghost).join_control(&saved("sun"));
        assert_eq!((source, spec), (SpecSource::Authored, saved("sun")));

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

    // ── Saved choices through a full generator load and save ──

    fn user_binding(id: &str) -> BindingDef {
        BindingDef { user_added: true, ..binding(id, lamp("intensity"), 1.0) }
    }

    fn scene_json(params: serde_json::Value, user_label: &str) -> String {
        let mut def = graph(vec![
            binding("1_intensity", lamp("intensity"), 1.0),
            binding("mix", BindingTarget::Composite { outer_name: "mix".into() }, 1.0),
            user_binding("user.lamp"),
        ]);
        let meta = def.preset_metadata.as_mut().unwrap();
        meta.params = vec![
            saved("1_intensity"),
            saved("mix"),
            ParamSpecDef { name: user_label.into(), ..saved("user.lamp") },
        ];
        serde_json::json!({
            "generatorType": "test.param_lookup",
            "params": params,
            "graph": def,
        })
        .to_string()
    }

    fn load(json: &str) -> crate::effects::PresetInstance {
        let mut de = serde_json::Deserializer::from_str(json);
        let mut fx = crate::effects::deserialize_generator_instance(&mut de).unwrap();
        fx.reconcile_manifest();
        fx
    }

    fn save(fx: &crate::effects::PresetInstance) -> String {
        serde_json::to_string(fx).unwrap()
    }

    fn state(fx: &crate::effects::PresetInstance) -> Vec<(ParamSpecDef, ParamChoices, SpecSource, f32)> {
        fx.params.iter().map(|p| (p.spec.clone(), p.choices.clone(), p.source, p.value)).collect()
    }

    fn chosen_params() -> serde_json::Value {
        serde_json::json!({
            "1_intensity": {
                "value": 3.0,
                "calibration": {"min": 2.0, "max": 4.0, "invert": true},
                "rename": "Key",
                "section": null,
                "pin": "shown",
            },
            "mix": {"value": 0.5},
            "user.lamp": {"value": 1.0},
        })
    }

    #[test]
    fn calibration_wire_round_trips_as_slider_choice() {
        let fx = load(&scene_json(chosen_params(), "Lamp Power"));
        let p = fx.params.get("1_intensity").unwrap();
        assert_eq!(p.choices.slider, Some((2.0, 4.0)));
        assert_eq!(p.choices.invert, Some(true));
        assert_eq!((p.spec.min, p.spec.max, p.spec.invert), (2.0, 4.0, true));
        let wire: serde_json::Value = serde_json::from_str(&save(&fx)).unwrap();
        assert_eq!(
            wire["params"]["1_intensity"]["calibration"],
            serde_json::json!({"min": 2.0, "max": 4.0, "invert": true})
        );
        assert_eq!(state(&load(&save(&fx))), state(&fx));
    }

    #[test]
    fn user_exposure_label_becomes_rename_once() {
        let fx = load(&scene_json(chosen_params(), "Lamp Power"));
        let p = fx.params.get("user.lamp").unwrap();
        assert_eq!(p.choices.rename.as_deref(), Some("Lamp Power"));
        assert_eq!(p.spec.name, "Lamp Power");
        let again = load(&save(&fx));
        assert_eq!(again.params.get("user.lamp").unwrap().choices, p.choices);
        // A label equal to what the lookup calls the target is no rename.
        let plain = load(&scene_json(chosen_params(), "Intensity"));
        assert_eq!(plain.params.get("user.lamp").unwrap().choices.rename, None);
    }

    #[test]
    fn user_exposure_range_becomes_slider_choice_once() {
        // The saved user control spans 0..1; the lookup says 0..10.
        let fx = load(&scene_json(chosen_params(), "Lamp Power"));
        let p = fx.params.get("user.lamp").unwrap();
        assert_eq!(p.choices.slider, Some((0.0, 1.0)));
        assert_eq!((p.spec.min, p.spec.max), (0.0, 1.0));
        let again = load(&save(&fx));
        assert_eq!(again.params.get("user.lamp").unwrap().choices, p.choices);
        assert_eq!(save(&again), save(&fx), "the conversion happens once");
        // A saved range equal to the lookup's is no slider choice.
        let mut wire: serde_json::Value =
            serde_json::from_str(&scene_json(chosen_params(), "Lamp Power")).unwrap();
        wire["graph"]["presetMetadata"]["params"][2]["max"] = serde_json::json!(10.0);
        let plain = load(&wire.to_string());
        assert_eq!(plain.params.get("user.lamp").unwrap().choices.slider, None);
    }

    #[test]
    fn join_preserves_choices_and_user_bindings() {
        let mut fx = load(&scene_json(chosen_params(), "Lamp Power"));
        let before = state(&fx);
        let bindings = fx.graph.as_ref().unwrap().preset_metadata.as_ref().unwrap().bindings.clone();
        fx.refresh_manifest_from_graph();
        assert_eq!(state(&fx), before);
        let p = fx.params.get("1_intensity").unwrap();
        assert_eq!(p.choices.rename.as_deref(), Some("Key"));
        assert_eq!(p.choices.section, Some(None));
        assert_eq!(p.choices.pin, Some(manifold_foundation::scene_surface::CardPin::Shown));
        let user = fx.params.get("user.lamp").unwrap();
        assert_eq!(user.origin, crate::params::ParamOrigin::UserAdded);
        assert_eq!(fx.graph.as_ref().unwrap().preset_metadata.as_ref().unwrap().bindings, bindings);
    }

    #[test]
    fn migrate_twice_is_noop() {
        let mut fx = load(&scene_json(chosen_params(), "Lamp Power"));
        fx.folded_sections = vec!["Key Light".into()];
        let first = save(&fx);
        let second = save(&load(&first));
        assert_eq!(first, second);
        let wire: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(wire["foldedSections"], serde_json::json!(["Key Light"]));
        // A described binding saves no label; composite and user ones keep it.
        let labels: Vec<_> = wire["graph"]["presetMetadata"]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| (b["id"].as_str().unwrap().to_string(), b.get("label").is_some()))
            .collect();
        assert_eq!(
            labels,
            [("1_intensity".into(), false), ("mix".into(), true), ("user.lamp".into(), true)]
        );
        let back = load(&first);
        let lamp = &back.graph.as_ref().unwrap().preset_metadata.as_ref().unwrap().bindings[0];
        assert_eq!(lamp.label, "Key", "a load names the binding from the join");
        fx.folded_sections.clear();
        assert!(!save(&fx).contains("foldedSections"));
    }

    /// INV-S16 (no reader goes around the join): the runtime, the editor
    /// routings and the instance fallbacks never read a saved binding label.
    /// Mirrors the design doc's gate, `binding.label` / `b.label`, outside
    /// `Composite` arms.
    #[test]
    fn no_reader_goes_around_the_join() {
        fn scan(path: &std::path::Path, hits: &mut Vec<String>) {
            if path.is_dir() {
                for entry in std::fs::read_dir(path).unwrap() {
                    scan(&entry.unwrap().path(), hits);
                }
                return;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                return;
            }
            let text = std::fs::read_to_string(path).unwrap();
            for (n, line) in text.lines().enumerate() {
                if line.trim_start().starts_with("//") || line.contains("Composite") {
                    continue;
                }
                let hit = ["binding.label", "b.label"].iter().any(|pat| {
                    line.match_indices(pat).any(|(i, m)| {
                        let next = line[i + m.len()..].chars().next();
                        !next.is_some_and(|c| c.is_alphanumeric() || c == '_')
                    })
                });
                if hit {
                    hits.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
        }
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut hits = Vec::new();
        for rel in [
            "manifold-node-engine/src",
            "manifold-app/src/content_thread.rs",
            "manifold-app/src/ui_snapshot",
            "manifold-core/src/effects/instance.rs",
        ] {
            scan(&crates.join(rel), &mut hits);
        }
        assert!(hits.is_empty(), "readers around the join:\n{}", hits.join("\n"));
    }
}
