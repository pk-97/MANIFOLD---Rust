//! Inspector-related dispatch: effect params, drivers, envelopes, generator params,
//! master/layer/clip chrome, slider interactions.

use manifold_core::effects::ParameterDriver;
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::LayerId;
use manifold_editing::commands::ableton::ChangeAbletonTrimCommand;
use manifold_editing::commands::audio_setup::{
    AddAudioSendCommand, RemoveAudioSendCommand, RenameAudioSendCommand, SetAudioCrossoversCommand,
    SetAudioInputDeviceCommand, SetAudioSendChannelsCommand, SetAudioSendFloorCommand,
    SetAudioSendGainCommand,
};
use manifold_editing::commands::layer::{
    AddLayerClipTriggerCommand, RemoveLayerClipTriggerCommand, SetLayerClipTriggerCommand,
};
use manifold_ui::{AudioShapeParam, DriverConfigAction, PanelAction};

use super::DispatchResult;
use super::dispatch::resolve::{
    ableton_mapping_target, audio_setup_command, clip_trigger_shape_dual_edit,
    macro_mapping_target, resolve_graph_target, resolve_param_range,
};

/// Send gain trim range (dB) — shared by the stepper (`AudioSendGainStep`) and
/// the D7 drag (`AudioSendGainDragChanged`/`Commit`).
const AUDIO_SEND_GAIN_MIN_DB: f32 = -24.0;
const AUDIO_SEND_GAIN_MAX_DB: f32 = 24.0;

pub(super) fn dispatch_inspector(
    action: &PanelAction,
    ctx: &mut super::DispatchCtx,
) -> DispatchResult {
    use crate::content_command::ContentCommand;

    // Ordered first-non-unhandled chain over the `dispatch/` handler modules
    // (D6). Each `_ => unhandled()` fall-through advances to the next; the
    // `dispatch_chain_completeness` invariant proves every module is chained.
    let r = super::dispatch::browser::dispatch_browser(action, ctx);
    if !r.unhandled { return r; }
    let r = super::dispatch::clip::dispatch_clip(action, ctx);
    if !r.unhandled { return r; }
    let r = super::dispatch::params::dispatch_params(action, ctx);
    if !r.unhandled { return r; }
    let r = super::dispatch::modulation::dispatch_modulation(action, ctx);
    if !r.unhandled { return r; }

    // The single-effect VALUE / expose / mapping arms address their instance by
    // stable `EffectId` via `super::resolve_effect_id(ctx.editor_target, …)` and
    // ignore `effective_tab` / `active_layer` when the editor supplies an
    // identity. The MODULATION arms (drivers, layer-stored envelopes, trims,
    // envelope targets) still resolve positionally through `(tab, active_layer)`
    // + the effect's row index, so they need a tab/layer that points at the
    // editor's WATCHED effect — not the main window's selection — when a card
    // action is dispatched from the editor. `editor_dispatch_context` expresses
    // the editor's identity in those positional terms (Master / its Layer /
    // Clip), byte-identical to the inspector's own context on the perform path
    // (`ctx.editor_target == None`). Arm bodies read `ctx` fields DIRECTLY (P-B
    // D6): a mis-referenced field is visible at the use site, and the split
    // moves each arm verbatim into its `(action, ctx)` sub-dispatcher.
    let (effective_tab, effective_active_layer) = super::editor_dispatch_context(
        ctx.editor_target,
        &*ctx.project,
        ctx.ui.inspector.last_effect_tab(),
        &*ctx.active_layer,
    );
    // No arm mutates `active_layer`; the immutable shadow routes every
    // downstream resolver through the effective layer.
    let active_layer: &Option<LayerId> = &effective_active_layer;

    match action {

        // ── Layer-owned clip triggers (P3b, AUDIO_SETUP_DOCK_AND_TRIGGER_
        // UNIFICATION_DESIGN.md D2/D5) — the inspector's AUDIO TRIGGERS
        // section. Addressed directly by `LayerId` + index (no
        // `resolve_graph_target`/`editor_target` involved — a clip trigger
        // isn't a graph param). Mutations route through P2's
        // Add/Remove/SetLayerClipTriggerCommand — whole-value-replace, same
        // shape as `SetAudioModTriggerModeCommand`.
        PanelAction::AudioTriggerSectionToggle => {
            ctx.ui.inspector.audio_trigger_section_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        PanelAction::AudioTriggerRowExpandToggle(_layer_id, index) => {
            ctx.ui.inspector.audio_trigger_section_mut().toggle_row_expanded(*index);
            DispatchResult::structural()
        }
        PanelAction::AudioTriggerAdd(layer_id) => {
            // One click = a firing trigger: enabled, listening to the first
            // send's kick cell (the dedicated ridge detector — the most
            // common thing a performer points a layer at), default shape,
            // 1b one-shot. The user hears it fire immediately and adjusts
            // from there. Inert until the Audio Setup dock defines a send
            // (mirrors `AudioModToggle`'s "arm" no-send case).
            if let Some(send_id) = ctx.project.audio_setup.sends.first().map(|s| s.id.clone()) {
                let mut trigger = manifold_core::audio_trigger::LayerClipTrigger::new(
                    manifold_core::audio_mod::AudioModSource {
                        send_id,
                        feature: manifold_core::AudioFeature::new(
                            manifold_core::AudioFeatureKind::Kick,
                            manifold_core::AudioBand::Low,
                        ),
                    },
                );
                trigger.enabled = true;
                let new_index = ctx.project
                    .timeline
                    .find_layer_by_id_mut(layer_id)
                    .map(|(_, l)| l.clip_triggers.len());
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                    Box::new(AddLayerClipTriggerCommand::new(layer_id.clone(), trigger));
                boxed.execute(ctx.project);
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                // Open the new row's drawer so its (now minimal) tuning is
                // immediately visible.
                if let Some(index) = new_index {
                    ctx.ui.inspector.audio_trigger_section_mut().expand_row(index);
                }
            }
            DispatchResult::structural()
        }
        PanelAction::AudioTriggerRemove(layer_id, index) => {
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                Box::new(RemoveLayerClipTriggerCommand::new(layer_id.clone(), *index));
            boxed.execute(ctx.project);
            ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        PanelAction::AudioTriggerEnabledToggle(layer_id, index) => {
            let old = ctx.project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .and_then(|(_, l)| l.clip_triggers.get(*index).cloned());
            if let Some(old) = old {
                let mut new = old.clone();
                new.enabled = !old.enabled;
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(
                    SetLayerClipTriggerCommand::new(layer_id.clone(), *index, old, new),
                );
                boxed.execute(ctx.project);
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        PanelAction::AudioTriggerSetSource(layer_id, index, send_id, feature) => {
            let old = ctx.project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .and_then(|(_, l)| l.clip_triggers.get(*index).cloned());
            if let Some(old) = old {
                let mut new = old.clone();
                new.source = manifold_core::audio_mod::AudioModSource {
                    send_id: send_id.clone(),
                    feature: crate::ui_translate::audio_feature_to_core(*feature),
                };
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(
                    SetLayerClipTriggerCommand::new(layer_id.clone(), *index, old, new),
                );
                boxed.execute(ctx.project);
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        PanelAction::AudioTriggerShapeSnapshot(layer_id, index) => {
            // Reuses `audio_shape_snapshot` (the param-mod shaping-slider
            // slot) rather than a dedicated field: only one drawer slider
            // can be mid-drag at a time (single-threaded UI dispatch), so
            // the snapshot/commit pair for this target never overlaps a
            // param-mod drag's own use of the same slot.
            ctx.scrub.audio_shape_snapshot = ctx.project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .and_then(|(_, l)| l.clip_triggers.get(*index))
                .map(|t| t.shape);
            if let Some(shape) = ctx.scrub.audio_shape_snapshot {
                ctx.scrub.active_inspector_drag = Some(crate::app::ActiveInspectorDrag::AudioTriggerShape {
                    layer_id: layer_id.clone(),
                    index: *index,
                    shape,
                });
            }
            DispatchResult::handled()
        }
        PanelAction::AudioTriggerShapeParamChanged(layer_id, index, which, value) => {
            let which = *which;
            let v = *value;
            if let Some(crate::app::ActiveInspectorDrag::AudioTriggerShape { shape, .. }) =
                &mut ctx.scrub.active_inspector_drag
            {
                match which {
                    AudioShapeParam::Sensitivity => shape.sensitivity = v,
                    AudioShapeParam::Attack => shape.attack_ms = v,
                    AudioShapeParam::Release => shape.release_ms = v,
                }
            }
            clip_trigger_shape_dual_edit(ctx.project, ctx.content_tx, layer_id, *index, move |shape| {
                match which {
                    AudioShapeParam::Sensitivity => shape.sensitivity = v,
                    AudioShapeParam::Attack => shape.attack_ms = v,
                    AudioShapeParam::Release => shape.release_ms = v,
                }
            });
            DispatchResult::handled()
        }
        PanelAction::AudioTriggerShapeCommit(layer_id, index) => {
            ctx.scrub.active_inspector_drag = None;
            if let Some(old_shape) = ctx.scrub.audio_shape_snapshot.take() {
                let current = ctx.project
                    .timeline
                    .find_layer_by_id_mut(layer_id)
                    .and_then(|(_, l)| l.clip_triggers.get(*index).cloned());
                if let Some(current) = current
                    && current.shape != old_shape
                {
                    let mut old = current.clone();
                    old.shape = old_shape;
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(
                        SetLayerClipTriggerCommand::new(layer_id.clone(), *index, old, current),
                    );
                    boxed.execute(ctx.project);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::handled()
        }
        PanelAction::AudioTriggerSetLength(layer_id, index, beats) => {
            let old = ctx.project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .and_then(|(_, l)| l.clip_triggers.get(*index).cloned());
            if let Some(old) = old {
                let mut new = old.clone();
                new.one_shot_beats = manifold_core::Beats(*beats as f64);
                if new != old {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(
                        SetLayerClipTriggerCommand::new(layer_id.clone(), *index, old, new),
                    );
                    boxed.execute(ctx.project);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }

        // ── Audio Setup (project-level send routing) ──────────────
        PanelAction::AudioSetDevice(device) => {
            let old = ctx.project.audio_setup.device.clone();
            audio_setup_command(
                ctx.project,
                ctx.content_tx,
                Box::new(SetAudioInputDeviceCommand::new(
                    old,
                    device.as_ref().map(crate::ui_translate::audio_device_ref_to_core),
                )),
            )
        }
        PanelAction::AudioAddSend => {
            let send = manifold_core::audio_setup::AudioSend::new(format!(
                "Audio {}",
                ctx.project.audio_setup.sends.len() + 1
            ));
            audio_setup_command(ctx.project, ctx.content_tx, Box::new(AddAudioSendCommand::new(send)))
        }
        PanelAction::AudioRemoveSend(id) => audio_setup_command(
            ctx.project,
            ctx.content_tx,
            Box::new(RemoveAudioSendCommand::new(id.clone())),
        ),
        PanelAction::AudioRenameSend(id, label) => {
            let old = ctx.project
                .audio_setup
                .find_send(id)
                .map(|s| s.label.clone())
                .unwrap_or_default();
            audio_setup_command(
                ctx.project,
                ctx.content_tx,
                Box::new(RenameAudioSendCommand::new(id.clone(), old, label.clone())),
            )
        }
        PanelAction::AudioSetSendChannels(id, ch) => {
            let old = ctx.project
                .audio_setup
                .find_send(id)
                .map(|s| s.channels.clone())
                .unwrap_or_default();
            audio_setup_command(
                ctx.project,
                ctx.content_tx,
                Box::new(SetAudioSendChannelsCommand::new(id.clone(), old, ch.clone())),
            )
        }
        // `AudioSendStereoToggle` is deleted (§7.2 item 6, P8, 2026-07-11) —
        // the channel dropdown now carries any channel vec directly via
        // `AudioSetSendChannels` above; mono falls out of picking one channel.
        PanelAction::AudioSendGainStep(id, delta_db) => {
            // The project is the source of truth: read current gain, apply the
            // delta, clamp to a sensible trim range, commit old→new. Capture
            // restart is avoided — the worker reads gain live (AudioModRuntime).
            let old = ctx.project
                .audio_setup
                .find_send(id)
                .map(|s| s.gain_db)
                .unwrap_or(0.0);
            let new = (old + delta_db).clamp(AUDIO_SEND_GAIN_MIN_DB, AUDIO_SEND_GAIN_MAX_DB);
            if (new - old).abs() < f32::EPSILON {
                return DispatchResult::structural();
            }
            audio_setup_command(
                ctx.project,
                ctx.content_tx,
                Box::new(SetAudioSendGainCommand::new(id.clone(), old, new)),
            )
        }
        PanelAction::AudioSendGainDragBegin(id) => {
            // Snapshot the pre-drag gain so the commit records one undo step —
            // the `AudioCrossoverDragBegin` pattern, per-send (D7).
            ctx.scrub.audio_send_gain_drag_snapshot = Some(
                ctx.project
                    .audio_setup
                    .find_send(id)
                    .map(|s| s.gain_db)
                    .unwrap_or(0.0),
            );
            if let Some(db) = ctx.scrub.audio_send_gain_drag_snapshot {
                ctx.scrub.active_inspector_drag = Some(crate::app::ActiveInspectorDrag::AudioSendGain {
                    send_id: id.clone(),
                    db,
                });
            }
            DispatchResult::handled()
        }
        PanelAction::AudioSendGainDragChanged(id, db) => {
            // Live edit (no per-frame undo): clamp to the stepper's trim range,
            // then apply to the local project and the content thread so the
            // label + `GainBank` track the cursor — no capture restart.
            let clamped = db.clamp(AUDIO_SEND_GAIN_MIN_DB, AUDIO_SEND_GAIN_MAX_DB);
            if let Some(crate::app::ActiveInspectorDrag::AudioSendGain { db: guard, .. }) =
                &mut ctx.scrub.active_inspector_drag
            {
                *guard = clamped;
            }
            if let Some(s) = ctx.project.audio_setup.find_send_mut(id) {
                s.gain_db = clamped;
            }
            let id = id.clone();
            ContentCommand::send(
                ctx.content_tx,
                ContentCommand::MutateProjectLive(Box::new(move |p| {
                    if let Some(s) = p.audio_setup.find_send_mut(&id) {
                        s.gain_db = clamped;
                    }
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::AudioSendGainDragCommit(id) => {
            // One undo step: snapshot (old) → current gain (new).
            ctx.scrub.active_inspector_drag = None;
            if let Some(old) = ctx.scrub.audio_send_gain_drag_snapshot.take() {
                let new = ctx.project.audio_setup.find_send(id).map(|s| s.gain_db).unwrap_or(old);
                if (new - old).abs() > f32::EPSILON {
                    return audio_setup_command(
                        ctx.project,
                        ctx.content_tx,
                        Box::new(SetAudioSendGainCommand::new(id.clone(), old, new)),
                    );
                }
            }
            DispatchResult::handled()
        }
        // P4 (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D8, audio-dock sibling):
        // the type-in commit — ONE undo step, no clamp. Unlike
        // `AudioSendGainDragChanged`'s live-drag path, a typed value is free
        // to exceed `AUDIO_SEND_GAIN_MIN_DB`/`MAX_DB` (PARAM_RANGE_CONTRACT
        // P1: those are the stepper's display travel, not a hard limit).
        PanelAction::AudioSendGainSetTyped(id, new_db) => {
            let old = ctx.project.audio_setup.find_send(id).map(|s| s.gain_db).unwrap_or(0.0);
            if (new_db - old).abs() < f32::EPSILON {
                return DispatchResult::structural();
            }
            audio_setup_command(
                ctx.project,
                ctx.content_tx,
                Box::new(SetAudioSendGainCommand::new(id.clone(), old, *new_db)),
            )
        }
        PanelAction::AudioSendFloorStep(id, delta_db) => {
            // Pre-analysis squelch (dB). Off is a sentinel below the usable range:
            // stepping up from off engages the gate at its bottom; stepping below
            // the bottom turns it back off. Applied live (AudioModRuntime).
            const FLOOR_MIN_DB: f32 = -100.0;
            const FLOOR_MAX_DB: f32 = -6.0;
            let off = manifold_core::audio_setup::FLOOR_DB_OFF;
            let old = ctx.project
                .audio_setup
                .find_send(id)
                .map(|s| s.floor_db)
                .unwrap_or(off);
            let new = if old <= off {
                if *delta_db > 0.0 { FLOOR_MIN_DB } else { off }
            } else {
                let v = old + *delta_db;
                if v < FLOOR_MIN_DB { off } else { v.min(FLOOR_MAX_DB) }
            };
            if (new - old).abs() < f32::EPSILON {
                return DispatchResult::structural();
            }
            audio_setup_command(
                ctx.project,
                ctx.content_tx,
                Box::new(SetAudioSendFloorCommand::new(id.clone(), old, new)),
            )
        }
        // The Audio Setup Triggers matrix's dispatch arms (AudioTriggerToggled,
        // AudioTriggerSensitivityStep, AudioSendSensitivityDragBegin/Changed/
        // Commit, AudioTriggerLengthStep, AudioTriggerSetLayer,
        // AudioTriggerLayerClicked) are deleted with the matrix (P3, D2). Clip
        // triggers are authored on the layer only (`LayerClipTrigger`, P2).
        // `AudioSendAddLayerClicked` (Inputs section "+ Layer") is deleted
        // with the section's authoring (§7.2 item 7, P8, 2026-07-11).
        PanelAction::AudioCrossoverDragBegin => {
            // Snapshot the pre-drag crossovers so the commit records one undo step.
            ctx.scrub.audio_crossover_snapshot =
                Some((ctx.project.audio_setup.low_hz, ctx.project.audio_setup.mid_hz));
            if let Some((low_hz, mid_hz)) = ctx.scrub.audio_crossover_snapshot {
                ctx.scrub.active_inspector_drag = Some(crate::app::ActiveInspectorDrag::AudioCrossover {
                    low_hz,
                    mid_hz,
                });
            }
            DispatchResult::handled()
        }
        PanelAction::AudioCrossoverChanged(band, hz) => {
            // Live edit (no per-frame undo): clamp the dragged line against the
            // other and the band edges, then apply to the local project and the
            // content thread so the divider + analysis bands track the cursor.
            let dragging_low = matches!(band, manifold_ui::BandDivider::Low);
            let (cur_low, cur_mid) = (ctx.project.audio_setup.low_hz, ctx.project.audio_setup.mid_hz);
            let (low, mid) = if dragging_low {
                manifold_core::audio_setup::AudioSetup::clamp_crossovers(*hz, cur_mid, true)
            } else {
                manifold_core::audio_setup::AudioSetup::clamp_crossovers(cur_low, *hz, false)
            };
            if let Some(crate::app::ActiveInspectorDrag::AudioCrossover { low_hz, mid_hz }) =
                &mut ctx.scrub.active_inspector_drag
            {
                *low_hz = low;
                *mid_hz = mid;
            }
            ctx.project.audio_setup.low_hz = low;
            ctx.project.audio_setup.mid_hz = mid;
            ContentCommand::send(
                ctx.content_tx,
                ContentCommand::MutateProjectLive(Box::new(move |p| {
                    p.audio_setup.low_hz = low;
                    p.audio_setup.mid_hz = mid;
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::AudioCrossoverCommit => {
            // One undo step: snapshot (old) → current crossovers (new).
            ctx.scrub.active_inspector_drag = None;
            if let Some(old) = ctx.scrub.audio_crossover_snapshot.take() {
                let new = (ctx.project.audio_setup.low_hz, ctx.project.audio_setup.mid_hz);
                if new != old {
                    return audio_setup_command(
                        ctx.project,
                        ctx.content_tx,
                        Box::new(SetAudioCrossoversCommand::new(old, new)),
                    );
                }
            }
            DispatchResult::handled()
        }

        PanelAction::MapParamToMacro(gpt, param_id, macro_idx) => {
            use manifold_core::{MacroCurve, MacroMapping};
            let macro_idx = *macro_idx;
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
                && let Some(mapping_target) = macro_mapping_target(&target, param_id)
            {
                // Graph-authority-first range so a generator's (or graph-backed
                // effect's) true slider range isn't squashed to the registry's.
                let (min, max) = ctx.project
                    .with_preset_graph_mut(&target, |inst| resolve_param_range(inst, param_id.as_ref()))
                    .unwrap_or((0.0, 1.0));
                let mapping = MacroMapping {
                    target: mapping_target,
                    range_min: min,
                    range_max: max,
                    curve: MacroCurve::Linear,
                    legacy_param_index: None,
                    legacy_effect_addr: None,
                };
                ctx.project.settings.macro_bank.slots[macro_idx]
                    .mappings
                    .push(mapping.clone());
                let mi = macro_idx;
                ContentCommand::send(
                    ctx.content_tx,
                    ContentCommand::MutateProject(Box::new(move |p| {
                        p.settings.macro_bank.slots[mi].mappings.push(mapping);
                    })),
                );
            }
            DispatchResult::handled()
        }
        // Label right-click consumed by try_open_dropdown — shouldn't reach here
        PanelAction::MacroLabelRightClick(_) => DispatchResult::handled(),

        PanelAction::UnmapMacro(macro_idx, mapping_idx) => {
            let macro_idx = *macro_idx;
            let mapping_idx = *mapping_idx;
            if macro_idx < manifold_core::MACRO_COUNT {
                let slot = &mut ctx.project.settings.macro_bank.slots[macro_idx];
                if mapping_idx < slot.mappings.len() {
                    slot.mappings.remove(mapping_idx);
                    ContentCommand::send(
                        ctx.content_tx,
                        ContentCommand::MutateProject(Box::new(move |p| {
                            let slot = &mut p.settings.macro_bank.slots[macro_idx];
                            if mapping_idx < slot.mappings.len() {
                                slot.mappings.remove(mapping_idx);
                            }
                        })),
                    );
                }
            }
            DispatchResult::handled()
        }
        PanelAction::ClearMacroMappings(macro_idx) => {
            let macro_idx = *macro_idx;
            if macro_idx < manifold_core::MACRO_COUNT {
                ctx.project.settings.macro_bank.slots[macro_idx]
                    .mappings
                    .clear();
                ContentCommand::send(
                    ctx.content_tx,
                    ContentCommand::MutateProject(Box::new(move |p| {
                        p.settings.macro_bank.slots[macro_idx].mappings.clear();
                    })),
                );
            }
            DispatchResult::handled()
        }

        // ── Ableton mapping ────────────────────────────────────────
        // Map + unmap run ONE path: resolve the unified `GraphTarget`, derive
        // the `AbletonMappingTarget` via the shared `ableton_mapping_target`
        // helper (effect by stable EffectId within master/layer; generator by
        // layer; clip tab → None, no clip-scoped Ableton mappings), then send
        // the content command. Mirrors `UnmapParamAbleton` below exactly — the
        // only difference is AbletonMapParam (with address) vs AbletonUnmapParam.
        PanelAction::MapParamToAbleton(gpt, param_id, address) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
                && let Some(mapping_target) =
                    ableton_mapping_target(&target, effective_tab, active_layer, ctx.project, param_id)
            {
                ContentCommand::send(
                    ctx.content_tx,
                    ContentCommand::AbletonMapParam {
                        target: mapping_target,
                        address: crate::ui_translate::ableton_macro_address_to_core(address),
                    },
                );
            }
            DispatchResult::handled()
        }
        PanelAction::UnmapParamAbleton(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
                && let Some(mapping_target) =
                    ableton_mapping_target(&target, effective_tab, active_layer, ctx.project, param_id)
            {
                ContentCommand::send(
                    ctx.content_tx,
                    ContentCommand::AbletonUnmapParam {
                        target: mapping_target,
                    },
                );
            }
            DispatchResult::handled()
        }

        PanelAction::MapMacroToAbleton(slot_idx, address) => {
            use manifold_core::ableton_mapping::AbletonMappingTarget;
            let target = AbletonMappingTarget::MacroSlot {
                slot_index: *slot_idx,
            };
            ContentCommand::send(
                ctx.content_tx,
                ContentCommand::AbletonMapParam {
                    target,
                    address: crate::ui_translate::ableton_macro_address_to_core(address),
                },
            );
            DispatchResult::handled()
        }
        PanelAction::UnmapMacroAbleton(slot_idx) => {
            use manifold_core::ableton_mapping::AbletonMappingTarget;
            let target = AbletonMappingTarget::MacroSlot {
                slot_index: *slot_idx,
            };
            ContentCommand::send(ctx.content_tx, ContentCommand::AbletonUnmapParam { target });
            DispatchResult::handled()
        }
        // Picker open is consumed by try_open_dropdown — never reaches dispatch.
        PanelAction::OpenAbletonPickerForMacro(_) => DispatchResult::handled(),

        // Driver / Ableton / audio trim handles are unified into the
        // `Trim{Changed,Snapshot,Commit}(TrimKind, …)` arms above.

        PanelAction::AbletonMacroTrimChanged(slot_idx, min, max) => {
            let slot_idx = *slot_idx;
            let min = *min;
            let max = *max;
            if let Some(crate::app::ActiveInspectorDrag::AbletonMacroTrim {
                min: g_min,
                max: g_max,
                ..
            }) = &mut ctx.scrub.active_inspector_drag
            {
                *g_min = min;
                *g_max = max;
            }
            if let Some(slot) = ctx.project.settings.macro_bank.slots.get_mut(slot_idx)
                && let Some(m) = &mut slot.ableton_mapping
            {
                m.range_min = min;
                m.range_max = max;
            }
            ContentCommand::send(
                ctx.content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some(slot) = p.settings.macro_bank.slots.get_mut(slot_idx)
                        && let Some(m) = &mut slot.ableton_mapping
                    {
                        m.range_min = min;
                        m.range_max = max;
                    }
                })),
            );
            DispatchResult::handled()
        }
        PanelAction::AbletonMacroTrimSnapshot(slot_idx) => {
            if let Some(range) = ctx.project
                .settings
                .macro_bank
                .slots
                .get(*slot_idx)
                .and_then(|s| s.ableton_mapping.as_ref())
                .map(|m| (m.range_min, m.range_max))
            {
                ctx.scrub.trim_snapshot = Some(range);
                ctx.scrub.active_inspector_drag = Some(crate::app::ActiveInspectorDrag::AbletonMacroTrim {
                    slot_idx: *slot_idx,
                    min: range.0,
                    max: range.1,
                });
            }
            DispatchResult::handled()
        }
        PanelAction::AbletonMacroTrimCommit(slot_idx) => {
            use manifold_core::ableton_mapping::AbletonMappingTarget;
            ctx.scrub.active_inspector_drag = None;
            if let Some((old_min, old_max)) = ctx.scrub.trim_snapshot.take()
                && let Some((new_min, new_max)) = ctx.project
                    .settings
                    .macro_bank
                    .slots
                    .get(*slot_idx)
                    .and_then(|s| s.ableton_mapping.as_ref())
                    .map(|m| (m.range_min, m.range_max))
                && ((old_min - new_min).abs() > f32::EPSILON
                    || (old_max - new_max).abs() > f32::EPSILON)
            {
                let cmd = ChangeAbletonTrimCommand::new(
                    AbletonMappingTarget::MacroSlot { slot_index: *slot_idx },
                    old_min,
                    old_max,
                    new_min,
                    new_max,
                );
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
            DispatchResult::handled()
        }

        PanelAction::AbletonInvertToggle(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
                && let Some(mapping_target) =
                    ableton_mapping_target(&target, effective_tab, active_layer, ctx.project, param_id)
            {
                if let Some(ms) = ctx.project
                    .ableton_param_mappings_mut(&mapping_target)
                    .and_then(|opt| opt.as_mut())
                    && let Some(m) = ms.iter_mut().find(|m| m.param_id == *param_id)
                {
                    m.inverted = !m.inverted;
                }
                let mt = mapping_target.clone();
                let pid = param_id.clone();
                ContentCommand::send(
                    ctx.content_tx,
                    ContentCommand::MutateProject(Box::new(move |p| {
                        if let Some(ms) =
                            p.ableton_param_mappings_mut(&mt).and_then(|opt| opt.as_mut())
                            && let Some(m) = ms.iter_mut().find(|m| m.param_id == pid)
                        {
                            m.inverted = !m.inverted;
                        }
                    })),
                );
            }
            DispatchResult::structural()
        }

        PanelAction::AbletonMacroInvertToggle(slot_idx) => {
            let slot_idx = *slot_idx;
            if let Some(slot) = ctx.project.settings.macro_bank.slots.get_mut(slot_idx)
                && let Some(m) = &mut slot.ableton_mapping
            {
                m.inverted = !m.inverted;
            }
            ContentCommand::send(
                ctx.content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    if let Some(slot) = p.settings.macro_bank.slots.get_mut(slot_idx)
                        && let Some(m) = &mut slot.ableton_mapping
                    {
                        m.inverted = !m.inverted;
                    }
                })),
            );
            DispatchResult::structural()
        }

        _ => DispatchResult::unhandled(),
    }
}

#[cfg(test)]
mod scene_card_convergence_tests {
    //! SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1a gates: a fog-density
    //! drag session dispatched through the REAL `dispatch_inspector` entry
    //! point (the same one `ui_bridge::dispatch` routes `PanelAction::
    //! ParamSnapshot`/`ParamChanged`/`ParamCommit` to) yields exactly ONE
    //! undo unit whose undo restores the pre-drag value, and the write
    //! lands in the layer's own instance def (mirrors project.rs's
    //! `scene_layer_project` SceneStarter fixture — C7's precedent for
    //! testing a scene write against the layer's REAL def, not a bare
    //! `EffectGraphDef` literal).
    use super::*;
    use crate::app::SelectionState;
    use crate::content_command::ContentCommand;
    use crate::ui_root::UIRoot;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;
    use manifold_core::project::Project;
    use manifold_core::types::LayerType;
    use manifold_renderer::node_graph::scene_vm::{AtmosphereVm, SceneVm};

    /// A fresh SceneStarter generator layer + its `render_scene` node id —
    /// same fixture `project.rs`'s `scene_layer_project` uses. SceneStarter
    /// ships a wired `node.atmosphere` (fog_density 0.04, height_falloff
    /// 0.3), so `AtmosphereVm::from_def` resolves `Wired` without any
    /// synthetic graph surgery.
    fn scene_layer_project() -> (Project, LayerId) {
        let mut project = Project::default();
        let idx = project.timeline.add_layer(
            "Scene",
            LayerType::Generator,
            PresetTypeId::from_string("SceneStarter".to_string()),
        );
        let layer_id = project.timeline.layers[idx].layer_id.clone();
        (project, layer_id)
    }

    /// The layer's fog-density write address, read straight off the SAME
    /// `SceneVm::from_def` production code walks (`state_sync.rs`'s VM
    /// builder) — never hand-picked.
    fn fog_density_addr(project: &Project, layer_id: &LayerId) -> manifold_core::effect_graph_def::EffectGraphDef {
        let (_, layer) = project.timeline.find_layer_by_id(layer_id).unwrap();
        layer.generator_graph().cloned().unwrap_or_else(|| {
            manifold_renderer::node_graph::bundled_preset_def(&layer.generator_type().clone())
                .cloned()
                .expect("SceneStarter is a bundled preset")
        })
    }

    fn density_node_id(def: &manifold_core::effect_graph_def::EffectGraphDef) -> u32 {
        let vm = SceneVm::from_def(def).expect("SceneStarter resolves as a scene");
        let AtmosphereVm::Wired(a) = vm.atmosphere else {
            panic!("SceneStarter's atmosphere must be Wired");
        };
        a.node_doc_id
    }

    #[allow(clippy::type_complexity)]
    struct Harness {
        content_tx: crossbeam_channel::Sender<ContentCommand>,
        content_rx: crossbeam_channel::Receiver<ContentCommand>,
        content_state: crate::content_state::ContentState,
        ui: UIRoot,
        selection: SelectionState,
        active_layer: Option<LayerId>,
        // `dispatch_inspector` ignores `user_prefs`, but `DispatchCtx` requires
        // the field — supply an in-memory instance (wiring, not behavior).
        user_prefs: crate::user_prefs::UserPrefs,
        scrub: crate::ui_bridge::ScrubState,
    }

    impl Harness {
        fn new(active_layer: Option<LayerId>) -> Self {
            let (content_tx, content_rx) = crossbeam_channel::unbounded();
            Self {
                content_tx,
                content_rx,
                content_state: crate::content_state::ContentState::default(),
                ui: UIRoot::new(),
                selection: manifold_ui::UIState::new(),
                active_layer,
                user_prefs: crate::user_prefs::UserPrefs::in_memory(),
                scrub: crate::ui_bridge::ScrubState::default(),
            }
        }

        fn dispatch(&mut self, action: &PanelAction, project: &mut Project) -> DispatchResult {
            let mut ctx = crate::ui_bridge::DispatchCtx {
                project,
                content_tx: &self.content_tx,
                content_state: &self.content_state,
                ui: &mut self.ui,
                selection: &mut self.selection,
                active_layer: &mut self.active_layer,
                user_prefs: &mut self.user_prefs,
                editor_target: None,
                scrub: &mut self.scrub,
            };
            dispatch_inspector(action, &mut ctx)
        }

        fn drain(&self) -> Vec<ContentCommand> {
            self.content_rx.try_iter().collect()
        }

        /// `dispatch`'s twin with an explicit `editor_target` — the graph
        /// editor's own identity-addressed entry point
        /// (`resolve_effect_id`/`editor_dispatch_context`, `ui_bridge/mod.rs`):
        /// a `Some(GraphTarget::Effect(id))` resolves that exact instance
        /// (master, layer, or clip) regardless of `GraphParamTarget`'s
        /// positional index or the ambient `last_effect_tab`. `row_dispatch`
        /// uses this to reach a master effect and a layer effect through the
        /// identical dispatch call — the only production path that can
        /// address a specific scope without driving a real pointer-down
        /// through `InspectorPanel::handle_click` (private to manifold-ui,
        /// P2's own test surface).
        fn dispatch_with_editor(
            &mut self,
            action: &PanelAction,
            project: &mut Project,
            editor_target: Option<&manifold_core::GraphTarget>,
        ) -> DispatchResult {
            let mut ctx = crate::ui_bridge::DispatchCtx {
                project,
                content_tx: &self.content_tx,
                content_state: &self.content_state,
                ui: &mut self.ui,
                selection: &mut self.selection,
                active_layer: &mut self.active_layer,
                user_prefs: &mut self.user_prefs,
                editor_target,
                scrub: &mut self.scrub,
            };
            dispatch_inspector(action, &mut ctx)
        }
    }

    /// Undo-race repro (param-feed regression, 2026-07-18): since
    /// `ac96c65c` the content thread ships a `ModulationSnapshot` EVERY
    /// tick and `app_render.rs` applies it to `local_project`
    /// unconditionally (only overlay drags gate it). The restore guard
    /// (`ActiveInspectorDrag`) has no Macro variant, so a stale snapshot
    /// landing mid-drag stomps the in-flight value back to pre-drag; the
    /// commit handler then sees old == new and emits NO undo command.
    #[test]
    fn macro_drag_survives_a_mid_gesture_modulation_snapshot() {
        let mut project = Project::default();
        project.settings.macro_bank.slots[0].value = 0.2;

        // The content thread's view is still pre-drag when it captures.
        let mut stale = crate::content_state::ModulationSnapshot::empty();
        stale.capture_into(&project);

        let mut h = Harness::new(None);
        h.dispatch(&PanelAction::MacroSnapshot(0), &mut project);
        h.dispatch(&PanelAction::MacroChanged(0, 0.8), &mut project);
        h.drain();

        // What the UI frame drain now does every tick (app_render.rs ~line
        // 868): apply the snapshot, then restore only the guarded drag kinds.
        stale.apply(&mut project);
        if let Some(ref drag) = h.scrub.active_inspector_drag {
            drag.apply(&mut project);
        }

        h.dispatch(&PanelAction::MacroCommit(0), &mut project);
        let cmds = h.drain();
        assert!(
            cmds.iter().any(|c| matches!(c, ContentCommand::Execute(_))),
            "a completed macro drag must produce an undo-tracked command; got {} commands",
            cmds.len()
        );
    }

    /// BUG-246 (trim family): while a modulation trim handle is dragged with
    /// playback running, a full snapshot is accepted every frame
    /// (`app_render.rs` ~808 replaces `local_project`, then the unguarded
    /// per-frame `sync_inspector_data` at ~3373 reconfigures cards from it).
    /// Before the `Trim` variant, `drag.apply` had no arm for trim, so the
    /// in-flight `[min,max]` reverted to the snapshot's stale range every
    /// frame — the handle jumped/vanished mid-gesture. The restore must write
    /// the dragged range back through the driver's `trim_min/trim_max`, the
    /// same store `TrimChanged`'s driver dual-edit uses.
    #[test]
    fn driver_trim_range_survives_a_mid_gesture_snapshot() {
        use crate::app::ActiveInspectorDrag;
        let (mut project, layer_id) = scene_layer_project();
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let pid: manifold_core::effects::ParamId = std::borrow::Cow::Owned("density".to_string());

        // Arm a driver carrying the user's in-flight trim range (0.3..0.9).
        project.with_preset_graph_mut(&target, |inst| {
            inst.drivers = Some(vec![ParameterDriver {
                param_id: pid.clone(),
                beat_division: manifold_core::types::BeatDivision::Quarter,
                waveform: manifold_core::types::DriverWaveform::Sine,
                enabled: true,
                phase: 0.0,
                base_value: 0.0,
                trim_min: 0.3,
                trim_max: 0.9,
                reversed: false,
                free_period_beats: None,
                legacy_param_index: None,
                is_paused_by_user: false,
            }]);
        });

        // The content thread's stale snapshot: same driver, DEFAULT trim.
        let mut stale = project.clone();
        stale.with_preset_graph_mut(&target, |inst| {
            if let Some(ds) = inst.drivers.as_mut() {
                ds[0].trim_min = 0.0;
                ds[0].trim_max = 1.0;
            }
        });

        // app_render mid-drag: local_project := stale, then restore the drag.
        let drag = ActiveInspectorDrag::Trim {
            kind: manifold_ui::panels::TrimKind::Driver,
            target: target.clone(),
            ableton_target: None,
            param_id: pid.clone(),
            min: 0.3,
            max: 0.9,
        };
        let mut local = stale;
        drag.apply(&mut local);

        let (mn, mx) = local
            .with_preset_graph_mut(&target, |inst| {
                let d = &inst.drivers.as_ref().unwrap()[0];
                (d.trim_min, d.trim_max)
            })
            .expect("generator instance resolves");
        assert!(
            (mn - 0.3).abs() < 1e-6 && (mx - 0.9).abs() < 1e-6,
            "trim range must survive the snapshot stomp; got ({mn}, {mx}) instead of (0.3, 0.9)"
        );
    }

    /// Phase-1 baseline for the undo/redo audit (Peter 2026-07-19: undo/redo
    /// "broken, out of order, or just don't respond" across sliders, buttons,
    /// toggles, clips, trims). Every undoable gesture family gets two probes:
    ///
    /// - CLEAN: gesture → exactly ONE undo-tracked `Execute` → execute/undo/
    ///   redo round-trips the probed value through a REAL `EditingService`
    ///   (the content thread's own gateway), and the undo stack grows by
    ///   exactly one per gesture.
    /// - STOMP (drag trios only): a full project snapshot lands mid-gesture
    ///   (data_version bump from any concurrent command — playback, MIDI
    ///   phantom commit, another gesture), simulated exactly the way
    ///   app_render.rs ~808-817 applies it: replace the local project, then
    ///   restore the guarded drag. Families without an `ActiveInspectorDrag`
    ///   variant lose the in-flight value here — the commit then sees
    ///   old == new and emits NO undo entry ("doesn't respond").
    mod undo_baseline {
        use super::*;
        use manifold_editing::service::EditingService;

        /// The content-thread side of the loop: a real `EditingService` over
        /// its own project, driven exactly the way content_commands.rs drives
        /// it (`Execute` → `service.execute`, `ExecuteBatch` → `execute_batch`,
        /// `MutateProject(Live)` → plain closure application, no undo entry).
        struct ContentSide {
            project: Project,
            service: EditingService,
            undo_depth: usize,
        }

        impl ContentSide {
            fn new(project: &Project) -> Self {
                Self {
                    project: project.clone(),
                    service: EditingService::new(),
                    undo_depth: 0,
                }
            }

            /// Apply every drained command the way the content thread would.
            /// Returns how many undo-tracked commands landed.
            fn apply(&mut self, cmds: Vec<ContentCommand>) -> usize {
                let mut n = 0;
                for c in cmds {
                    match c {
                        ContentCommand::Execute(cmd) => {
                            self.service.execute(cmd, &mut self.project);
                            self.undo_depth += 1;
                            n += 1;
                        }
                        ContentCommand::ExecuteBatch(cmds, desc) => {
                            let k = cmds.len();
                            self.service.execute_batch(cmds, desc, &mut self.project);
                            self.undo_depth += k.max(1);
                            n += 1;
                        }
                        ContentCommand::MutateProject(f) | ContentCommand::MutateProjectLive(f) => {
                            f(&mut self.project);
                        }
                        _ => {}
                    }
                }
                n
            }
        }

        /// Full gesture → undo → redo cycle assertion: the gesture must emit
        /// exactly one undo-tracked command; executing it lands `after`;
        /// undo restores `before`; redo reapplies `after`; stack grows by 1.
        fn assert_undo_cycle<P>(
            side: &mut ContentSide,
            cmds: Vec<ContentCommand>,
            probe: impl Fn(&Project) -> P,
            before: P,
            after: P,
            label: &str,
        ) where
            P: PartialEq + std::fmt::Debug,
        {
            let depth0 = side.undo_depth;
            let landed = side.apply(cmds);
            assert_eq!(
                landed, 1,
                "{label}: gesture must emit exactly ONE undo-tracked Execute; got {landed}"
            );
            assert_eq!(
                side.undo_depth,
                depth0 + 1,
                "{label}: undo stack must grow by exactly one per gesture"
            );
            assert_eq!(probe(&side.project), after, "{label}: execute must land the new value");
            assert!(side.service.undo(&mut side.project), "{label}: undo must be available");
            assert_eq!(
                probe(&side.project),
                before,
                "{label}: undo must restore the pre-gesture value"
            );
            assert!(side.service.redo(&mut side.project), "{label}: redo must be available");
            assert_eq!(probe(&side.project), after, "{label}: redo must reapply the value");
        }

        /// Mirror app_render's mid-gesture full-snapshot acceptance: replace
        /// the local project with the stale pre-gesture one, then restore the
        /// guarded drag (app_render.rs ~808-817).
        fn snapshot_stomp(h: &Harness, stale: &Project) -> Project {
            let mut p = stale.clone();
            if let Some(ref drag) = h.scrub.active_inspector_drag {
                drag.apply(&mut p);
            }
            p
        }

        /// Drive a drag trio and assert the undo cycle, clean or stomped.
        /// `gesture` runs Snapshot + Changed ticks (NOT the commit); `commit`
        /// dispatches the commit action and returns the drained commands.
        fn trio_cycle<P>(
            label: &str,
            mut project: Project,
            h: &mut Harness,
            gesture: impl Fn(&mut Harness, &mut Project),
            commit: impl Fn(&mut Harness, &mut Project) -> DispatchResult,
            probe: impl Fn(&Project) -> P,
            before: P,
            after: P,
            stomp: bool,
        ) where
            P: PartialEq + std::fmt::Debug,
        {
            let stale = project.clone();
            let mut side = ContentSide::new(&project);
            gesture(h, &mut project);
            // Live ticks reach the content thread as non-undoable writes.
            side.apply(h.drain());
            if stomp {
                project = snapshot_stomp(h, &stale);
            }
            commit(h, &mut project);
            let label = if stomp { format!("{label} [stomp]") } else { label.to_string() };
            assert_undo_cycle(&mut side, h.drain(), probe, before, after, &label);
        }

        // ── Fixtures ─────────────────────────────────────────────

        fn gpt() -> manifold_ui::GraphParamTarget {
            manifold_ui::GraphParamTarget::Generator
        }

        /// Resolve a REAL exposed param id for the scene layer's fog_density
        /// node. P2 slice 2a (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md):
        /// fog_density is a P1-stamped exposed card param from creation —
        /// `migrate_scene_exposures` runs on every bundled generator preset
        /// at load (`bundled_generator_presets.rs`'s own comment), so
        /// SceneStarter's atmosphere node is ALREADY exposed, no
        /// expose-then-arm dance through the scene panel's (now-dead
        /// this slice, per BUG_BACKLOG.md) synthesized-id path needed —
        /// this is byte-for-byte the same "real exposed param" every other
        /// `undo_baseline` fixture below already exercises for effects/
        /// generators, just using the scene layer's own atmosphere param as
        /// the specimen. `h` is unused now that no `PanelAction` dispatch is
        /// needed to arm anything; kept in the signature so every call site
        /// below reads uniformly.
        fn materialized_param(
            _h: &mut Harness,
            project: &mut Project,
            layer_id: &LayerId,
        ) -> manifold_core::effects::ParamId {
            let catalog_default = fog_density_addr(project, layer_id);
            let node_doc_id = density_node_id(&catalog_default);
            let (_, layer) = project
                .timeline
                .find_layer_by_id_mut(layer_id)
                .expect("layer resolves");
            let inst = layer.gen_params_or_init();
            // A freshly-init'd instance still TRACKS its catalog preset
            // (`graph: None`) — `binding_id_for_node_param` only resolves
            // against an instance's OWN graph override, so the effective-def
            // fallback (the same one BUG-260's `display_value` uses) is
            // mandatory here, not optional.
            let real = inst
                .binding_id_for_node_param(node_doc_id, "fog_density")
                .or_else(|| {
                    manifold_core::effects::binding_id_for_node_param_in(
                        &catalog_default,
                        node_doc_id,
                        "fog_density",
                    )
                })
                .expect("SceneStarter's fog_density must already be exposed by P1 stamping");
            // The OLD synth-id `DriverToggle` dispatch this fixture used to
            // run did double duty: it exposed AND armed an enabled driver in
            // one shot (BUG-249's "expose-then-arm"). `driver_toggle_atomic`/
            // `driver_trim_clean`/`driver_trim_stomp` below assume that
            // pre-armed driver as their "before" state — reconstruct it
            // directly (no dispatch needed now that exposure is a given).
            if inst.drivers.as_ref().is_none_or(|ds| ds.is_empty()) {
                inst.drivers = Some(vec![manifold_core::effects::ParameterDriver {
                    param_id: std::borrow::Cow::Owned(real.clone()),
                    beat_division: manifold_core::types::BeatDivision::Quarter,
                    waveform: manifold_core::types::DriverWaveform::Sine,
                    enabled: true,
                    phase: 0.0,
                    base_value: 0.0,
                    trim_min: 0.0,
                    trim_max: 1.0,
                    reversed: false,
                    free_period_beats: None,
                    legacy_param_index: None,
                    is_paused_by_user: false,
                }]);
            }
            std::borrow::Cow::Owned(real)
        }

        /// Immutable read of a layer's generator instance — the probe-side
        /// counterpart of `with_preset_graph_mut` (no immutable variant
        /// exists, and probes only get `&Project`).
        fn gen_inst<'p>(
            p: &'p Project,
            layer_id: &LayerId,
        ) -> &'p manifold_core::effects::PresetInstance {
            let (_, layer) = p.timeline.find_layer_by_id(layer_id).expect("layer resolves");
            layer.gen_params().expect("generator instance materialized")
        }

        fn with_send(project: &mut Project) -> manifold_core::AudioSendId {
            let send = manifold_core::audio_setup::AudioSend::new("Kick");
            let id = send.id.clone();
            project.audio_setup.sends.push(send);
            id
        }

        fn test_feature() -> manifold_core::audio_mod::AudioFeature {
            manifold_core::audio_mod::AudioFeature::new(
                manifold_core::audio_mod::AudioFeatureKind::Amplitude,
                manifold_core::audio_mod::AudioBand::Full,
            )
        }

        // ── Settings sliders ─────────────────────────────────────

        fn master_opacity_case(stomp: bool) {
            let project = Project::default();
            let mut h = Harness::new(None);
            let before = project.settings.master_opacity;
            let after = 0.42f32;
            trio_cycle(
                "master_opacity",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::MasterOpacitySnapshot, p);
                    h.dispatch(&PanelAction::MasterOpacityChanged(0.6), p);
                    h.dispatch(&PanelAction::MasterOpacityChanged(after), p);
                },
                |h, p| h.dispatch(&PanelAction::MasterOpacityCommit, p),
                |p| p.settings.master_opacity,
                before,
                after,
                stomp,
            );
        }

        #[test]
        fn master_opacity_clean() {
            master_opacity_case(false);
        }

        #[test]
        fn master_opacity_stomp() {
            master_opacity_case(true);
        }

        fn led_brightness_case(stomp: bool) {
            let project = Project::default();
            let mut h = Harness::new(None);
            let before = project.settings.led_brightness;
            let after = 0.37f32;
            trio_cycle(
                "led_brightness",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::LedBrightnessSnapshot, p);
                    h.dispatch(&PanelAction::LedBrightnessChanged(0.9), p);
                    h.dispatch(&PanelAction::LedBrightnessChanged(after), p);
                },
                |h, p| h.dispatch(&PanelAction::LedBrightnessCommit, p),
                |p| p.settings.led_brightness,
                before,
                after,
                stomp,
            );
        }

        #[test]
        fn led_brightness_clean() {
            led_brightness_case(false);
        }

        #[test]
        fn led_brightness_stomp() {
            led_brightness_case(true);
        }

        fn macro_case(stomp: bool) {
            let mut project = Project::default();
            project.settings.macro_bank.slots[0].value = 0.2;
            let mut h = Harness::new(None);
            trio_cycle(
                "macro",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::MacroSnapshot(0), p);
                    h.dispatch(&PanelAction::MacroChanged(0, 0.5), p);
                    h.dispatch(&PanelAction::MacroChanged(0, 0.8), p);
                },
                |h, p| h.dispatch(&PanelAction::MacroCommit(0), p),
                |p| p.settings.macro_bank.slots[0].value,
                0.2,
                0.8,
                stomp,
            );
        }

        #[test]
        fn macro_clean() {
            macro_case(false);
        }

        #[test]
        fn macro_stomp() {
            macro_case(true);
        }

        // ── Layer sliders ────────────────────────────────────────

        fn layer_opacity_case(stomp: bool) {
            let (project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let lid = layer_id.clone();
            let before = project
                .timeline
                .find_layer_by_id(&layer_id)
                .map(|(_, l)| l.opacity)
                .unwrap();
            trio_cycle(
                "layer_opacity",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::LayerOpacitySnapshot, p);
                    h.dispatch(&PanelAction::LayerOpacityChanged(0.9), p);
                    h.dispatch(&PanelAction::LayerOpacityChanged(0.55), p);
                },
                |h, p| h.dispatch(&PanelAction::LayerOpacityCommit, p),
                move |p| {
                    p.timeline
                        .find_layer_by_id(&lid)
                        .map(|(_, l)| l.opacity)
                        .unwrap()
                },
                before,
                0.55,
                stomp,
            );
        }

        #[test]
        fn layer_opacity_clean() {
            layer_opacity_case(false);
        }

        #[test]
        fn layer_opacity_stomp() {
            layer_opacity_case(true);
        }

        fn audio_gain_case(stomp: bool) {
            let (project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let lid = layer_id.clone();
            let lid2 = layer_id.clone();
            let before = project
                .timeline
                .find_layer_by_id(&layer_id)
                .map(|(_, l)| l.audio_gain_db)
                .unwrap();
            trio_cycle(
                "audio_gain",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::AudioGainSnapshot(lid.clone()), p);
                    h.dispatch(&PanelAction::AudioGainChanged(lid.clone(), 3.0), p);
                    h.dispatch(&PanelAction::AudioGainChanged(lid.clone(), -6.0), p);
                },
                |h, p| h.dispatch(&PanelAction::AudioGainCommit(lid.clone()), p),
                move |p| {
                    p.timeline
                        .find_layer_by_id(&lid2)
                        .map(|(_, l)| l.audio_gain_db)
                        .unwrap()
                },
                before,
                -6.0,
                stomp,
            );
        }

        #[test]
        fn audio_gain_clean() {
            audio_gain_case(false);
        }

        #[test]
        fn audio_gain_stomp() {
            audio_gain_case(true);
        }

        // ── Card param drag (exposed manifest slot) ──────────────

        fn card_param_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_id);
            let before = gen_inst(&project, &layer_id).get_base_param(pid.as_ref());
            let after = before + 0.25;
            let probe_lid = layer_id.clone();
            let probe_pid = pid.clone();
            trio_cycle(
                "card_param",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::ParamSnapshot(gpt(), pid.clone()), p);
                    h.dispatch(&PanelAction::ParamChanged(gpt(), pid.clone(), before + 0.1), p);
                    h.dispatch(&PanelAction::ParamChanged(gpt(), pid.clone(), after), p);
                },
                |h, p| h.dispatch(&PanelAction::ParamCommit(gpt(), pid.clone()), p),
                move |p| gen_inst(p, &probe_lid).get_base_param(probe_pid.as_ref()),
                before,
                after,
                stomp,
            );
        }

        #[test]
        fn card_param_clean() {
            card_param_case(false);
        }

        #[test]
        fn card_param_stomp() {
            card_param_case(true);
        }

        /// Two SceneStarter generator layers (same structural preset, so a
        /// P1-stamped exposed param id resolves in either instance).
        fn two_scene_layer_project() -> (Project, LayerId, LayerId) {
            let mut project = Project::default();
            let idx_a = project.timeline.add_layer(
                "A",
                LayerType::Generator,
                PresetTypeId::from_string("SceneStarter".to_string()),
            );
            let layer_a = project.timeline.layers[idx_a].layer_id.clone();
            let idx_b = project.timeline.add_layer(
                "B",
                LayerType::Generator,
                PresetTypeId::from_string("SceneStarter".to_string()),
            );
            let layer_b = project.timeline.layers[idx_b].layer_id.clone();
            (project, layer_a, layer_b)
        }

        /// BUG-292: the scene panel's rows dispatch via
        /// `GraphParamTarget::GeneratorOf(<the panel's own bound layer>)`,
        /// never plain `Generator` (which `resolve_graph_target` resolves
        /// through `active_layer`). Panel bound to layer A, app active layer
        /// B — a scene-row write must land on A and leave B untouched, the
        /// exact mismatch the old plain-`Generator` dispatch silently wrote
        /// to the wrong layer under.
        #[test]
        fn bug_292_scene_row_writes_target_the_panels_bound_layer_not_active() {
            let (mut project, layer_a, layer_b) = two_scene_layer_project();
            let mut h = Harness::new(Some(layer_b.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_a);
            let before_a = gen_inst(&project, &layer_a).get_base_param(pid.as_ref());
            let before_b = gen_inst(&project, &layer_b).get_base_param(pid.as_ref());
            let after = before_a + 0.25;

            let target = manifold_ui::GraphParamTarget::GeneratorOf(layer_a.clone());
            h.dispatch(&PanelAction::ParamSnapshot(target.clone(), pid.clone()), &mut project);
            h.dispatch(&PanelAction::ParamChanged(target.clone(), pid.clone(), after), &mut project);
            h.dispatch(&PanelAction::ParamCommit(target, pid.clone()), &mut project);

            assert_eq!(
                gen_inst(&project, &layer_a).get_base_param(pid.as_ref()),
                after,
                "scene row write must land on the panel's BOUND layer (A), not the active layer (B)"
            );
            assert_eq!(
                gen_inst(&project, &layer_b).get_base_param(pid.as_ref()),
                before_b,
                "the active layer (B) must be untouched by a scene row write bound to layer A"
            );
        }

        // ── Modulation trims + envelope handles ──────────────────

        /// Arm a driver (trim 0..1) on a materialized exposed param — the
        /// materialize dispatch itself arms the driver, so this is one call.
        fn arm_driver(
            h: &mut Harness,
            project: &mut Project,
            layer_id: &LayerId,
        ) -> manifold_core::effects::ParamId {
            materialized_param(h, project, layer_id)
        }

        fn driver_trim_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = arm_driver(&mut h, &mut project, &layer_id);
            let probe_lid = layer_id.clone();
            trio_cycle(
                "driver_trim",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(
                        &PanelAction::TrimSnapshot(manifold_ui::panels::TrimKind::Driver, gpt(), pid.clone()),
                        p,
                    );
                    h.dispatch(
                        &PanelAction::TrimChanged(
                            manifold_ui::panels::TrimKind::Driver,
                            gpt(),
                            pid.clone(),
                            0.3,
                            0.9,
                        ),
                        p,
                    );
                },
                |h, p| {
                    h.dispatch(
                        &PanelAction::TrimCommit(manifold_ui::panels::TrimKind::Driver, gpt(), pid.clone()),
                        p,
                    )
                },
                move |p| {
                    let d = &gen_inst(p, &probe_lid).drivers.as_ref().unwrap()[0];
                    (d.trim_min, d.trim_max)
                },
                (0.0, 1.0),
                (0.3, 0.9),
                stomp,
            );
        }

        #[test]
        fn driver_trim_clean() {
            driver_trim_case(false);
        }

        #[test]
        fn driver_trim_stomp() {
            driver_trim_case(true);
        }

        /// Arm an envelope on a materialized exposed param.
        fn arm_envelope(
            h: &mut Harness,
            project: &mut Project,
            layer_id: &LayerId,
        ) -> manifold_core::effects::ParamId {
            let pid = materialized_param(h, project, layer_id);
            let target = manifold_core::GraphTarget::Generator(layer_id.clone());
            project.with_preset_graph_mut(&target, |inst| {
                inst.envelopes = Some(vec![manifold_core::effects::ParamEnvelope {
                    param_id: pid.clone(),
                    enabled: true,
                    target_normalized: 0.2,
                    decay_beats: 1.0,
                    legacy_param_index: None,
                    current_level: 0.0,
                    was_clip_active: false,
                }]);
            });
            pid
        }

        fn envelope_target_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = arm_envelope(&mut h, &mut project, &layer_id);
            let probe_lid = layer_id.clone();
            trio_cycle(
                "envelope_target",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::TargetSnapshot(gpt(), pid.clone()), p);
                    h.dispatch(&PanelAction::TargetChanged(gpt(), pid.clone(), 0.75), p);
                },
                |h, p| h.dispatch(&PanelAction::TargetCommit(gpt(), pid.clone()), p),
                move |p| gen_inst(p, &probe_lid).envelopes.as_ref().unwrap()[0].target_normalized,
                0.2,
                0.75,
                stomp,
            );
        }

        #[test]
        fn envelope_target_clean() {
            envelope_target_case(false);
        }

        #[test]
        fn envelope_target_stomp() {
            envelope_target_case(true);
        }

        fn envelope_decay_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = arm_envelope(&mut h, &mut project, &layer_id);
            let probe_lid = layer_id.clone();
            trio_cycle(
                "envelope_decay",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::EnvDecaySnapshot(gpt(), pid.clone()), p);
                    h.dispatch(&PanelAction::EnvDecayChanged(gpt(), pid.clone(), 3.5), p);
                },
                |h, p| h.dispatch(&PanelAction::EnvDecayCommit(gpt(), pid.clone()), p),
                move |p| gen_inst(p, &probe_lid).envelopes.as_ref().unwrap()[0].decay_beats,
                1.0,
                3.5,
                stomp,
            );
        }

        #[test]
        fn envelope_decay_clean() {
            envelope_decay_case(false);
        }

        #[test]
        fn envelope_decay_stomp() {
            envelope_decay_case(true);
        }

        // ── Audio modulation drawer sliders ──────────────────────

        fn arm_audio_mod(
            h: &mut Harness,
            project: &mut Project,
            layer_id: &LayerId,
        ) -> manifold_core::effects::ParamId {
            let send_id = with_send(project);
            let pid = materialized_param(h, project, layer_id);
            let target = manifold_core::GraphTarget::Generator(layer_id.clone());
            project.with_preset_graph_mut(&target, |inst| {
                inst.audio_mods_mut().push(
                    manifold_core::audio_mod::ParameterAudioMod::new(
                        pid.clone(),
                        send_id,
                        test_feature(),
                    ),
                );
            });
            pid
        }

        fn audio_mod_shape_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = arm_audio_mod(&mut h, &mut project, &layer_id);
            let before = gen_inst(&project, &layer_id)
                .find_audio_mod(pid.as_ref())
                .map(|m| m.shape.sensitivity)
                .unwrap();
            let probe_lid = layer_id.clone();
            let probe_pid = pid.clone();
            trio_cycle(
                "audio_mod_shape",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::AudioModShapeSnapshot(gpt(), pid.clone()), p);
                    h.dispatch(
                        &PanelAction::AudioModShapeParamChanged(
                            gpt(),
                            pid.clone(),
                            manifold_ui::panels::AudioShapeParam::Sensitivity,
                            0.83,
                        ),
                        p,
                    );
                },
                |h, p| h.dispatch(&PanelAction::AudioModShapeCommit(gpt(), pid.clone()), p),
                move |p| {
                    gen_inst(p, &probe_lid)
                        .find_audio_mod(probe_pid.as_ref())
                        .map(|m| m.shape.sensitivity)
                        .unwrap()
                },
                before,
                0.83,
                stomp,
            );
        }

        #[test]
        fn audio_mod_shape_clean() {
            audio_mod_shape_case(false);
        }

        #[test]
        fn audio_mod_shape_stomp() {
            audio_mod_shape_case(true);
        }

        fn audio_trigger_shape_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let send_id = with_send(&mut project);
            let (_, layer) = project.timeline.find_layer_by_id_mut(&layer_id).unwrap();
            layer.clip_triggers.push(
                manifold_core::audio_trigger::LayerClipTrigger::new(
                    manifold_core::audio_mod::AudioModSource {
                        send_id,
                        feature: test_feature(),
                    },
                ),
            );
            let before = project
                .timeline
                .find_layer_by_id(&layer_id)
                .map(|(_, l)| l.clip_triggers[0].shape.sensitivity)
                .unwrap();
            let mut h = Harness::new(Some(layer_id.clone()));
            let lid = layer_id.clone();
            let lid2 = layer_id.clone();
            let lid3 = layer_id.clone();
            trio_cycle(
                "audio_trigger_shape",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::AudioTriggerShapeSnapshot(lid.clone(), 0), p);
                    h.dispatch(
                        &PanelAction::AudioTriggerShapeParamChanged(
                            lid.clone(),
                            0,
                            manifold_ui::panels::AudioShapeParam::Sensitivity,
                            0.91,
                        ),
                        p,
                    );
                },
                move |h, p| h.dispatch(&PanelAction::AudioTriggerShapeCommit(lid2.clone(), 0), p),
                move |p| {
                    p.timeline
                        .find_layer_by_id(&lid3)
                        .map(|(_, l)| l.clip_triggers[0].shape.sensitivity)
                        .unwrap()
                },
                before,
                0.91,
                stomp,
            );
        }

        #[test]
        fn audio_trigger_shape_clean() {
            audio_trigger_shape_case(false);
        }

        #[test]
        fn audio_trigger_shape_stomp() {
            audio_trigger_shape_case(true);
        }

        // ── Audio Setup panel drags ──────────────────────────────

        fn audio_send_gain_case(stomp: bool) {
            let mut project = Project::default();
            let send_id = with_send(&mut project);
            let before = project.audio_setup.find_send(&send_id).unwrap().gain_db;
            let mut h = Harness::new(None);
            let sid = send_id.clone();
            let sid2 = send_id.clone();
            let sid3 = send_id.clone();
            trio_cycle(
                "audio_send_gain",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::AudioSendGainDragBegin(sid.clone()), p);
                    h.dispatch(&PanelAction::AudioSendGainDragChanged(sid.clone(), 4.0), p);
                    h.dispatch(&PanelAction::AudioSendGainDragChanged(sid.clone(), -3.0), p);
                },
                move |h, p| h.dispatch(&PanelAction::AudioSendGainDragCommit(sid2.clone()), p),
                move |p| p.audio_setup.find_send(&sid3).unwrap().gain_db,
                before,
                -3.0,
                stomp,
            );
        }

        #[test]
        fn audio_send_gain_clean() {
            audio_send_gain_case(false);
        }

        #[test]
        fn audio_send_gain_stomp() {
            audio_send_gain_case(true);
        }

        fn audio_crossover_case(stomp: bool) {
            let project = Project::default();
            let before = (project.audio_setup.low_hz, project.audio_setup.mid_hz);
            let after = (before.0, before.1 + 1000.0);
            let mut h = Harness::new(None);
            trio_cycle(
                "audio_crossover",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::AudioCrossoverDragBegin, p);
                    h.dispatch(
                        &PanelAction::AudioCrossoverChanged(manifold_ui::BandDivider::Mid, after.1),
                        p,
                    );
                },
                |h, p| h.dispatch(&PanelAction::AudioCrossoverCommit, p),
                |p| (p.audio_setup.low_hz, p.audio_setup.mid_hz),
                before,
                after,
                stomp,
            );
        }

        #[test]
        fn audio_crossover_clean() {
            audio_crossover_case(false);
        }

        #[test]
        fn audio_crossover_stomp() {
            audio_crossover_case(true);
        }

        // ── Relight knobs ────────────────────────────────────────

        fn relight_param_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let _pid = materialized_param(&mut h, &mut project, &layer_id);
            let field = manifold_ui::panels::UiRelightField::Gain;
            let core_field = crate::ui_translate::relight_field_to_editing(field);
            let before = core_field.get(&gen_inst(&project, &layer_id).relight_params);
            let after = before + 0.5;
            let probe_lid = layer_id.clone();
            trio_cycle(
                "relight_param",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::RelightParamSnapshot(gpt(), field), p);
                    h.dispatch(&PanelAction::RelightParamChanged(gpt(), field, after), p);
                },
                |h, p| h.dispatch(&PanelAction::RelightParamCommit(gpt(), field), p),
                move |p| core_field.get(&gen_inst(p, &probe_lid).relight_params),
                before,
                after,
                stomp,
            );
        }

        #[test]
        fn relight_param_clean() {
            relight_param_case(false);
        }

        #[test]
        fn relight_param_stomp() {
            relight_param_case(true);
        }

        // ── Atomic one-shots (buttons / toggles) ─────────────────

        /// Atomic gesture: dispatch once, feed everything to the content
        /// side, assert the undo cycle.
        fn atomic_cycle<P>(
            label: &str,
            project: Project,
            h: &mut Harness,
            gesture: impl Fn(&mut Harness, &mut Project),
            probe: impl Fn(&Project) -> P,
            before: P,
            after: P,
        ) where
            P: PartialEq + std::fmt::Debug,
        {
            let mut side = ContentSide::new(&project);
            let mut project = project;
            gesture(h, &mut project);
            assert_undo_cycle(&mut side, h.drain(), probe, before, after, label);
        }

        #[test]
        fn param_toggle_atomic() {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_id);
            let before = gen_inst(&project, &layer_id).get_base_param(pid.as_ref());
            let after = if before > 0.5 { 0.0 } else { 1.0 };
            let probe_lid = layer_id.clone();
            let probe_pid = pid.clone();
            atomic_cycle(
                "param_toggle",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::ParamToggle(gpt(), pid.clone()), p);
                },
                move |p| gen_inst(p, &probe_lid).get_base_param(probe_pid.as_ref()),
                before,
                after,
            );
        }

        #[test]
        fn param_fire_atomic() {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_id);
            let before = gen_inst(&project, &layer_id).get_base_param(pid.as_ref());
            let probe_lid = layer_id.clone();
            let probe_pid = pid.clone();
            atomic_cycle(
                "param_fire",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::ParamFire(gpt(), pid.clone()), p);
                },
                move |p| gen_inst(p, &probe_lid).get_base_param(probe_pid.as_ref()),
                before,
                before + 1.0,
            );
        }

        #[test]
        fn driver_toggle_atomic() {
            // The materialize dispatch arms an ENABLED driver; the toggle
            // under test flips it off (one undo unit).
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_id);
            let lid = layer_id.clone();
            let probe_pid = pid.clone();
            atomic_cycle(
                "driver_toggle_disarm",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::DriverToggle(gpt(), pid.clone()), p);
                },
                move |p| {
                    gen_inst(p, &lid)
                        .drivers
                        .as_ref()
                        .and_then(|ds| ds.iter().find(|d| d.param_id == probe_pid).map(|d| d.enabled))
                        .unwrap_or(true)
                },
                true,
                false,
            );
        }

        #[test]
        fn envelope_toggle_atomic() {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_id);
            let lid = layer_id.clone();
            let probe_pid = pid.clone();
            atomic_cycle(
                "envelope_toggle_arm",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::EnvelopeToggle(gpt(), pid.clone()), p);
                },
                move |p| {
                    gen_inst(p, &lid)
                        .envelopes
                        .as_ref()
                        .map(|es| es.iter().filter(|e| e.param_id == probe_pid).count())
                        .unwrap_or(0)
                },
                0usize,
                1usize,
            );
        }

        #[test]
        fn audio_mod_toggle_atomic() {
            let (mut project, layer_id) = scene_layer_project();
            with_send(&mut project);
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = materialized_param(&mut h, &mut project, &layer_id);
            let lid = layer_id.clone();
            let probe_pid = pid.clone();
            atomic_cycle(
                "audio_mod_toggle_arm",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::AudioModToggle(gpt(), pid.clone()), p);
                },
                move |p| {
                    gen_inst(p, &lid)
                        .find_audio_mod(probe_pid.as_ref())
                        .map(|m| m.enabled)
                        .unwrap_or(false)
                },
                false,
                true,
            );
        }

        #[test]
        fn audio_trigger_add_then_toggle_then_remove_atomic() {
            let (mut project, layer_id) = scene_layer_project();
            with_send(&mut project);
            let mut h = Harness::new(Some(layer_id.clone()));
            let mut side = ContentSide::new(&project);
            let probe = |p: &Project, lid: &LayerId| {
                p.timeline
                    .find_layer_by_id(lid)
                    .map(|(_, l)| (l.clip_triggers.len(), l.clip_triggers.first().map(|t| t.enabled)))
                    .unwrap()
            };
            // Add: one undo unit, (0, None) → (1, Some(true)) — the clip-trigger
            // drawer redesign lands an ENABLED kick trigger so one click fires.
            h.dispatch(&PanelAction::AudioTriggerAdd(layer_id.clone()), &mut project);
            let cmds = h.drain();
            assert_undo_cycle(
                &mut side,
                cmds,
                |p| probe(p, &layer_id),
                (0usize, None),
                (1usize, Some(true)),
                "audio_trigger_add",
            );
            // Toggle the fresh (already-enabled) row off: one undo unit,
            // Some(true) → Some(false).
            h.dispatch(
                &PanelAction::AudioTriggerEnabledToggle(layer_id.clone(), 0),
                &mut project,
            );
            let cmds = h.drain();
            assert_undo_cycle(
                &mut side,
                cmds,
                |p| probe(p, &layer_id),
                (1usize, Some(true)),
                (1usize, Some(false)),
                "audio_trigger_toggle",
            );
            // Remove: one undo unit, back to (0, None).
            h.dispatch(&PanelAction::AudioTriggerRemove(layer_id.clone(), 0), &mut project);
            let cmds = h.drain();
            assert_undo_cycle(
                &mut side,
                cmds,
                |p| probe(p, &layer_id),
                (1usize, Some(false)),
                (0usize, None),
                "audio_trigger_remove",
            );
        }

        #[test]
        fn audio_send_gain_typed_atomic() {
            let mut project = Project::default();
            let send_id = with_send(&mut project);
            let before = project.audio_setup.find_send(&send_id).unwrap().gain_db;
            let mut h = Harness::new(None);
            let sid = send_id.clone();
            let sid2 = send_id.clone();
            atomic_cycle(
                "audio_send_gain_typed",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::AudioSendGainSetTyped(sid.clone(), 7.5), p);
                },
                move |p| p.audio_setup.find_send(&sid2).unwrap().gain_db,
                before,
                7.5,
            );
        }

        #[test]
        fn audio_send_floor_step_atomic() {
            let mut project = Project::default();
            let send_id = with_send(&mut project);
            let before = project.audio_setup.find_send(&send_id).unwrap().floor_db;
            let mut h = Harness::new(None);
            let sid = send_id.clone();
            let sid2 = send_id.clone();
            atomic_cycle(
                "audio_send_floor_step",
                project,
                &mut h,
                move |h, p| {
                    h.dispatch(&PanelAction::AudioSendFloorStep(sid.clone(), 1.0), p);
                },
                move |p| p.audio_setup.find_send(&sid2).unwrap().floor_db,
                before,
                -100.0,
            );
        }

        // ── Ableton macro trim + step-amount (same stomp class) ──

        fn ableton_macro_trim_case(stomp: bool) {
            let mut project = Project::default();
            project.settings.macro_bank.slots[0].ableton_mapping =
                Some(manifold_core::ableton_mapping::AbletonParamMapping {
                    param_id: std::borrow::Cow::Owned("m0".to_string()),
                    address: manifold_core::ableton_mapping::AbletonMacroAddress {
                        track_id: 0,
                        device_id: 0,
                        param_id: 0,
                        device_identity: manifold_core::ableton_mapping::AbletonDeviceIdentity {
                            device_class_name: "InstrumentGroupDevice".to_string(),
                        },
                        track_name: String::new(),
                        device_name: String::new(),
                        macro_name: String::new(),
                    },
                    range_min: 0.0,
                    range_max: 1.0,
                    inverted: false,
                    legacy_param_index: None,
                    last_value: 0.0,
                    status: manifold_core::ableton_mapping::AbletonMappingStatus::default(),
                });
            let mut h = Harness::new(None);
            trio_cycle(
                "ableton_macro_trim",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::AbletonMacroTrimSnapshot(0), p);
                    h.dispatch(&PanelAction::AbletonMacroTrimChanged(0, 0.2, 0.7), p);
                },
                |h, p| h.dispatch(&PanelAction::AbletonMacroTrimCommit(0), p),
                |p| {
                    let m = p.settings.macro_bank.slots[0].ableton_mapping.as_ref().unwrap();
                    (m.range_min, m.range_max)
                },
                (0.0, 1.0),
                (0.2, 0.7),
                stomp,
            );
        }

        #[test]
        fn ableton_macro_trim_clean() {
            ableton_macro_trim_case(false);
        }

        #[test]
        fn ableton_macro_trim_stomp() {
            ableton_macro_trim_case(true);
        }

        fn audio_mod_step_amount_case(stomp: bool) {
            let (mut project, layer_id) = scene_layer_project();
            let mut h = Harness::new(Some(layer_id.clone()));
            let pid = arm_audio_mod(&mut h, &mut project, &layer_id);
            // The Step-amount row only exists while the action is Step.
            let target = manifold_core::GraphTarget::Generator(layer_id.clone());
            project.with_preset_graph_mut(&target, |inst| {
                if let Some(m) = inst
                    .audio_mods
                    .as_mut()
                    .and_then(|ms| ms.iter_mut().find(|a| a.param_id == pid))
                {
                    m.action = manifold_core::audio_mod::TriggerAction::Step {
                        amount: 0.1,
                        wrap: manifold_core::audio_mod::WrapMode::Wrap,
                    };
                }
            });
            let probe_lid = layer_id.clone();
            let probe_pid = pid.clone();
            let read_amount = move |p: &Project| {
                match gen_inst(p, &probe_lid).find_audio_mod(probe_pid.as_ref()).map(|m| m.action) {
                    Some(manifold_core::audio_mod::TriggerAction::Step { amount, .. }) => amount,
                    _ => f32::NAN,
                }
            };
            trio_cycle(
                "audio_mod_step_amount",
                project,
                &mut h,
                |h, p| {
                    h.dispatch(&PanelAction::AudioModStepAmountSnapshot(gpt(), pid.clone()), p);
                    h.dispatch(&PanelAction::AudioModStepAmountChanged(gpt(), pid.clone(), 0.65), p);
                },
                |h, p| h.dispatch(&PanelAction::AudioModStepAmountCommit(gpt(), pid.clone()), p),
                read_amount,
                0.1,
                0.65,
                stomp,
            );
        }

        #[test]
        fn audio_mod_step_amount_clean() {
            audio_mod_step_amount_case(false);
        }

        #[test]
        fn audio_mod_step_amount_stomp() {
            audio_mod_step_amount_case(true);
        }

        // ── Clip gestures (timeline host path) ───────────────────

        use manifold_ui::timeline_editing_host::TimelineEditingHost;

        /// Owns the pieces `AppEditingHost` borrows, so a test can build the
        /// host, drive a gesture, drop the host, then drain the channel.
        struct ClipRig {
            project: Project,
            tx: crossbeam_channel::Sender<ContentCommand>,
            rx: crossbeam_channel::Receiver<ContentCommand>,
            content_state: crate::content_state::ContentState,
            cursor: manifold_ui::cursors::CursorManager,
            active_layer: Option<LayerId>,
            needs_rebuild: bool,
            needs_structural_sync: bool,
            scroll_dirty: crate::ui_root::ScrollDirty,
            invalidate: Vec<usize>,
            pre_drag: Vec<Box<dyn manifold_editing::command::Command>>,
        }

        impl ClipRig {
            fn new(project: Project) -> Self {
                let (tx, rx) = crossbeam_channel::unbounded();
                Self {
                    project,
                    tx,
                    rx,
                    content_state: crate::content_state::ContentState::default(),
                    cursor: manifold_ui::cursors::CursorManager::default(),
                    active_layer: None,
                    needs_rebuild: false,
                    needs_structural_sync: false,
                    scroll_dirty: crate::ui_root::ScrollDirty::default(),
                    invalidate: Vec::new(),
                    pre_drag: Vec::new(),
                }
            }

            fn host(&mut self) -> crate::editing_host::AppEditingHost<'_> {
                crate::editing_host::AppEditingHost::new(
                    &mut self.project,
                    &self.tx,
                    &self.content_state,
                    &mut self.cursor,
                    &mut self.active_layer,
                    &mut self.needs_rebuild,
                    &mut self.needs_structural_sync,
                    &mut self.scroll_dirty,
                    &mut self.invalidate,
                    &mut self.pre_drag,
                )
            }

            fn drain(&self) -> Vec<ContentCommand> {
                self.rx.try_iter().collect()
            }
        }

        /// One video layer + one clip [4..8] created through the REAL host
        /// path; both the rig's project and the returned content side carry it.
        fn clip_project() -> (ClipRig, ContentSide, manifold_core::ClipId) {
            let mut project = Project::default();
            project.timeline.add_layer(
                "V",
                manifold_core::types::LayerType::Video,
                manifold_core::PresetTypeId::from_string("Video".to_string()),
            );
            let mut rig = ClipRig::new(project);
            let clip_id = rig
                .host()
                .create_clip_at_position(manifold_core::Beats(4.0), 0, manifold_core::Beats(4.0))
                .expect("clip creation resolves");
            // Setup is NOT under test: the content side simply starts from the
            // post-create project with an empty undo history.
            rig.drain();
            let side = ContentSide::new(&rig.project);
            (rig, side, clip_id)
        }

        /// Immutable clip lookup (the timeline's `find_clip_by_id` takes &mut
        /// for its cache; probes only get `&Project`).
        fn find_clip<'p>(p: &'p Project, id: &manifold_core::ClipId) -> Option<&'p manifold_core::clip::TimelineClip> {
            p.timeline
                .layers
                .iter()
                .flat_map(|l| l.clips.iter())
                .find(|c| c.id == *id)
        }

        fn clip_start(p: &Project, id: &manifold_core::ClipId) -> manifold_core::Beats {
            find_clip(p, id).map(|c| c.start_beat).expect("clip resolves")
        }

        fn clip_duration(p: &Project, id: &manifold_core::ClipId) -> manifold_core::Beats {
            find_clip(p, id).map(|c| c.duration_beats).expect("clip resolves")
        }

        fn clip_count(p: &Project) -> usize {
            p.timeline.layers.iter().map(|l| l.clips.len()).sum()
        }

        #[test]
        fn clip_create_atomic() {
            // clip_project's setup IS the create gesture — verify it recorded
            // exactly one undo unit that round-trips. Rebuild it inline so the
            // drain isn't consumed as setup.
            let mut project = Project::default();
            project.timeline.add_layer(
                "V",
                manifold_core::types::LayerType::Video,
                manifold_core::PresetTypeId::from_string("Video".to_string()),
            );
            let mut rig = ClipRig::new(project);
            let mut side = ContentSide::new(&rig.project);
            let id = rig
                .host()
                .create_clip_at_position(manifold_core::Beats(4.0), 0, manifold_core::Beats(4.0));
            assert!(id.is_some(), "create resolves a clip id");
            assert_undo_cycle(
                &mut side,
                rig.drain(),
                clip_count,
                0usize,
                1usize,
                "clip_create",
            );
        }

        #[test]
        fn clip_move_atomic() {
            let (mut rig, mut side, clip_id) = clip_project();
            {
                let mut host = rig.host();
                host.begin_command_batch();
                host.set_clip_start_beat(clip_id.as_str(), manifold_core::Beats(12.0));
                host.record_move(clip_id.as_str(), manifold_core::Beats(4.0), manifold_core::Beats(12.0), 0, 0);
                host.commit_command_batch("Move Clip");
            }
            let pid = clip_id.clone();
            assert_undo_cycle(
                &mut side,
                rig.drain(),
                move |p| clip_start(p, &pid),
                manifold_core::Beats(4.0),
                manifold_core::Beats(12.0),
                "clip_move",
            );
        }

        #[test]
        fn clip_trim_atomic() {
            let (mut rig, mut side, clip_id) = clip_project();
            let old_dur = clip_duration(&rig.project, &clip_id);
            {
                let mut host = rig.host();
                host.begin_command_batch();
                host.set_clip_trim(
                    clip_id.as_str(),
                    manifold_core::Beats(4.0),
                    manifold_core::Beats(2.0),
                    manifold_core::Seconds(0.0),
                );
                host.record_trim(
                    clip_id.as_str(),
                    manifold_core::Beats(4.0),
                    manifold_core::Beats(4.0),
                    old_dur,
                    manifold_core::Beats(2.0),
                    manifold_core::Seconds(0.0),
                    manifold_core::Seconds(0.0),
                );
                host.commit_command_batch("Trim Clip");
            }
            let pid = clip_id.clone();
            assert_undo_cycle(
                &mut side,
                rig.drain(),
                move |p| clip_duration(p, &pid),
                old_dur,
                manifold_core::Beats(2.0),
                "clip_trim",
            );
        }

        #[test]
        fn clip_delete_atomic() {
            let (mut rig, mut side, clip_id) = clip_project();
            let mut ui = UIRoot::new();
            let mut selection = manifold_ui::UIState::new();
            let mut active_layer = None;
            let mut prefs = crate::user_prefs::UserPrefs::for_test();
            crate::ui_bridge::editing::dispatch_editing(
                &PanelAction::ContextDeleteClip(clip_id.to_string()),
                &mut rig.project,
                &rig.tx,
                &rig.content_state,
                &mut ui,
                &mut selection,
                &mut active_layer,
                &mut prefs,
            );
            assert_undo_cycle(
                &mut side,
                rig.drain(),
                clip_count,
                1usize,
                0usize,
                "clip_delete",
            );
        }

        /// WIDGET_TREE_DESIGN.md §7 P4, gaps #2/#3 carried from
        /// `docs/landings/2026-07-21-widget-tree-p2.md`: bridge-level
        /// dispatch tests for the modulation-family action kinds — every
        /// test above this point dispatches against a GENERATOR target
        /// only. Each kind here dispatches the SAME `PanelAction` against
        /// BOTH a master-effect `GraphTarget` and a layer-effect
        /// `GraphTarget`, through the identical generic path
        /// (`resolve_mod_target`/`resolve_graph_target` →
        /// `with_preset_graph_mut`/`DriverTarget::from`/`Project::
        /// find_effect_by_id`) — the "fixed for Master, forgot Layer" class
        /// detector the twin consolidation (D2) exists to make impossible.
        ///
        /// Master and layer targets are reached via `Harness::
        /// dispatch_with_editor`'s `editor_target = Some(GraphTarget::
        /// Effect(id))` — the graph editor's own identity-addressed entry
        /// point (`resolve_effect_id`/`editor_dispatch_context`, `ui_bridge/
        /// mod.rs`). That is the only production path that can select a
        /// SPECIFIC effect instance from a bridge-level test: the ambient
        /// route (`editor_target: None`) resolves positionally through
        /// `ui.inspector.last_effect_tab()`, a field only a real
        /// pointer-down sets (`InspectorPanel::update_last_effect_tab`,
        /// private to `manifold-ui`) — driving that is P2's own click-
        /// routing test surface, not this bridge's.
        mod row_dispatch {
            use super::*;

            /// A real effect instance — `init_defaults()` populates `params`
            /// from the registry the same way a live Add Effect does — plus
            /// its first manifest param id.
            fn effect_with_first_param(
                effect_type: manifold_core::PresetTypeId,
            ) -> (PresetInstance, manifold_core::effects::ParamId) {
                let mut fx = PresetInstance::new(effect_type);
                fx.init_defaults();
                let pid = manifold_core::preset_definition_registry::try_get(fx.effect_type())
                    .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()))
                    .expect("preset has at least one manifest param");
                (fx, std::borrow::Cow::Owned(pid))
            }

            /// One project carrying the SAME preset type as both a master
            /// effect and a layer effect, so a test can dispatch the
            /// identical action against either `GraphTarget` and compare.
            struct TwoScopes {
                project: Project,
                master_target: manifold_core::GraphTarget,
                layer_target: manifold_core::GraphTarget,
                pid: manifold_core::effects::ParamId,
            }

            fn two_scopes(effect_type: &'static str) -> TwoScopes {
                let et = manifold_core::PresetTypeId::new(effect_type);
                let (master_fx, pid) = effect_with_first_param(et.clone());
                let master_id = master_fx.id.clone();
                let (layer_fx, _) = effect_with_first_param(et);
                let layer_effect_id = layer_fx.id.clone();

                let (mut project, layer_id) = scene_layer_project();
                project.settings.master_effects.push(master_fx);
                project
                    .timeline
                    .find_layer_by_id_mut(&layer_id)
                    .expect("fixture layer resolves")
                    .1
                    .effects_mut()
                    .push(layer_fx);

                TwoScopes {
                    project,
                    master_target: manifold_core::GraphTarget::Effect(master_id),
                    layer_target: manifold_core::GraphTarget::Effect(layer_effect_id),
                    pid,
                }
            }

            fn arm_driver(
                project: &mut Project,
                target: &manifold_core::GraphTarget,
                param_id: &manifold_core::effects::ParamId,
            ) {
                project.with_preset_graph_mut(target, |inst| {
                    inst.drivers = Some(vec![ParameterDriver {
                        param_id: param_id.clone(),
                        beat_division: BeatDivision::Quarter,
                        waveform: DriverWaveform::Sine,
                        enabled: true,
                        phase: 0.0,
                        base_value: 0.0,
                        trim_min: 0.0,
                        trim_max: 1.0,
                        reversed: false,
                        free_period_beats: None,
                        legacy_param_index: None,
                        is_paused_by_user: false,
                    }]);
                });
            }

            fn arm_ableton_mapping(
                project: &mut Project,
                target: &manifold_core::GraphTarget,
                param_id: &manifold_core::effects::ParamId,
            ) {
                project.with_preset_graph_mut(target, |inst| {
                    inst.ableton_mappings = Some(vec![manifold_core::ableton_mapping::AbletonParamMapping {
                        param_id: param_id.clone(),
                        address: manifold_core::ableton_mapping::AbletonMacroAddress {
                            track_id: 0,
                            device_id: 0,
                            param_id: 0,
                            device_identity: manifold_core::ableton_mapping::AbletonDeviceIdentity {
                                device_class_name: "InstrumentGroupDevice".to_string(),
                            },
                            track_name: String::new(),
                            device_name: String::new(),
                            macro_name: String::new(),
                        },
                        range_min: 0.0,
                        range_max: 1.0,
                        inverted: false,
                        legacy_param_index: None,
                        last_value: 0.0,
                        status: manifold_core::ableton_mapping::AbletonMappingStatus::default(),
                    }]);
                });
            }

            /// Dispatch `action` against `target` via `editor_target`, drain
            /// into a fresh `ContentSide`, and assert the atomic-gesture
            /// undo-cycle shape `atomic_cycle` proves for the generator
            /// target — replicated per scope target here.
            fn scope_atomic<P>(
                label: &str,
                project: Project,
                target: &manifold_core::GraphTarget,
                action: PanelAction,
                probe: impl Fn(&Project) -> P,
                before: P,
                after: P,
            ) where
                P: PartialEq + std::fmt::Debug,
            {
                let mut side = ContentSide::new(&project);
                let mut project = project;
                let mut h = Harness::new(None);
                h.dispatch_with_editor(&action, &mut project, Some(target));
                assert_undo_cycle(&mut side, h.drain(), probe, before, after, label);
            }

            /// The gesture must produce NO commands at all (dispatch is a
            /// documented no-op for this scope) and leave `probe` unchanged.
            fn scope_inert<P>(
                label: &str,
                mut project: Project,
                target: &manifold_core::GraphTarget,
                action: PanelAction,
                probe: impl Fn(&Project) -> P,
            ) where
                P: PartialEq + std::fmt::Debug,
            {
                let before = probe(&project);
                let mut h = Harness::new(None);
                h.dispatch_with_editor(&action, &mut project, Some(target));
                let cmds = h.drain();
                assert!(
                    cmds.is_empty(),
                    "{label}: must be a documented no-op for this scope; got {} commands",
                    cmds.len()
                );
                assert_eq!(probe(&project), before, "{label}: state must not move");
            }

            // ── DriverToggle ──────────────────────────────────────────

            #[test]
            fn driver_toggle_master() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.master_target.clone();
                scope_atomic(
                    "driver_toggle_master",
                    s.project,
                    &s.master_target,
                    PanelAction::DriverToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone()),
                    move |p| {
                        p.preset_instance(&t)
                            .and_then(|inst| inst.drivers.as_ref())
                            .and_then(|ds| ds.iter().find(|d| d.param_id == pid).map(|d| d.enabled))
                    },
                    None,
                    Some(true),
                );
            }

            #[test]
            fn driver_toggle_layer() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.layer_target.clone();
                scope_atomic(
                    "driver_toggle_layer",
                    s.project,
                    &s.layer_target,
                    PanelAction::DriverToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone()),
                    move |p| {
                        p.preset_instance(&t)
                            .and_then(|inst| inst.drivers.as_ref())
                            .and_then(|ds| ds.iter().find(|d| d.param_id == pid).map(|d| d.enabled))
                    },
                    None,
                    Some(true),
                );
            }

            // ── AudioModToggle ────────────────────────────────────────

            #[test]
            fn audio_mod_toggle_master() {
                let mut s = two_scopes("Bloom");
                with_send(&mut s.project);
                let pid = s.pid.clone();
                let t = s.master_target.clone();
                scope_atomic(
                    "audio_mod_toggle_master",
                    s.project,
                    &s.master_target,
                    PanelAction::AudioModToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone()),
                    move |p| p.preset_instance(&t).and_then(|inst| inst.find_audio_mod(pid.as_ref())).map(|m| m.enabled),
                    None,
                    Some(true),
                );
            }

            #[test]
            fn audio_mod_toggle_layer() {
                let mut s = two_scopes("Bloom");
                with_send(&mut s.project);
                let pid = s.pid.clone();
                let t = s.layer_target.clone();
                scope_atomic(
                    "audio_mod_toggle_layer",
                    s.project,
                    &s.layer_target,
                    PanelAction::AudioModToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone()),
                    move |p| p.preset_instance(&t).and_then(|inst| inst.find_audio_mod(pid.as_ref())).map(|m| m.enabled),
                    None,
                    Some(true),
                );
            }

            // ── EnvelopeToggle — layer arms; master is a documented
            //    no-op ("effects are clip-timed", inspector.rs's own
            //    comment on the handler) ─────────────────────────────

            #[test]
            fn envelope_toggle_layer_arms() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.layer_target.clone();
                scope_atomic(
                    "envelope_toggle_layer",
                    s.project,
                    &s.layer_target,
                    PanelAction::EnvelopeToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone()),
                    move |p| {
                        p.preset_instance(&t)
                            .and_then(|inst| inst.envelopes.as_ref())
                            .map(|es| es.iter().filter(|e| e.param_id == pid).count())
                            .unwrap_or(0)
                    },
                    0usize,
                    1usize,
                );
            }

            #[test]
            fn envelope_toggle_master_is_inert() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.master_target.clone();
                scope_inert(
                    "envelope_toggle_master",
                    s.project,
                    &s.master_target,
                    PanelAction::EnvelopeToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone()),
                    move |p| {
                        p.preset_instance(&t)
                            .and_then(|inst| inst.envelopes.as_ref())
                            .map(|es| es.iter().filter(|e| e.param_id == pid).count())
                            .unwrap_or(0)
                    },
                );
            }

            // ── ParamToggle / ParamFire ───────────────────────────────

            #[test]
            fn param_toggle_master() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.master_target.clone();
                let before = s.project.preset_instance(&s.master_target).unwrap().get_base_param(pid.as_ref());
                let after = if before > 0.5 { 0.0 } else { 1.0 };
                scope_atomic(
                    "param_toggle_master",
                    s.project,
                    &s.master_target,
                    PanelAction::ParamToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone()),
                    move |p| p.preset_instance(&t).unwrap().get_base_param(pid.as_ref()),
                    before,
                    after,
                );
            }

            #[test]
            fn param_toggle_layer() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.layer_target.clone();
                let before = s.project.preset_instance(&s.layer_target).unwrap().get_base_param(pid.as_ref());
                let after = if before > 0.5 { 0.0 } else { 1.0 };
                scope_atomic(
                    "param_toggle_layer",
                    s.project,
                    &s.layer_target,
                    PanelAction::ParamToggle(manifold_ui::GraphParamTarget::Effect(0), pid.clone()),
                    move |p| p.preset_instance(&t).unwrap().get_base_param(pid.as_ref()),
                    before,
                    after,
                );
            }

            #[test]
            fn param_fire_master() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.master_target.clone();
                let before = s.project.preset_instance(&s.master_target).unwrap().get_base_param(pid.as_ref());
                scope_atomic(
                    "param_fire_master",
                    s.project,
                    &s.master_target,
                    PanelAction::ParamFire(manifold_ui::GraphParamTarget::Effect(0), pid.clone()),
                    move |p| p.preset_instance(&t).unwrap().get_base_param(pid.as_ref()),
                    before,
                    before + 1.0,
                );
            }

            #[test]
            fn param_fire_layer() {
                let s = two_scopes("Bloom");
                let pid = s.pid.clone();
                let t = s.layer_target.clone();
                let before = s.project.preset_instance(&s.layer_target).unwrap().get_base_param(pid.as_ref());
                scope_atomic(
                    "param_fire_layer",
                    s.project,
                    &s.layer_target,
                    PanelAction::ParamFire(manifold_ui::GraphParamTarget::Effect(0), pid.clone()),
                    move |p| p.preset_instance(&t).unwrap().get_base_param(pid.as_ref()),
                    before,
                    before + 1.0,
                );
            }

            // ── DriverConfig (one representative: a BeatDiv click) ────

            #[test]
            fn driver_config_beat_div_master() {
                let mut s = two_scopes("Bloom");
                arm_driver(&mut s.project, &s.master_target, &s.pid);
                let pid = s.pid.clone();
                let t = s.master_target.clone();
                scope_atomic(
                    "driver_config_beat_div_master",
                    s.project,
                    &s.master_target,
                    PanelAction::DriverConfig(
                        manifold_ui::GraphParamTarget::Effect(0),
                        pid.clone(),
                        DriverConfigAction::BeatDiv(4), // -> Half
                    ),
                    move |p| {
                        p.preset_instance(&t)
                            .and_then(|inst| inst.drivers.as_ref())
                            .and_then(|ds| ds.iter().find(|d| d.param_id == pid).map(|d| d.beat_division))
                    },
                    Some(BeatDivision::Quarter),
                    Some(BeatDivision::Half),
                );
            }

            #[test]
            fn driver_config_beat_div_layer() {
                let mut s = two_scopes("Bloom");
                arm_driver(&mut s.project, &s.layer_target, &s.pid);
                let pid = s.pid.clone();
                let t = s.layer_target.clone();
                scope_atomic(
                    "driver_config_beat_div_layer",
                    s.project,
                    &s.layer_target,
                    PanelAction::DriverConfig(
                        manifold_ui::GraphParamTarget::Effect(0),
                        pid.clone(),
                        DriverConfigAction::BeatDiv(4), // -> Half
                    ),
                    move |p| {
                        p.preset_instance(&t)
                            .and_then(|inst| inst.drivers.as_ref())
                            .and_then(|ds| ds.iter().find(|d| d.param_id == pid).map(|d| d.beat_division))
                    },
                    Some(BeatDivision::Quarter),
                    Some(BeatDivision::Half),
                );
            }

            // ── AbletonInvertToggle — NOT undo-tracked (mirrors
            //    `TrimChanged`'s Ableton branch: a `MutateProject` write,
            //    no `Execute`) — assert the flip lands identically on both
            //    scopes without asserting a false undo requirement ───────

            fn ableton_invert_case(label: &str, mut project: Project, target: &manifold_core::GraphTarget, pid: manifold_core::effects::ParamId) {
                let t = target.clone();
                let p2 = pid.clone();
                let probe = move |proj: &Project| -> Option<bool> {
                    proj.preset_instance(&t)
                        .and_then(|inst| inst.ableton_mappings.as_ref())
                        .and_then(|ms| ms.iter().find(|m| m.param_id == p2).map(|m| m.inverted))
                };
                let before = probe(&project).expect("fixture must start with a mapping present");
                assert!(!before, "fixture starts uninverted");

                let mut side = ContentSide::new(&project);
                let mut h = Harness::new(None);
                h.dispatch_with_editor(
                    &PanelAction::AbletonInvertToggle(manifold_ui::GraphParamTarget::Effect(0), pid),
                    &mut project,
                    Some(target),
                );
                assert_eq!(probe(&project), Some(true), "{label}: local project must flip");

                let cmds = h.drain();
                assert!(
                    !cmds.iter().any(|c| matches!(c, ContentCommand::Execute(_))),
                    "{label}: Ableton mapping edits are deliberately not undo-tracked"
                );
                let landed = side.apply(cmds);
                assert_eq!(landed, 0, "{label}: no undo-tracked command should land");
                assert_eq!(probe(&side.project), Some(true), "{label}: content mirror must flip too");
            }

            #[test]
            fn ableton_invert_toggle_master() {
                let mut s = two_scopes("Bloom");
                arm_ableton_mapping(&mut s.project, &s.master_target, &s.pid);
                let pid = s.pid.clone();
                ableton_invert_case("ableton_invert_master", s.project, &s.master_target, pid);
            }

            #[test]
            fn ableton_invert_toggle_layer() {
                let mut s = two_scopes("Bloom");
                arm_ableton_mapping(&mut s.project, &s.layer_target, &s.pid);
                let pid = s.pid.clone();
                ableton_invert_case("ableton_invert_layer", s.project, &s.layer_target, pid);
            }
        }
    }

    /// BUG-262 regression. The mapping-sidebar range/affine drags dispatch
    /// through `app_render`'s `pending_actions` loop, not the inspector host
    /// the matrix above drives, so they can't ride `trio_cycle`. What made
    /// them lose undo entries was a mid-gesture full-snapshot *stomp*
    /// reverting the in-flight reshape before the commit read it back via
    /// `watched_reshape` — the exact failure `ActiveInspectorDrag::apply` now
    /// prevents. These prove the restore at that level: given the guard a live
    /// drag installs, a stale pre-drag snapshot must come back carrying the
    /// dragged value, so the commit sees new != old and records one undo.
    mod mapping_undo_baseline {
        use super::*;

        /// A master effect carrying one user param binding whose reshape lives
        /// in the per-instance graph (mirrors the editing crate's
        /// `project_with_one_user_binding`). Pre-drag range 0..1, affine 1/0.
        fn project_with_binding() -> (Project, manifold_core::GraphTarget, String) {
            let mut project = Project::default();
            let mut fx = manifold_core::effects::PresetInstance::new(
                manifold_core::PresetTypeId::new("Mirror"),
            );
            let effect_id = fx.id.clone();
            let binding_id = "user.uv_transform.rotation.1".to_string();
            fx.append_user_binding(manifold_core::effects::UserParamBinding {
                id: binding_id.clone(),
                label: "Original Label".to_string(),
                node_id: manifold_core::NodeId::new("uv_transform"),
                legacy_node_handle: None,
                inner_param: "rotation".to_string(),
                min: 0.0,
                max: 1.0,
                default_value: 0.25,
                convert: manifold_core::effects::ParamConvert::Float,
                is_angle: false,
                invert: false,
                curve: manifold_core::macro_bank::MacroCurve::Linear,
                scale: 1.0,
                offset: 0.0,
                value_labels: Vec::new(),
                section: None,
            });
            project.settings.master_effects.push(fx);
            (
                project,
                manifold_core::GraphTarget::Effect(effect_id),
                binding_id,
            )
        }

        /// Read the binding's live `(min, max, scale, offset)` back the way
        /// `watched_reshape` does — through the synthesized binding view.
        fn reshape(project: &Project, id: &str) -> (f32, f32, f32, f32) {
            let b = project.settings.master_effects[0]
                .user_param_bindings()
                .into_iter()
                .find(|b| b.id == id)
                .expect("binding present");
            (b.min, b.max, b.scale, b.offset)
        }

        #[test]
        fn mapping_range_drag_survives_snapshot_stomp() {
            let (project, target, binding_id) = project_with_binding();
            let (min0, max0, _, _) = reshape(&project, &binding_id);
            assert_eq!((min0, max0), (0.0, 1.0), "fixture starts at the default range");

            // The guard a live range drag installs (in-flight range 0.2..0.8).
            let guard = crate::app::ActiveInspectorDrag::MappingRange {
                target,
                param_id: binding_id.clone(),
                min: 0.2,
                max: 0.8,
            };
            // A full snapshot lands mid-drag carrying the stale pre-drag
            // project; app_render restores the guarded drag onto it.
            let mut stomped = project.clone();
            guard.apply(&mut stomped);

            let (min, max, _, _) = reshape(&stomped, &binding_id);
            assert_eq!(
                (min, max),
                (0.2, 0.8),
                "range stomp must be undone so the commit sees new != old and records undo"
            );
        }

        #[test]
        fn mapping_affine_drag_survives_snapshot_stomp() {
            let (project, target, binding_id) = project_with_binding();
            let (_, _, scale0, offset0) = reshape(&project, &binding_id);
            assert_eq!((scale0, offset0), (1.0, 0.0), "fixture starts at identity affine");

            let guard = crate::app::ActiveInspectorDrag::MappingAffine {
                target,
                param_id: binding_id.clone(),
                scale: 2.5,
                offset: -0.5,
            };
            let mut stomped = project.clone();
            guard.apply(&mut stomped);

            let (_, _, scale, offset) = reshape(&stomped, &binding_id);
            assert_eq!(
                (scale, offset),
                (2.5, -0.5),
                "affine stomp must be undone so the commit sees new != old and records undo"
            );
        }
    }

    /// BUG-266: the inspector tab pin was clearing on ANY `selection_version`
    /// bump, including ones from command side effects (add-effect's
    /// behind-the-scenes selection touch) that never change WHICH thing is
    /// selected. Three probes on the real path (`state_sync::
    /// sync_inspector_data`, the same fn the app's per-frame sync calls):
    /// a version bump with unchanged selection identity must not clear the
    /// pin; a genuine identity change must; a transient empty selection
    /// (clear-then-reselect churn) must not.
    mod bug_266_tab_pin {
        use super::*;
        use manifold_ui::InspectorTab;

        fn active_tab(
            ui: &mut UIRoot,
            project: &Project,
            active_layer: Option<usize>,
            selection: &SelectionState,
        ) -> InspectorTab {
            crate::ui_bridge::state_sync::sync_inspector_data(
                ui,
                project,
                active_layer,
                selection,
                &[],
            );
            ui.inspector.active_tab()
        }

        #[test]
        fn incidental_version_bump_does_not_clear_the_pin() {
            let (project, layer_id) = scene_layer_project();
            let idx = project.timeline.find_layer_index_by_id(&layer_id).unwrap();
            let mut ui = UIRoot::new();
            let mut selection = SelectionState::new();
            selection.select_layer(layer_id.clone());
            selection.pin_scope(InspectorTab::Master);
            assert_eq!(
                active_tab(&mut ui, &project, Some(idx), &selection),
                InspectorTab::Master
            );

            // What add-effect's behind-the-scenes selection touch looks like
            // at the ui_state level: re-selecting the SAME layer bumps
            // `selection_version` without changing WHICH layer is selected.
            let before = selection.selection_version;
            selection.select_layer(layer_id.clone());
            assert!(
                selection.selection_version > before,
                "sanity: version must actually bump"
            );
            assert_eq!(
                active_tab(&mut ui, &project, Some(idx), &selection),
                InspectorTab::Master,
                "pin must survive a version bump that doesn't change WHICH layer is selected"
            );
        }

        #[test]
        fn genuine_selection_change_clears_the_pin() {
            let (mut project, layer_id) = scene_layer_project();
            let idx2 = project.timeline.add_layer(
                "Scene2",
                LayerType::Generator,
                PresetTypeId::from_string("SceneStarter".to_string()),
            );
            let layer_id_2 = project.timeline.layers[idx2].layer_id.clone();
            let idx1 = project.timeline.find_layer_index_by_id(&layer_id).unwrap();

            let mut ui = UIRoot::new();
            let mut selection = SelectionState::new();
            selection.select_layer(layer_id.clone());
            selection.pin_scope(InspectorTab::Master);
            assert_eq!(
                active_tab(&mut ui, &project, Some(idx1), &selection),
                InspectorTab::Master
            );

            selection.select_layer(layer_id_2.clone());
            let idx2 = project.timeline.find_layer_index_by_id(&layer_id_2).unwrap();
            assert_eq!(
                active_tab(&mut ui, &project, Some(idx2), &selection),
                InspectorTab::Layer,
                "a genuine selection change (different layer) must drop the pin back to \
                 the selection-derived default"
            );
        }

        #[test]
        fn transient_empty_selection_holds_the_pin() {
            let (project, layer_id) = scene_layer_project();
            let idx = project.timeline.find_layer_index_by_id(&layer_id).unwrap();
            let mut ui = UIRoot::new();
            let mut selection = SelectionState::new();
            selection.select_layer(layer_id.clone());
            selection.pin_scope(InspectorTab::Master);
            assert_eq!(
                active_tab(&mut ui, &project, Some(idx), &selection),
                InspectorTab::Master
            );

            // Clear-then-reselect churn: an empty selection observed
            // mid-gesture must not itself kill the pin.
            selection.clear_layer_selection();
            assert_eq!(
                active_tab(&mut ui, &project, None, &selection),
                InspectorTab::Master,
                "a transient empty selection must not clear the pin"
            );

            // ...and reselecting the SAME layer afterward still finds it pinned.
            selection.select_layer(layer_id.clone());
            assert_eq!(
                active_tab(&mut ui, &project, Some(idx), &selection),
                InspectorTab::Master
            );
        }
    }
}
