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
use crate::effect_graph_def::SerializedParamValue;
use crate::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, ParamSpecDef,
};
use crate::effects::ParamConvert;
use crate::params::{ParamChoices, SpecSource};

/// Metadata for one inner-node parameter, produced from the primitive's own
/// `ParamDef` by the registered [`NodeParamSource`].
#[derive(Debug, Clone, PartialEq)]
pub struct NodeParamMetadata {
    pub name: String,
    pub label: String,
    pub min: f32,
    pub max: f32,
    pub default_value: SerializedParamValue,
    pub is_angle: bool,
    /// Whether values outside the declared range wrap back into it.
    pub wraps: bool,
    pub whole_numbers: bool,
    pub is_toggle: bool,
    pub is_trigger: bool,
    pub value_labels: Vec<String>,
    pub convert: ParamConvert,
    /// Optional material-inspector semantic role supplied by the renderer's
    /// classifier and copied into the persisted manifest descriptor.
    pub material_role: Option<crate::material_inspector::MaterialParamRole>,
    /// Where the param lives on the scene surfaces. `None` outside the scene
    /// vocabulary.
    pub scene: Option<manifold_foundation::scene_surface::SceneParamDesc>,
    pub unit: Option<manifold_foundation::scene_surface::Unit>,
    pub limit: manifold_foundation::param_range::HardLimit,
}

pub(crate) fn serialized_default_as_f32(value: &SerializedParamValue) -> f32 {
    match value {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Int { value } => *value as f32,
        SerializedParamValue::Bool { value } => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
        SerializedParamValue::Enum { value } => *value as f32,
        _ => 0.0,
    }
}

/// The bounding-sphere radius of a `(min, max)` bbox: half the diagonal,
/// floored at 0.01 so a degenerate bbox still yields nonzero slider widths.
pub fn scene_radius_from_bounds(bounds: ([f32; 3], [f32; 3])) -> f32 {
    let (min, max) = bounds;
    let dims = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    ((dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5).max(0.01)
}

/// The single scene-scaled slider range table. Radius in world units; every
/// band is a multiple of it so a scene twice as big gets sliders twice as wide.
/// Returns `None` for params not in the table (the generic refresh path).
///
/// Why each multiplier:
/// - Position (`pos_*`, `aim_*`): ±2·radius — one radius covers the model,
///   two gives the model plus neighbourhood, enough to dock at the edge of
///   the shot without squeezing the useful band into the first few percent.
/// - Light range: 0.01..4·radius — the soft falloff half-distance fills the
///   whole set while keeping fine control in the near half.
/// - Focus distance: 0..4·radius — 0 is the hyperfocal pin (focus off),
///   far end reaches two scene-widths out (import camera sits at ~2.2·radius).
/// - F-stop: 0.5..max(32, 64·radius) — 0.5 is the photographic floor; 64·R
///   keeps migrated f_stop×R values on-slider; 32 keeps unit-scale default.
/// - Orbit distance: 0.01..6·radius — ~3× headroom past the 2.2·radius
///   framing distance so the full orbit range is on-slider.
/// - Orbit near: 0.001..2·radius — import stamps near at ~0.6·radius, so
///   2·radius contains it with room.
/// - Orbit far: 1.0..min(20·radius, 10_000) — 20× covers the import-stamped
///   far with headroom; 10_000 is the primitive's declared range max.
/// - Fog density: 0.0..2/radius — fog is per-world-unit (1 - exp(-d·dist)),
///   so the useful slider range scales inversely with scene size.
pub fn scene_scaled_range(type_id: &str, param: &str, radius: f32) -> Option<(f32, f32)> {
    match type_id {
        "node.transform_3d" if matches!(param, "pos_x" | "pos_y" | "pos_z") => {
            Some((-2.0 * radius, 2.0 * radius))
        }
        "node.light"
            if matches!(
                param,
                "pos_x" | "pos_y" | "pos_z" | "aim_x" | "aim_y" | "aim_z"
            ) =>
        {
            Some((-2.0 * radius, 2.0 * radius))
        }
        "node.light" if param == "range" => Some((0.01, 4.0 * radius)),
        "node.camera_lens" if param == "focus_distance" => Some((0.0, 4.0 * radius)),
        "node.camera_lens" if param == "f_stop" => Some((0.5, (32.0_f32).max(64.0 * radius))),
        "node.orbit_camera" if param == "distance" => Some((0.01, 6.0 * radius)),
        "node.orbit_camera" if param == "near" => Some((0.001, 2.0 * radius)),
        "node.orbit_camera" if param == "far" => Some((1.0, (20.0 * radius).min(10_000.0))),
        "node.atmosphere" if param == "fog_density" => Some((0.0, 2.0 / radius)),
        _ => None,
    }
}

/// One control's lookup key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamKey<'a> {
    Template {
        preset: &'a PresetTypeId,
        id: &'a str,
    },
    Node {
        type_id: &'a str,
        param: &'a str,
        scene_radius: Option<f32>,
    },
    Modifier {
        recipe: &'a PresetTypeId,
        param: &'a str,
    },
}

/// The node half of the lookup: every param a node type declares, unfiltered,
/// and the Physics World controls it takes. Registered once at link time by
/// the crate that owns `PrimitiveRegistry` (`manifold-nodes-scene`);
/// read-only, no lock.
pub struct NodeParamSource {
    pub params_for_type: fn(&str) -> Vec<NodeParamMetadata>,
    pub world_inputs_for_type:
        fn(&str) -> &'static [crate::scene_world_controls::WorldControlInput],
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

/// Every param `type_id` declares, in declaration order, Hidden ones
/// included. Empty when the type is unknown or no source is linked.
pub fn node_params(type_id: &str) -> Vec<NodeParamMetadata> {
    node_source()
        .map(|source| (source.params_for_type)(type_id))
        .unwrap_or_default()
}

/// The Physics World controls `type_id` declares it takes, each with its own
/// input. Empty when the type takes none or no source is linked.
pub fn world_control_inputs(
    type_id: &str,
) -> &'static [crate::scene_world_controls::WorldControlInput] {
    node_source().map_or(&[], |source| (source.world_inputs_for_type)(type_id))
}

/// The params a scene node stamps as exposures (creation sites and the
/// load-time migration), in card order: every param [`node_params`] returns
/// except those [`hidden_from_stamp`] drops, and the box spawner, which only
/// a copies body stamps ([`stamped_params_for`]).
pub fn stamped_node_params(type_id: &str) -> Vec<NodeParamMetadata> {
    stamped_params_for(type_id, false)
}

/// [`stamped_node_params`] for a node in its role: a rigid body wired into
/// a Physics World's `copies` input also stamps the box spawner
/// (`crate::copies_body`).
pub fn stamped_params_for(type_id: &str, copies_body: bool) -> Vec<NodeParamMetadata> {
    let mut params = node_params(type_id);
    params.retain(|meta| {
        !hidden_from_stamp(meta)
            && (copies_body || !crate::copies_body::is_copies_body_param(type_id, &meta.name))
    });
    params
}

/// The one Hidden filter: the scene table marks the param Hidden. A type
/// outside the scene vocabulary has no scene facts and keeps every param.
fn hidden_from_stamp(meta: &NodeParamMetadata) -> bool {
    meta.scene
        .is_some_and(|s| s.role == manifold_foundation::scene_surface::SurfaceRole::Hidden)
}

/// What the code declares about one control. Never a user choice.
pub fn describe(key: ParamKey<'_>) -> Option<ParamSpecDef> {
    Resolver::default().describe(key)
}

/// Per-join cache: a node type's params are built once per manifest build,
/// not once per control.
#[derive(Default)]
struct Resolver {
    node_params: AHashMap<String, Vec<NodeParamMetadata>>,
}

impl Resolver {
    fn describe(&mut self, key: ParamKey<'_>) -> Option<ParamSpecDef> {
        match key {
            ParamKey::Template { preset, id } => template_spec(preset, id),
            ParamKey::Modifier { recipe, param } => template_spec(recipe, param),
            ParamKey::Node {
                type_id,
                param,
                scene_radius,
            } => {
                let params = self
                    .node_params
                    .entry(type_id.to_string())
                    .or_insert_with(|| node_params(type_id));
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
                    limit: meta.limit,
                    unit: meta.unit,
                    scene: meta.scene,
                    ..ParamSpecDef::default()
                })
            }
        }
    }

    /// An exact auto-stamp (`{doc_id}_{param}`, not user-added, on that
    /// node's param) is a copy of its node's declaration, so the node owns
    /// its name, range, value labels and angle flag, the range widened to the
    /// default. Anything else comes back unchanged.
    fn stamp_overlay(
        &mut self,
        mut spec: ParamSpecDef,
        bindings: &[BindingDef],
        nodes: &NodeTypes,
        scene_radius: Option<f32>,
    ) -> ParamSpecDef {
        let Some((type_id, param)) = exact_stamp(&spec.id, bindings, nodes) else {
            return spec;
        };
        let Some(d) = self.describe(ParamKey::Node {
            type_id,
            param,
            scene_radius,
        }) else {
            return spec;
        };
        let (min, max) = widen_to(
            &ParamSpecDef {
                min: d.min,
                max: d.max,
                ..spec.clone()
            },
            spec.default_value,
        );
        spec.name = d.name;
        spec.is_angle = d.is_angle;
        spec.value_labels = d.value_labels;
        spec.min = min;
        spec.max = max;
        spec.scene = d.scene;
        spec.unit = d.unit;
        spec.limit = d.limit;
        spec
    }

    /// D23 (scene facts follow the bound node): a template control bound to
    /// exactly one node target takes that target's scene placement, unit and
    /// limit wherever the template declares none.
    fn scene_overlay(
        &mut self,
        mut spec: ParamSpecDef,
        bindings: &[BindingDef],
        nodes: &NodeTypes,
        scene_radius: Option<f32>,
    ) -> ParamSpecDef {
        let mut bound = bindings.iter().filter(|b| b.id == spec.id);
        let (Some(binding), None) = (bound.next(), bound.next()) else {
            return spec;
        };
        if binding.user_added {
            return spec;
        }
        let BindingTarget::Node { node_id, param } = &binding.target else {
            return spec;
        };
        let Some(type_id) = nodes.nodes.get(node_id.as_str()) else {
            return spec;
        };
        let Some(d) = self.describe(ParamKey::Node {
            type_id,
            param,
            scene_radius,
        }) else {
            return spec;
        };
        spec.scene = spec.scene.or(d.scene);
        spec.unit = spec.unit.or(d.unit);
        if spec.limit.is_slider() {
            spec.limit = d.limit;
        }
        spec
    }

    /// A template control as the lookup declares it: its node's declaration
    /// for an exact auto-stamp, then the bound node's scene facts.
    fn template_overlay(
        &mut self,
        spec: ParamSpecDef,
        bindings: &[BindingDef],
        nodes: &NodeTypes,
        scene_radius: Option<f32>,
    ) -> ParamSpecDef {
        let spec = self.stamp_overlay(spec, bindings, nodes, scene_radius);
        self.scene_overlay(spec, bindings, nodes, scene_radius)
    }
}

/// The node type and param of an exact auto-stamp: id `{doc_id}_{param}`,
/// not user-added, targeting that node's param.
fn exact_stamp<'n>(
    id: &str,
    bindings: &'n [BindingDef],
    nodes: &'n NodeTypes,
) -> Option<(&'n str, &'n str)> {
    bindings.iter().find_map(|b| match &b.target {
        BindingTarget::Node { node_id, param }
            if !b.user_added
                && b.id == id
                && nodes
                    .doc_ids
                    .get(node_id.as_str())
                    .is_some_and(|doc| b.id == format!("{doc}_{param}")) =>
        {
            Some((nodes.nodes.get(node_id.as_str())?.as_str(), param.as_str()))
        }
        _ => None,
    })
}

/// A preset's controls as the lookup declares them, for readers of the
/// template itself (the node catalog): each exact auto-stamp takes its node's
/// declaration and each node-bound control its scene facts, as the join does.
pub fn template_params(def: &EffectGraphDef) -> Vec<ParamSpecDef> {
    let Some(meta) = def.preset_metadata.as_ref() else {
        return Vec::new();
    };
    let nodes = NodeTypes::from_graph(def);
    let radius = meta.scene_bounds.map(scene_radius_from_bounds);
    let mut resolver = Resolver::default();
    meta.params
        .iter()
        .map(|p| resolver.template_overlay(p.clone(), &meta.bindings, &nodes, radius))
        .collect()
}

/// A graph-less instance reads its template's specs straight from the
/// registry; each takes its bound node's scene facts here (D23 (scene facts
/// follow the bound node)), as a per-instance graph's join gives them.
pub(crate) fn fill_template_scene_facts(preset: &PresetTypeId, specs: &mut [ParamSpecDef]) {
    let Some(template) = inventory::iter::<TemplateGraphSource>
        .into_iter()
        .find_map(|s| (s.graph_for)(preset))
    else {
        return;
    };
    let Some(meta) = template.preset_metadata.as_ref() else {
        return;
    };
    let nodes = NodeTypes::from_graph(&template);
    let radius = meta.scene_bounds.map(scene_radius_from_bounds);
    let mut resolver = Resolver::default();
    for spec in specs {
        *spec = resolver.scene_overlay(std::mem::take(spec), &meta.bindings, &nodes, radius);
    }
}

fn template_spec(preset: &PresetTypeId, id: &str) -> Option<ParamSpecDef> {
    let def = crate::preset_definition_registry::try_get(preset)?;
    def.param_defs
        .iter()
        .find(|pd| pd.spec.id == id)
        .map(|pd| pd.spec.clone())
}

/// Node and scene-modifier types in one graph, keyed by stable id, group
/// bodies included.
#[derive(Debug, Default)]
pub struct NodeTypes {
    nodes: AHashMap<String, String>,
    /// Document ids, which name auto-stamps (`{doc_id}_{param}`).
    doc_ids: AHashMap<String, u32>,
    modifiers: AHashMap<String, PresetTypeId>,
}

impl NodeTypes {
    pub fn from_graph(def: &EffectGraphDef) -> Self {
        fn walk(
            nodes: &[EffectGraphNode],
            out: &mut AHashMap<String, String>,
            doc_ids: &mut AHashMap<String, u32>,
        ) {
            for node in nodes {
                out.insert(node.node_id.as_str().to_string(), node.type_id.clone());
                doc_ids.insert(node.node_id.as_str().to_string(), node.id);
                if let Some(body) = node.group.as_deref() {
                    walk(&body.nodes, out, doc_ids);
                }
            }
        }
        let mut nodes = AHashMap::new();
        let mut doc_ids = AHashMap::new();
        walk(&def.nodes, &mut nodes, &mut doc_ids);
        let modifiers = def
            .scene_modifiers
            .iter()
            .filter_map(|m| {
                let recipe = m.graph.preset_metadata.as_ref()?.id.clone();
                Some((m.id.as_str().to_string(), recipe))
            })
            .collect();
        Self {
            nodes,
            doc_ids,
            modifiers,
        }
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
        return Some(ParamKey::Template {
            preset,
            id: &binding.id,
        });
    }
    match &binding.target {
        BindingTarget::Node { node_id, param } => {
            let type_id = nodes.nodes.get(node_id.as_str())?;
            Some(ParamKey::Node {
                type_id,
                param,
                scene_radius,
            })
        }
        BindingTarget::SceneModifier {
            modifier_id,
            param_id,
        } => {
            let recipe = nodes.modifiers.get(modifier_id.as_str())?;
            Some(ParamKey::Modifier {
                recipe,
                param: param_id,
            })
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
    /// The join reads a saved file, so a scene-modifier rename held only in
    /// the saved name converts ([`saved_modifier_rename`]).
    from_file: bool,
}

impl<'a> Join<'a> {
    pub(crate) fn new(preset: &'a PresetTypeId, def: &'a EffectGraphDef) -> Self {
        let meta = def.preset_metadata.as_ref();
        Self {
            preset,
            bindings: meta.map(|m| m.bindings.as_slice()).unwrap_or(&[]),
            nodes: NodeTypes::from_graph(def),
            scene_radius: meta
                .and_then(|m| m.scene_bounds)
                .map(scene_radius_from_bounds),
            resolver: Resolver::default(),
            from_file: false,
        }
    }

    /// A join over a graph read from a saved file. Only that read converts
    /// old saved renames; an in-memory rebuild carries them as choices
    /// already, and its saved names may be stale display copies.
    pub(crate) fn reading_file(mut self) -> Self {
        self.from_file = true;
        self
    }

    /// The joined spec and where it came from. `saved` is the graph's
    /// `meta.params` entry.
    /// A saved copy that differs from the lookup is ignored, never converted
    /// (only user-added exposures convert, in [`Self::user_choices`]), with
    /// one exception: a file's scene-modifier rename held only in the saved
    /// name ([`saved_modifier_rename`]).
    pub(crate) fn join_control(
        &mut self,
        saved: &ParamSpecDef,
    ) -> (ParamSpecDef, SpecSource, ParamChoices) {
        let none = ParamChoices::default();
        let single = self.single_binding(&saved.id);
        if let Some(template) = self.template_for(&saved.id) {
            // The default is the instance's own stamp (a material's authored
            // factor, re-seeded from the node), not the template's, as on the
            // node path.
            let (min, max) = widen_to(&template, saved.default_value);
            let base = ParamSpecDef {
                default_value: saved.default_value,
                min,
                max,
                ..template.clone()
            };
            let spec =
                self.resolver
                    .template_overlay(base, self.bindings, &self.nodes, self.scene_radius);
            // A saved name or section that differs is an old copy, ignored,
            // never converted into a choice: a later label fix must not read
            // as a user rename (D2 (one live lookup), old projects).
            return (spec, SpecSource::Lookup, none);
        }
        // A fan-out (several bindings under one id) is described by its first
        // binding's target, in authored order (D22).
        let Some(binding) = single.or_else(|| self.bindings.iter().find(|b| b.id == saved.id))
        else {
            return (saved.clone(), SpecSource::Authored, none);
        };
        if single.is_none() && !matches!(binding.target, BindingTarget::Node { .. }) {
            return (saved.clone(), SpecSource::Authored, none);
        }
        let fan_out = single.is_none();
        let unresolved = if fan_out {
            SpecSource::Authored
        } else {
            SpecSource::Unresolved
        };
        let Some(key) = key_for(self.preset, binding, &self.nodes, self.scene_radius) else {
            return match binding.target {
                BindingTarget::Composite { .. } => (saved.clone(), SpecSource::Authored, none),
                _ => (saved.clone(), unresolved, none),
            };
        };
        let Some(described) = self.resolver.describe(key) else {
            return (saved.clone(), unresolved, none);
        };
        let words_from_lookup = self.lookup_owns_words(&saved.id);
        let choices = match key {
            ParamKey::Modifier { recipe, param } if self.from_file && words_from_lookup => {
                ParamChoices {
                    rename: saved_modifier_rename(recipe, param, &saved.name, &described.name),
                    ..ParamChoices::default()
                }
            }
            _ => none,
        };
        (
            overlay(saved, described, binding.default_value, words_from_lookup),
            SpecSource::Lookup,
            choices,
        )
    }

    /// A control that is a copy of one declaration: a lone exact auto-stamp,
    /// or a lone scene-modifier control (a copy of its recipe's). Its saved
    /// name and tooltip are old copies; a user rename lives in its choices
    /// (D7 (one saved choice set)).
    fn lookup_owns_words(&self, id: &str) -> bool {
        self.single_binding(id).is_some_and(|b| match &b.target {
            BindingTarget::Node { .. } => exact_stamp(id, self.bindings, &self.nodes).is_some(),
            BindingTarget::SceneModifier { .. } => !b.user_added,
            BindingTarget::Composite { .. } => false,
        })
    }

    fn single_binding(&self, id: &str) -> Option<&'a BindingDef> {
        let mut matching = self.bindings.iter().filter(|b| b.id == id);
        match (matching.next(), matching.next()) {
            (Some(b), None) => Some(b),
            _ => None,
        }
    }

    /// The template's entry when the template answers this control.
    fn template_for(&self, id: &str) -> Option<ParamSpecDef> {
        let template_ok = self
            .single_binding(id)
            .is_none_or(|b| template_answers(self.preset, b, &self.nodes));
        template_ok
            .then(|| template_spec(self.preset, id))
            .flatten()
    }

    /// Whether the lookup supplies this control's name, so a save writes it
    /// without one: a template control, a lone exact auto-stamp or a lone
    /// scene-modifier control. Every other control owns its name
    /// (SCENE_SURFACES_DESIGN.md D24 (who owns a control's name)).
    pub(crate) fn lookup_names(&self, id: &str) -> bool {
        self.template_for(id).is_some() || self.lookup_owns_words(id)
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
        Some(Self {
            join: Join::new(&meta.id, def),
            params: &meta.params,
        })
    }

    pub fn spec(&mut self, id: &str) -> Option<ParamSpecDef> {
        let saved = self.params.iter().find(|p| p.id == id)?;
        if self
            .join
            .bindings
            .iter()
            .any(|b| b.user_added && b.id == id)
        {
            return Some(saved.clone());
        }
        Some(self.join.join_control(saved).0)
    }

    /// The control's display name, or empty when the graph has no entry.
    pub fn name(&mut self, id: &str) -> String {
        self.spec(id).map(|s| s.name).unwrap_or_default()
    }

    /// Whether the lookup supplies this control's name (D24 (who owns a
    /// control's name)).
    pub fn lookup_names(&self, id: &str) -> bool {
        self.join.lookup_names(id)
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

/// Every label a bundled scene-modifier recipe once shipped for a control and
/// later changed, as (recipe id, param id, former labels), read from the
/// git history of the recipe manifests (`crates/manifold-renderer/assets/
/// scene-modifier-presets/`, then `crates/manifold-nodes/assets/
/// scene-modifier-presets/`). A recipe relabel adds the old label here, or a
/// project saved under it reads the old label as a user rename.
const FORMER_RECIPE_LABELS: &[(&str, &str, &[&str])] = &[
    ("OrderedRecon", "attack_beats", &["Attack (beats)"]),
    ("OrderedRecon", "hold_beats", &["Hold (beats)"]),
    ("OrderedRecon", "return_beats", &["Return (beats)"]),
    ("OrderedRecon", "stagger_beats", &["Stagger (beats)"]),
    ("SurfacePeel", "burst_duration", &["Burst Duration (beats)"]),
    ("RadialForce", "center_x", &["Center X"]),
    ("RadialForce", "center_y", &["Center Y"]),
    ("RadialForce", "center_z", &["Center Z"]),
    ("RadialForce", "fire", &["Fire", "Kick"]),
    (
        "RadialForce",
        "impulse_strength",
        &["Impulse (m/s)", "Kick Size"],
    ),
    ("RadialForce", "strength", &["Strength (m/s²)"]),
    ("UniformForce", "direction_x", &["Vector X"]),
    ("UniformForce", "direction_y", &["Vector Y"]),
    ("UniformForce", "direction_z", &["Vector Z"]),
    ("UniformForce", "fire", &["Fire", "Kick"]),
    (
        "UniformForce",
        "impulse_strength",
        &["Impulse (m/s)", "Kick Size"],
    ),
    ("UniformForce", "strength", &["Strength (m/s²)"]),
    ("VortexForce", "center_x", &["Center X"]),
    ("VortexForce", "center_y", &["Center Y"]),
    ("VortexForce", "center_z", &["Center Z"]),
    ("VortexForce", "fire", &["Fire", "Kick"]),
    (
        "VortexForce",
        "impulse_strength",
        &["Impulse (m/s)", "Kick Size"],
    ),
    ("VortexForce", "strength", &["Strength (m/s²)"]),
];

/// A scene-modifier control's rename held only in its saved name. Before the
/// drawer wrote rename choices, a rename went straight into the saved name;
/// a saved name that is neither the recipe's label now nor one it shipped
/// before is that rename, and becomes a choice (D7 (one saved choice set)).
fn saved_modifier_rename(
    recipe: &PresetTypeId,
    param: &str,
    saved: &str,
    current: &str,
) -> Option<String> {
    let shipped = saved.is_empty()
        || saved == current
        || FORMER_RECIPE_LABELS
            .iter()
            .any(|(r, p, labels)| *r == recipe.as_str() && *p == param && labels.contains(&saved));
    (!shipped).then(|| saved.to_string())
}

/// A node or modifier description laid over the saved control. The lookup
/// owns range and flags, and the section where it declares one (a node
/// declares none yet, so its saved section stays). Name and tooltip are the
/// lookup's only for a copy of one declaration ([`Join::lookup_owns_words`]);
/// every other control owns its name (D24), and an empty one takes the
/// lookup's. Identity, default, card visibility, curve, invert and
/// periodicity stay as saved.
fn overlay(
    saved: &ParamSpecDef,
    d: ParamSpecDef,
    binding_default: f32,
    words_from_lookup: bool,
) -> ParamSpecDef {
    let (min, max) = widen_to(&d, binding_default);
    let keep_name = !words_from_lookup && !saved.name.is_empty();
    ParamSpecDef {
        name: if keep_name {
            saved.name.clone()
        } else {
            d.name
        },
        tooltip: if words_from_lookup {
            d.tooltip
        } else {
            saved.tooltip.clone()
        },
        section: if d.section.is_some() {
            d.section
        } else {
            saved.section.clone()
        },
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

    fn lamp_scene() -> manifold_foundation::scene_surface::SceneParamDesc {
        use manifold_foundation::scene_surface::{
            CardBand, CardLabel, ControlGroup, SceneParamDesc, SurfaceRole,
        };
        SceneParamDesc {
            role: SurfaceRole::Performance,
            group: ControlGroup::Light,
            band: Some(CardBand::Lighting),
            card_order: 0,
            card_label: CardLabel::ItemName,
            gate: None,
            switch: None,
            colour: None,
        }
    }

    /// The one node source in core's test binary.
    fn test_params(type_id: &str) -> Vec<NodeParamMetadata> {
        fn float_meta(name: &str, label: &str) -> NodeParamMetadata {
            NodeParamMetadata {
                name: name.to_string(),
                label: label.to_string(),
                min: 0.0,
                max: 1.0,
                default_value: SerializedParamValue::Float { value: 0.5 },
                is_angle: false,
                wraps: false,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: Vec::new(),
                convert: ParamConvert::Float,
                material_role: None,
                scene: None,
                unit: None,
                limit: Default::default(),
            }
        }
        match type_id {
            "node.test_lamp" => {}
            "node.light" => return vec![float_meta("intensity", "Intensity")],
            "node.transform_3d" => return vec![float_meta("pos_x", "X")],
            "node.test_hidden" => {
                let mut hidden = float_meta("hidden", "Hidden");
                hidden.scene = Some(manifold_foundation::scene_surface::SceneParamDesc {
                    role: manifold_foundation::scene_surface::SurfaceRole::Hidden,
                    ..lamp_scene()
                });
                return vec![
                    float_meta("first", "First"),
                    hidden,
                    float_meta("last", "Last"),
                ];
            }
            _ => return Vec::new(),
        }
        vec![NodeParamMetadata {
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
            scene: Some(lamp_scene()),
            unit: Some(manifold_foundation::scene_surface::Unit::Ev),
            limit: manifold_foundation::param_range::HardLimit::AtLeast(0.0),
        }]
    }

    inventory::submit! { NodeParamSource { params_for_type: test_params, world_inputs_for_type: |_| &[] } }

    #[test]
    fn stamped_node_params_drops_only_hidden_and_keeps_order() {
        let names =
            |params: Vec<NodeParamMetadata>| params.into_iter().map(|m| m.name).collect::<Vec<_>>();
        assert_eq!(
            names(node_params("node.test_hidden")),
            ["first", "hidden", "last"]
        );
        assert_eq!(
            names(stamped_node_params("node.test_hidden")),
            ["first", "last"]
        );
        assert!(stamped_node_params("node.not_registered").is_empty());
    }

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
                shape: None,
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
            nodes: vec![
                node("lamp", "node.test_lamp"),
                node("ghost", "node.not_registered"),
            ],
            wires: Vec::new(),
        }
    }

    fn lamp(param: &str) -> BindingTarget {
        BindingTarget::Node {
            node_id: NodeId::new("lamp"),
            param: param.into(),
        }
    }

    #[test]
    fn node_control_takes_range_from_the_lookup() {
        let def = graph(vec![binding("1_intensity", lamp("intensity"), 12.0)]);
        let preset = def.preset_metadata.as_ref().unwrap().id.clone();
        let mut join = Join::new(&preset, &def);
        let (spec, source, _) = join.join_control(&saved("1_intensity"));
        assert_eq!(source, SpecSource::Lookup);
        // A saved name is an old copy: the lookup's wins, and it is no choice.
        let (_, _, choices) = join.join_control(&saved("1_intensity"));
        assert_eq!(
            (spec.name.as_str(), choices),
            ("Intensity", ParamChoices::default())
        );
        // The declared band widens to keep the binding's seeded value on it.
        assert_eq!((spec.min, spec.max), (0.0, 12.0));
        // Identity, default and the fields later phases move stay as saved.
        assert_eq!(spec.id, "1_intensity");
        assert_eq!(spec.default_value, 12.0);
        assert_eq!(spec.section.as_deref(), Some("Key Light"));
        assert!(!spec.card_visible);
    }

    /// D24: a curated control on a generic node keeps its authored name and
    /// saves it; only the exact auto-stamp takes the node's label.
    #[test]
    fn curated_control_owns_its_name() {
        let def = graph(vec![
            binding("1_intensity", lamp("intensity"), 12.0),
            binding("key", lamp("intensity"), 12.0),
        ]);
        let preset = def.preset_metadata.as_ref().unwrap().id.clone();
        let mut join = Join::new(&preset, &def);
        let (curated, source, _) = join.join_control(&saved("key"));
        assert_eq!(
            (curated.name.as_str(), source),
            ("Stale Copy", SpecSource::Lookup)
        );
        assert_eq!(
            (curated.min, curated.max),
            (0.0, 12.0),
            "range still comes from the lookup"
        );
        assert_eq!(join.join_control(&saved("1_intensity")).0.name, "Intensity");
        assert!(join.lookup_names("1_intensity"));
        assert!(!join.lookup_names("key"));
    }

    /// A bundled template's exact auto-stamp shows its node's name, range,
    /// value labels and angle flag, as the old load-time refresh gave it.
    #[test]
    fn template_auto_stamp_takes_its_node_declaration() {
        let user = BindingDef {
            user_added: true,
            ..binding("1_intensity", lamp("intensity"), 12.0)
        };
        let mut def = graph(vec![
            binding("1_intensity", lamp("intensity"), 12.0),
            binding("key", lamp("intensity"), 12.0),
        ]);
        def.preset_metadata.as_mut().unwrap().params = vec![saved("1_intensity"), saved("key")];
        let params = template_params(&def);
        assert_eq!(params[0].name, "Intensity");
        assert_eq!(
            (params[0].min, params[0].max),
            (0.0, 12.0),
            "widened to the default"
        );
        assert_eq!(
            (params[0].default_value, params[0].section.as_deref()),
            (12.0, Some("Key Light"))
        );
        assert_eq!(
            params[0].scene,
            Some(lamp_scene()),
            "an auto-stamp takes its node's scene facts"
        );
        // D23: a curated id keeps its authored name and range, and takes the
        // bound node's scene facts because the template declares none.
        let key = &params[1];
        assert_eq!(
            (key.name.as_str(), key.min, key.max),
            ("Stale Copy", 0.0, 1.0)
        );
        assert_eq!(key.scene, Some(lamp_scene()));
        assert_eq!(key.unit, Some(manifold_foundation::scene_surface::Unit::Ev));
        assert_eq!(
            key.limit,
            manifold_foundation::param_range::HardLimit::AtLeast(0.0)
        );
        // Scene facts the template declares stand.
        let mut declared = def.clone();
        declared.preset_metadata.as_mut().unwrap().params[1].unit =
            Some(manifold_foundation::scene_surface::Unit::Metres);
        assert_eq!(
            template_params(&declared)[1].unit,
            Some(manifold_foundation::scene_surface::Unit::Metres)
        );

        def.preset_metadata.as_mut().unwrap().bindings = vec![user];
        assert_eq!(
            template_params(&def)[0],
            saved("1_intensity"),
            "a user exposure stays authored"
        );
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
        assert_eq!(
            (source, spec.name.as_str()),
            (SpecSource::Lookup, "Stale Copy")
        );
        assert_eq!((spec.min, spec.max), (0.0, 10.0));
        let ghost = graph(vec![
            binding(
                "sun",
                BindingTarget::Node {
                    node_id: NodeId::new("ghost"),
                    param: "x".into(),
                },
                1.0,
            ),
            binding("sun", lamp("intensity"), 1.0),
        ]);
        let (spec, source, _) = Join::new(&preset, &ghost).join_control(&saved("sun"));
        assert_eq!((source, spec), (SpecSource::Authored, saved("sun")));

        let composite = graph(vec![binding(
            "mix",
            BindingTarget::Composite {
                outer_name: "mix".into(),
            },
            1.0,
        )]);
        let (spec, source, _) = Join::new(&preset, &composite).join_control(&saved("mix"));
        assert_eq!(
            (source, spec.name.as_str()),
            (SpecSource::Authored, "Stale Copy")
        );
    }

    #[test]
    fn unknown_type_or_param_keeps_the_saved_spec_unresolved() {
        let def = graph(vec![
            binding(
                "1_x",
                BindingTarget::Node {
                    node_id: NodeId::new("ghost"),
                    param: "x".into(),
                },
                1.0,
            ),
            binding("1_gone", lamp("gone"), 1.0),
            binding(
                "1_dangling",
                BindingTarget::Node {
                    node_id: NodeId::new("nowhere"),
                    param: "x".into(),
                },
                1.0,
            ),
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
        let key = ParamKey::Node {
            type_id: "node.test_lamp",
            param: "intensity",
            scene_radius: Some(5.0),
        };
        // node.test_lamp has no scene-scaled band, so the declared range holds.
        assert_eq!(describe(key).map(|s| (s.min, s.max)), Some((0.0, 10.0)));
        assert!(
            describe(ParamKey::Node {
                type_id: "node.test_lamp",
                param: "nope",
                scene_radius: None
            })
            .is_none()
        );
    }

    #[test]
    fn old_scene_copies_are_ignored_then_dropped() {
        let def = graph(vec![binding("1_intensity", lamp("intensity"), 1.0)]);
        let preset = def.preset_metadata.as_ref().unwrap().id.clone();
        let mut join = Join::new(&preset, &def);
        let (joined, source, _) = join.join_control(&saved("1_intensity"));
        assert_eq!(source, SpecSource::Lookup);
        assert_eq!(
            (joined.min, joined.max),
            (0.0, 10.0),
            "the stale saved range is ignored"
        );
        // Saved as a stub: no range, no flags.
        let mut meta = def.preset_metadata.clone().unwrap();
        meta.params = vec![joined.lookup_stub()];
        let json = serde_json::to_value(&meta).unwrap();
        let entry = &json["params"][0];
        assert!(
            entry.get("min").is_none() && entry.get("max").is_none(),
            "{entry}"
        );
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
        BindingDef {
            user_added: true,
            ..binding(id, lamp("intensity"), 1.0)
        }
    }

    fn scene_json(params: serde_json::Value, user_label: &str) -> String {
        let mut def = graph(vec![
            binding("1_intensity", lamp("intensity"), 1.0),
            binding(
                "mix",
                BindingTarget::Composite {
                    outer_name: "mix".into(),
                },
                1.0,
            ),
            user_binding("user.lamp"),
        ]);
        let meta = def.preset_metadata.as_mut().unwrap();
        meta.params = vec![
            saved("1_intensity"),
            saved("mix"),
            ParamSpecDef {
                name: user_label.into(),
                ..saved("user.lamp")
            },
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

    fn state(
        fx: &crate::effects::PresetInstance,
    ) -> Vec<(ParamSpecDef, ParamChoices, SpecSource, f32)> {
        fx.params
            .iter()
            .map(|p| (p.spec.clone(), p.choices.clone(), p.source, p.value))
            .collect()
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
        assert_eq!((p.slider_span(), p.spec.invert), ((2.0, 4.0), true));
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
        assert_eq!(p.slider_span(), (0.0, 1.0));
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
        let bindings = fx
            .graph
            .as_ref()
            .unwrap()
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .clone();
        fx.refresh_manifest_from_graph();
        assert_eq!(state(&fx), before);
        let p = fx.params.get("1_intensity").unwrap();
        assert_eq!(p.choices.rename.as_deref(), Some("Key"));
        assert_eq!(p.choices.section, Some(None));
        assert_eq!(
            p.choices.pin,
            Some(manifold_foundation::scene_surface::CardPin::Shown)
        );
        let user = fx.params.get("user.lamp").unwrap();
        assert_eq!(user.origin, crate::params::ParamOrigin::UserAdded);
        assert_eq!(
            fx.graph
                .as_ref()
                .unwrap()
                .preset_metadata
                .as_ref()
                .unwrap()
                .bindings,
            bindings
        );
    }

    #[test]
    fn migrate_twice_is_noop() {
        let mut fx = load(&scene_json(chosen_params(), "Lamp Power"));
        fx.folded_sections = vec!["Key Light".into()];
        fx.unfolded_sections = vec!["Lighting".into()];
        let first = save(&fx);
        let second = save(&load(&first));
        assert_eq!(first, second);
        let wire: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(wire["foldedSections"], serde_json::json!(["Key Light"]));
        assert_eq!(wire["unfoldedSections"], serde_json::json!(["Lighting"]));
        // A described binding saves no label; composite and user ones keep it.
        let labels: Vec<_> = wire["graph"]["presetMetadata"]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| {
                (
                    b["id"].as_str().unwrap().to_string(),
                    b.get("label").is_some(),
                )
            })
            .collect();
        assert_eq!(
            labels,
            [
                ("1_intensity".into(), false),
                ("mix".into(), true),
                ("user.lamp".into(), true)
            ]
        );
        let back = load(&first);
        let lamp = &back
            .graph
            .as_ref()
            .unwrap()
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings[0];
        assert_eq!(lamp.label, "Key", "a load names the binding from the join");
        fx.folded_sections.clear();
        fx.unfolded_sections.clear();
        assert!(!save(&fx).contains("foldedSections"));
        assert!(!save(&fx).contains("unfoldedSections"));
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
        assert!(
            hits.is_empty(),
            "readers around the join:\n{}",
            hits.join("\n")
        );
    }
}
