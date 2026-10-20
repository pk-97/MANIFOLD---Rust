//! The setup check on the cards and the gizmos (SCENE_SETUP_PROGRAMME.md
//! section 3 B3). The check itself is `SceneVm::problems` (declared shapes at
//! the values the knobs show) plus `problems_with_runtime` (what each
//! solver's fit did to the values it ran). This module carries the solver's
//! half from the content thread to the cards whatever Show gizmos says
//! ([`SetupFitObservations`], published as `ContentState::setup_fits`), puts
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
use manifold_core::liquid_domain::liquid_domains_in;
use manifold_core::liquid_setup::SetupFit;
use manifold_core::project::Project;
use manifold_core::{LayerId, NodeId};
use manifold_nodes_scene::node_graph::scene_vm::{RuntimeFit, SceneVm, SetupProblem, problems_with_runtime};
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

/// Every such domain on the layers that rendered: the content thread's
/// report to the cards. Empty while every setup fits.
pub(crate) type SetupFitReport = Arc<[SetupFitEntry]>;

/// Content-thread side of the report. The buffers are reused; a new report
/// is allocated only when an entry changes.
pub(crate) struct SetupFitObservations {
    domains: Vec<(NodeId, FluidDomainSnapshot)>,
    scratch: Vec<SetupFitEntry>,
    pub report: SetupFitReport,
}

impl Default for SetupFitObservations {
    fn default() -> Self {
        Self { domains: Vec::new(), scratch: Vec::new(), report: Arc::from([]) }
    }
}

/// Hands each rendered layer's domain snapshots to the callback, refilling
/// the buffer it is given.
pub(crate) type EachRenderedDomains<'a> = &'a mut dyn FnMut(&LayerId, &[(NodeId, FluidDomainSnapshot)]);

impl SetupFitObservations {
    /// Take this frame's report from `observe`, which walks the layers that
    /// rendered (`GeneratorRenderer::each_rendered_fluid_domains`).
    pub fn record(&mut self, observe: impl FnOnce(&mut Vec<(NodeId, FluidDomainSnapshot)>, EachRenderedDomains<'_>)) {
        self.scratch.clear();
        let scratch = &mut self.scratch;
        observe(&mut self.domains, &mut |layer, domains| {
            for (node, snapshot) in domains {
                if !snapshot.fit.is_clean() || snapshot.refusal.is_some() {
                    let refusal = snapshot.refusal.clone();
                    scratch.push(SetupFitEntry { layer: layer.clone(), node: node.clone(), fit: snapshot.fit, refusal });
                }
            }
        });
        // Layers arrive in hash order: sort, so an unchanged report is equal.
        self.scratch.sort_unstable_by(|a, b| (a.layer.as_str(), a.node.as_str()).cmp(&(b.layer.as_str(), b.node.as_str())));
        if self.scratch.as_slice() != &*self.report {
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

/// What can move a shape, a card value or which graph a layer resolves.
const CHECK_PARTS: ChangeSet = ChangeSet::of(&[Part::Layers, Part::Effects, Part::Overrides, Part::Shapes, Part::EmbeddedPresets, Part::Values]);

/// What the cards' list was built from: the check's stamps, the catalog
/// generation, the two layers and the solver's report.
type CardKey = (u64, u64, [Option<LayerId>; 2], SetupFitReport);

/// The cards' setup check as a frame-planner input (UI_INVALIDATION_DESIGN.md
/// D6): a hash of the problem list the active layer's card and the Scene
/// panel's card show. A knob scrub that leaves the list alone, or one on
/// another layer, leaves the hash alone, so live values still update in
/// place (D22).
#[derive(Default)]
pub(crate) struct CardCheck {
    key: Option<CardKey>,
    hash: u64,
}

impl CardCheck {
    /// The input this frame. The list is rebuilt only when what it reads
    /// moved; 0 while neither layer has a problem (a layer with no liquid
    /// domain builds nothing).
    pub fn input(&mut self, project: &Project, parts: PartStamps, layers: [Option<&LayerId>; 2], report: &SetupFitReport) -> u64 {
        let stamps = parts.key(CHECK_PARTS);
        let generation = manifold_node_engine::load::preset_loader::catalog_generation();
        if let Some((cached, cached_generation, cached_layers, cached_report)) = &self.key
            && *cached == stamps
            && *cached_generation == generation
            && cached_layers[0].as_ref() == layers[0]
            && cached_layers[1].as_ref() == layers[1]
            && Arc::ptr_eq(cached_report, report)
        {
            return self.hash;
        }
        let mut hasher = DefaultHasher::new();
        let mut any = false;
        for (slot, layer) in layers.iter().enumerate() {
            let Some((_, layer)) = layer.and_then(|id| project.timeline.find_layer_by_id(id)) else { continue };
            let problems = with_layer_def(layer, |def, inst| problems_of(def, inst, runtime_fits(report, &layer.layer_id)));
            let Some(problems) = problems.filter(|problems| !problems.is_empty()) else { continue };
            any = true;
            slot.hash(&mut hasher);
            for problem in &problems {
                problem.hash_card(&mut hasher);
            }
        }
        self.hash = if any { hasher.finish() } else { 0 };
        self.key = Some((stamps, generation, layers.map(Option::<&LayerId>::cloned), report.clone()));
        self.hash
    }
}

/// A gizmo host's setup problems, for its red boxes: the shown scene's
/// authored list plus what the shown frame's solvers reported. Rebuilt only
/// when the scene or the domains change, compared by pointer (both are
/// re-allocated only on change), so a steady frame allocates nothing.
#[derive(Default)]
pub(crate) struct GizmoProblems {
    key: Option<(Arc<SceneVm>, Option<SharedDomains>)>,
    problems: Vec<SetupProblem>,
}

impl GizmoProblems {
    /// Follow the shown scene and domains. True when the list was rebuilt.
    pub fn update(&mut self, scene: Option<&Arc<SceneVm>>, domains: Option<&SharedDomains>) -> bool {
        let Some(scene) = scene else {
            let rebuilt = self.key.take().is_some();
            self.problems.clear();
            return rebuilt;
        };
        if let Some((cached, cached_domains)) = &self.key
            && Arc::ptr_eq(cached, scene)
            && match (cached_domains, domains) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
        {
            return false;
        }
        let runtime = domains.map_or(&[][..], |domains| &domains[..]);
        problems_with_runtime(&scene.setup_anchors, &scene.problems, runtime.iter().map(RuntimeFit::of), &mut self.problems);
        self.key = Some((scene.clone(), domains.cloned()));
        true
    }

    pub fn problems(&self) -> &[SetupProblem] {
        &self.problems
    }
}
