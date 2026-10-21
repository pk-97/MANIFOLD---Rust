//! The setup check on the cards and the gizmos (SCENE_SETUP_PROGRAMME.md
//! section 3 B3). The check itself is `SceneVm::problems` (declared shapes at
//! the values the knobs show) plus `problems_with_runtime` (what each
//! solver's fit did to the values it ran). This module carries the solver's
//! half from the content thread to the cards whatever Show gizmos says
//! ([`SetupFitObservations`], published as `ContentState::setup_fits` and
//! latched), puts
//! each warning on the card row that edits its shape ([`mark_rows`]), keys
//! the frame planner on the cards' problem list ([`CardCheck`]) and keeps
//! each gizmo host's list for its red boxes ([`GizmoProblems`]).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use manifold_core::change::{ChangeSet, Part, PartStamps};
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::{ParamConvert, PresetInstance, apply_card_reshape};
use manifold_core::fluid_domain::FluidDomainSnapshot;
use manifold_core::layer::Layer;
use manifold_core::liquid_domain::liquid_domains_in;
use manifold_core::liquid_setup::{ColumnOutcome, SetupFit};
use manifold_core::macro_bank::MacroCurve;
use manifold_core::project::Project;
use manifold_core::{LayerId, NodeId, PresetTypeId};
use manifold_node_engine::param_binding::convert_param_value;
use manifold_node_engine::parameters::ParamValue;
use manifold_nodes_scene::node_graph::scene_vm::{
    ParamAddr, RuntimeFit, SceneVm, SetupAnchor, SetupProblem, problems_with_runtime,
};
use manifold_ui::param_surface::{ParamRow, ParamSurface};

use crate::physics_overlay::SharedDomains;

/// One liquid domain whose solver capped, clipped or refused the setup it
/// ran on the last rendered frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SetupFitEntry {
    pub layer: LayerId,
    pub node: NodeId,
    pub fit: SetupFit,
    pub refusal: Option<Arc<str>>,
}

/// Every such domain the cards warn about: the content thread's report,
/// latched ([`SetupFitObservations`]). Empty while nothing is latched.
pub(crate) type SetupFitReport = Arc<[SetupFitEntry]>;

/// Content-thread side of the report. A warning the solver reports latches
/// on like a clip light and stays until an authored edit changes that layer:
/// a graph edit, or a card value a command through `EditingService` (an edit
/// or an undo) moved. Automation, a live hand and modulation never release
/// it, so a driven fill or column sweeping in and out of the walls shows one
/// steady line, not a flicker. A column seen both clipped and outside latches
/// as clipped, one line. The buffers are reused; a new report is allocated
/// only when the latched set changes.
pub(crate) struct SetupFitObservations {
    domains: Vec<(NodeId, FluidDomainSnapshot)>,
    /// The latched entries; `report` is a copy taken when they change.
    scratch: Vec<SetupFitEntry>,
    latched_layers: Vec<LatchedLayer>,
    /// The edit stamps at the last frame.
    edits: Option<u64>,
    pub report: SetupFitReport,
}

impl Default for SetupFitObservations {
    fn default() -> Self {
        Self { domains: Vec::new(), scratch: Vec::new(), latched_layers: Vec::new(), edits: None, report: Arc::from([]) }
    }
}

/// What can release a latch: an edit to a layer's cards, graph or manifest.
const LATCH_PARTS: ChangeSet = ChangeSet::of(&[Part::Layers, Part::Effects, Part::Overrides, Part::EmbeddedPresets, Part::Values]);

/// A layer holding latched warnings, followed frame by frame so an authored
/// edit is told apart from automation. Allocated when it latches.
struct LatchedLayer {
    layer: LayerId,
    graph: (u32, u32, PresetTypeId),
    /// Each card's authored value at the last frame, in manifest order.
    values: Vec<f32>,
    /// Cards whose value moved on a frame with no edit (automation or a live
    /// hand): their moves never release the latch.
    playing: Vec<bool>,
}

impl LatchedLayer {
    fn graph(layer: &Layer) -> (u32, u32, PresetTypeId) {
        (layer.generator_graph_version(), layer.generator_graph_structure_version(), layer.generator_type().clone())
    }

    fn new(layer: &Layer) -> Self {
        let values: Vec<f32> = layer.gen_params().map_or_else(Vec::new, |inst| inst.params.iter().map(|param| authored_value(inst, param)).collect());
        Self { layer: layer.layer_id.clone(), graph: Self::graph(layer), playing: vec![false; values.len()], values }
    }

    /// Follow this frame's card values. True when an authored edit changed the
    /// layer: its graph, its manifest, or on a frame with an edit (`edit`) a
    /// card that is not playing.
    fn edited(&mut self, layer: &Layer, edit: bool) -> bool {
        let (version, structure, kind) = &self.graph;
        if layer.generator_graph_version() != *version || layer.generator_graph_structure_version() != *structure || layer.generator_type() != kind {
            return true;
        }
        let Some(inst) = layer.gen_params() else { return true };
        if inst.params.len() != self.values.len() {
            return true;
        }
        for ((param, value), playing) in inst.params.iter().zip(&mut self.values).zip(&mut self.playing) {
            let now = authored_value(inst, param);
            if now.to_bits() == value.to_bits() {
                continue;
            }
            if edit && !*playing {
                return true;
            }
            *playing |= !edit;
            *value = now;
        }
        false
    }
}

/// Hands each rendered layer's domain snapshots to the callback, refilling
/// the buffer it is given.
pub(crate) type EachRenderedDomains<'a> = &'a mut dyn FnMut(&LayerId, &[(NodeId, FluidDomainSnapshot)]);

/// A card param's authored value, as `PresetInstance::get_base_param` reads
/// it, from the param in hand rather than a lookup by id.
fn authored_value(inst: &PresetInstance, param: &manifold_core::params::Param) -> f32 {
    if inst.base_tracked { param.base } else { param.value }
}

impl SetupFitObservations {
    /// Take this frame's report from `observe`, which walks the layers that
    /// rendered (`GeneratorRenderer::each_rendered_fluid_domains`), into the
    /// latch. `project` and the editing service's `stamps` release it.
    pub fn record(
        &mut self,
        project: Option<&Project>,
        stamps: PartStamps,
        observe: impl FnOnce(&mut Vec<(NodeId, FluidDomainSnapshot)>, EachRenderedDomains<'_>),
    ) {
        let find = |id: &LayerId| project.and_then(|p| p.timeline.find_layer_by_id(id)).map(|(_, layer)| layer);
        let edits = stamps.key(LATCH_PARTS);
        let edit = self.edits.replace(edits).is_some_and(|last| last != edits);
        let before = self.scratch.len();
        self.latched_layers.retain_mut(|latched| find(&latched.layer).is_some_and(|layer| !latched.edited(layer, edit)));
        let latched_layers = &self.latched_layers;
        self.scratch.retain(|entry| latched_layers.iter().any(|latched| latched.layer == entry.layer));
        let mut changed = self.scratch.len() != before;
        let (scratch, latched_layers) = (&mut self.scratch, &mut self.latched_layers);
        observe(&mut self.domains, &mut |layer, domains| {
            for (node, snapshot) in domains {
                if snapshot.fit.is_clean() && snapshot.refusal.is_none() {
                    continue;
                }
                if !latched_layers.iter().any(|latched| &latched.layer == layer) {
                    let Some(found) = find(layer) else { continue };
                    latched_layers.push(LatchedLayer::new(found));
                }
                match scratch.iter_mut().find(|entry| &entry.layer == layer && &entry.node == node) {
                    Some(entry) => {
                        let mut fit = entry.fit;
                        fit.fill_capped |= snapshot.fit.fill_capped;
                        fit.pool_full |= snapshot.fit.pool_full;
                        fit.column = match (fit.column, snapshot.fit.column) {
                            (ColumnOutcome::Fits, ran) => ran,
                            // A column that was outside also crossed the wall.
                            (held, ran) if ran != ColumnOutcome::Fits && ran != held => ColumnOutcome::Clipped,
                            (held, _) => held,
                        };
                        if fit != entry.fit {
                            entry.fit = fit;
                            changed = true;
                        }
                        if entry.refusal.is_none() && snapshot.refusal.is_some() {
                            entry.refusal = snapshot.refusal.clone();
                            changed = true;
                        }
                    }
                    None => {
                        scratch.push(SetupFitEntry { layer: layer.clone(), node: node.clone(), fit: snapshot.fit, refusal: snapshot.refusal.clone() });
                        changed = true;
                    }
                }
            }
        });
        if changed {
            // Layers arrive in hash order: sort, so the report reads the same each time.
            self.scratch.sort_unstable_by(|a, b| (a.layer.as_str(), a.node.as_str()).cmp(&(b.layer.as_str(), b.node.as_str())));
            self.report = Arc::from(self.scratch.as_slice());
        }
    }
}

/// `layer`'s entries in the report, as the check reads them. The cards draw
/// no box, so no bounds ride along.
pub(crate) fn runtime_fits<'a>(report: &'a [SetupFitEntry], layer: &'a LayerId) -> impl Iterator<Item = RuntimeFit<'a>> + 'a {
    report.iter().filter(move |entry| &entry.layer == layer).map(|entry| RuntimeFit {
        node: &entry.node,
        fit: entry.fit,
        refusal: entry.refusal.as_deref(),
        layout: None,
        column_box: None,
    })
}

/// The problems of the scene `def` is, with `inst`'s card values folded into
/// the params they drive so shapes sit where the knobs put them, plus what
/// the solvers reported (`runtime`). Empty for a graph with no liquid domain,
/// which builds no scene model at all.
pub(crate) fn problems_of<'a>(def: &EffectGraphDef, inst: &PresetInstance, runtime: impl IntoIterator<Item = RuntimeFit<'a>>) -> Vec<SetupProblem> {
    if liquid_domains_in(&def.nodes).is_empty() {
        return Vec::new();
    }
    let mut authored = def.clone();
    if crate::scene_gizmo::fold_card_values(&mut authored, inst).is_none() {
        authored = def.clone();
    }
    let Some(vm) = SceneVm::from_def(&authored) else { return Vec::new() };
    let mut out = Vec::new();
    problems_with_runtime(&vm.setup_anchors, &vm.problems, runtime, &mut out);
    out
}

/// `f` over a generator layer's graph (its own, else the catalog's) and its
/// card values.
fn with_layer_def<R>(layer: &Layer, f: impl FnOnce(&EffectGraphDef, &PresetInstance) -> R) -> Option<R> {
    let inst = layer.gen_params()?;
    match layer.generator_graph() {
        Some(def) => Some(f(def, inst)),
        None => manifold_nodes::bundled_presets::bundled_preset_def(layer.generator_type()).map(|def| f(&def, inst)),
    }
}

/// Mark a generator layer's card with its problems.
pub(crate) fn mark_layer_card(card: &mut ParamSurface, layer: &Layer, report: &[SetupFitEntry]) {
    with_layer_def(layer, |def, inst| mark_rows(&mut card.rows, &problems_of(def, inst, runtime_fits(report, &layer.layer_id)), def));
}

/// Put each problem's warning on the first exposed row that edits one of its
/// params, so the line sits under the control that fixes it. A problem none
/// of whose params a row edits (an unbound shape) shows in the viewport only.
pub(crate) fn mark_rows(rows: &mut [ParamRow], problems: &[SetupProblem], def: &EffectGraphDef) {
    for problem in problems {
        let message = problem.kind.message();
        for addr in &problem.params {
            let Some(id) = crate::ui_bridge::project::scene_binding_id(def, &addr.scope_path, addr.node_doc_id, &addr.param_id)
            else {
                continue;
            };
            if let Some(row) = rows.iter_mut().find(|row| row.value.exposed && row.id.as_ref() == id.as_str()) {
                row.spec.setup_warning.get_or_insert(message);
                break;
            }
        }
    }
}

/// A card param that places a setup shape: how its value reaches the node
/// param the check reads, the same reshape `scene_gizmo::fold_card_values`
/// folds it with.
struct BoundParam {
    addr: ParamAddr,
    card: String,
    /// The card param's place in the manifest; the model is rebuilt when the
    /// manifest's order changes.
    index: usize,
    min: f32,
    max: f32,
    invert: bool,
    curve: MacroCurve,
    scale: f32,
    offset: f32,
    convert: ParamConvert,
    /// A change needs a rebuild ([`SceneVm::setup_params`]).
    structural: bool,
}

impl BoundParam {
    /// The node param value the card's base value puts here. `None` keeps the
    /// value the scene model was built at (no card value, or one the fold
    /// does not write either).
    fn value(&self, inst: &PresetInstance) -> Option<f32> {
        let param = inst.params.iter().nth(self.index).filter(|param| param.id() == self.card)?;
        let base = authored_value(inst, param);
        if !base.is_finite() {
            return None;
        }
        let effective = apply_card_reshape(base, self.min, self.max, self.invert, self.curve, self.scale, self.offset);
        match convert_param_value(self.convert, effective) {
            ParamValue::Float(v) if v.is_finite() => Some(v),
            ParamValue::Enum(v) => Some(v as f32),
            _ => None,
        }
    }
}

/// One layer's scene model, built once per graph, with the card params that
/// move its setup shapes.
struct SlotModel {
    vm: SceneVm,
    bound: Vec<BoundParam>,
}

impl SlotModel {
    /// `None` for a graph with no liquid domain, which has no setup to check.
    fn build(def: &EffectGraphDef, inst: &PresetInstance) -> Option<Self> {
        if liquid_domains_in(&def.nodes).is_empty() {
            return None;
        }
        let mut authored = def.clone();
        if crate::scene_gizmo::fold_card_values(&mut authored, inst).is_none() {
            authored = def.clone();
        }
        let vm = SceneVm::from_def(&authored)?;
        let metadata = def.preset_metadata.as_ref();
        let mut bound: Vec<BoundParam> = Vec::new();
        for (addr, structural) in vm.setup_params() {
            if bound.iter().any(|known| &known.addr == addr) {
                continue;
            }
            let Some(card) = crate::ui_bridge::project::scene_binding_id(def, &addr.scope_path, addr.node_doc_id, &addr.param_id) else {
                continue;
            };
            let Some(binding) = metadata.and_then(|m| m.bindings.iter().find(|b| b.id == card)) else { continue };
            let Some(spec) = metadata.and_then(|m| m.params.iter().find(|spec| spec.id == card)) else { continue };
            if binding.convert == ParamConvert::Trigger || spec.is_trigger {
                continue;
            }
            // A card param the instance lacks folds its default, never a value.
            let Some(index) = inst.params.iter().position(|param| param.id() == card) else { continue };
            bound.push(BoundParam {
                addr: addr.clone(),
                card,
                index,
                min: spec.min,
                max: spec.max,
                invert: spec.invert,
                curve: spec.curve,
                scale: binding.scale,
                offset: binding.offset,
                convert: binding.convert,
                structural,
            });
        }
        Some(Self { vm, bound })
    }

    /// The bound card values of one kind, hashed without allocating.
    fn hash_values(&self, inst: &PresetInstance, structural: bool, state: &mut impl Hasher) {
        for param in self.bound.iter().filter(|param| param.structural == structural) {
            param.value(inst).map(f32::to_bits).hash(state);
        }
    }

    fn value(&self, inst: &PresetInstance, addr: &ParamAddr) -> Option<f32> {
        self.bound.iter().find(|param| !param.structural && &param.addr == addr)?.value(inst)
    }
}

/// What can move a shape, a card value or which graph a layer resolves: a
/// frame where none moved skips the per-layer keys.
const CHECK_PARTS: ChangeSet = ChangeSet::of(&[Part::Layers, Part::Effects, Part::Overrides, Part::Shapes, Part::EmbeddedPresets, Part::Values]);

/// One card's setup check: the active layer's or the Scene panel's.
#[derive(Default)]
struct SlotCheck {
    layer: Option<LayerId>,
    /// The graph, catalog and manifest the model was built from.
    graph: u64,
    /// The card values that set the model's form, at build.
    form: u64,
    model: Option<SlotModel>,
    /// Every card's base value: when it holds, no card value moved.
    card: u64,
    /// The card values the setup shapes read, at the last check.
    values: u64,
    report: Option<SetupFitReport>,
    authored: Vec<SetupProblem>,
    anchors: Vec<SetupAnchor>,
    problems: Vec<SetupProblem>,
    /// What the card shows of the problems; 0 when none.
    hash: u64,
    builds: usize,
    checks: usize,
}

impl SlotCheck {
    fn graph_key(layer: &Layer, inst: &PresetInstance, generation: u64) -> u64 {
        let mut hasher = DefaultHasher::new();
        layer.generator_graph().is_some().hash(&mut hasher);
        layer.generator_graph_version().hash(&mut hasher);
        layer.generator_graph_structure_version().hash(&mut hasher);
        layer.generator_type().hash(&mut hasher);
        generation.hash(&mut hasher);
        inst.params.topology().hash(&mut hasher);
        inst.params.len().hash(&mut hasher);
        hasher.finish()
    }

    /// Every card base value in one pass: a manifest lookup per bound param
    /// costs more than this, so it gates them.
    fn card_key(inst: &PresetInstance) -> u64 {
        let mut hasher = DefaultHasher::new();
        for param in inst.params.iter() {
            authored_value(inst, param).to_bits().hash(&mut hasher);
        }
        hasher.finish()
    }

    fn model_values(&self, inst: &PresetInstance, structural: bool) -> u64 {
        self.model.as_ref().map_or(0, |model| {
            let mut hasher = DefaultHasher::new();
            model.hash_values(inst, structural, &mut hasher);
            hasher.finish()
        })
    }

    /// Follow `layer`'s graph, card values and solver report. The scene model
    /// is rebuilt only when the graph or its form changes; a card value the
    /// setup reads re-checks it; any other card value costs one pass over the
    /// card's base values. Unless `woke` (a part the check reads moved) or the
    /// report changed, it costs nothing.
    fn follow(&mut self, project: &Project, layer: Option<&LayerId>, report: &SetupFitReport, generation: u64, woke: bool) {
        if !woke && self.layer.as_ref() == layer && self.report.as_ref().is_some_and(|seen| Arc::ptr_eq(seen, report)) {
            return;
        }
        let found = layer.and_then(|id| project.timeline.find_layer_by_id(id)).and_then(|(_, layer)| Some((layer, layer.gen_params()?)));
        let Some((layer, inst)) = found else {
            if self.layer.take().is_some() || self.hash != 0 {
                self.model = None;
                self.report = None;
                self.problems.clear();
                self.hash = 0;
            }
            return;
        };
        let graph = Self::graph_key(layer, inst, generation);
        let card = Self::card_key(inst);
        let same_layer = self.layer.as_ref() == Some(&layer.layer_id);
        let mut moved = false;
        if !same_layer || graph != self.graph || (card != self.card && self.model_values(inst, true) != self.form) {
            self.layer = Some(layer.layer_id.clone());
            self.model = with_layer_def(layer, SlotModel::build).flatten();
            self.graph = graph;
            self.form = self.model_values(inst, true);
            self.builds += 1;
            moved = true;
        }
        if moved || card != self.card {
            self.card = card;
            let values = self.model_values(inst, false);
            if moved || values != self.values {
                self.values = values;
                match &self.model {
                    Some(model) => model.vm.recheck_setup(&|addr| model.value(inst, addr), &mut self.authored, &mut self.anchors),
                    None => {
                        self.authored.clear();
                        self.anchors.clear();
                    }
                }
                self.checks += 1;
                moved = true;
            }
        }
        if !moved && self.report.as_ref().is_some_and(|seen| Arc::ptr_eq(seen, report)) {
            return;
        }
        self.report = Some(report.clone());
        problems_with_runtime(&self.anchors, &self.authored, runtime_fits(report, &layer.layer_id), &mut self.problems);
        let mut hasher = DefaultHasher::new();
        for problem in &self.problems {
            problem.hash_card(&mut hasher);
        }
        self.hash = if self.problems.is_empty() { 0 } else { hasher.finish() };
    }
}

/// The cards' setup check as a frame-planner input (UI_INVALIDATION_DESIGN.md
/// D6): a hash of the problem list the active layer's card and the Scene
/// panel's card show. A knob scrub that leaves the list alone, or one on
/// another layer, leaves the hash alone, so live values still update in
/// place (D22). Each card keeps its own layer's scene model and re-checks it
/// only when a value its setup shapes read moves.
#[derive(Default)]
pub(crate) struct CardCheck {
    slots: [SlotCheck; 2],
    /// The parts' stamps and catalog generation at the last frame.
    seen: Option<(u64, u64)>,
}

impl CardCheck {
    /// The input this frame; 0 while neither layer has a problem.
    pub fn input(&mut self, project: &Project, parts: PartStamps, layers: [Option<&LayerId>; 2], report: &SetupFitReport) -> u64 {
        let generation = manifold_node_engine::load::preset_loader::catalog_generation();
        let seen = Some((parts.key(CHECK_PARTS), generation));
        let woke = seen != self.seen;
        self.seen = seen;
        // The Scene panel on the active layer shows the same list: check it once.
        let second = layers[1].filter(|layer| Some(*layer) != layers[0]);
        self.slots[0].follow(project, layers[0], report, generation, woke);
        self.slots[1].follow(project, second, report, generation, woke);
        if self.slots.iter().all(|slot| slot.hash == 0) {
            return 0;
        }
        let mut hasher = DefaultHasher::new();
        for slot in &self.slots {
            slot.hash.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Scene model builds and setup checks so far, over both cards.
    #[cfg(test)]
    pub fn work(&self) -> (usize, usize) {
        self.slots.iter().fold((0, 0), |(builds, checks), slot| (builds + slot.builds, checks + slot.checks))
    }
}

/// A gizmo host's setup problems, for its red boxes: the shown scene's
/// authored list plus what the shown frame's solvers reported. Keyed on the
/// scene by pointer and on each domain's fit and refusal, never its boxes: a
/// driven column moving inside its warning rebuilds nothing, and the box is
/// read from the frame at draw time ([`SetupProblem::live_bounds`]). A steady
/// frame allocates nothing.
#[derive(Default)]
pub(crate) struct GizmoProblems {
    scene: Option<Arc<SceneVm>>,
    fits: Vec<(NodeId, SetupFit, Option<Arc<str>>)>,
    problems: Vec<SetupProblem>,
}

impl GizmoProblems {
    /// Follow the shown scene and domains. True when the list was rebuilt.
    pub fn update(&mut self, scene: Option<&Arc<SceneVm>>, domains: Option<&SharedDomains>) -> bool {
        let Some(scene) = scene else {
            let rebuilt = self.scene.take().is_some();
            self.fits.clear();
            self.problems.clear();
            return rebuilt;
        };
        let domains = domains.map_or(&[][..], |domains| &domains[..]);
        let same_fits = self.fits.len() == domains.len()
            && self.fits.iter().zip(domains).all(|((node, fit, refusal), (id, snapshot))| {
                node == id && *fit == snapshot.fit && refusal.as_deref() == snapshot.refusal.as_deref()
            });
        if same_fits && self.scene.as_ref().is_some_and(|cached| Arc::ptr_eq(cached, scene)) {
            return false;
        }
        problems_with_runtime(&scene.setup_anchors, &scene.problems, domains.iter().map(RuntimeFit::of), &mut self.problems);
        self.scene = Some(scene.clone());
        if !same_fits {
            self.fits.clear();
            self.fits.extend(domains.iter().map(|(id, snapshot)| (id.clone(), snapshot.fit, snapshot.refusal.clone())));
        }
        true
    }

    pub fn problems(&self) -> &[SetupProblem] {
        &self.problems
    }
}
