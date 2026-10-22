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
use manifold_core::effects::PresetInstance;
use manifold_core::fluid_domain::FluidDomainSnapshot;
use manifold_core::layer::Layer;
use manifold_core::liquid_setup::{ColumnOutcome, SetupFit};
use manifold_core::project::Project;
use manifold_core::{LayerId, NodeId, PresetTypeId};
use manifold_nodes_scene::node_graph::scene_vm::{
    BodyAnchor, CopiesAnchor, ParamAddr, RuntimeFit, SceneVm, SetupProblem, body_anchors, body_problems, copies_anchors, copies_problems,
    problems_with_runtime,
};
use manifold_ui::param_surface::{ParamRow, ParamSurface};

use crate::physics_overlay::SharedDomains;

/// One liquid domain whose solver capped, clipped or refused the setup it
/// ran on the last rendered frame, or one Physics World whose copy pool left
/// copies unmade.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SetupFitEntry {
    pub layer: LayerId,
    pub node: NodeId,
    pub fit: SetupFit,
    pub refusal: Option<Arc<str>>,
    /// Copies asked for that the world's pool could not make: the most seen
    /// while latched.
    pub copies_not_made: u32,
    /// Coupled-world bodies the liquid could not take: the most seen while
    /// latched.
    pub uncoupled_bodies: u32,
    /// Bodies and copy groups the world left out: (world input, reason), the
    /// first reason seen for each input while latched.
    pub refused: Vec<(String, Arc<str>)>,
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
    not_made: Vec<(NodeId, u32)>,
    refused: Vec<(NodeId, String, Arc<str>)>,
    /// The latched entries; `report` is a copy taken when they change.
    scratch: Vec<SetupFitEntry>,
    latched_layers: Vec<LatchedLayer>,
    /// The edit stamps at the last frame.
    edits: Option<u64>,
    pub report: SetupFitReport,
}

impl Default for SetupFitObservations {
    fn default() -> Self {
        Self { domains: Vec::new(), not_made: Vec::new(), refused: Vec::new(), scratch: Vec::new(), latched_layers: Vec::new(), edits: None, report: Arc::from([]) }
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

/// Hands each rendered layer's domain snapshots, its worlds' unmade copies
/// and the bodies they left out to the callback, refilling the buffers it is
/// given.
pub(crate) type EachRenderedSetup<'a> =
    &'a mut dyn FnMut(&LayerId, &[(NodeId, FluidDomainSnapshot)], &[(NodeId, u32)], &[(NodeId, String, Arc<str>)]);

/// A card param's authored value, as `PresetInstance::get_base_param` reads
/// it, from the param in hand rather than a lookup by id.
fn authored_value(inst: &PresetInstance, param: &manifold_core::params::Param) -> f32 {
    if inst.base_tracked { param.base } else { param.value }
}

/// What one node reported this frame, folded into its latched entry.
struct Seen<'a> {
    fit: SetupFit,
    refusal: Option<&'a Arc<str>>,
    copies_not_made: u32,
    uncoupled_bodies: u32,
    /// The world's refused bodies: (world, input, reason).
    refused: &'a [(NodeId, String, Arc<str>)],
}

impl Seen<'_> {
    /// Fold into `entry`; true when it changed.
    fn fold_into(self, entry: &mut SetupFitEntry) -> bool {
        let mut fit = entry.fit;
        fit.fill_capped |= self.fit.fill_capped;
        fit.pool_full |= self.fit.pool_full;
        fit.column = match (fit.column, self.fit.column) {
            (ColumnOutcome::Fits, ran) => ran,
            // A column that was outside also crossed the wall.
            (held, ran) if ran != ColumnOutcome::Fits && ran != held => ColumnOutcome::Clipped,
            (held, _) => held,
        };
        let mut changed = false;
        if fit != entry.fit {
            entry.fit = fit;
            changed = true;
        }
        if entry.refusal.is_none() && self.refusal.is_some() {
            entry.refusal = self.refusal.cloned();
            changed = true;
        }
        if self.copies_not_made > entry.copies_not_made {
            entry.copies_not_made = self.copies_not_made;
            changed = true;
        }
        if self.uncoupled_bodies > entry.uncoupled_bodies {
            entry.uncoupled_bodies = self.uncoupled_bodies;
            changed = true;
        }
        for (_, input, reason) in self.refused {
            if !entry.refused.iter().any(|(held, _)| held == input) {
                entry.refused.push((input.clone(), reason.clone()));
                changed = true;
            }
        }
        changed
    }
}

impl SetupFitObservations {
    /// Take this frame's report from `observe`, which walks the layers that
    /// rendered (`GeneratorRenderer::each_rendered_setup`), into the latch.
    /// `project` and the editing service's `stamps` release it.
    pub fn record(
        &mut self,
        project: Option<&Project>,
        stamps: PartStamps,
        observe: impl FnOnce(&mut Vec<(NodeId, FluidDomainSnapshot)>, &mut Vec<(NodeId, u32)>, &mut Vec<(NodeId, String, Arc<str>)>, EachRenderedSetup<'_>),
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
        observe(&mut self.domains, &mut self.not_made, &mut self.refused, &mut |layer, domains, not_made, refused| {
            // A world's refusals arrive together.
            let refused_by = |node: &NodeId| refused.chunk_by(|a, b| a.0 == b.0).find(|chunk| &chunk[0].0 == node).unwrap_or(&[]);
            let domains = domains.iter().map(|(node, snapshot)| (node, Seen { fit: snapshot.fit, refusal: snapshot.refusal.as_ref(), copies_not_made: 0, uncoupled_bodies: snapshot.uncoupled_bodies, refused: &[] }));
            let worlds = not_made.iter().map(|(node, count)| (node, Seen { fit: SetupFit::default(), refusal: None, copies_not_made: *count, uncoupled_bodies: 0, refused: refused_by(node) }));
            let refusing = refused
                .chunk_by(|a, b| a.0 == b.0)
                .filter(|chunk| !not_made.iter().any(|(node, _)| *node == chunk[0].0))
                .map(|chunk| (&chunk[0].0, Seen { fit: SetupFit::default(), refusal: None, copies_not_made: 0, uncoupled_bodies: 0, refused: chunk }));
            for (node, seen) in domains.chain(worlds).chain(refusing) {
                if seen.fit.is_clean() && seen.refusal.is_none() && seen.copies_not_made == 0 && seen.uncoupled_bodies == 0 && seen.refused.is_empty() {
                    continue;
                }
                if !latched_layers.iter().any(|latched| &latched.layer == layer) {
                    let Some(found) = find(layer) else { continue };
                    latched_layers.push(LatchedLayer::new(found));
                }
                match scratch.iter_mut().find(|entry| &entry.layer == layer && &entry.node == node) {
                    Some(entry) => changed |= seen.fold_into(entry),
                    None => {
                        scratch.push(SetupFitEntry {
                            layer: layer.clone(),
                            node: node.clone(),
                            fit: seen.fit,
                            refusal: seen.refusal.cloned(),
                            copies_not_made: seen.copies_not_made,
                            uncoupled_bodies: seen.uncoupled_bodies,
                            refused: seen.refused.iter().map(|(_, input, reason)| (input.clone(), reason.clone())).collect(),
                        });
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
        uncoupled_bodies: entry.uncoupled_bodies,
        copies_not_made: entry.copies_not_made,
    })
}

/// The problems of the graph `def`: what the copy pools of its Physics Worlds
/// reported (`runtime`) as unmade copies.
pub(crate) fn problems_of<'a>(def: &EffectGraphDef, runtime: impl IntoIterator<Item = RuntimeFit<'a>>) -> Vec<SetupProblem> {
    let mut out = Vec::new();
    copies_problems(&copies_anchors(def), runtime, &mut out);
    out
}

/// The first body `now` reports refused that `before` did not, as the toast
/// says it.
pub(crate) fn new_refusal(before: &[SetupFitEntry], now: &[SetupFitEntry]) -> Option<String> {
    now.iter().find_map(|entry| {
        let held = before.iter().find(|old| old.layer == entry.layer && old.node == entry.node);
        entry
            .refused
            .iter()
            .find(|(input, _)| held.is_none_or(|old| !old.refused.iter().any(|(seen, _)| seen == input)))
            .map(|(_, reason)| format!("Physics: a body is out of the world: {reason}"))
    })
}

/// `layer`'s refused bodies in the report: (world, world input, reason).
pub(crate) fn refused_bodies<'a>(report: &'a [SetupFitEntry], layer: &'a LayerId) -> impl Iterator<Item = (&'a NodeId, &'a str, &'a str)> + 'a {
    report
        .iter()
        .filter(move |entry| &entry.layer == layer)
        .flat_map(|entry| entry.refused.iter().map(move |(input, reason)| (&entry.node, input.as_str(), &**reason)))
}

/// `f` over a generator layer's graph, its own else the catalog's.
fn with_layer_def<R>(layer: &Layer, f: impl FnOnce(&EffectGraphDef) -> R) -> Option<R> {
    match layer.generator_graph() {
        Some(def) => Some(f(def)),
        None => manifold_nodes::bundled_presets::bundled_preset_def(layer.generator_type()).map(|def| f(&def)),
    }
}

/// The graph node with doc id `node_doc_id` at `scope_path`, a path of group
/// doc ids to descend into (empty is the document root).
fn find_node_by_scope<'a>(def: &'a EffectGraphDef, scope_path: &[u32], node_doc_id: u32) -> Option<&'a manifold_core::effect_graph_def::EffectGraphNode> {
    let mut nodes = def.nodes.as_slice();
    for group_id in scope_path {
        nodes = &nodes.iter().find(|n| n.id == *group_id)?.group.as_ref()?.nodes;
    }
    nodes.iter().find(|n| n.id == node_doc_id)
}

/// The card param bound to a node param, resolved through its group scope.
/// Recipe-local numeric ids may repeat in nested groups; stable binding
/// targets do not.
fn card_binding_id(def: &EffectGraphDef, addr: &ParamAddr) -> Option<String> {
    let Some(node) = find_node_by_scope(def, &addr.scope_path, addr.node_doc_id) else {
        return addr.scope_path.is_empty().then(|| manifold_core::effects::binding_id_for_node_param_in(def, addr.node_doc_id, &addr.param_id)).flatten();
    };
    let identity = if node.node_id.is_empty() {
        NodeId::new(node.handle.clone().unwrap_or_else(|| format!("node{}", addr.node_doc_id)))
    } else {
        node.node_id.clone()
    };
    def.preset_metadata.as_ref()?.bindings.iter().find(|binding| {
        matches!(&binding.target, manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } if node_id == &identity && param == &addr.param_id)
    }).map(|binding| binding.id.clone())
}

/// Mark a generator layer's card with its problems.
pub(crate) fn mark_layer_card(card: &mut ParamSurface, layer: &Layer, report: &[SetupFitEntry]) {
    with_layer_def(layer, |def| {
        let mut problems = problems_of(def, runtime_fits(report, &layer.layer_id));
        body_problems(&body_anchors(def), refused_bodies(report, &layer.layer_id), &mut problems);
        mark_rows(&mut card.rows, &problems, def);
    });
}

/// Put each problem's warning on the first exposed row that edits one of its
/// params, so the line sits under the control that fixes it. A problem none
/// of whose params a row edits (an unbound shape) shows in the viewport only.
pub(crate) fn mark_rows(rows: &mut [ParamRow], problems: &[SetupProblem], def: &EffectGraphDef) {
    for problem in problems {
        let message = problem.kind.message();
        for addr in &problem.params {
            let Some(id) = card_binding_id(def, addr)
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

/// What can move a copy pool's report or which graph a layer resolves: a
/// frame where none moved skips the per-layer keys.
const CHECK_PARTS: ChangeSet = ChangeSet::of(&[Part::Layers, Part::Effects, Part::Overrides, Part::Shapes, Part::EmbeddedPresets, Part::Values]);

/// One card's setup check: the active layer's or the Scene panel's.
#[derive(Default)]
struct SlotCheck {
    layer: Option<LayerId>,
    /// The graph and catalog the copy anchors were read from.
    graph: u64,
    /// The Physics Worlds that copy bodies, read once per graph.
    copies: Vec<CopiesAnchor>,
    /// The Physics Worlds' wired bodies, read once per graph.
    bodies: Vec<BodyAnchor>,
    report: Option<SetupFitReport>,
    problems: Vec<SetupProblem>,
    /// What the card shows of the problems; 0 when none.
    hash: u64,
}

impl SlotCheck {
    fn graph_key(layer: &Layer, generation: u64) -> u64 {
        let mut hasher = DefaultHasher::new();
        layer.generator_graph().is_some().hash(&mut hasher);
        layer.generator_graph_version().hash(&mut hasher);
        layer.generator_graph_structure_version().hash(&mut hasher);
        layer.generator_type().hash(&mut hasher);
        generation.hash(&mut hasher);
        hasher.finish()
    }

    /// Follow `layer`'s graph and solver report. The anchors are re-read only
    /// when the graph changes. Unless `woke` (a part the check reads moved) or
    /// the report changed, it costs nothing.
    fn follow(&mut self, project: &Project, layer: Option<&LayerId>, report: &SetupFitReport, generation: u64, woke: bool) {
        if !woke && self.layer.as_ref() == layer && self.report.as_ref().is_some_and(|seen| Arc::ptr_eq(seen, report)) {
            return;
        }
        let Some(layer) = layer.and_then(|id| project.timeline.find_layer_by_id(id)).map(|(_, layer)| layer).filter(|layer| layer.gen_params().is_some()) else {
            if self.layer.take().is_some() || self.hash != 0 {
                self.copies.clear();
                self.bodies.clear();
                self.report = None;
                self.problems.clear();
                self.hash = 0;
            }
            return;
        };
        let graph = Self::graph_key(layer, generation);
        let same_layer = self.layer.as_ref() == Some(&layer.layer_id);
        let mut moved = false;
        if !same_layer || graph != self.graph {
            self.layer = Some(layer.layer_id.clone());
            self.copies = with_layer_def(layer, copies_anchors).unwrap_or_default();
            self.bodies = with_layer_def(layer, body_anchors).unwrap_or_default();
            self.graph = graph;
            moved = true;
        }
        if !moved && self.report.as_ref().is_some_and(|seen| Arc::ptr_eq(seen, report)) {
            return;
        }
        self.report = Some(report.clone());
        self.problems.clear();
        copies_problems(&self.copies, runtime_fits(report, &layer.layer_id), &mut self.problems);
        body_problems(&self.bodies, refused_bodies(report, &layer.layer_id), &mut self.problems);
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
