//! Dispatch resolvers and dual-edit helpers shared across the `dispatch/`
//! domain modules (UI_FUNNEL_DECOMPOSITION P-B). Not a handler domain itself
//! — no `PanelAction` match arms live here, so `dispatch_inspector`'s chain
//! never calls into this module and `dispatch_chain_completeness` exempts it
//! by name. Moved verbatim from `inspector.rs`.

use manifold_core::effects::{ParamEnvelope, ParameterDriver, PresetInstance};
use manifold_core::project::Project;
use manifold_core::LayerId;
use manifold_ui::{GraphParamTarget, InspectorTab};

use super::super::DispatchResult;
use crate::app::SelectionState;
use crate::ui_root::UIRoot;

/// Apply `edit` to the envelope matched by `param_id` on `target`, in both the
/// local UI project and the content thread (the next snapshot must not stomp
/// the live tweak). Edits the existing envelope only — no create. The unified
/// non-undoable live-drag envelope helper, for effects and generators alike:
/// the kind fork lives entirely inside `with_preset_graph_mut`.
pub(crate) fn graph_env_dual_edit<F>(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    target: &manifold_core::GraphTarget,
    param_id: manifold_core::effects::ParamId,
    edit: F,
) where
    F: Fn(&mut ParamEnvelope) + Clone + Send + 'static,
{
    use crate::content_command::ContentCommand;
    project.with_preset_graph_mut(target, |inst| {
        if let Some(env) = inst
            .envelopes
            .as_mut()
            .and_then(|es| es.iter_mut().find(|e| e.param_id == param_id))
        {
            edit(env);
        }
    });
    let edit2 = edit.clone();
    let pid = param_id.clone();
    let t = target.clone();
    ContentCommand::send(
        content_tx,
        ContentCommand::MutateProjectLive(Box::new(move |p| {
            p.with_preset_graph_mut(&t, |inst| {
                if let Some(env) = inst
                    .envelopes
                    .as_mut()
                    .and_then(|es| es.iter_mut().find(|e| e.param_id == pid))
                {
                    edit2(env);
                }
            });
        })),
    );
}

/// Driver twin of [`graph_env_dual_edit`]: apply `edit` to the driver matched
/// by `param_id` on `target`, locally and on the content thread.
pub(crate) fn graph_driver_dual_edit<F>(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    target: &manifold_core::GraphTarget,
    param_id: manifold_core::effects::ParamId,
    edit: F,
) where
    F: Fn(&mut ParameterDriver) + Clone + Send + 'static,
{
    use crate::content_command::ContentCommand;
    project.with_preset_graph_mut(target, |inst| {
        if let Some(driver) = inst
            .drivers
            .as_mut()
            .and_then(|ds| ds.iter_mut().find(|d| d.param_id == param_id))
        {
            edit(driver);
        }
    });
    let edit2 = edit.clone();
    let pid = param_id.clone();
    let t = target.clone();
    ContentCommand::send(
        content_tx,
        ContentCommand::MutateProjectLive(Box::new(move |p| {
            p.with_preset_graph_mut(&t, |inst| {
                if let Some(driver) = inst
                    .drivers
                    .as_mut()
                    .and_then(|ds| ds.iter_mut().find(|d| d.param_id == pid))
                {
                    edit2(driver);
                }
            });
        })),
    );
}

/// Live dual-edit of one audio modulation, mirroring [`graph_driver_dual_edit`]
/// for the green trim-handle drag: applies `edit` to the matching
/// `ParameterAudioMod` on both the UI-thread project mirror and (via
/// `MutateProjectLive`) the content thread, so the handle tracks under the
/// cursor without an undo entry per frame (the undo command lands on commit).
pub(crate) fn graph_audio_mod_dual_edit<F>(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    target: &manifold_core::GraphTarget,
    param_id: manifold_core::effects::ParamId,
    edit: F,
) where
    F: Fn(&mut manifold_core::audio_mod::ParameterAudioMod) + Clone + Send + 'static,
{
    use crate::content_command::ContentCommand;
    project.with_preset_graph_mut(target, |inst| {
        if let Some(m) = inst
            .audio_mods
            .as_mut()
            .and_then(|ms| ms.iter_mut().find(|a| a.param_id == param_id))
        {
            edit(m);
        }
    });
    let edit2 = edit.clone();
    let pid = param_id.clone();
    let t = target.clone();
    ContentCommand::send(
        content_tx,
        ContentCommand::MutateProjectLive(Box::new(move |p| {
            p.with_preset_graph_mut(&t, |inst| {
                if let Some(m) = inst
                    .audio_mods
                    .as_mut()
                    .and_then(|ms| ms.iter_mut().find(|a| a.param_id == pid))
                {
                    edit2(m);
                }
            });
        })),
    );
}

/// Live (non-undo) edit of one `LayerClipTrigger`'s shape, mirroring
/// `graph_audio_mod_dual_edit` — applies `edit` to the UI-side `project`
/// snapshot immediately AND queues the same edit onto the content thread's
/// live project via `MutateProjectLive`, so a drag reads back correctly on
/// both sides without an undo entry per frame (the commit, on drag-end,
/// records the one undo step).
pub(crate) fn clip_trigger_shape_dual_edit<F>(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    layer_id: &LayerId,
    index: usize,
    edit: F,
) where
    F: Fn(&mut manifold_core::audio_mod::AudioModShape) + Clone + Send + 'static,
{
    use crate::content_command::ContentCommand;
    if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id)
        && let Some(t) = layer.clip_triggers.get_mut(index)
    {
        edit(&mut t.shape);
    }
    let edit2 = edit.clone();
    let lid = layer_id.clone();
    ContentCommand::send(
        content_tx,
        ContentCommand::MutateProjectLive(Box::new(move |p| {
            if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&lid)
                && let Some(t) = layer.clip_triggers.get_mut(index)
            {
                edit2(&mut t.shape);
            }
        })),
    );
}

/// Resolve a `GraphParamTarget` (the card's effect-row index or generator
/// marker) to a stable `GraphTarget`, for routing through
/// `Project::with_preset_graph_mut` and the GraphTarget-keyed editing
/// commands. The single resolver behind every collapsed param/modulation
/// dispatch arm: effects address by stable `EffectId` (editor-aware via
/// `resolve_effect_id`), generators by the active layer's `LayerId`.
pub(crate) fn resolve_graph_target(
    gpt: &GraphParamTarget,
    editor_target: Option<&manifold_core::GraphTarget>,
    tab: InspectorTab,
    active_layer: &Option<LayerId>,
    selection: &SelectionState,
    project: &Project,
) -> Option<manifold_core::GraphTarget> {
    match gpt {
        GraphParamTarget::Effect(idx) => {
            super::resolve_effect_id(editor_target, tab, active_layer, selection, project, *idx)
                .map(manifold_core::GraphTarget::Effect)
        }
        GraphParamTarget::Generator => {
            let layer_idx = super::resolve_active_layer_index(active_layer, project)?;
            let lid = project.timeline.layers.get(layer_idx)?.layer_id.clone();
            Some(manifold_core::GraphTarget::Generator(lid))
        }
        // BUG-292: resolve by the CARRIED layer id, never `active_layer` —
        // the scene panel's rows must land on the panel's own bound layer
        // even when it isn't the app's active layer.
        GraphParamTarget::GeneratorOf(lid) => {
            project.timeline.find_layer_index_by_id(lid.as_str())?;
            Some(manifold_core::GraphTarget::Generator(lid.clone()))
        }
    }
}

/// Resolve a modulation-family action's `(target, param_id)` — the ONE
/// funnel every driver/envelope/audio-mod dispatch arm in this file uses.
/// Historically (BUG-249) this also re-resolved a scene-panel row's
/// synthesized id to the real exposed param bound to its inner node,
/// materializing the exposure on first arm if needed — scene rows now carry
/// real exposed param ids directly (P2 slice 2b,
/// `SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md`), so that re-resolution is
/// gone and this is exactly `resolve_graph_target` plus the unchanged
/// `param_id`. `_ui`/`_content_tx`/`_materialize` stay in the signature so
/// the ~20 call sites this funnel serves don't need touching if a future row
/// kind needs the same re-resolution seam again.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_mod_target(
    _ui: &UIRoot,
    project: &mut Project,
    _content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    gpt: &GraphParamTarget,
    param_id: &manifold_core::effects::ParamId,
    editor_target: Option<&manifold_core::GraphTarget>,
    effective_tab: InspectorTab,
    active_layer: &Option<LayerId>,
    selection: &SelectionState,
    _materialize: bool,
) -> Option<(manifold_core::GraphTarget, manifold_core::effects::ParamId)> {
    let target = resolve_graph_target(gpt, editor_target, effective_tab, active_layer, selection, project)?;
    Some((target, param_id.clone()))
}

/// The `AbletonMappingTarget` for a resolved param `target` on `tab`, so the
/// Ableton trim/invert arms route through the shared
/// `Project::ableton_param_mappings_mut` locate-fork (effects addressed by
/// `effect_type` within master/layer — first match — generators by layer).
/// `None` for the clip tab (no clip-scoped Ableton mappings).
pub(crate) fn ableton_mapping_target(
    target: &manifold_core::GraphTarget,
    tab: InspectorTab,
    active_layer: &Option<LayerId>,
    project: &Project,
    param_id: &manifold_core::effects::ParamId,
) -> Option<manifold_core::ableton_mapping::AbletonMappingTarget> {
    use manifold_core::ableton_mapping::AbletonMappingTarget as T;
    match target {
        manifold_core::GraphTarget::Effect(eid) => {
            let effect_type = project.find_effect_by_id(eid)?.effect_type().clone();
            match tab {
                InspectorTab::Master => Some(T::MasterEffect {
                    effect_type,
                    param_id: param_id.clone(),
                }),
                InspectorTab::Layer | InspectorTab::Group => Some(T::LayerEffect {
                    layer_id: active_layer.clone()?,
                    effect_type,
                    param_id: param_id.clone(),
                }),
                InspectorTab::Clip => None,
            }
        }
        manifold_core::GraphTarget::Generator(lid) => Some(T::GenParam {
            layer_id: lid.clone(),
            param_id: param_id.clone(),
        }),
    }
}

/// The `MacroMappingTarget` for a resolved param `target` (macro twin of
/// [`ableton_mapping_target`]). Effects address by stable `EffectId`, which
/// reaches master / layer / clip effects directly — so the macro drives the
/// exact instance the user mapped, even with two same-type effects on a layer.
pub(crate) fn macro_mapping_target(
    target: &manifold_core::GraphTarget,
    param_id: &manifold_core::effects::ParamId,
) -> Option<manifold_core::MacroMappingTarget> {
    use manifold_core::MacroMappingTarget as T;
    match target {
        manifold_core::GraphTarget::Effect(eid) => Some(T::Effect {
            effect_id: eid.clone(),
            param_id: param_id.clone(),
        }),
        manifold_core::GraphTarget::Generator(lid) => Some(T::GenParam {
            layer_id: lid.clone(),
            param_id: param_id.clone(),
        }),
    }
}

/// The `(min, max)` range for `param_id` on `inst`, graph-authority-first:
/// the per-instance graph `preset_metadata` (where graph-backed presets —
/// notably generators — carry their authoritative ranges) takes precedence
/// over the registry, falling back to [`PresetInstance::resolve_param`] (and
/// `(0.0, 1.0)` if unresolved). Unifies the old per-kind range lookups.
pub(crate) fn resolve_param_range(inst: &PresetInstance, param_id: &str) -> (f32, f32) {
    // The manifest entry is the single range authority: calibration edits
    // `spec.min`/`spec.max` in place (D6), so the old override-then-catalog
    // lookup collapses to reading the `Param.spec`.
    inst.params
        .get(param_id)
        .map(|p| (p.spec.min, p.spec.max))
        .unwrap_or((0.0, 1.0))
}

/// The preset graph def to fork or export for a resolved `target`: the
/// per-instance diverged graph if the instance carries one, else the catalog
/// canonical def from the loaded preset view. Paired with the current preset
/// id (the export filename stem). One path for both kinds — the fork / export
/// dispatch arms resolve a `GraphTarget` (effect or generator) and hand it
/// here, so make-unique / export behave identically on either card.
pub(crate) fn preset_source_def(
    target: &manifold_core::GraphTarget,
    project: &Project,
) -> Option<(
    manifold_core::effect_graph_def::EffectGraphDef,
    manifold_core::PresetTypeId,
)> {
    let inst = project.preset_instance(target)?;
    let preset_id = inst.effect_type().clone();
    let mut def = inst.graph.clone().or_else(|| {
        manifold_renderer::node_graph::loaded_preset_view_by_id(&preset_id)
            .map(|v| (*v.canonical_def).clone())
    })?;
    // Snapshot the card's current slider values into the def's defaults so Make
    // Unique / Export freeze the configured look rather than the stock template.
    // The live values stay on the instance (the performance surface); this only
    // makes the def reproduce them on a later add/import/load.
    inst.snapshot_values_into_def(&mut def);
    Some((def, preset_id))
}

/// Apply a project-level audio-setup command locally and forward it to the
/// content thread. The shared tail for every Audio Setup action (they all
/// mutate `project.audio_setup` and need no target resolution).
pub(crate) fn audio_setup_command(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    mut cmd: Box<dyn manifold_editing::command::Command + Send>,
) -> DispatchResult {
    cmd.execute(project);
    crate::content_command::ContentCommand::send(
        content_tx,
        crate::content_command::ContentCommand::Execute(cmd),
    );
    DispatchResult::structural()
}
