//! UI Bridge — connects panel actions to PlaybackEngine + EditingService.
//!
//! This module translates UI-emitted `PanelAction` values into engine
//! mutations. The app layer calls `dispatch()` after collecting actions
//! from all panels, and `push_state()` to sync engine state back to panels.

mod context;
mod dispatch;
pub(crate) use dispatch::resolve::preset_source_def;
pub(crate) use dispatch::resolve::resolve_graph_target;
mod editing;
mod inspector;
mod layer;
mod marker;
mod material_colour;
pub(crate) mod project;
mod projection;
pub(crate) use projection::preview::{project_clip_preview, project_lane_preview};
mod rack_controls;
#[cfg(test)]
pub(crate) use projection::scene::scene_state;
pub use projection::timeline::sync_automation_lane_order;
pub(crate) use projection::trigger_routing::modifier_label_for_param;
#[cfg(test)]
pub(crate) use projection::trigger_routing::tests::{duplicate_force_project, force_project};
pub(crate) use projection::trigger_routing::{
    TriggerRoutingCatalog, TriggerSourceChoice, TriggerTargetChoice,
};
// `pub(crate)` so the frame-resident gestures in `app_render` (graph-editor
// mapping drags, graph-canvas node-param drags) can name `ResolvedScrub`
// directly — they open the one `ScrubState.active` slot without going through
// the `PanelAction::Scrub` wire (P-I, Fork-2).
pub(crate) mod scrub;
#[cfg(test)]
mod selection_dispatch_tests;
mod state_sync;
mod transport;

pub(crate) use context::{DispatchCtx, LocalProject, ProjectRef};
pub(crate) use scrub::ScrubState;
pub(crate) use state_sync::SceneImpulseToast;

use manifold_core::LayerId;
use manifold_core::project::Project;
use manifold_ui::{InspectorTab, PanelAction, RootAction};

use crate::app::SelectionState;
use crate::ui_root::UIRoot;

/// Result of dispatching a panel action.
pub struct DispatchResult {
    /// Set by the `SaveToLibrary`/`SaveToProject` card-menu actions
    ///: the resolved kind + current effective
    /// definition + destination, for the caller to open the shared
    /// name-prompt text-input session with. `dispatch_inspector` resolves the
    /// `GraphParamTarget` and the def (it already has `project` + the
    /// tab/selection context); only the CALLER (`app_render.rs`'s per-frame
    /// loop) has `self.text_input`, so the handoff rides here rather than
    /// `dispatch_inspector` reaching for UI-thread-only state it isn't given.
    pub begin_save_preset: Option<(
        manifold_core::preset_def::PresetKind,
        manifold_core::effect_graph_def::EffectGraphDef,
        crate::text_input::SavePresetDestination,
    )>,
    /// Set by `BrowserRenamePresetClicked`:
    /// the resolved kind + id + source + CURRENT display name, for the
    /// caller to open the shared name-prompt text-input session with — same
    /// handoff shape as `begin_save_preset` and for the same reason
    /// (`dispatch_inspector` has `project`/`ui` but not `self.text_input`).
    pub begin_rename_preset: Option<(
        manifold_core::preset_def::PresetKind,
        manifold_core::PresetTypeId,
        manifold_ui::panels::picker_core::Source,
        String,
    )>,
    /// Set by the Scene tab's "Mesh from file" Add entry and Model File swap:
    /// the picked model file and where it goes. Only the caller can start the
    /// import worker a drop uses, so the handoff rides here.
    pub import_model: Option<(std::path::PathBuf, crate::import_worker::ImportTarget)>,
}

impl DispatchResult {
    pub(crate) fn handled() -> Self {
        Self {
            begin_save_preset: None,
            begin_rename_preset: None,
            import_model: None,
        }
    }
    /// Distinction retired with the chain router (P-D); kept as call-site
    /// documentation for unclaimed variants — identical to `handled()`.
    pub(crate) fn unhandled() -> Self {
        Self::handled()
    }
}

/// Handle an inspector tab-strip click. A tab is a **view** over the live
/// timeline selection, not a selection change: clicking a rung pins the
/// inspector to that scope (Clip / Layer / Group / Master) and repoints the
/// inspector's layer focus (`active_layer`) at it, but never touches what's
/// selected — so the whole ownership chain stays available and the clip you
/// were on stays selected. The pin auto-clears on the next selection change.
fn inspector_select_tab(
    tab: InspectorTab,
    project: &Project,
    selection: &mut SelectionState,
    active_layer: &mut Option<LayerId>,
    ui: &mut UIRoot,
) {
    use manifold_core::types::LayerType;
    // The selection's own layer — the clip's layer, or the selected layer. The
    // tab repoints `active_layer` relative to this; the selection itself is
    // never changed.
    let sel_layer = selection
        .selected_layer_id_for_clip()
        .or(selection.primary_selected_layer_id())
        .cloned();
    match tab {
        // Master needs no layer focus; just pin the scope.
        InspectorTab::Master => selection.pin_scope(InspectorTab::Master),
        // Clip / Layer / Scene focus the selection's own layer (clip chrome +
        // gen params + that layer's effects, or that layer's scene).
        InspectorTab::Clip | InspectorTab::Layer | InspectorTab::Scene => {
            if let Some(lid) = sel_layer.or_else(|| active_layer.clone()) {
                *active_layer = Some(lid);
            }
            ui.inspector.clear_effect_selection(&mut ui.tree);
            selection.pin_scope(tab);
        }
        // Group focuses the group: the selection layer's group parent, or the
        // selection layer itself when it is already a group.
        InspectorTab::Group => {
            let gid = sel_layer
                .as_ref()
                .and_then(|lid| project.timeline.find_layer_index_by_id(lid))
                .and_then(|idx| project.timeline.find_group_parent(idx))
                .map(|(_, l)| l.layer_id.clone())
                .or_else(|| {
                    sel_layer
                        .as_ref()
                        .and_then(|lid| project.timeline.find_layer_by_id(lid))
                        .filter(|(_, l)| l.layer_type == LayerType::Group)
                        .map(|(_, l)| l.layer_id.clone())
                });
            if let Some(gid) = gid {
                *active_layer = Some(gid);
            }
            ui.inspector.clear_effect_selection(&mut ui.tree);
            selection.pin_scope(InspectorTab::Group);
        }
    }
}

/// Dispatch a panel action. Mutates local_project for immediate feedback;
/// sends commands to the content thread for authoritative execution.
pub fn dispatch(action: &PanelAction, ctx: &mut DispatchCtx) -> DispatchResult {
    // Flat sum routing (UI_FUNNEL_DECOMPOSITION P-D / D-D1): one arm per domain,
    // exhaustive over `PanelAction` — the compiler now PROVES routing totality,
    // which is why the former `dispatch_inspector` first-non-unhandled chain and
    // its `dispatch_chain_completeness` invariant were retired in this same
    // change (a second copy of the routing beside an exhaustive match is exactly
    // the parallel-old-path the design forbids). Each arm destructures the sum
    // ONCE and hands the domain value to its handler.
    match action {
        PanelAction::Transport(a) => transport::dispatch_transport(
            a,
            &mut ctx.project,
            ctx.content_tx,
            ctx.content_state,
            ctx.ui,
            ctx.selection,
        ),
        PanelAction::Editing(a) => editing::dispatch_editing(
            a,
            &mut ctx.project,
            ctx.content_tx,
            ctx.content_state,
            ctx.ui,
            ctx.selection,
            ctx.active_layer,
            ctx.user_prefs,
            ctx.dialogs,
        ),
        PanelAction::Layer(a) => layer::dispatch_layer(
            a,
            &mut ctx.project,
            ctx.content_tx,
            ctx.content_state,
            ctx.ui,
            ctx.selection,
            ctx.active_layer,
        ),
        PanelAction::Marker(a) => marker::dispatch_marker(
            a,
            &mut ctx.project,
            ctx.content_tx,
            ctx.ui,
            ctx.selection,
            ctx.scrub,
            ctx.content_state.data_version,
        ),
        PanelAction::Project(a) => project::dispatch_project(
            a,
            &mut ctx.project,
            ctx.content_tx,
            ctx.content_state,
            ctx.ui,
            ctx.selection,
            ctx.active_layer,
            ctx.user_prefs,
            ctx.dialogs,
        ),
        PanelAction::Browser(a) => dispatch::browser::dispatch_browser(a, ctx),
        PanelAction::Clip(a) => dispatch::clip::dispatch_clip(a, ctx),
        PanelAction::Params(a) => dispatch::params::dispatch_params(a, ctx),
        PanelAction::Modulation(a) => dispatch::modulation::dispatch_modulation(a, ctx),
        PanelAction::Mapping(a) => dispatch::mapping::dispatch_mapping(a, ctx),
        PanelAction::AudioSetup(a) => dispatch::audio_setup::dispatch_audio_setup(a, ctx),

        // ── Root: actions with no domain handler — inline terminal arms, moved
        // verbatim from the pre-decomposition inline match (bodies unchanged).
        PanelAction::Root(a) => match a {
            // Right-click slider reset: recurses into this same `dispatch` for
            // the value-change trio (BUG-061), so undo == a drag to the default.
            RootAction::SliderReset {
                snapshot,
                changed,
                commit,
            } => {
                dispatch(snapshot, ctx);
                dispatch(changed, ctx);
                dispatch(commit, ctx)
            }
            // Inspector tab strip — mirrors the timeline selection. The scope
            // pin and active layer are planner inputs.
            RootAction::SelectInspectorTab(tab) => {
                inspector_select_tab(*tab, &ctx.project, ctx.selection, ctx.active_layer, ctx.ui);
                DispatchResult::handled()
            }
            // Opened at the Application layer (app_render) / ui_root before
            // dispatch; these arms only keep the match exhaustive and no-op if
            // reached (e.g. via the headless script harness).
            RootAction::BeginParamTextInput { .. }
            | RootAction::BeginSliderRangeInput { .. }
            | RootAction::BeginColourTextInput { .. }
            | RootAction::OpenColourMenu { .. }
            | RootAction::BeginDriverPeriodTextInput { .. }
            | RootAction::ParamEnumDropdown { .. }
            | RootAction::OpenRtQualityTierDropdown { .. }
            | RootAction::OpenRtQualityResDropdown { .. }
            | RootAction::OpenRtQualityDenoiseDropdown { .. }
            | RootAction::SceneSetupActionMenuClicked { .. }
            | RootAction::AudioSendGainBeginTextInput(..)
            | RootAction::CopyOscAddress(_)
            | RootAction::OpenGraphEditor(_)
            | RootAction::OpenGraphTarget(_)
            | RootAction::OpenCardMapping { .. }
            | RootAction::OpenGeneratorGraphEditor
            | RootAction::SceneSetupCopyViewportCamera(..)
            | RootAction::SceneDrawAgain(..)
            | RootAction::SceneResetPreviewView
            | RootAction::EffectMappingRangeSnapshot { .. }
            | RootAction::EffectMappingRangeChanged { .. }
            | RootAction::EffectMappingRangeCommit { .. }
            | RootAction::EffectMappingLabel { .. }
            | RootAction::EffectMappingSection { .. }
            | RootAction::EffectMappingInvert { .. }
            | RootAction::EffectMappingCurve { .. }
            | RootAction::EffectMappingAffineSnapshot { .. }
            | RootAction::EffectMappingAffineChanged { .. }
            | RootAction::EffectMappingAffineCommit { .. }
            | RootAction::EffectMappingCancel { .. }
            | RootAction::EffectMappingGotoNode { .. }
            | RootAction::AudioSendLabelClicked(_)
            | RootAction::SceneRenameClicked(..)
            | RootAction::AudioSendClicked(_)
            | RootAction::AudioDrawerClick(..)
            | RootAction::ClipTriggerDrawerClick(..) => DispatchResult::handled(),
            // The panel already updated its UI-local selection; its selection
            // version rebuilds Properties this frame.
            RootAction::SceneItemRightClicked => DispatchResult::handled(),
            // The blocking native picker, as every other file field uses; the
            // picked path is one `SceneFieldSet`.
            RootAction::SceneFieldFileClicked {
                layer_id,
                item,
                field,
                extensions,
            } => {
                // A new Model File is a swap: the parts whose file node is
                // still there keep their edits.
                if field == "model_file" {
                    ctx.ui.pending_keyboard_actions.push(
                        manifold_ui::panels::PanelAction::Project(
                            manifold_ui::panels::ProjectAction::SceneSetupSwapModelFile(
                                layer_id.clone(),
                                item.clone(),
                            ),
                        ),
                    );
                    return DispatchResult::handled();
                }
                let picked = crate::file_dialogs::pick_file_of(
                    crate::file_dialogs::DialogAction::NodeFile,
                    ctx.dialogs,
                    ctx.user_prefs,
                    extensions,
                );
                match picked {
                    Some(path) => {
                        let pick = manifold_core::scene_surface::FieldPick::File(
                            path.to_string_lossy().into_owned(),
                        );
                        let built = crate::scene_edit::set_field(
                            &ctx.project,
                            layer_id,
                            item,
                            field,
                            &pick,
                        );
                        project::send_scene_edit(ctx.content_tx, layer_id, built)
                    }
                    None => DispatchResult::handled(),
                }
            }
            RootAction::SceneItemMove(delta) => {
                if let Some(item) = ctx.ui.scene_setup_panel.selected_scene_item() {
                    let built =
                        crate::scene_edit::nudge(&ctx.project, &item.layer_id, &item.item, *delta);
                    return project::send_scene_edit(ctx.content_tx, &item.layer_id, built);
                }
                DispatchResult::handled()
            }
            RootAction::SceneItemRename | RootAction::SceneItemFrame => {
                let action = if matches!(a, RootAction::SceneItemRename) {
                    ctx.ui.scene_setup_panel.rename_selection_action()
                } else {
                    ctx.ui.scene_setup_panel.frame_selection_action()
                };
                // The queued action's own dispatch decides its work.
                if let Some(action) = action {
                    ctx.ui.pending_keyboard_actions.push(action);
                }
                DispatchResult::handled()
            }
            RootAction::SceneSetupSelectionChanged(_) => DispatchResult::handled(),
            // Audio Setup dock toggle (headless-harness path; the live app
            // toggles in app_render). The dock's view version moves the
            // rebuild and its device query.
            RootAction::OpenAudioSetup => {
                ctx.ui.toggle_audio_dock();
                DispatchResult::handled()
            }
            // Menu, header button, the panel's close button and Escape all
            // toggle the inspector's Scene tab (SCENE_SURFACES D16).
            RootAction::OpenSceneSetup => {
                let tab = if ctx.ui.inspector.active_tab() == InspectorTab::Scene {
                    InspectorTab::Layer
                } else {
                    InspectorTab::Scene
                };
                inspector_select_tab(tab, &ctx.project, ctx.selection, ctx.active_layer, ctx.ui);
                DispatchResult::handled()
            }
            // RT Quality overlay (headless-harness path; the live app opens it
            // in app_render's PanelAction loop). The overlay open set is a
            // planner input.
            RootAction::OpenRtQuality => {
                ctx.ui.rt_quality_panel.open();
                ctx.ui.mark_overlay_dirty();
                DispatchResult::handled()
            }
            // Dropdown/picker opens intercepted earlier in ui_root/app_render.
            // The pre-decomposition router routed these through the inspector
            // chain, which no handler claimed, so they returned `unhandled()`;
            // preserve that exactly.
            RootAction::AudioSetupDeviceClicked | RootAction::OpenAbletonPickerForParam(..) => {
                DispatchResult::unhandled()
            }
        },

        // ── Scrub: the unified value-scrub gesture (P-I / D4). One handler for
        // every ported family, addressed by `ValueRef`, phased Begin/Move/Commit.
        PanelAction::Scrub(value_ref, phase) => scrub::dispatch_scrub(value_ref, phase, ctx),
    }
}

/// Shift+Click region selection with correct anchor precedence.
pub(crate) fn select_region_to_with_project(
    target_beat: manifold_core::Beats,
    target_layer: usize,
    selection: &mut SelectionState,
    project: &Project,
) {
    let layer_count = project.timeline.layers.len();
    if layer_count == 0 {
        return;
    }

    // A cursor or region whose layers were deleted anchors nothing, never layer 0.
    let anchor: Option<(manifold_core::Beats, usize)> = if selection.has_insert_cursor() {
        selection
            .insert_cursor_layer_id()
            .as_ref()
            .and_then(|id| project.timeline.find_layer_index_by_id(id))
            .map(|anchor_idx| {
                (
                    selection
                        .insert_cursor_beat()
                        .unwrap_or(manifold_core::Beats::ZERO),
                    anchor_idx,
                )
            })
    } else if let Some(r) = selection.current_region() {
        r.layer_index_range(&crate::ui_translate::layers_to_ui(&project.timeline.layers))
            .map(|(lo, _)| (r.start_beat, lo))
    } else if let Some(ref clip_id) = selection.primary_selected_clip_id().cloned() {
        project
            .timeline
            .layers
            .iter()
            .enumerate()
            .find_map(|(li, l)| {
                l.clips
                    .iter()
                    .find(|c| c.id == *clip_id)
                    .map(|_c| (_c.start_beat, li))
            })
    } else {
        None
    };

    match anchor {
        Some((anchor_beat, anchor_layer)) => {
            let min_beat = anchor_beat.min(target_beat);
            let max_beat = anchor_beat.max(target_beat);
            let min_layer = anchor_layer.min(target_layer).min(layer_count - 1) as i32;
            let max_layer = anchor_layer.max(target_layer).min(layer_count - 1) as i32;
            selection.set_region(
                min_beat,
                max_beat,
                min_layer,
                max_layer,
                &crate::ui_translate::layers_to_ui(&project.timeline.layers),
            );
        }
        None => {
            let lid = project
                .timeline
                .layers
                .get(target_layer)
                .map(|l| l.layer_id.clone())
                .unwrap_or_default();
            selection.set_insert_cursor(target_beat, lid);
        }
    }
}

/// Shift-click on a CLIP is a
/// contiguous whole-clip range selection, NOT a time-range region — the
/// `select_region_to_with_project` call this arm used to make was S1/S3's
/// root. This is the project-based twin of `interaction_overlay::
/// select_clip_range_to` (manifold-ui), which does the identical thing
/// against a `&dyn TimelineEditingHost` for the live gesture path; both must
/// move together (`select_region_to_with_project` stays for the empty-lane
/// shift path, unaffected). Extends from the current `Clips` selection's
/// anchor to `target_clip_id`, selecting every WHOLE clip on the anchor's
/// layer whose start beat falls between the anchor and the target,
/// inclusive — a gap between clips simply isn't a clip, so nothing
/// synthesizes a region there. No live anchor falls back to a plain
/// single-clip select.
pub(crate) fn select_clip_range_to_with_project(
    target_clip_id: &manifold_core::ClipId,
    selection: &mut SelectionState,
    project: &Project,
) {
    let find = |id: &manifold_core::ClipId| -> Option<(usize, LayerId, manifold_core::Beats)> {
        project
            .timeline
            .layers
            .iter()
            .enumerate()
            .find_map(|(li, l)| {
                l.clips
                    .iter()
                    .find(|c| c.id == *id)
                    .map(|c| (li, l.layer_id.clone(), c.start_beat))
            })
    };

    let Some((_, target_layer_id, target_start)) = find(target_clip_id) else {
        return; // clip vanished under us
    };

    let anchor_id = selection.clip_selection_anchor();
    let anchor_lookup = anchor_id.as_ref().and_then(find);

    let Some((anchor_layer_idx, _, anchor_start)) = anchor_lookup else {
        // No live anchor to extend from — behaves like a plain click on the target.
        selection.select_clip(target_clip_id.clone(), target_layer_id);
        return;
    };

    let min_beat = anchor_start.min(target_start);
    let max_beat = anchor_start.max(target_start);

    let ids: std::collections::HashSet<manifold_core::ClipId> = project.timeline.layers
        [anchor_layer_idx]
        .clips
        .iter()
        .filter(|c| c.start_beat >= min_beat && c.start_beat <= max_beat)
        .map(|c| c.id.clone())
        .collect();

    selection.set_clip_range(
        ids,
        anchor_id.expect("anchor_lookup Some implies anchor_id Some"),
        target_clip_id.clone(),
        target_layer_id,
    );
}

/// Paste the clip clipboard, select what landed, and say when clips were
/// skipped for a layer-type mismatch — the one receive path for every paste.
pub(crate) fn paste_clips_and_select(
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    target_beat: manifold_core::Beats,
    target_layer: i32,
    selection: &mut SelectionState,
    toast: &mut manifold_ui::panels::toast::ToastPanel,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    crate::content_command::ContentCommand::send(
        content_tx,
        crate::content_command::ContentCommand::PasteClips {
            target_beat,
            target_layer,
            result_tx: tx,
        },
    );
    let Ok((pasted_ids, skip_reason)) = rx.recv_timeout(std::time::Duration::from_millis(100))
    else {
        return;
    };
    if !pasted_ids.is_empty() {
        selection.select_clips(pasted_ids);
    }
    if let Some(message) = skip_reason {
        toast.show(message);
    }
}

/// Handle undo (called from keyboard shortcut). Sends to content thread.
pub fn undo(content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>) {
    crate::content_command::ContentCommand::send(
        content_tx,
        crate::content_command::ContentCommand::Undo,
    );
}

/// Handle redo (called from keyboard shortcut). Sends to content thread.
pub fn redo(content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>) {
    crate::content_command::ContentCommand::send(
        content_tx,
        crate::content_command::ContentCommand::Redo,
    );
}

// ── Effect tab routing helpers ───────────────────────────────────

/// Resolve the active layer index from an active LayerId.
pub(crate) fn resolve_active_layer_index(
    active_layer: &Option<LayerId>,
    project: &Project,
) -> Option<usize> {
    active_layer
        .as_ref()
        .and_then(|id| project.timeline.find_layer_index_by_id(id))
}

/// Resolve a card's `EffectId` address for dispatch. The editor's watched
/// effect wins outright (it IS the card); otherwise the id is looked up across
/// master, layer and clip effects, so a stale id (effect deleted mid-gesture)
/// resolves to nothing instead of another effect.
pub(crate) fn resolve_effect_by_id(
    editor_target: Option<&manifold_core::GraphTarget>,
    project: &Project,
    id: &manifold_core::EffectId,
) -> Option<manifold_core::EffectId> {
    if let Some(manifold_core::GraphTarget::Effect(eid)) = editor_target {
        return Some(eid.clone());
    }
    project.find_effect_by_id(id).map(|fx| fx.id.clone())
}

/// The layer a dispatch resolves against: the graph editor's watched target
/// names its own layer, otherwise the main window's active layer.
///
/// - **Generator** target → the generator's layer.
/// - **Effect** target → the effect's owning layer, or `None` for a master
///   effect. A clip-scoped or unresolved effect keeps the main window's layer.
/// - **`None`** (inspector / perform path) → the main window's active layer.
pub(crate) fn editor_active_layer(
    editor_target: Option<&manifold_core::GraphTarget>,
    project: &Project,
    active_layer: &Option<LayerId>,
) -> Option<LayerId> {
    match editor_target.and_then(manifold_core::GraphTarget::host_target) {
        Some(manifold_core::GraphTarget::Generator(lid)) => Some(lid.clone()),
        Some(manifold_core::GraphTarget::Effect(eid)) => {
            if project
                .settings
                .master_effects
                .iter()
                .any(|fx| &fx.id == eid)
            {
                None
            } else if let Some(lid) = project.timeline.layers.iter().find_map(|l| {
                l.effects
                    .as_deref()
                    .filter(|e| e.iter().any(|fx| &fx.id == eid))
                    .map(|_| l.layer_id.clone())
            }) {
                Some(lid)
            } else {
                active_layer.clone()
            }
        }
        Some(manifold_core::GraphTarget::SceneModifier { .. }) => None,
        None if editor_target.is_some() => None,
        None => active_layer.clone(),
    }
}

/// Write the display label for the LED exit path dropdown button over `out`,
/// reusing its allocation.
pub(crate) fn write_led_exit_path_label(
    out: &mut String,
    led_exit_index: i32,
    master_effects: &[manifold_core::effects::PresetInstance],
) {
    use manifold_core::preset_type_registry;
    out.clear();
    let after = (led_exit_index > 0)
        .then(|| master_effects.get((led_exit_index - 1) as usize))
        .flatten();
    match (led_exit_index, after) {
        (0, _) => out.push_str("Before FX"),
        (_, Some(fx)) => {
            out.push_str("After ");
            out.push_str(preset_type_registry::display_name(fx.effect_type()));
        }
        _ => out.push_str("After All FX"),
    }
}

// Re-export public functions from sub-modules
#[cfg(feature = "ui-snapshot")]
pub use state_sync::push_state;
pub use state_sync::{
    LiveGroups, TransportDisplayCache, check_auto_scroll, drain_events, intake_overlay_state,
    push_chrome, push_clock, push_frame, push_live, push_selection, push_zoom_label,
    sync_audio_dock, sync_automation_lane_geometry, sync_clip_positions, sync_driver_readouts,
    sync_inspector_data, sync_markers, sync_project_all, sync_project_data, sync_scene_row_values,
    sync_track_layout, sync_trigger_catalog,
};

#[cfg(test)]
mod d2_d3_tests {
    //! Regression tests for the
    //! project-based twin of the clip-range gesture, and the Cmd+D mode
    //! dispatch it feeds. Mirrors the `ui_snapshot` harness's `selectionclips`
    //! fixture shape (4 clips on one layer) without depending on the
    //! feature-gated `ui_snapshot` module, so these run under a plain
    //! `cargo test -p manifold-app --lib`.
    use super::*;
    use manifold_core::clip::TimelineClip;
    use manifold_core::layer::Layer;
    use manifold_core::types::LayerType;
    use manifold_core::{Beats, ClipId, Seconds};
    use manifold_editing::service::EditingService;
    use manifold_ui::UIState;

    /// Census bug 19's class (a dead region becomes layer 0), app twin: a
    /// region whose edge layers were deleted anchors nothing on Shift+click.
    #[test]
    fn shift_click_after_region_edge_layers_are_deleted_does_not_anchor_on_layer_0() {
        let mut project = Project::default();
        for (i, name) in ["a", "b", "c", "d"].iter().enumerate() {
            project
                .timeline
                .insert_layer(i, Layer::new((*name).into(), LayerType::Video, i as i32));
        }
        let mut selection = UIState::new();
        let ui_layers = crate::ui_translate::layers_to_ui(&project.timeline.layers);
        selection.set_region(Beats(2.0), Beats(6.0), 1, 2, &ui_layers);
        project.timeline.remove_layer(2);
        project.timeline.remove_layer(1);
        let survivor = project.timeline.layers[1].layer_id.clone();

        select_region_to_with_project(Beats(8.0), 1, &mut selection, &project);

        assert!(
            selection.current_region().is_none(),
            "region {:?}",
            selection.current_region()
        );
        assert_eq!(selection.insert_cursor_layer_id(), Some(&survivor));
    }

    fn clip(id: &str, start: f64, dur: f64) -> manifold_core::clip::TimelineClip {
        let mut c = TimelineClip::new_video(id.into(), Beats(start), Beats(dur), Seconds::ZERO);
        c.id = ClipId::new(id);
        c
    }

    /// `clip_a`[0,8) `clip_b`[8,16) — contiguous — then a GAP (16..24, no
    /// clip there) — then `clip_c`[24,32) `clip_d`[32,40). A second layer
    /// carries `other`[0,40), spanning the same beats, to prove the range is
    /// scoped to the anchor's layer only.
    fn gap_fixture_project() -> Project {
        let mut main = Layer::new("MAIN".into(), LayerType::Video, 0);
        main.layer_id = LayerId::new("main");
        main.clips.push(clip("clip_a", 0.0, 8.0));
        main.clips.push(clip("clip_b", 8.0, 8.0));
        main.clips.push(clip("clip_c", 24.0, 8.0));
        main.clips.push(clip("clip_d", 32.0, 8.0));

        let mut other = Layer::new("OTHER".into(), LayerType::Video, 1);
        other.layer_id = LayerId::new("other");
        other.clips.push(clip("other", 0.0, 40.0));

        let mut project = Project::default();
        project.timeline.layers = vec![main, other];
        project
    }

    #[test]
    fn shift_click_clip_range_across_a_gap_selects_only_whole_clips() {
        let project = gap_fixture_project();
        let mut selection = UIState::new();

        // Anchor: plain click on clip_a.
        selection.select_clip(ClipId::new("clip_a"), LayerId::new("main"));
        // Shift-click extends the range to clip_c — spanning the empty
        // 16..24 gap and clip_b, but NOT clip_d (past the target) or `other`
        // (a different layer, even though its span covers the same beats).
        select_clip_range_to_with_project(&ClipId::new("clip_c"), &mut selection, &project);

        let ids: std::collections::HashSet<ClipId> =
            selection.get_selected_clip_ids().into_iter().collect();
        assert_eq!(
            ids,
            std::collections::HashSet::from([
                ClipId::new("clip_a"),
                ClipId::new("clip_b"),
                ClipId::new("clip_c"),
            ]),
            "range selects every whole clip between anchor and target, gap included, nothing beyond"
        );
        // The mode leak this phase kills: no synthesized region, ever.
        assert!(
            selection.current_region().is_none(),
            "a clip-range shift-click must produce a Clips selection, never a TimeRange region"
        );
    }

    #[test]
    fn shift_click_clip_range_no_live_anchor_falls_back_to_plain_click() {
        // Fresh selection (no anchor yet) — shift-clicking a clip behaves
        // like a plain click on it, per the D2 fallback.
        let project = gap_fixture_project();
        let mut selection = UIState::new();
        select_clip_range_to_with_project(&ClipId::new("clip_b"), &mut selection, &project);

        let ids: Vec<ClipId> = selection.get_selected_clip_ids();
        assert_eq!(ids, vec![ClipId::new("clip_b")]);
        assert!(selection.current_region().is_none());
    }

    /// Mirrors `input_host.rs`'s `duplicate_clips` dispatch exactly (region =
    /// `selection.current_region().cloned().unwrap_or_default()`, mode =
    /// `region.is_active`) — the D3 typed dispatch this phase's fix is
    /// downstream of (D1 already made `current_region()` return `None` for a
    /// `Clips` selection; D2's fix is what keeps shift-click FROM producing a
    /// stray `TimeRange` in the first place).
    fn duplicate_selected_clips(
        project: &mut Project,
        selection: &UIState,
    ) -> (Vec<Box<dyn manifold_editing::command::Command>>, bool) {
        let clip_ids = selection.get_selected_clip_ids();
        let region = selection.current_region().cloned().unwrap_or_default();
        let used_region_mode = region.is_active;
        let region_core = crate::ui_translate::selection_region_to_core(&region);
        let commands = EditingService::duplicate_clips(project, &clip_ids, &region_core);
        (commands, used_region_mode)
    }

    #[test]
    fn cmd_d_on_four_contiguous_clips_lands_flush_via_the_clips_path() {
        let mut project = gap_fixture_project();
        let mut selection = UIState::new();
        // Select all 4 clips as a plain `Clips` set (as D2's fixed shift-click
        // range now produces) — NOT via a region.
        selection.select_clips(vec![
            ClipId::new("clip_a"),
            ClipId::new("clip_b"),
            ClipId::new("clip_c"),
            ClipId::new("clip_d"),
        ]);
        assert!(
            selection.current_region().is_none(),
            "precondition: a Clips selection must not carry a region"
        );

        let (mut commands, used_region_mode) = duplicate_selected_clips(&mut project, &selection);
        assert!(
            !used_region_mode,
            "Clips selection must dispatch the individual/clip-span path, not region mode"
        );
        assert_eq!(commands.len(), 4);

        for c in commands.iter_mut() {
            c.execute(&mut project);
        }

        // Old span: clip_a start=0 .. clip_d end=40 (with the 16..24 gap
        // inside it). New copies must land flush at the span's END (40) —
        // not offset by some other span/region duration.
        let main = project
            .timeline
            .layers
            .iter()
            .find(|l| l.layer_id == LayerId::new("main"))
            .unwrap();
        let new_min_start = main
            .clips
            .iter()
            .filter(|c| !["clip_a", "clip_b", "clip_c", "clip_d"].contains(&c.id.as_str()))
            .map(|c| c.start_beat)
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
            .expect("4 new copies were created");
        assert_eq!(
            new_min_start,
            Beats(40.0),
            "copies land flush at the old selection's span end, not gapped"
        );
    }

    #[test]
    fn cmd_d_on_a_rubber_band_time_range_preserves_region_mode() {
        let mut project = gap_fixture_project();
        let mut selection = UIState::new();
        let ui_layers = crate::ui_translate::layers_to_ui(&project.timeline.layers);
        // A rubber-band region covering clip_a..clip_b on the main layer only.
        selection.set_region(Beats(0.0), Beats(16.0), 0, 0, &ui_layers);
        assert!(selection.current_region().is_some());

        let (commands, used_region_mode) = duplicate_selected_clips(&mut project, &selection);
        assert!(
            used_region_mode,
            "a TimeRange selection must still dispatch today's region-duplicate mode"
        );
        assert!(!commands.is_empty());
    }
}

#[cfg(test)]
mod command_site_plan_tests {
    //! UI_INVALIDATION P4d batch a: a command site returns `handled()` and the
    //! planner follows the stamps its write moved, one test per write kind.
    use super::*;
    use crate::content_command::ContentCommand;
    use crate::frame_plan::{FrameInputs, Projection, UiStamps, plan_frame};
    use manifold_core::change::PartStamps;
    use manifold_ui::{LayerAction, ProjectAction};

    /// Dispatch `action` against `project` with fresh stamps; the result, the
    /// stamps it left and what it sent.
    fn dispatch_stamped(
        action: &PanelAction,
        project: &Project,
    ) -> (DispatchResult, UiStamps, Vec<ContentCommand>) {
        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let content_state = crate::content_state::ContentState::default();
        let mut ui = UIRoot::new();
        let mut selection = manifold_ui::UIState::new();
        let mut active_layer = project.timeline.layers.first().map(|l| l.layer_id.clone());
        let mut user_prefs = crate::user_prefs::UserPrefs::in_memory();
        let mut scrub = ScrubState::default();
        let mut edits = crate::gesture_preview::UiEdits::default();
        let result = dispatch(
            action,
            &mut DispatchCtx {
                project: ProjectRef::new(project, &mut edits, &content_tx),
                content_tx: &content_tx,
                content_state: &content_state,
                ui: &mut ui,
                selection: &mut selection,
                active_layer: &mut active_layer,
                user_prefs: &mut user_prefs,
                editor_target: None,
                scrub: &mut scrub,
                dialogs: Default::default(),
            },
        );
        (result, edits.stamps, content_rx.try_iter().collect())
    }

    /// The content thread runs what dispatch sent and its snapshot's stamps
    /// fold into the UI's, as the App folds an accepted snapshot.
    fn land(project: &mut Project, stamps: &mut UiStamps, sent: Vec<ContentCommand>) {
        let mut service = manifold_editing::service::EditingService::new();
        crate::edit_runner::apply_sent(&mut service, project, sent);
        stamps.fold_snapshot(&service.stamps());
    }

    fn plan_after(stamps: &UiStamps) -> crate::frame_plan::FramePlan {
        plan_frame(
            &FrameInputs::default(),
            &FrameInputs {
                parts: stamps.parts(),
                ..Default::default()
            },
        )
    }

    #[test]
    fn execute_site_projects_from_its_stamps() {
        let mut project = Project::default();
        project.timeline.add_layer_default();
        let (_, mut stamps, sent) =
            dispatch_stamped(&PanelAction::Layer(LayerAction::AddLayerClicked), &project);
        assert_eq!(project.timeline.layers.len(), 1, "the UI's copy waits");
        assert!(plan_after(&stamps).projections == Default::default());
        assert!(matches!(sent.as_slice(), [ContentCommand::Execute(_)]));
        land(&mut project, &mut stamps, sent);
        assert_eq!(
            project.timeline.layers.len(),
            2,
            "the content thread ran it"
        );
        let plan = plan_after(&stamps);
        assert!(plan.projections.contains(Projection::ProjectSync));
        assert!(plan.projections.contains(Projection::InspectorSync));
    }

    #[test]
    fn mutate_site_projects_from_its_stamps() {
        let mut project = Project::default();
        project.timeline.add_layer_default();
        let id = project.timeline.layers[0].layer_id.clone();
        let collapsed = project.timeline.layers[0].is_collapsed;
        let (_, mut stamps, sent) = dispatch_stamped(
            &PanelAction::Layer(LayerAction::ChevronClicked(id)),
            &project,
        );
        assert_eq!(project.timeline.layers[0].is_collapsed, collapsed);
        assert!(plan_after(&stamps).projections == Default::default());
        assert!(matches!(
            sent.as_slice(),
            [ContentCommand::MutateProject { .. }]
        ));
        land(&mut project, &mut stamps, sent);
        assert_ne!(
            project.timeline.layers[0].is_collapsed, collapsed,
            "the content thread wrote it"
        );
        let plan = plan_after(&stamps);
        assert!(plan.projections.contains(Projection::ProjectSync));
        assert!(plan.projections.contains(Projection::ClipPositions));
    }

    /// BUG-bo2mx (scene setup skin edits run against the UI snapshot): the
    /// skin edit is content-owned. Dispatch leaves the UI project and stamps
    /// untouched; the content receipt's stamps drive the projection.
    #[test]
    fn execute_on_content_site_waits_for_the_receipt() {
        let mut project = Project::default();
        let idx = project.timeline.add_layer(
            "Scene",
            manifold_core::types::LayerType::Generator,
            manifold_core::PresetTypeId::from_string("Scene".to_string()),
        );
        let layer_id = project.timeline.layers[idx].layer_id.clone();
        let before = serde_json::to_value(&project).unwrap();
        {
            let action = ProjectAction::SceneRemove(
                layer_id.clone(),
                vec![manifold_core::scene_doc::ItemId::new("cube")],
            );
            let (_, stamps, sent) =
                dispatch_stamped(&PanelAction::Project(action.clone()), &project);
            assert_eq!(
                serde_json::to_value(&project).unwrap(),
                before,
                "{action:?}: UI snapshot untouched"
            );
            assert!(
                plan_after(&stamps).projections == Default::default(),
                "{action:?}: nothing to project yet"
            );
            let [ContentCommand::Execute(command)] = sent.as_slice() else {
                panic!("{action:?}: expected one content-owned edit");
            };
            // The receipt: the content thread stamps what the command touches
            // and the accepted snapshot folds those stamps in.
            let mut content = PartStamps::default();
            content.bump(command.touches());
            let mut folded = stamps;
            folded.fold_snapshot(&content);
            let plan = plan_after(&folded);
            assert!(
                plan.projections.contains(Projection::InspectorSync),
                "{action:?}"
            );
        }
    }
}
