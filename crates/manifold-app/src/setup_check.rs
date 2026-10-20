//! The setup check on the cards (SCENE_SETUP_PROGRAMME.md section 3 B3).
//! The check itself is `SceneVm::problems`, read from the declared shapes at
//! the values the knobs show; this module only hands that list to the two
//! readers that live in the app: a warning line on the card row that edits
//! the shape ([`mark_rows`]), and the planner input that moves when the list
//! does ([`key`]). The viewport's red outline reads the same list through
//! `GizmoChrome::problems`.

use std::hash::{DefaultHasher, Hash, Hasher};

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::PresetInstance;
use manifold_core::layer::Layer;
use manifold_core::liquid_domain::liquid_domains_in;
use manifold_nodes_scene::node_graph::scene_vm::{SceneVm, SetupProblem};
use manifold_ui::param_surface::ParamRow;

/// The problems of the scene `def` is, with `inst`'s card values folded into
/// the params they drive so shapes sit where the knobs put them. Empty for
/// a graph with no liquid domain, which builds no scene model at all.
pub(crate) fn problems_of(def: &EffectGraphDef, inst: &PresetInstance) -> Vec<SetupProblem> {
    if liquid_domains_in(&def.nodes).is_empty() {
        return Vec::new();
    }
    let mut authored = def.clone();
    if crate::scene_gizmo::fold_card_values(&mut authored, inst).is_none() {
        authored = def.clone();
    }
    SceneVm::from_def(&authored).map(|vm| vm.problems).unwrap_or_default()
}

/// A generator layer's problems: its own graph, else the catalog's.
pub(crate) fn layer_problems(layer: &Layer) -> Vec<SetupProblem> {
    let Some(inst) = layer.gen_params() else { return Vec::new() };
    match layer.generator_graph() {
        Some(def) => problems_of(def, inst),
        None => manifold_nodes::bundled_presets::bundled_preset_def(layer.generator_type())
            .map(|def| problems_of(&def, inst))
            .unwrap_or_default(),
    }
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

/// A number that moves when the list does: the frame planner's input, so a
/// knob drag that pushes a shape past a wall re-projects the cards.
pub(crate) fn key(problems: &[SetupProblem]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for problem in problems {
        problem.kind.message().hash(&mut hasher);
        for addr in &problem.params {
            (&addr.scope_path, addr.node_doc_id, &addr.param_id).hash(&mut hasher);
        }
    }
    hasher.finish()
}
