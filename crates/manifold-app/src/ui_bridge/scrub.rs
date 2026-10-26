//! The scrub-gesture engine — one addressed gesture, four operations
//! (UI_FUNNEL_DECOMPOSITION P-I, D4).
//!
//! A value-scrub gesture (slider drag, knob, discrete enum cycle) arrives on
//! the wire as `PanelAction::Scrub(ValueRef, ScrubPhase)` (manifold-ui). This
//! module resolves each [`manifold_ui::ValueRef`] to a core write target and
//! runs the phase's operation. **Begin** resolves, reads the pre-gesture value
//! as the undo baseline, and stores a [`ResolvedScrub`] record. **Move** updates
//! the record's live value and sends a non-undoable write to the content thread
//! only; the UI's project copy keeps the pre-drag value. **Commit** builds the
//! one undo command from the record and sends it for the content thread to run.
//! The record keeps drawing the dragged value until the snapshot carrying the
//! commit arrives ([`ScrubState::snapshot_arrived`]).
//!
//! MIGRATION STATE (P-I complete for the restore slot): every scrubable family
//! — panel-wired (`PanelAction::Scrub`) and frame-resident (the graph-editor
//! mapping drags + graph-canvas node-param drags, driven from `app_render`'s
//! pending-actions loop, not the wire) — rides the single `active` slot.
//! [`ScrubState::check_single_active_on_begin`] machine-checks that only one
//! gesture is ever live (one pointer, one gesture).

use crate::content_command::ContentCommand;
use manifold_core::audio_mod::{AudioModShape, TriggerAction, WrapMode};
use manifold_core::effects::ParamId;
use manifold_core::project::Project;
use manifold_core::{GraphTarget, LayerId};
use manifold_editing::command::Command;
use manifold_editing::commands::ableton::ChangeAbletonTrimCommand;
use manifold_editing::commands::audio_mod::{SetAudioModActionCommand, SetAudioModShapeCommand};
use manifold_editing::commands::audio_setup::{SetAudioCrossoversCommand, SetAudioSendGainCommand};
use manifold_editing::commands::drivers::ChangeTrimCommand;
use manifold_editing::commands::effect_groups::ChangeGroupWetDryCommand;
use manifold_editing::commands::effect_target::DriverTarget;
use manifold_editing::commands::effects::ChangeGraphParamCommand;
use manifold_editing::commands::envelopes::{
    ChangeEnvelopeDecayCommand, ChangeEnvelopeTargetCommand, SetEnvelopeActionCommand,
};
use manifold_editing::commands::layer::{SetLayerAudioGainCommand, SetLayerClipTriggerCommand};
use manifold_editing::commands::settings::{
    ChangeLayerOpacityCommand, ChangeLedBrightnessCommand, ChangeMacroCommand,
    ChangeMasterOpacityCommand,
};
use manifold_ui::panels::TrimKind;
use manifold_ui::panels::param_slider_shared::RowShapes;
use manifold_ui::{AudioShapeParam, ScrubPhase, ValueRef};

use super::dispatch::audio_setup::{AUDIO_SEND_GAIN_MAX_DB, AUDIO_SEND_GAIN_MIN_DB};
use super::dispatch::resolve::{
    ableton_mapping_target, clip_trigger_shape_move, graph_audio_mod_move, graph_driver_move,
    graph_env_move, resolve_graph_target,
};
use super::{DispatchCtx, DispatchResult};

/// The single in-flight scrub gesture (P-I). `active` holds the undo baseline
/// plus the live value for whichever gesture is live — panel-wired or
/// frame-resident — or `None` when none is. One pointer means one gesture, so
/// one slot: [`Self::check_single_active_on_begin`] flags any Begin that finds
/// the slot already occupied (a prior gesture that leaked it).
#[derive(Default)]
pub struct ScrubState {
    /// The single live gesture: undo baseline + live value.
    pub active: Option<ResolvedScrub>,
    /// A committed gesture and the data version its commit lands at. It keeps
    /// drawing until that project snapshot arrives, so a released control
    /// never shows its old value for a frame.
    landing: Option<(ResolvedScrub, u64)>,
}

impl ScrubState {
    /// The gesture whose value displays draw: the live one, else a committed
    /// one still waiting for its snapshot.
    pub fn drawn(&self) -> Option<&ResolvedScrub> {
        self.active
            .as_ref()
            .or(self.landing.as_ref().map(|(record, _)| record))
    }

    /// A project snapshot at `data_version` arrived: a landed gesture whose
    /// commit it carries has nothing left to draw.
    pub fn snapshot_arrived(&mut self, data_version: u64) {
        if self
            .landing
            .as_ref()
            .is_some_and(|(_, lands_at)| data_version >= *lands_at)
        {
            self.landing = None;
        }
    }

    /// Machine-check the single-active-gesture invariant (P-I): at a gesture's
    /// Begin/open, `active` must be empty — one pointer can drive only one
    /// gesture at a time. A non-empty slot means a prior gesture leaked without
    /// committing; loud in debug (assert) and in release (warn), then the caller
    /// overwrites (the pre-P-I behavior a leaked snapshot field also had). Called
    /// at BOTH entry points: the dispatch wire ([`dispatch_scrub`]) and the
    /// frame-resident gesture opens (mapping drags, node-param drags).
    pub fn check_single_active_on_begin(&self, entry: &str) {
        if self.active.is_some() {
            debug_assert!(
                false,
                "single-active-gesture invariant violated: {entry} Begin while a scrub gesture is already active"
            );
            log::warn!(
                "[scrub] single-active-gesture invariant: {entry} Begin while a gesture is already active — the prior gesture leaked its slot"
            );
        }
    }
}

/// A resolved, self-contained scrub gesture: the undo baseline plus the
/// resolved write target and the latest live value. One variant per scrubable
/// family — panel-wired and frame-resident alike.
pub enum ResolvedScrub {
    ParamRgb {
        target: GraphTarget,
        param_ids: [ParamId; 3],
        preset: manifold_core::PresetTypeId,
        baseline: [f32; 3],
        live: [f32; 3],
    },
    /// An exposed card param on an effect/generator graph. `baseline` is the
    /// pre-gesture base value the commit diffs against; `live` is the latest
    /// dragged value.
    Param {
        target: GraphTarget,
        param_id: ParamId,
        baseline: f32,
        live: f32,
    },
    /// Card params and unbound node params of one graph moved as one
    /// gesture: a gizmo turning a direction, or moving, turning or scaling
    /// a selection (REALTIME_3D_DESIGN.md D8). Each keeps its own knob; the
    /// gesture lands as one undo entry named `label`.
    ParamGroup {
        target: GraphTarget,
        cards: Vec<CardScrub>,
        nodes: Vec<crate::app_render::UnboundNodeParamDrag>,
        label: &'static str,
    },
    /// The master-opacity slider (`settings.master_opacity`).
    MasterOpacity { baseline: f32, live: f32 },
    /// The LED master-brightness slider (`settings.led_brightness`).
    LedBrightness { baseline: f32, live: f32 },
    /// A layer's opacity — `layer_id` captured at Begin.
    LayerOpacity {
        layer_id: LayerId,
        baseline: f32,
        live: f32,
    },
    /// A rack header wet/dry slider; the target is resolved at Begin.
    RackWetDry {
        target: manifold_editing::commands::effect_target::EffectTarget,
        group_id: manifold_core::EffectGroupId,
        baseline: f32,
        live: f32,
    },
    /// A macro-bank knob (`idx`).
    Macro {
        idx: usize,
        baseline: f32,
        live: f32,
    },
    /// A layer's audio-input gain (dB) — `layer_id` captured at Begin.
    LayerAudioGain {
        layer_id: LayerId,
        baseline: f32,
        live: f32,
    },
    /// A modulation trim-range handle (driver / audio-mod / Ableton sub-range
    /// bars). `kind` selects the store; `ableton_target` is resolved only for
    /// `TrimKind::Ableton` (`None` for driver/audio). `baseline`/`live` are
    /// `(min, max)` pairs.
    Trim {
        kind: TrimKind,
        target: GraphTarget,
        ableton_target: Option<manifold_core::ableton_mapping::AbletonMappingTarget>,
        param_id: ParamId,
        baseline: (f32, f32),
        live: (f32, f32),
    },
    /// An envelope target handle (`target_normalized`) — `target`/`param_id`
    /// resolved at Begin.
    EnvelopeTarget {
        target: GraphTarget,
        param_id: ParamId,
        baseline: f32,
        live: f32,
    },
    /// An envelope decay slider (`decay_beats`) — resolved at Begin.
    EnvDecay {
        target: GraphTarget,
        param_id: ParamId,
        baseline: f32,
        live: f32,
    },
    /// An audio-mod drawer shaping slider — holds the WHOLE shape at both
    /// `baseline` and `live` (the drag edits one scalar; the other two hold).
    /// `which` scalar is drag-local and carried on the wire, not here.
    AudioModShape {
        target: GraphTarget,
        param_id: ParamId,
        baseline: AudioModShape,
        live: AudioModShape,
    },
    /// An audio-mod Step-action amount slider. The undo `baseline` is the WHOLE
    /// pre-drag `TriggerAction`; the commit re-stamps `live_amount` with the
    /// baseline's wrap.
    AudioModStepAmount {
        target: GraphTarget,
        param_id: ParamId,
        baseline: TriggerAction,
        live_amount: f32,
    },
    /// An envelope Step-action amount slider; mirrors `AudioModStepAmount` for
    /// the T drawer.
    EnvelopeStepAmount {
        target: GraphTarget,
        param_id: ParamId,
        baseline: TriggerAction,
        live_amount: f32,
    },
    /// A layer clip-trigger drawer shaping slider (AudioSetup-domain twin of
    /// `AudioModShape`). Addressed by `(layer_id, index)` into `clip_triggers`;
    /// holds the WHOLE shape at both `baseline` and `live`.
    AudioTriggerShape {
        layer_id: LayerId,
        index: usize,
        baseline: AudioModShape,
        live: AudioModShape,
    },
    /// An Audio Setup send-gain (dB) calibration drag. `baseline` is the pre-drag
    /// gain the commit diffs against; `live` is the latest clamped dB.
    AudioSendGain {
        send_id: manifold_core::AudioSendId,
        baseline: f32,
        live: f32,
    },
    /// An Audio Setup band-divider (crossover) drag. Global; `baseline`/`live`
    /// are the whole `(low_hz, mid_hz)` pair.
    AudioCrossover {
        baseline: (f32, f32),
        live: (f32, f32),
    },
    /// An Ableton macro-bank trim-bar drag. Keyed by the macro slot `slot_idx`;
    /// `baseline`/`live` are the `(min, max)` range. Distinct from `Trim` — this
    /// addresses a macro-bank slot, not a `GraphParamTarget`.
    AbletonMacroTrim {
        slot_idx: usize,
        baseline: (f32, f32),
        live: (f32, f32),
    },
    /// A timeline-marker drag. NOT a `PanelAction::Scrub` family — the gesture
    /// is viewport-driven (`ViewportDrag::MarkerDrag` → the
    /// `MarkerAction::MarkerDrag{Started,Moved,Ended}` arms in `marker.rs`).
    /// `baseline` is the pre-drag beat the commit diffs against, `live` the
    /// in-flight beat the markers draw at.
    Marker {
        marker_id: manifold_core::MarkerId,
        baseline: f32,
        live: f32,
    },
    /// A graph-editor mapping-sidebar range drag (`EffectMappingRange*`).
    /// NOT a `PanelAction::Scrub` family — dispatched from `editor_bridge`.
    /// `baseline`/`live` are `(min, max)` pairs.
    MappingRange {
        target: GraphTarget,
        param_id: String,
        baseline: (f32, f32),
        live: (f32, f32),
    },
    /// A graph-editor mapping-sidebar affine (scale/offset) drag
    /// (`EffectMappingAffine*`). Same frame-resident story as `MappingRange`;
    /// `baseline`/`live` are `(scale, offset)` pairs.
    MappingAffine {
        target: GraphTarget,
        param_id: String,
        baseline: (f32, f32),
        live: (f32, f32),
    },
    /// A card-bound graph-node-face param scrub (`BoundNodeParamDrag`). NOT a
    /// `PanelAction::Scrub` family — driven from the graph canvas's
    /// `SetGraphNodeParam` reroute arm in `app_render`. The wrapped struct holds
    /// the undo baseline, the in-flight card value and the session identity.
    BoundNodeParam(Box<crate::app_render::BoundNodeParamDrag>),
    /// An unbound graph-node-face param scrub (`UnboundNodeParamDrag`). Its
    /// live writes go through `MutateProject` only; the record holds the
    /// session so the one undo-worthy commit sees it.
    UnboundNodeParam(Box<crate::app_render::UnboundNodeParamDrag>),
}

impl ResolvedScrub {
    /// The in-flight value of card param `param_id` on `target` while this
    /// gesture drags it. Drawing reads this before live values and the
    /// project, so a dragged slider shows where the pointer put it.
    pub(crate) fn live_param(&self, target: &GraphTarget, param_id: &str) -> Option<f32> {
        match self {
            ResolvedScrub::Param {
                target: t,
                param_id: p,
                live,
                ..
            } if t == target && p.as_ref() == param_id => Some(*live),
            ResolvedScrub::ParamRgb {
                target: t,
                param_ids,
                live,
                ..
            } if t == target => param_ids
                .iter()
                .position(|p| p.as_ref() == param_id)
                .map(|i| live[i]),
            ResolvedScrub::ParamGroup {
                target: t, cards, ..
            } if t == target => cards
                .iter()
                .find(|c| c.id.as_ref() == param_id)
                .map(|c| c.live),
            ResolvedScrub::BoundNodeParam(drag) => drag.live_card_value(target, param_id),
            _ => None,
        }
    }

    /// The in-flight value of macro `idx` while this gesture drags it.
    pub(crate) fn live_macro(&self, idx: usize) -> Option<f32> {
        match self {
            ResolvedScrub::Macro { idx: i, live, .. } if *i == idx => Some(*live),
            _ => None,
        }
    }

    /// Lay the in-flight value over `shapes`, the scalars one card row draws,
    /// when this gesture is on `(target, param_id)`. Only a field the row
    /// already has is filled: a row without a driver shows no driver trim.
    pub(crate) fn overlay_shapes(
        &self,
        target: &GraphTarget,
        param_id: &str,
        shapes: &mut RowShapes,
    ) {
        match self {
            ResolvedScrub::Trim {
                kind: TrimKind::Driver,
                target: t,
                param_id: p,
                live,
                ..
            } if t == target && p.as_ref() == param_id => {
                if let Some(range) = &mut shapes.driver_trim {
                    *range = *live;
                }
            }
            ResolvedScrub::Trim {
                kind: TrimKind::Audio,
                target: t,
                param_id: p,
                live,
                ..
            } if t == target && p.as_ref() == param_id => {
                if let Some(audio) = &mut shapes.audio {
                    audio.range_min = live.0;
                    audio.range_max = live.1;
                }
            }
            ResolvedScrub::Trim {
                kind: TrimKind::Ableton,
                target: t,
                param_id: p,
                live,
                ..
            } if t == target && p.as_ref() == param_id => {
                if let Some(range) = &mut shapes.ableton_range {
                    *range = *live;
                }
            }
            ResolvedScrub::EnvelopeTarget {
                target: t,
                param_id: p,
                live,
                ..
            } if t == target && p.as_ref() == param_id => {
                if let Some(envelope) = &mut shapes.envelope {
                    envelope.target_norm = *live;
                }
            }
            ResolvedScrub::EnvDecay {
                target: t,
                param_id: p,
                live,
                ..
            } if t == target && p.as_ref() == param_id => {
                if let Some(envelope) = &mut shapes.envelope {
                    envelope.decay = *live;
                }
            }
            ResolvedScrub::AudioModShape {
                target: t,
                param_id: p,
                live,
                ..
            } if t == target && p.as_ref() == param_id => {
                if let Some(audio) = &mut shapes.audio {
                    audio.sensitivity = live.sensitivity;
                    audio.attack_ms = live.attack_ms;
                    audio.release_ms = live.release_ms;
                }
            }
            ResolvedScrub::AudioModStepAmount {
                target: t,
                param_id: p,
                live_amount,
                ..
            } if t == target && p.as_ref() == param_id => {
                if let Some(step) = shapes.audio.as_mut().and_then(|a| a.step_amount.as_mut()) {
                    *step = *live_amount;
                }
            }
            ResolvedScrub::EnvelopeStepAmount {
                target: t,
                param_id: p,
                live_amount,
                ..
            } if t == target && p.as_ref() == param_id => {
                if let Some(step) = shapes
                    .envelope
                    .as_mut()
                    .and_then(|e| e.step_amount.as_mut())
                {
                    *step = *live_amount;
                }
            }
            _ => {}
        }
    }
}

/// Send a gesture's one undoable edit for the content thread to run, and keep
/// drawing the gesture until the project snapshot that carries it arrives.
/// `data_version` is the content state's version the commit lands after.
pub(crate) fn send_commit(
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    scrub: &mut ScrubState,
    data_version: u64,
    record: ResolvedScrub,
    cmd: Box<dyn Command + Send>,
) {
    ContentCommand::send(content_tx, ContentCommand::Execute(cmd));
    scrub.landing = Some((record, data_version + 1));
}

/// Whether two scalars differ by more than the commit tolerance.
fn moved_by_eps(a: f32, b: f32) -> bool {
    (a - b).abs() > f32::EPSILON
}

/// The envelope index of `param_id` on `target`, for an envelope command.
fn envelope_index(project: &Project, target: &GraphTarget, param_id: &ParamId) -> Option<usize> {
    project
        .with_preset_graph(target, |inst| {
            inst.envelopes
                .as_ref()
                .and_then(|es| es.iter().position(|e| e.param_id == *param_id))
        })
        .flatten()
}

/// The Step action with `amount`, keeping the wrap `action` already has (a
/// Step's own wrap, else the default `Wrap`).
fn stepped(action: TriggerAction, amount: f32) -> TriggerAction {
    let wrap = match action {
        TriggerAction::Step { wrap, .. } => wrap,
        _ => WrapMode::Wrap,
    };
    TriggerAction::Step { amount, wrap }
}

/// The trim-range command for a `Trim` record: the kind's store decides the
/// command, and the indexes and targets are read from `project` by param id.
fn trim_command(
    project: &Project,
    kind: TrimKind,
    target: &GraphTarget,
    ableton_target: Option<&manifold_core::ableton_mapping::AbletonMappingTarget>,
    param_id: &ParamId,
    baseline: (f32, f32),
    live: (f32, f32),
) -> Option<Box<dyn Command + Send>> {
    if !(moved_by_eps(baseline.0, live.0) || moved_by_eps(baseline.1, live.1)) {
        return None;
    }
    match kind {
        TrimKind::Driver => {
            let index = project
                .with_preset_graph(target, |inst| {
                    inst.drivers
                        .as_ref()
                        .and_then(|ds| ds.iter().position(|d| d.param_id == *param_id))
                })
                .flatten()?;
            Some(Box::new(ChangeTrimCommand::new(
                DriverTarget::from(target),
                index,
                baseline.0,
                baseline.1,
                live.0,
                live.1,
            )))
        }
        TrimKind::Audio => {
            let shape = project
                .with_preset_graph(target, |inst| {
                    inst.audio_mods
                        .as_ref()
                        .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                        .map(|m| m.shape)
                })
                .flatten()?;
            let mut old_shape = shape;
            old_shape.range_min = baseline.0;
            old_shape.range_max = baseline.1;
            let mut new_shape = shape;
            new_shape.range_min = live.0;
            new_shape.range_max = live.1;
            Some(Box::new(SetAudioModShapeCommand::new(
                DriverTarget::from(target),
                param_id.clone(),
                old_shape,
                new_shape,
            )))
        }
        TrimKind::Ableton => Some(Box::new(ChangeAbletonTrimCommand::new(
            ableton_target?.clone(),
            baseline.0,
            baseline.1,
            live.0,
            live.1,
        ))),
    }
}

/// The one undoable edit a slider gesture lands, built from its record: the
/// baseline is the old value, the live value the new one. `None` when nothing
/// moved. Groups, markers, mapping ranges and node params land their own
/// commands, so they fall through to `None` here.
fn commit_command(project: &Project, record: &ResolvedScrub) -> Option<Box<dyn Command + Send>> {
    match record {
        ResolvedScrub::Param {
            target,
            param_id,
            baseline,
            live,
        } if moved_by_eps(*baseline, *live) => Some(Box::new(ChangeGraphParamCommand::new(
            target.clone(),
            param_id.clone(),
            *baseline,
            *live,
        ))),
        ResolvedScrub::ParamRgb {
            target,
            param_ids,
            preset,
            baseline,
            live,
        } if baseline != live => Some(Box::new(
            super::material_colour::ChangeMaterialColourCommand::new(
                target.clone(),
                param_ids.clone(),
                preset.clone(),
                *baseline,
                *live,
            ),
        )),
        ResolvedScrub::MasterOpacity { baseline, live } if moved_by_eps(*baseline, *live) => {
            Some(Box::new(ChangeMasterOpacityCommand::new(*baseline, *live)))
        }
        ResolvedScrub::LedBrightness { baseline, live } if moved_by_eps(*baseline, *live) => {
            Some(Box::new(ChangeLedBrightnessCommand::new(*baseline, *live)))
        }
        ResolvedScrub::LayerOpacity {
            layer_id,
            baseline,
            live,
        } if moved_by_eps(*baseline, *live) => Some(Box::new(ChangeLayerOpacityCommand::new(
            layer_id.clone(),
            *baseline,
            *live,
        ))),
        ResolvedScrub::RackWetDry {
            target,
            group_id,
            baseline,
            live,
        } if moved_by_eps(*baseline, *live) => Some(Box::new(ChangeGroupWetDryCommand::new(
            target.clone(),
            group_id.clone(),
            *baseline,
            *live,
        ))),
        ResolvedScrub::Macro {
            idx,
            baseline,
            live,
        } if moved_by_eps(*baseline, *live) => {
            Some(Box::new(ChangeMacroCommand::new(*idx, *baseline, *live)))
        }
        ResolvedScrub::LayerAudioGain {
            layer_id,
            baseline,
            live,
        } if moved_by_eps(*baseline, *live) => Some(Box::new(SetLayerAudioGainCommand::new(
            layer_id.clone(),
            *baseline,
            *live,
        ))),
        ResolvedScrub::Trim {
            kind,
            target,
            ableton_target,
            param_id,
            baseline,
            live,
        } => trim_command(
            project,
            *kind,
            target,
            ableton_target.as_ref(),
            param_id,
            *baseline,
            *live,
        ),
        ResolvedScrub::EnvelopeTarget {
            target,
            param_id,
            baseline,
            live,
        } if moved_by_eps(*baseline, *live) => {
            let index = envelope_index(project, target, param_id)?;
            Some(Box::new(ChangeEnvelopeTargetCommand::new(
                target.clone(),
                index,
                *baseline,
                *live,
            )))
        }
        ResolvedScrub::EnvDecay {
            target,
            param_id,
            baseline,
            live,
        } if moved_by_eps(*baseline, *live) => {
            let index = envelope_index(project, target, param_id)?;
            Some(Box::new(ChangeEnvelopeDecayCommand::new(
                target.clone(),
                index,
                *baseline,
                *live,
            )))
        }
        ResolvedScrub::AudioModShape {
            target,
            param_id,
            baseline,
            live,
        } if baseline != live => Some(Box::new(SetAudioModShapeCommand::new(
            DriverTarget::from(target),
            param_id.clone(),
            *baseline,
            *live,
        ))),
        ResolvedScrub::AudioModStepAmount {
            target,
            param_id,
            baseline,
            live_amount,
        } => {
            let new_action = stepped(*baseline, *live_amount);
            (new_action != *baseline).then(|| {
                Box::new(SetAudioModActionCommand::new(
                    DriverTarget::from(target),
                    param_id.clone(),
                    *baseline,
                    new_action,
                )) as Box<dyn Command + Send>
            })
        }
        ResolvedScrub::EnvelopeStepAmount {
            target,
            param_id,
            baseline,
            live_amount,
        } => {
            let new_action = stepped(*baseline, *live_amount);
            if new_action == *baseline {
                return None;
            }
            let index = envelope_index(project, target, param_id)?;
            Some(Box::new(SetEnvelopeActionCommand::new(
                target.clone(),
                index,
                *baseline,
                new_action,
            )))
        }
        ResolvedScrub::AudioTriggerShape {
            layer_id,
            index,
            baseline,
            live,
        } if baseline != live => {
            let current = project
                .timeline
                .find_layer_by_id(layer_id)
                .and_then(|(_, l)| l.clip_triggers.get(*index).cloned())?;
            let mut old = current.clone();
            old.shape = *baseline;
            let mut new = current;
            new.shape = *live;
            Some(Box::new(SetLayerClipTriggerCommand::new(
                layer_id.clone(),
                *index,
                old,
                new,
            )))
        }
        ResolvedScrub::AudioSendGain {
            send_id,
            baseline,
            live,
        } if moved_by_eps(*baseline, *live) => Some(Box::new(SetAudioSendGainCommand::new(
            send_id.clone(),
            *baseline,
            *live,
        ))),
        ResolvedScrub::AudioCrossover { baseline, live } if baseline != live => {
            Some(Box::new(SetAudioCrossoversCommand::new(*baseline, *live)))
        }
        ResolvedScrub::AbletonMacroTrim {
            slot_idx,
            baseline,
            live,
        } if moved_by_eps(baseline.0, live.0) || moved_by_eps(baseline.1, live.1) => {
            Some(Box::new(ChangeAbletonTrimCommand::new(
                manifold_core::ableton_mapping::AbletonMappingTarget::MacroSlot {
                    slot_index: *slot_idx,
                },
                baseline.0,
                baseline.1,
                live.0,
                live.1,
            )))
        }
        _ => None,
    }
}

/// Release the pointer hold a Param Begin sent. Armed Touch recording keeps
/// writing the slider while it is held, so every Param gesture end runs this.
fn release_param_hold(
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    record: &ResolvedScrub,
) {
    if let ResolvedScrub::Param {
        target, param_id, ..
    } = record
    {
        ContentCommand::send(
            content_tx,
            ContentCommand::AutomationHold {
                target: target.clone(),
                param_id: param_id.clone(),
                held: false,
            },
        );
    }
}

/// Commit the live slider gesture: take its record, release a Param hold, and
/// send the one undo command built from the record when the value moved.
/// The UI's project copy is never read for the new value.
pub(crate) fn commit_record(ctx: &mut DispatchCtx) -> DispatchResult {
    if let Some(record) = ctx.scrub.active.take() {
        release_param_hold(ctx.content_tx, &record);
        if let Some(cmd) = commit_command(&ctx.project, &record) {
            send_commit(
                ctx.content_tx,
                ctx.scrub,
                ctx.content_state.data_version,
                record,
                cmd,
            );
        }
    }
    DispatchResult::handled()
}

/// Drop a Param gesture's record without a commit (a lock that arrived
/// mid-drag): the pointer hold its Begin sent is released.
fn cancel_param_hold(ctx: &mut DispatchCtx) {
    if let Some(record) = ctx.scrub.active.take() {
        release_param_hold(ctx.content_tx, &record);
    }
}

/// Dispatch a scrub gesture (`PanelAction::Scrub`). Resolves the address and
/// runs the phase's operation — same commands, same one-undo-entry-per-gesture
/// cadence the retired per-family trios produced (D4 parity oracle:
/// `undo_baseline`).
pub(crate) fn dispatch_scrub(
    value_ref: &ValueRef,
    phase: &ScrubPhase,
    ctx: &mut DispatchCtx,
) -> DispatchResult {
    // The active layer the params dispatcher resolves against, so the graph
    // editor's watched target wins over the main window's selection.
    let effective_active_layer =
        super::editor_active_layer(ctx.editor_target, &ctx.project, ctx.active_layer);
    let active_layer = &effective_active_layer;

    if let ValueRef::Param(gpt, param_id) = value_ref
        && let Some(target) =
            resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project)
        && let Some(reason) = crate::graph_target::macro_parameter_lock_reason(
            &ctx.project,
            &target,
            param_id.as_ref(),
        )
    {
        match phase {
            ScrubPhase::Begin => crate::content_command::ContentCommand::send(
                ctx.content_tx,
                crate::content_command::ContentCommand::GraphEditRejected(reason.into()),
            ),
            // A lock that arrived mid-gesture still ends the gesture: the
            // hold sent at Begin must be released.
            ScrubPhase::Commit => cancel_param_hold(ctx),
            ScrubPhase::Move(_) => {}
        }
        return DispatchResult::handled();
    }

    // Single-active-gesture invariant (P-I): every wire family's Begin writes
    // `ctx.scrub.active` — one check here covers them all (the frame-resident
    // gestures check at their own opens).
    if matches!(phase, ScrubPhase::Begin) {
        ctx.scrub.check_single_active_on_begin("wire");
    }

    // A Scale axis of an item whose Scale link is on: every card edit of it
    // (a drag, a typed value, a reset) moves all three in proportion, as one
    // gesture and one undo step (SCENE_DOCUMENT_DESIGN.md section 7.17).
    // Modulation, MIDI and OSC never come through here.
    if let ValueRef::Param(gpt @ manifold_ui::GraphParamTarget::GeneratorOf(layer), param_id) =
        value_ref
        && let Some((ids, axis)) = ctx
            .ui
            .scene_setup_panel
            .linked_scale_axes(layer, param_id.as_ref())
    {
        let target = resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project);
        return dispatch_linked_scale(target, ids, axis, phase, ctx);
    }

    match value_ref {
        ValueRef::ParamRgb(gpt, ids) => {
            let target = resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project);
            dispatch_rgb_scrub(target, ids, phase, ctx)
        }
        ValueRef::Param(gpt, param_id) => match phase {
            ScrubPhase::Begin => {
                if let Some(target) =
                    resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project)
                {
                    let val = ctx
                        .project
                        .with_preset_graph(&target, |inst| {
                            inst.params
                                .contains(param_id.as_ref())
                                .then(|| inst.get_base_param(param_id.as_ref()))
                        })
                        .flatten();
                    if let Some(val) = val {
                        // Touch-to-select: the
                        // one funnel every param drag fires through, once per
                        // touch. Layer-scoped only.
                        if let Some(layer_id) = super::dispatch::params::automation_owner_layer_id(
                            &ctx.project,
                            &target,
                        ) {
                            ctx.selection.set_chosen_automation_param(
                                layer_id,
                                crate::editing_host::to_ui_graph_target(&target),
                                param_id.clone(),
                            );
                        }
                        // The slider is under the pointer until Commit: armed
                        // Touch recording keeps writing it while still and
                        // ends the take on release.
                        ContentCommand::send(
                            ctx.content_tx,
                            ContentCommand::AutomationHold {
                                target: target.clone(),
                                param_id: param_id.clone(),
                                held: true,
                            },
                        );
                        ctx.scrub.active = Some(ResolvedScrub::Param {
                            target: target.clone(),
                            param_id: param_id.clone(),
                            baseline: val,
                            live: val,
                        });
                    }
                }
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let (Some(val), Some(target)) = (
                    sv.scalar(),
                    resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project),
                ) {
                    // A typed or dragged value stops only at a declared hard
                    // limit .
                    let val = ctx
                        .project
                        .with_preset_graph(&target, |inst| {
                            inst.params
                                .get(param_id.as_ref())
                                .map(|p| p.spec.clamp_input(val))
                        })
                        .flatten()
                        .unwrap_or(val);
                    if let Some(ResolvedScrub::Param { live, .. }) = &mut ctx.scrub.active {
                        *live = val;
                    }
                    let pid = param_id.clone();
                    let t = target.clone();
                    ctx.gesture_move(crate::content_command::touch::VALUES, move |p| {
                        p.with_preset_graph_mut(&t, |inst| {
                            inst.set_base_param(pid.as_ref(), val);
                        });
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::MasterOpacity => match phase {
            ScrubPhase::Begin => {
                let baseline = ctx.project.settings.master_opacity;
                ctx.scrub.active = Some(ResolvedScrub::MasterOpacity {
                    baseline,
                    live: baseline,
                });
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let Some(v) = sv.scalar() {
                    if let Some(ResolvedScrub::MasterOpacity { live, .. }) = &mut ctx.scrub.active {
                        *live = v;
                    }
                    ctx.gesture_move(crate::content_command::touch::VALUES, move |p| {
                        p.settings.master_opacity = v;
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::LedBrightness => match phase {
            ScrubPhase::Begin => {
                let baseline = ctx.project.settings.led_brightness;
                ctx.scrub.active = Some(ResolvedScrub::LedBrightness {
                    baseline,
                    live: baseline,
                });
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let Some(v) = sv.scalar() {
                    if let Some(ResolvedScrub::LedBrightness { live, .. }) = &mut ctx.scrub.active {
                        *live = v;
                    }
                    // LED brightness lands via a plain (non-Live) mutation, as
                    // the retired `LedBrightnessChanged` did.
                    ctx.gesture_move(crate::content_command::touch::VALUES, move |p| {
                        p.settings.led_brightness = v;
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::LayerOpacity => match phase {
            ScrubPhase::Begin => {
                if let Some(idx) = super::resolve_active_layer_index(active_layer, &ctx.project)
                    && let Some(layer) = ctx.project.timeline.layers.get(idx)
                {
                    ctx.scrub.active = Some(ResolvedScrub::LayerOpacity {
                        layer_id: layer.layer_id.clone(),
                        baseline: layer.opacity,
                        live: layer.opacity,
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let (Some(v), Some(idx)) = (
                    sv.scalar(),
                    super::resolve_active_layer_index(active_layer, &ctx.project),
                ) {
                    if let Some(ResolvedScrub::LayerOpacity { live, .. }) = &mut ctx.scrub.active {
                        *live = v;
                    }
                    let layer_id = ctx.project.timeline.layers[idx].layer_id.clone();
                    ctx.gesture_move(crate::content_command::touch::VALUES, move |p| {
                        if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&layer_id) {
                            layer.opacity = v;
                        }
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::Macro(idx) => {
            let idx = *idx;
            match phase {
                ScrubPhase::Begin => {
                    if idx < manifold_core::macro_bank::MACRO_COUNT {
                        let value = ctx
                            .content_state
                            .modulation_snapshot
                            .as_deref()
                            .and_then(|s| s.macros().get(idx).copied())
                            .unwrap_or(ctx.project.settings.macro_bank.slots[idx].value);
                        ctx.scrub.active = Some(ResolvedScrub::Macro {
                            idx,
                            baseline: value,
                            live: value,
                        });
                    }
                    DispatchResult::handled()
                }
                ScrubPhase::Move(sv) => {
                    if let Some(v) = sv.scalar() {
                        if let Some(ResolvedScrub::Macro { idx: di, live, .. }) =
                            &mut ctx.scrub.active
                            && *di == idx
                        {
                            *live = v;
                        }
                        ctx.gesture_move(crate::content_command::touch::VALUES, move |p| {
                            manifold_core::macro_bank::MacroBank::apply_macro(p, idx, v);
                        });
                    }
                    DispatchResult::handled()
                }
                ScrubPhase::Commit => commit_record(ctx),
            }
        }

        ValueRef::LayerAudioGain(id) => match phase {
            ScrubPhase::Begin => {
                if let Some((_, layer)) = ctx.project.timeline.find_layer_by_id(id) {
                    let db = layer.audio_gain_db;
                    ctx.scrub.active = Some(ResolvedScrub::LayerAudioGain {
                        layer_id: id.clone(),
                        baseline: db,
                        live: db,
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let Some(v) = sv.scalar()
                    && ctx.project.timeline.find_layer_by_id(id).is_some()
                {
                    if let Some(ResolvedScrub::LayerAudioGain { live, .. }) = &mut ctx.scrub.active
                    {
                        *live = v;
                    }
                    let id = id.clone();
                    ctx.gesture_move(crate::content_command::touch::SHAPES, move |p| {
                        if let Some((_, l)) = p.timeline.find_layer_by_id_mut(&id) {
                            l.audio_gain_db = v;
                        }
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::Trim(kind, gpt, param_id) => {
            match phase {
                ScrubPhase::Begin => {
                    if let Some(target) =
                        resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project)
                    {
                        // The ValueRef→resolved mapping done ONCE, at Begin (FORK
                        // 1): resolve the Ableton mapping target for the record,
                        // and read the pre-gesture range from the kind's store as
                        // the undo baseline.
                        let ableton_target = matches!(kind, TrimKind::Ableton)
                            .then(|| ableton_mapping_target(&target, &ctx.project, param_id))
                            .flatten();
                        let baseline = match kind {
                            TrimKind::Driver => ctx
                                .project
                                .with_preset_graph(&target, |inst| {
                                    inst.drivers
                                        .as_ref()
                                        .and_then(|ds| ds.iter().find(|d| d.param_id == *param_id))
                                        .map(|d| (d.trim_min, d.trim_max))
                                })
                                .flatten(),
                            TrimKind::Audio => ctx
                                .project
                                .with_preset_graph(&target, |inst| {
                                    inst.audio_mods
                                        .as_ref()
                                        .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                                        .map(|m| (m.shape.range_min, m.shape.range_max))
                                })
                                .flatten(),
                            TrimKind::Ableton => ableton_target.as_ref().and_then(|mt| {
                                ctx.project
                                    .ableton_param_mappings(mt)
                                    .and_then(|opt| opt.as_ref())
                                    .and_then(|ms| ms.iter().find(|m| m.param_id == *param_id))
                                    .map(|m| (m.range_min, m.range_max))
                            }),
                        };
                        if let Some(baseline) = baseline {
                            ctx.scrub.active = Some(ResolvedScrub::Trim {
                                kind: *kind,
                                target,
                                ableton_target,
                                param_id: param_id.clone(),
                                baseline,
                                live: baseline,
                            });
                        }
                    }
                    DispatchResult::handled()
                }
                ScrubPhase::Move(sv) => {
                    if let (Some((mn, mx)), Some(target)) = (
                        sv.range(),
                        resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project),
                    ) {
                        if let Some(ResolvedScrub::Trim { live, .. }) = &mut ctx.scrub.active {
                            *live = (mn, mx);
                        }
                        // Each kind keeps its own live edit (driver, audio and
                        // Ableton trim stores), sent to the content thread only.
                        match kind {
                            TrimKind::Driver => {
                                graph_driver_move(
                                    &mut ctx.project,
                                    &target,
                                    param_id.clone(),
                                    move |d| {
                                        d.trim_min = mn;
                                        d.trim_max = mx;
                                    },
                                );
                            }
                            TrimKind::Audio => {
                                graph_audio_mod_move(
                                    &mut ctx.project,
                                    &target,
                                    param_id.clone(),
                                    move |m| {
                                        m.shape.range_min = mn;
                                        m.shape.range_max = mx;
                                    },
                                );
                            }
                            TrimKind::Ableton => {
                                if let Some(mapping_target) =
                                    ableton_mapping_target(&target, &ctx.project, param_id)
                                {
                                    let mt = mapping_target.clone();
                                    let pid = param_id.clone();
                                    ctx.gesture_move(
                                        crate::content_command::touch::MAPPING_RANGES,
                                        move |p| {
                                            if let Some(ms) = p
                                                .ableton_param_mappings_mut(&mt)
                                                .and_then(|opt| opt.as_mut())
                                                && let Some(m) =
                                                    ms.iter_mut().find(|m| m.param_id == pid)
                                            {
                                                m.range_min = mn;
                                                m.range_max = mx;
                                            }
                                        },
                                    );
                                }
                            }
                        }
                    }
                    DispatchResult::handled()
                }
                ScrubPhase::Commit => commit_record(ctx),
            }
        }

        ValueRef::EnvelopeTarget(gpt, param_id) => match phase {
            ScrubPhase::Begin => {
                if let Some(target) =
                    resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project)
                {
                    let baseline = ctx
                        .project
                        .with_preset_graph(&target, |inst| {
                            inst.envelopes
                                .as_ref()
                                .and_then(|es| es.iter().find(|e| e.param_id == *param_id))
                                .map(|e| e.target_normalized)
                        })
                        .flatten();
                    if let Some(baseline) = baseline {
                        ctx.scrub.active = Some(ResolvedScrub::EnvelopeTarget {
                            target,
                            param_id: param_id.clone(),
                            baseline,
                            live: baseline,
                        });
                    }
                }
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let (Some(v), Some(target)) = (
                    sv.scalar(),
                    resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project),
                ) {
                    if let Some(ResolvedScrub::EnvelopeTarget { live, .. }) = &mut ctx.scrub.active
                    {
                        *live = v;
                    }
                    graph_env_move(&mut ctx.project, &target, param_id.clone(), move |env| {
                        env.target_normalized = v;
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::EnvDecay(gpt, param_id) => match phase {
            ScrubPhase::Begin => {
                if let Some(target) =
                    resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project)
                {
                    let baseline = ctx
                        .project
                        .with_preset_graph(&target, |inst| {
                            inst.envelopes
                                .as_ref()
                                .and_then(|es| es.iter().find(|e| e.param_id == *param_id))
                                .map(|e| e.decay_beats)
                        })
                        .flatten();
                    if let Some(baseline) = baseline {
                        ctx.scrub.active = Some(ResolvedScrub::EnvDecay {
                            target,
                            param_id: param_id.clone(),
                            baseline,
                            live: baseline,
                        });
                    }
                }
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let (Some(v), Some(target)) = (
                    sv.scalar(),
                    resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project),
                ) {
                    if let Some(ResolvedScrub::EnvDecay { live, .. }) = &mut ctx.scrub.active {
                        *live = v;
                    }
                    graph_env_move(&mut ctx.project, &target, param_id.clone(), move |env| {
                        env.decay_beats = v;
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::AudioModShape(gpt, param_id, which) => match phase {
            ScrubPhase::Begin => {
                if let Some(target) =
                    resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project)
                {
                    // Capture the WHOLE pre-drag shape as the undo baseline.
                    let baseline = ctx
                        .project
                        .with_preset_graph(&target, |inst| {
                            inst.audio_mods
                                .as_ref()
                                .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                                .map(|m| m.shape)
                        })
                        .flatten();
                    if let Some(baseline) = baseline {
                        ctx.scrub.active = Some(ResolvedScrub::AudioModShape {
                            target,
                            param_id: param_id.clone(),
                            baseline,
                            live: baseline,
                        });
                    }
                }
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let (Some(v), Some(target)) = (
                    sv.scalar(),
                    resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project),
                ) {
                    let which = *which;
                    if let Some(ResolvedScrub::AudioModShape { live, .. }) = &mut ctx.scrub.active {
                        match which {
                            AudioShapeParam::Sensitivity => live.sensitivity = v,
                            AudioShapeParam::Attack => live.attack_ms = v,
                            AudioShapeParam::Release => live.release_ms = v,
                        }
                    }
                    graph_audio_mod_move(&mut ctx.project, &target, param_id.clone(), move |m| {
                        match which {
                            AudioShapeParam::Sensitivity => m.shape.sensitivity = v,
                            AudioShapeParam::Attack => m.shape.attack_ms = v,
                            AudioShapeParam::Release => m.shape.release_ms = v,
                        }
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::AudioModStepAmount(gpt, param_id) => match phase {
            ScrubPhase::Begin => {
                if let Some(target) =
                    resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project)
                {
                    // Capture the WHOLE pre-drag action as the undo baseline (the
                    // commit diffs it against the new action); the live amount
                    // keeps the wrap.
                    let baseline = ctx
                        .project
                        .with_preset_graph(&target, |inst| {
                            inst.find_audio_mod(param_id.as_ref()).map(|m| m.action)
                        })
                        .flatten();
                    if let Some(baseline) = baseline {
                        let live_amount = match baseline {
                            TriggerAction::Step { amount, .. } => amount,
                            _ => 0.0,
                        };
                        ctx.scrub.active = Some(ResolvedScrub::AudioModStepAmount {
                            target,
                            param_id: param_id.clone(),
                            baseline,
                            live_amount,
                        });
                    }
                }
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let (Some(v), Some(target)) = (
                    sv.scalar(),
                    resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project),
                ) {
                    if let Some(ResolvedScrub::AudioModStepAmount { live_amount, .. }) =
                        &mut ctx.scrub.active
                    {
                        *live_amount = v;
                    }
                    // Re-stamp the Step action preserving the current wrap.
                    graph_audio_mod_move(&mut ctx.project, &target, param_id.clone(), move |m| {
                        let wrap = match m.action {
                            TriggerAction::Step { wrap, .. } => wrap,
                            _ => WrapMode::Wrap,
                        };
                        m.action = TriggerAction::Step { amount: v, wrap };
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::EnvelopeStepAmount(gpt, param_id) => match phase {
            ScrubPhase::Begin => {
                if let Some(target) =
                    resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project)
                {
                    let baseline = ctx
                        .project
                        .with_preset_graph(&target, |inst| {
                            inst.envelopes
                                .as_ref()
                                .and_then(|es| es.iter().find(|e| e.param_id == *param_id))
                                .map(|e| e.action)
                        })
                        .flatten();
                    if let Some(baseline) = baseline {
                        let live_amount = match baseline {
                            TriggerAction::Step { amount, .. } => amount,
                            _ => 0.0,
                        };
                        ctx.scrub.active = Some(ResolvedScrub::EnvelopeStepAmount {
                            target,
                            param_id: param_id.clone(),
                            baseline,
                            live_amount,
                        });
                    }
                }
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let (Some(v), Some(target)) = (
                    sv.scalar(),
                    resolve_graph_target(gpt, ctx.editor_target, active_layer, &ctx.project),
                ) {
                    if let Some(ResolvedScrub::EnvelopeStepAmount { live_amount, .. }) =
                        &mut ctx.scrub.active
                    {
                        *live_amount = v;
                    }
                    graph_env_move(&mut ctx.project, &target, param_id.clone(), move |env| {
                        let wrap = match env.action {
                            TriggerAction::Step { wrap, .. } => wrap,
                            _ => WrapMode::Wrap,
                        };
                        env.action = TriggerAction::Step { amount: v, wrap };
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::AudioTriggerShape(layer_id, index, which) => match phase {
            ScrubPhase::Begin => {
                // Addressed directly by `(layer_id, index)` — no
                // `resolve_graph_target`. Capture the WHOLE pre-drag shape as the
                // undo baseline.
                let baseline = ctx
                    .project
                    .timeline
                    .find_layer_by_id(layer_id)
                    .and_then(|(_, l)| l.clip_triggers.get(*index))
                    .map(|t| t.shape);
                if let Some(baseline) = baseline {
                    ctx.scrub.active = Some(ResolvedScrub::AudioTriggerShape {
                        layer_id: layer_id.clone(),
                        index: *index,
                        baseline,
                        live: baseline,
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let Some(v) = sv.scalar() {
                    let which = *which;
                    if let Some(ResolvedScrub::AudioTriggerShape { live, .. }) =
                        &mut ctx.scrub.active
                    {
                        match which {
                            AudioShapeParam::Sensitivity => live.sensitivity = v,
                            AudioShapeParam::Attack => live.attack_ms = v,
                            AudioShapeParam::Release => live.release_ms = v,
                        }
                    }
                    clip_trigger_shape_move(&mut ctx.project, layer_id, *index, move |shape| {
                        match which {
                            AudioShapeParam::Sensitivity => shape.sensitivity = v,
                            AudioShapeParam::Attack => shape.attack_ms = v,
                            AudioShapeParam::Release => shape.release_ms = v,
                        }
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::AudioSendGain(send_id) => match phase {
            ScrubPhase::Begin => {
                // Snapshot the pre-drag gain as the undo baseline — addressed
                // directly by `AudioSendId`, no resolution.
                let baseline = ctx
                    .project
                    .audio_setup
                    .find_send(send_id)
                    .map(|s| s.gain_db)
                    .unwrap_or(0.0);
                ctx.scrub.active = Some(ResolvedScrub::AudioSendGain {
                    send_id: send_id.clone(),
                    baseline,
                    live: baseline,
                });
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let Some(db) = sv.scalar() {
                    // Clamp to the stepper's trim range, then push a LIVE
                    // (non-undo) edit to the content thread so the label +
                    // `GainBank` track the cursor.
                    let clamped = db.clamp(AUDIO_SEND_GAIN_MIN_DB, AUDIO_SEND_GAIN_MAX_DB);
                    if let Some(ResolvedScrub::AudioSendGain { live, .. }) = &mut ctx.scrub.active {
                        *live = clamped;
                    }
                    let id = send_id.clone();
                    ctx.gesture_move(crate::content_command::touch::SHAPES, move |p| {
                        if let Some(s) = p.audio_setup.find_send_mut(&id) {
                            s.gain_db = clamped;
                        }
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::AudioCrossover(band) => match phase {
            ScrubPhase::Begin => {
                // Snapshot the whole pre-drag crossover pair as the undo baseline
                // (global gesture — no key).
                let baseline = (
                    ctx.project.audio_setup.low_hz,
                    ctx.project.audio_setup.mid_hz,
                );
                ctx.scrub.active = Some(ResolvedScrub::AudioCrossover {
                    baseline,
                    live: baseline,
                });
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let Some(hz) = sv.scalar() {
                    // Clamp the dragged line against the other line's live value
                    // and the band edges (core-only `clamp_crossovers`), then send
                    // BOTH low and mid as a live, non-undo content edit.
                    let Some(ResolvedScrub::AudioCrossover { live, .. }) = &mut ctx.scrub.active
                    else {
                        return DispatchResult::handled();
                    };
                    let dragging_low = matches!(band, manifold_ui::BandDivider::Low);
                    let (cur_low, cur_mid) = *live;
                    let (low, mid) = if dragging_low {
                        manifold_core::audio_setup::AudioSetup::clamp_crossovers(hz, cur_mid, true)
                    } else {
                        manifold_core::audio_setup::AudioSetup::clamp_crossovers(cur_low, hz, false)
                    };
                    *live = (low, mid);
                    ctx.gesture_move(crate::content_command::touch::SHAPES, move |p| {
                        p.audio_setup.low_hz = low;
                        p.audio_setup.mid_hz = mid;
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },

        ValueRef::RackWetDry {
            tab,
            layer_id,
            group_id,
        } => super::rack_controls::dispatch_wet_dry(*tab, layer_id, group_id, phase, ctx),

        ValueRef::AbletonMacroTrim(slot_idx) => match phase {
            ScrubPhase::Begin => {
                // Snapshot the pre-drag `(min, max)` range as the undo baseline —
                // addressed directly by the macro slot index, no resolution.
                let baseline = ctx
                    .project
                    .settings
                    .macro_bank
                    .slots
                    .get(*slot_idx)
                    .and_then(|s| s.ableton_mapping.as_ref())
                    .map(|m| (m.range_min, m.range_max));
                if let Some(baseline) = baseline {
                    ctx.scrub.active = Some(ResolvedScrub::AbletonMacroTrim {
                        slot_idx: *slot_idx,
                        baseline,
                        live: baseline,
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Move(sv) => {
                if let Some((min, max)) = sv.range() {
                    // Live edit (no undo): the mapping tracks the cursor on the
                    // content thread only.
                    let slot_idx = *slot_idx;
                    if let Some(ResolvedScrub::AbletonMacroTrim { live, .. }) =
                        &mut ctx.scrub.active
                    {
                        *live = (min, max);
                    }
                    ctx.gesture_move(crate::content_command::touch::MAPPING_RANGES, move |p| {
                        if let Some(m) = p
                            .settings
                            .macro_bank
                            .slots
                            .get_mut(slot_idx)
                            .and_then(|s| s.ableton_mapping.as_mut())
                        {
                            m.range_min = min;
                            m.range_max = max;
                        }
                    });
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => commit_record(ctx),
        },
    }
}

/// One card param of a [`ResolvedScrub::ParamGroup`]: the base value the
/// commit diffs against and the latest dragged one.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CardScrub {
    pub id: ParamId,
    pub baseline: f32,
    pub live: f32,
}

/// Begin moving the card params `card_ids` and the unbound node params
/// `nodes` of `target` as one gesture, landing as one undo entry named
/// `label`. False, with the reason sent, when a card is locked, or missing.
pub(crate) fn begin_param_group(
    ctx: &mut DispatchCtx,
    target: GraphTarget,
    card_ids: Vec<ParamId>,
    nodes: Vec<crate::app_render::UnboundNodeParamDrag>,
    label: &'static str,
) -> bool {
    if let Some(reason) = card_ids.iter().find_map(|id| {
        crate::graph_target::macro_parameter_lock_reason(&ctx.project, &target, id.as_ref())
    }) {
        ContentCommand::send(
            ctx.content_tx,
            ContentCommand::GraphEditRejected(reason.into()),
        );
        return false;
    }
    let Some(cards) = ctx
        .project
        .with_preset_graph(&target, |inst| {
            card_ids
                .iter()
                .map(|id| {
                    inst.params.contains(id.as_ref()).then(|| {
                        let baseline = inst.get_base_param(id.as_ref());
                        CardScrub {
                            id: id.clone(),
                            baseline,
                            live: baseline,
                        }
                    })
                })
                .collect::<Option<Vec<_>>>()
        })
        .flatten()
    else {
        return false;
    };
    ctx.scrub.check_single_active_on_begin("param-group");
    for card in &cards {
        ContentCommand::send(
            ctx.content_tx,
            ContentCommand::AutomationHold {
                target: target.clone(),
                param_id: card.id.clone(),
                held: true,
            },
        );
    }
    ctx.scrub.active = Some(ResolvedScrub::ParamGroup {
        target,
        cards,
        nodes,
        label,
    });
    true
}

/// Move the live group gesture: each card to `cards[i]`, stopped at its
/// hard limit as its own knob stops it, and each node param to `nodes[i]`.
pub(crate) fn move_param_group(ctx: &mut DispatchCtx, cards: &[f32], nodes: &[f32]) {
    let Some(ResolvedScrub::ParamGroup {
        target,
        cards: scrubs,
        ..
    }) = &ctx.scrub.active
    else {
        return;
    };
    let target = target.clone();
    let Some(clamped) = ctx
        .project
        .with_preset_graph(&target, |inst| -> Option<Vec<f32>> {
            scrubs
                .iter()
                .zip(cards)
                .map(|(card, value)| {
                    Some(inst.params.get(card.id.as_ref())?.spec.clamp_input(*value))
                })
                .collect()
        })
        .flatten()
    else {
        return;
    };
    let graph_absent = ctx
        .project
        .preset_instance(&target)
        .is_none_or(|inst| inst.graph.is_none());
    let Some(ResolvedScrub::ParamGroup {
        cards: scrubs,
        nodes: drags,
        ..
    }) = &mut ctx.scrub.active
    else {
        return;
    };
    for (card, value) in scrubs.iter_mut().zip(&clamped) {
        card.live = *value;
    }
    for (drag, value) in drags.iter_mut().zip(nodes) {
        drag.set_current(
            manifold_core::effect_graph_def::SerializedParamValue::Float { value: *value },
        );
    }
    let ids: Vec<ParamId> = scrubs.iter().map(|card| card.id.clone()).collect();
    let live = drags.clone();
    let touches = if live.is_empty() {
        crate::content_command::touch::VALUES
    } else {
        // A node write may materialise the graph override (D21).
        crate::content_command::touch::shapes_first_move(graph_absent)
    };
    ctx.gesture_move(touches, move |project| {
        project.with_preset_graph_mut(&target, |inst| {
            for (id, value) in ids.iter().zip(&clamped) {
                inst.set_base_param(id.as_ref(), *value);
            }
        });
        for drag in &live {
            manifold_editing::command::Command::execute(&mut drag.live_command(), project);
        }
    });
}

/// End the group gesture: one undo entry for every param it changed, and
/// the pointer holds its Begin sent released.
pub(crate) fn commit_param_group(ctx: &mut DispatchCtx) {
    let Some(record) = ctx.scrub.active.take() else {
        return;
    };
    let ResolvedScrub::ParamGroup {
        target,
        cards,
        nodes,
        label,
    } = &record
    else {
        return;
    };
    for card in cards {
        ContentCommand::send(
            ctx.content_tx,
            ContentCommand::AutomationHold {
                target: target.clone(),
                param_id: card.id.clone(),
                held: false,
            },
        );
    }
    let mut changes: Vec<Box<dyn Command>> = cards
        .iter()
        .filter(|card| (card.baseline - card.live).abs() > f32::EPSILON)
        .map(|card| {
            Box::new(ChangeGraphParamCommand::new(
                target.clone(),
                card.id.clone(),
                card.baseline,
                card.live,
            )) as Box<dyn Command>
        })
        .collect();
    changes.extend(
        nodes
            .iter()
            .filter(|drag| drag.moved())
            .map(|drag| Box::new(drag.clone().into_command()) as Box<dyn Command>),
    );
    if !changes.is_empty() {
        let cmd = manifold_editing::command::CompositeCommand::new(changes, (*label).into());
        send_commit(
            ctx.content_tx,
            ctx.scrub,
            ctx.content_state.data_version,
            record,
            Box::new(cmd),
        );
    }
}

/// A card edit of Scale axis `axis` of a linked item, as one gesture over its
/// three Scale params `ids`: each Move scales all three by the dragged
/// axis's change from its value before the gesture, so the press that jumps
/// the slider to the pointer scales the other two with it.
fn dispatch_linked_scale(
    target: Option<GraphTarget>,
    ids: [ParamId; 3],
    axis: usize,
    phase: &ScrubPhase,
    ctx: &mut DispatchCtx,
) -> DispatchResult {
    match phase {
        ScrubPhase::Begin => {
            if let Some(target) = target {
                begin_param_group(ctx, target, ids.to_vec(), Vec::new(), "Scale");
            }
        }
        ScrubPhase::Move(manifold_ui::ScrubValue::Scalar(value)) => {
            let Some(ResolvedScrub::ParamGroup { cards, .. }) = &ctx.scrub.active else {
                return DispatchResult::handled();
            };
            if !cards.iter().map(|card| &card.id).eq(ids.iter()) {
                return DispatchResult::handled();
            }
            let from = cards[axis].baseline;
            let values: Vec<f32> = cards
                .iter()
                .map(|card| {
                    if from.abs() > f32::EPSILON {
                        card.baseline * value / from
                    } else {
                        *value
                    }
                })
                .collect();
            move_param_group(ctx, &values, &[]);
        }
        ScrubPhase::Move(_) => {}
        ScrubPhase::Commit => commit_param_group(ctx),
    }
    DispatchResult::handled()
}

fn dispatch_rgb_scrub(
    target: Option<GraphTarget>,
    ids: &[ParamId; 3],
    phase: &ScrubPhase,
    ctx: &mut DispatchCtx,
) -> DispatchResult {
    use super::projection::material::rgb_editable;
    use manifold_ui::ScrubValue;
    match phase {
        ScrubPhase::Begin => {
            if let Some(target) = target {
                if rgb_editable(&ctx.project, &target, ids) {
                    let inst = ctx
                        .project
                        .preset_instance(&target)
                        .expect("preflight checked owner");
                    let baseline = std::array::from_fn(|i| inst.get_base_param(&ids[i]));
                    let preset = inst.effect_type().clone();
                    ctx.scrub.active = Some(ResolvedScrub::ParamRgb {
                        target,
                        param_ids: ids.clone(),
                        preset,
                        baseline,
                        live: baseline,
                    });
                } else {
                    ContentCommand::send(ctx.content_tx, ContentCommand::GraphEditRejected(
                        "Colour is controlled by a wire, mapping or custom binding; edit its individual channels.".into()));
                }
            }
        }
        ScrubPhase::Move(ScrubValue::Rgb(values)) => {
            if let Some(ResolvedScrub::ParamRgb {
                target: captured,
                param_ids,
                preset,
                live,
                ..
            }) = &mut ctx.scrub.active
            {
                if target.as_ref() != Some(captured)
                    || ids != param_ids
                    || !values.iter().all(|v| v.is_finite())
                    || !ctx.project.preset_instance(captured).is_some_and(|inst| {
                        inst.effect_type() == preset
                            && param_ids.iter().zip(values).all(|(id, value)| {
                                inst.params
                                    .get(id)
                                    .is_some_and(|p| *value >= p.spec.min && *value <= p.spec.max)
                            })
                    })
                    || !rgb_editable(&ctx.project, captured, param_ids)
                {
                    return DispatchResult::handled();
                }
                let values = *values;
                *live = values;
                let t = captured.clone();
                let pids = param_ids.clone();
                let preset = preset.clone();
                ctx.gesture_move(crate::content_command::touch::VALUES, move |project| {
                    if project
                        .preset_instance(&t)
                        .is_some_and(|inst| inst.effect_type() == &preset)
                        && rgb_editable(project, &t, &pids)
                    {
                        project.with_preset_graph_mut(&t, |inst| {
                            for (id, value) in pids.iter().zip(values) {
                                inst.set_base_param(id, value);
                            }
                        });
                    }
                });
            }
        }
        ScrubPhase::Move(_) => {}
        ScrubPhase::Commit => {
            if matches!(&ctx.scrub.active, Some(ResolvedScrub::ParamRgb {param_ids,..}) if param_ids == ids)
            {
                commit_record(ctx);
            }
        }
    }
    DispatchResult::handled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{EffectId, PresetTypeId};

    #[test]
    fn live_param_reads_only_the_dragged_param() {
        let target = GraphTarget::Effect(EffectId::new("fx"));
        let scrub = ResolvedScrub::Param {
            target: target.clone(),
            param_id: ParamId::Borrowed("amount"),
            baseline: 0.1,
            live: 0.9,
        };
        assert_eq!(scrub.live_param(&target, "amount"), Some(0.9));
        assert_eq!(scrub.live_param(&target, "other"), None);
        assert_eq!(
            scrub.live_param(&GraphTarget::Effect(EffectId::new("other")), "amount"),
            None
        );
    }

    #[test]
    fn live_param_rgb_returns_the_dragged_channel() {
        let target = GraphTarget::Effect(EffectId::new("fx"));
        let scrub = ResolvedScrub::ParamRgb {
            target: target.clone(),
            param_ids: [
                ParamId::Borrowed("r"),
                ParamId::Borrowed("g"),
                ParamId::Borrowed("b"),
            ],
            preset: PresetTypeId::new("test.preset"),
            baseline: [0.1, 0.2, 0.3],
            live: [0.4, 0.5, 0.6],
        };
        assert_eq!(scrub.live_param(&target, "g"), Some(0.5));
        assert_eq!(scrub.live_param(&target, "missing"), None);
    }

    #[test]
    fn live_macro_reads_only_the_dragged_slot() {
        let scrub = ResolvedScrub::Macro {
            idx: 2,
            baseline: 0.1,
            live: 0.7,
        };
        assert_eq!(scrub.live_macro(2), Some(0.7));
        assert_eq!(scrub.live_macro(1), None);
    }

    #[test]
    fn drawn_reads_the_live_gesture_then_the_landing_until_its_snapshot() {
        let mut scrub = ScrubState::default();
        let record = |live: f32| ResolvedScrub::MasterOpacity {
            baseline: 0.8,
            live,
        };
        let value = |s: &ScrubState| match s.drawn() {
            Some(ResolvedScrub::MasterOpacity { live, .. }) => Some(*live),
            _ => None,
        };
        scrub.active = Some(record(0.35));
        assert_eq!(value(&scrub), Some(0.35));
        scrub.active = None;
        assert_eq!(value(&scrub), None);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let cmd = manifold_editing::commands::settings::ChangeMasterOpacityCommand::new(0.8, 0.35);
        send_commit(&tx, &mut scrub, 7, record(0.35), Box::new(cmd));
        assert_eq!(
            value(&scrub),
            Some(0.35),
            "landing draws until its snapshot"
        );
        scrub.snapshot_arrived(7);
        assert_eq!(
            value(&scrub),
            Some(0.35),
            "version 7 is before the commit's 8"
        );
        scrub.snapshot_arrived(8);
        assert_eq!(value(&scrub), None);
    }
}
