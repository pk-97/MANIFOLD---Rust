//! The setup check on the cards (SCENE_SETUP_PROGRAMME.md section 3 B3).
//! The check itself is `SceneVm::problems` (declared shapes at the values the
//! knobs show) plus `problems_with_runtime` (what the solver's fit did to
//! values a wire or modulator drives); this module only hands that list to
//! the two readers that live in the app: a warning line on the card row that
//! edits the shape ([`mark_rows`]), and the planner input that moves when
//! either half does ([`input_key`]). The viewport's red outline reads the same list through
//! `GizmoChrome::problems`.

use std::hash::{DefaultHasher, Hash, Hasher};

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::PresetInstance;
use manifold_core::fluid_domain::FluidDomainSnapshot;
use manifold_core::layer::Layer;
use manifold_core::change::{ChangeSet, Part, PartStamps};
use manifold_core::{LayerId, NodeId};
use manifold_core::liquid_domain::liquid_domains_in;
use manifold_nodes_scene::node_graph::scene_vm::{SceneVm, SetupProblem, problems_with_runtime};
use manifold_ui::param_surface::{ParamRow, ParamSurface};

use crate::physics_overlay::{PhysicsOverlayFrames, SharedDomains};

/// The problems of the scene `def` is, with `inst`'s card values folded into
/// the params they drive so shapes sit where the knobs put them, plus what
/// the solver reported for `domains`. Empty for a graph with no liquid
/// domain, which builds no scene model at all.
pub(crate) fn problems_of(def: &EffectGraphDef, inst: &PresetInstance, domains: &[(NodeId, FluidDomainSnapshot)]) -> Vec<SetupProblem> {
    if liquid_domains_in(&def.nodes).is_empty() {
        return Vec::new();
    }
    let mut authored = def.clone();
    if crate::scene_gizmo::fold_card_values(&mut authored, inst).is_none() {
        authored = def.clone();
    }
    let Some(vm) = SceneVm::from_def(&authored) else { return Vec::new() };
    let mut out = Vec::new();
    problems_with_runtime(&vm.setup_anchors, &vm.problems, domains, &mut out);
    out
}

/// Mark a generator layer's card: its own graph, else the catalog's.
pub(crate) fn mark_layer_card(card: &mut ParamSurface, layer: &Layer, domains: &[(NodeId, FluidDomainSnapshot)]) {
    let Some(inst) = layer.gen_params() else { return };
    let bundled;
    let def: &EffectGraphDef = match layer.generator_graph() {
        Some(def) => def,
        None => match manifold_nodes::bundled_presets::bundled_preset_def(layer.generator_type()) {
            Some(def) => {
                bundled = def;
                &bundled
            }
            None => return,
        },
    };
    mark_rows(&mut card.rows, &problems_of(def, inst, domains), def);
}

/// The domains the newest frame of `layer` reported; empty before one.
pub(crate) fn runtime_domains<'a>(runtime: &'a [(LayerId, SharedDomains)], layer: &LayerId) -> &'a [(NodeId, FluidDomainSnapshot)] {
    runtime.iter().rev().find(|(id, _)| id == layer).map_or(&[][..], |(_, domains)| &domains[..])
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

const CHECK_PARTS: ChangeSet = ChangeSet::of(&[Part::Layers, Part::Effects, Part::Overrides, Part::Shapes, Part::EmbeddedPresets, Part::Values]);

/// The frame planner's input: everything the check reads. The authored half
/// is the graph and value stamps (any edit that can move a shape moves one);
/// the run-time half is what the solver reported for the two layers' newest
/// frames. Nothing is rebuilt to compute it.
pub(crate) fn input_key(parts: PartStamps, layers: [Option<&LayerId>; 2], frames: &PhysicsOverlayFrames) -> u64 {
    let mut hasher = DefaultHasher::new();
    parts.key(CHECK_PARTS).hash(&mut hasher);
    for layer in layers {
        layer.hash(&mut hasher);
        let Some(layer) = layer else { continue };
        for (_, snapshot) in runtime_domains_of(frames, layer) {
            snapshot.fit.hash(&mut hasher);
            snapshot.refusal.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn runtime_domains_of<'a>(frames: &'a PhysicsOverlayFrames, layer: &LayerId) -> &'a [(NodeId, FluidDomainSnapshot)] {
    frames
        .iter()
        .flatten()
        .filter(|frame| &frame.layer == layer)
        .max_by_key(|frame| frame.frame)
        .map_or(&[][..], |frame| &frame.domains[..])
}
