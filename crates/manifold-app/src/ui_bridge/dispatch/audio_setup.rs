//! Inspector dispatch handlers: the audio_setup domain (UI_FUNNEL_DECOMPOSITION
//! P-B, D6) — the layer-owned AUDIO TRIGGERS section (`AUDIO_SETUP_DOCK_AND_
//! TRIGGER_UNIFICATION_DESIGN.md` D2/D5) and the project-level Audio Setup
//! (send routing, gain/floor, crossovers). One slice of the inspector
//! dispatch, reached by `dispatch_inspector`'s first-non-unhandled chain. Arms
//! are the former `dispatch_inspector` arms VERBATIM (they already read `ctx`
//! fields directly); a `_ => unhandled()` fall-through lets the chain advance.
//!
//! No D-11 preamble: every arm here addresses its target directly (`LayerId`
//! + index for clip triggers, `AudioSendId` for sends, no positional
//!   `(tab, active_layer)` resolution), so `effective_tab`/`effective_active_
//!   layer` are never needed.

use manifold_editing::commands::audio_setup::{
    AddAudioSendCommand, RemoveAudioSendCommand, RenameAudioSendCommand, SetAudioCrossoversCommand,
    SetAudioInputDeviceCommand, SetAudioSendChannelsCommand, SetAudioSendFloorCommand,
    SetAudioSendGainCommand,
};
use manifold_editing::commands::layer::{
    AddLayerClipTriggerCommand, RemoveLayerClipTriggerCommand, SetLayerClipTriggerCommand,
};
use manifold_ui::AudioSetupAction;

use super::super::DispatchResult;
use super::resolve::audio_setup_command;

/// Send gain trim range (dB) — shared by the stepper (`AudioSendGainStep`, here)
/// and the D7 calibration drag (`ValueRef::AudioSendGain`, now in `scrub.rs`).
pub(crate) const AUDIO_SEND_GAIN_MIN_DB: f32 = -24.0;
pub(crate) const AUDIO_SEND_GAIN_MAX_DB: f32 = 24.0;

pub(crate) fn dispatch_audio_setup(action: &AudioSetupAction, ctx: &mut super::super::DispatchCtx) -> DispatchResult {
    use crate::content_command::ContentCommand;

    match action {

        // ── Layer-owned clip triggers (P3b, AUDIO_SETUP_DOCK_AND_TRIGGER_
        // UNIFICATION_DESIGN.md D2/D5) — the inspector's AUDIO TRIGGERS
        // section. Addressed directly by `LayerId` + index (no
        // `resolve_graph_target`/`editor_target` involved — a clip trigger
        // isn't a graph param). Mutations route through P2's
        // Add/Remove/SetLayerClipTriggerCommand — whole-value-replace, same
        // shape as `SetAudioModTriggerModeCommand`.
        AudioSetupAction::AudioTriggerSectionToggle => {
            ctx.ui.inspector.audio_trigger_section_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        AudioSetupAction::AudioTriggerRowExpandToggle(_layer_id, index) => {
            ctx.ui.inspector.audio_trigger_section_mut().toggle_row_expanded(*index);
            DispatchResult::structural()
        }
        AudioSetupAction::AudioTriggerAdd(layer_id) => {
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
        AudioSetupAction::AudioTriggerRemove(layer_id, index) => {
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                Box::new(RemoveLayerClipTriggerCommand::new(layer_id.clone(), *index));
            boxed.execute(ctx.project);
            ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        AudioSetupAction::AudioTriggerEnabledToggle(layer_id, index) => {
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
        AudioSetupAction::AudioTriggerSetSource(layer_id, index, send_id, feature) => {
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
        // Layer clip-trigger shaping-slider scrub trio migrated to the unified
        // `PanelAction::Scrub` wire (`ValueRef::AudioTriggerShape`, P-I / D4):
        // `(LayerId, index)` addresses the trigger, the `AudioShapeParam` rides
        // the address, the whole shape is the restore payload, and Commit emits
        // `SetLayerClipTriggerCommand`.
        AudioSetupAction::AudioTriggerSetLength(layer_id, index, beats) => {
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
        AudioSetupAction::AudioSetDevice(device) => {
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
        AudioSetupAction::AudioAddSend => {
            let send = manifold_core::audio_setup::AudioSend::new(format!(
                "Audio {}",
                ctx.project.audio_setup.sends.len() + 1
            ));
            audio_setup_command(ctx.project, ctx.content_tx, Box::new(AddAudioSendCommand::new(send)))
        }
        AudioSetupAction::AudioRemoveSend(id) => audio_setup_command(
            ctx.project,
            ctx.content_tx,
            Box::new(RemoveAudioSendCommand::new(id.clone())),
        ),
        AudioSetupAction::AudioRenameSend(id, label) => {
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
        AudioSetupAction::AudioSetSendChannels(id, ch) => {
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
        AudioSetupAction::AudioSendGainStep(id, delta_db) => {
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
        // Send-gain calibration-drag trio migrated to the unified
        // `PanelAction::Scrub` wire (`ValueRef::AudioSendGain`, P-I / D4): keyed
        // by `AudioSendId`, the raw dB rides `ScrubValue::Scalar` (Move clamps +
        // pushes a live edit), Commit emits `SetAudioSendGainCommand`. The
        // stepper / type-in / floor actions below stay one-shot commands.
        // P4 (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D8, audio-dock sibling):
        // the type-in commit — ONE undo step, no clamp. Unlike
        // `AudioSendGainDragChanged`'s live-drag path, a typed value is free
        // to exceed `AUDIO_SEND_GAIN_MIN_DB`/`MAX_DB` (PARAM_RANGE_CONTRACT
        // P1: those are the stepper's display travel, not a hard limit).
        AudioSetupAction::AudioSendGainSetTyped(id, new_db) => {
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
        AudioSetupAction::AudioSendFloorStep(id, delta_db) => {
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
        AudioSetupAction::AudioCrossoverDragBegin => {
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
        AudioSetupAction::AudioCrossoverChanged(band, hz) => {
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
        AudioSetupAction::AudioCrossoverCommit => {
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

    }
}
