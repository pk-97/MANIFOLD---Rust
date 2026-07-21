//! The scrub-gesture engine — one addressed gesture, four operations
//! (UI_FUNNEL_DECOMPOSITION P-I, D4).
//!
//! A value-scrub gesture (slider drag, knob, discrete enum cycle) arrives on
//! the wire as `PanelAction::Scrub(ValueRef, ScrubPhase)` (manifold-ui). This
//! module resolves each [`manifold_ui::ValueRef`] to a core write target and
//! runs the phase's operation. **Begin** resolves, reads the pre-gesture value
//! as the undo baseline, and stashes a self-contained restore payload
//! ([`ResolvedScrub`]). **Move** resolves, applies the new value locally for
//! immediate feedback, and ships a non-undoable live write to the content
//! thread. **Commit** resolves, reads the final value, and emits ONE
//! undo-tracked command spanning the whole gesture (baseline → final).
//!
//! Plus the restore path ([`ScrubState::restore_dragged`]): after a mid-gesture
//! content-thread snapshot swap stomps `local_project`, re-apply the in-flight
//! value with only `&mut Project` — no dispatch context — which is why the
//! resolved target is captured at Begin, not re-resolved.
//!
//! MIGRATION STATE (P-I in progress): the `Param` family rides the new
//! `active` gesture; the remaining families still ride the interim per-gesture
//! snapshot `Option`s + `ActiveInspectorDrag` below and port in batches. Both
//! restore paths coexist until the batches complete, then the interim slots and
//! `ActiveInspectorDrag` die.

use crate::app::ActiveInspectorDrag;
use crate::content_command::ContentCommand;
use manifold_core::audio_mod::{AudioModShape, TriggerAction};
use manifold_core::effects::ParamId;
use manifold_core::project::Project;
use manifold_core::{GraphTarget, LayerId};
use manifold_editing::commands::ableton::ChangeAbletonTrimCommand;
use manifold_editing::commands::audio_mod::{SetAudioModActionCommand, SetAudioModShapeCommand};
use manifold_editing::commands::drivers::ChangeTrimCommand;
use manifold_editing::commands::effect_target::DriverTarget;
use manifold_editing::commands::effects::{ChangeGraphParamCommand, SetRelightParamCommand};
use manifold_editing::commands::envelopes::{ChangeEnvelopeDecayCommand, ChangeEnvelopeTargetCommand};
use manifold_editing::commands::layer::SetLayerClipTriggerCommand;
use manifold_editing::commands::settings::{
    ChangeLayerOpacityCommand, ChangeLedBrightnessCommand, ChangeMacroCommand,
    ChangeMasterOpacityCommand,
};
use manifold_ui::{AudioShapeParam, ScrubPhase, ValueRef};

use super::dispatch::resolve::{
    ableton_mapping_target, clip_trigger_shape_dual_edit, graph_audio_mod_dual_edit,
    graph_driver_dual_edit, graph_env_dual_edit, resolve_graph_target,
};
use super::{DispatchCtx, DispatchResult};

/// The in-flight scrub-gesture snapshots threaded through `dispatch`. Every
/// interim field is the undo baseline captured on a drag's `…Snapshot`/
/// `…DragBegin` and consumed on its `…Commit`; `None` when no such gesture is
/// active. `active` is the P-I-ported gesture (baseline + resolved restore,
/// unified — the shape every interim field collapses into).
#[derive(Default)]
pub struct ScrubState {
    /// Slider drag snapshot for undo (opacity, slip, etc.). Threaded as
    /// `drag_snapshot` in the dispatch handlers (the arm bodies' name).
    pub slider_snapshot: Option<f32>,
    /// Trim drag snapshot (min, max) for undo.
    pub trim_snapshot: Option<(f32, f32)>,
    /// Band-divider drag snapshot `(low_hz, mid_hz)` for undo.
    pub audio_crossover_snapshot: Option<(f32, f32)>,
    /// Send-gain drag snapshot (old dB) for undo (D7).
    pub audio_send_gain_drag_snapshot: Option<f32>,
    /// Active inspector drag — prevents snapshot from overwriting dragged field.
    pub active_inspector_drag: Option<ActiveInspectorDrag>,
    /// The single P-I-ported gesture: undo baseline + resolved restore payload.
    pub active: Option<ResolvedScrub>,
}

impl ScrubState {
    /// Re-apply the in-flight scrub value after a mid-gesture content-thread
    /// snapshot swap stomped `local_project` (the restore path). Handles both
    /// the interim `active_inspector_drag` families and the P-I-ported `active`
    /// gesture; at most one is live at a time.
    pub fn restore_dragged(&self, project: &mut Project) {
        if let Some(drag) = &self.active_inspector_drag {
            drag.apply(project);
        }
        if let Some(active) = &self.active {
            active.restore(project);
        }
    }
}

/// A resolved, self-contained scrub gesture: the undo baseline plus the
/// resolved write target and the latest live value, enough to restore the
/// gesture after a snapshot stomp with only `&mut Project`. One variant per
/// ported family — the surviving essence of `ActiveInspectorDrag`'s per-family
/// write logic, unified with the undo baseline. Grows as P-I batches port; at
/// completion it fully replaces `ActiveInspectorDrag`.
pub enum ResolvedScrub {
    /// An exposed card param on an effect/generator graph (was the
    /// `ParamSnapshot`/`ParamChanged`/`ParamCommit` trio +
    /// `ActiveInspectorDrag::Param`). `baseline` is the pre-gesture base value
    /// the commit diffs against; `live` is the latest dragged value the restore
    /// path re-stamps.
    Param {
        target: GraphTarget,
        param_id: ParamId,
        baseline: f32,
        live: f32,
    },
    /// The master-opacity slider (`settings.master_opacity`).
    MasterOpacity { baseline: f32, live: f32 },
    /// The LED master-brightness slider (`settings.led_brightness`).
    LedBrightness { baseline: f32, live: f32 },
    /// A layer's opacity — `layer_id` captured at Begin so the restore path can
    /// re-stamp it without the active-layer context.
    LayerOpacity {
        layer_id: LayerId,
        baseline: f32,
        live: f32,
    },
    /// A macro-bank knob (`idx`). Macros ride every ModulationSnapshot block, so
    /// the restore path re-applies through `apply_macro` — the same write Move
    /// uses — or a per-tick apply stomps the in-flight value.
    Macro { idx: usize, baseline: f32, live: f32 },
    /// A layer's audio-input gain (dB) — `layer_id` captured at Begin.
    LayerAudioGain {
        layer_id: LayerId,
        baseline: f32,
        live: f32,
    },
    /// A "3D Shading" relight knob — `field` is the resolved core
    /// [`manifold_core::effects::RelightField`] captured at Begin.
    RelightParam {
        target: GraphTarget,
        field: manifold_core::effects::RelightField,
        baseline: f32,
        live: f32,
    },
    /// A modulation trim-range handle (driver / audio-mod / Ableton sub-range
    /// bars, BUG-246). `kind` selects the store; `ableton_target` is resolved
    /// only for `TrimKind::Ableton` (`None` for driver/audio). `baseline`/`live`
    /// are `(min, max)` pairs — the pre-gesture range the commit diffs against
    /// and the latest dragged range the restore path re-stamps.
    Trim {
        kind: manifold_ui::panels::TrimKind,
        target: GraphTarget,
        ableton_target: Option<manifold_core::ableton_mapping::AbletonMappingTarget>,
        param_id: ParamId,
        baseline: (f32, f32),
        live: (f32, f32),
    },
    /// An envelope target handle (`target_normalized`) — `target`/`param_id`
    /// resolved at Begin so the restore path can re-stamp without dispatch ctx.
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
    /// `baseline` and `live` (the drag edits one scalar; the restore re-stamps
    /// the whole shape so the other two hold, matching the retired
    /// `ActiveInspectorDrag::AudioModShape`). `which` scalar is drag-local and
    /// carried on the wire, not here.
    AudioModShape {
        target: GraphTarget,
        param_id: ParamId,
        baseline: AudioModShape,
        live: AudioModShape,
    },
    /// An audio-mod Step-action amount slider. The undo `baseline` is the WHOLE
    /// pre-drag `TriggerAction` (the commit emits `SetAudioModActionCommand`
    /// old→new), while the restore path only needs `live_amount`: it re-stamps
    /// `TriggerAction::Step { amount: live_amount, wrap }` reading the current
    /// wrap from the store, matching the retired
    /// `ActiveInspectorDrag::AudioModStepAmount`.
    AudioModStepAmount {
        target: GraphTarget,
        param_id: ParamId,
        baseline: TriggerAction,
        live_amount: f32,
    },
    /// A layer clip-trigger drawer shaping slider (AudioSetup-domain twin of
    /// `AudioModShape`). Addressed by `(layer_id, index)` into `clip_triggers`;
    /// holds the WHOLE shape at both `baseline` and `live` (the drag edits one
    /// scalar; the restore re-stamps the whole shape so the other two hold). The
    /// commit diffs the whole `ClipTrigger` via `SetLayerClipTriggerCommand`.
    AudioTriggerShape {
        layer_id: LayerId,
        index: usize,
        baseline: AudioModShape,
        live: AudioModShape,
    },
}

impl ResolvedScrub {
    /// Re-stamp the in-flight value through the SAME write the family's live
    /// `Move` uses, so a mid-drag snapshot swap can't revert it (the
    /// `ActiveInspectorDrag::apply` precedent, generalized).
    fn restore(&self, project: &mut Project) {
        match self {
            ResolvedScrub::Param {
                target,
                param_id,
                live,
                ..
            } => {
                // Restore through `set_base_param` — the write `Move` ticks use
                // and the commit reads back; an effective-only restore would
                // leave base stale and the commit would see old == new.
                project.with_preset_graph_mut(target, |inst| {
                    inst.set_base_param(param_id.as_ref(), *live);
                });
            }
            ResolvedScrub::MasterOpacity { live, .. } => {
                project.settings.master_opacity = *live;
            }
            ResolvedScrub::LedBrightness { live, .. } => {
                project.settings.led_brightness = *live;
            }
            ResolvedScrub::LayerOpacity { layer_id, live, .. } => {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id) {
                    layer.opacity = *live;
                }
            }
            ResolvedScrub::Macro { idx, live, .. } => {
                manifold_core::macro_bank::MacroBank::apply_macro(project, *idx, *live);
            }
            ResolvedScrub::LayerAudioGain { layer_id, live, .. } => {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id) {
                    layer.audio_gain_db = *live;
                }
            }
            ResolvedScrub::RelightParam {
                target, field, live, ..
            } => {
                project.with_preset_graph_mut(target, |inst| {
                    field.set(&mut inst.relight_params, *live);
                });
            }
            ResolvedScrub::Trim {
                kind,
                target,
                ableton_target,
                param_id,
                live: (min, max),
                ..
            } => {
                use manifold_ui::panels::TrimKind;
                // Same store each kind's live Move write lands in, re-applied so a
                // mid-drag snapshot swap can't revert the in-flight range.
                match kind {
                    TrimKind::Driver => {
                        project.with_preset_graph_mut(target, |inst| {
                            if let Some(d) = inst
                                .drivers
                                .as_mut()
                                .and_then(|ds| ds.iter_mut().find(|d| d.param_id == *param_id))
                            {
                                d.trim_min = *min;
                                d.trim_max = *max;
                            }
                        });
                    }
                    TrimKind::Audio => {
                        project.with_preset_graph_mut(target, |inst| {
                            if let Some(m) = inst
                                .audio_mods
                                .as_mut()
                                .and_then(|ms| ms.iter_mut().find(|a| a.param_id == *param_id))
                            {
                                m.shape.range_min = *min;
                                m.shape.range_max = *max;
                            }
                        });
                    }
                    TrimKind::Ableton => {
                        if let Some(mt) = ableton_target
                            && let Some(ms) = project
                                .ableton_param_mappings_mut(mt)
                                .and_then(|opt| opt.as_mut())
                            && let Some(m) = ms.iter_mut().find(|m| m.param_id == *param_id)
                        {
                            m.range_min = *min;
                            m.range_max = *max;
                        }
                    }
                }
            }
            ResolvedScrub::EnvelopeTarget {
                target,
                param_id,
                live,
                ..
            } => {
                project.with_preset_graph_mut(target, |inst| {
                    if let Some(e) = inst
                        .envelopes
                        .as_mut()
                        .and_then(|es| es.iter_mut().find(|e| e.param_id == *param_id))
                    {
                        e.target_normalized = *live;
                    }
                });
            }
            ResolvedScrub::EnvDecay {
                target,
                param_id,
                live,
                ..
            } => {
                project.with_preset_graph_mut(target, |inst| {
                    if let Some(e) = inst
                        .envelopes
                        .as_mut()
                        .and_then(|es| es.iter_mut().find(|e| e.param_id == *param_id))
                    {
                        e.decay_beats = *live;
                    }
                });
            }
            ResolvedScrub::AudioModShape {
                target,
                param_id,
                live,
                ..
            } => {
                project.with_preset_graph_mut(target, |inst| {
                    if let Some(m) = inst
                        .audio_mods
                        .as_mut()
                        .and_then(|ms| ms.iter_mut().find(|a| a.param_id == *param_id))
                    {
                        m.shape = *live;
                    }
                });
            }
            ResolvedScrub::AudioModStepAmount {
                target,
                param_id,
                live_amount,
                ..
            } => {
                project.with_preset_graph_mut(target, |inst| {
                    if let Some(m) = inst
                        .audio_mods
                        .as_mut()
                        .and_then(|ms| ms.iter_mut().find(|a| a.param_id == *param_id))
                    {
                        // Re-stamp the Step action, preserving the current wrap —
                        // the same write the live `Move` arm lands.
                        let wrap = match m.action {
                            TriggerAction::Step { wrap, .. } => wrap,
                            _ => manifold_core::audio_mod::WrapMode::Wrap,
                        };
                        m.action = TriggerAction::Step {
                            amount: *live_amount,
                            wrap,
                        };
                    }
                });
            }
            ResolvedScrub::AudioTriggerShape {
                layer_id,
                index,
                live,
                ..
            } => {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id)
                    && let Some(t) = layer.clip_triggers.get_mut(*index)
                {
                    t.shape = *live;
                }
            }
        }
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
    // The `(tab, active_layer)` resolution context, byte-identical to the
    // params dispatcher's D-11 preamble so the graph editor's watched target
    // wins over the main window's selection.
    let (effective_tab, effective_active_layer) = super::editor_dispatch_context(
        ctx.editor_target,
        &*ctx.project,
        ctx.ui.inspector.last_effect_tab(),
        ctx.active_layer,
    );
    let active_layer = &effective_active_layer;

    match value_ref {
        ValueRef::Param(gpt, param_id) => match phase {
            ScrubPhase::Begin => {
                if let Some(target) = resolve_graph_target(
                    gpt,
                    ctx.editor_target,
                    effective_tab,
                    active_layer,
                    ctx.selection,
                    ctx.project,
                ) {
                    let val = ctx
                        .project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.params
                                .contains(param_id.as_ref())
                                .then(|| inst.get_base_param(param_id.as_ref()))
                        })
                        .flatten();
                    if let Some(val) = val {
                        // Touch-to-select (`AUTOMATION_LANES_DESIGN.md` §7): the
                        // one funnel every param drag fires through, once per
                        // touch. Layer-scoped only.
                        if effective_tab.is_layer_scope()
                            && let Some(layer_id) = active_layer.clone()
                        {
                            ctx.selection.set_chosen_automation_param(
                                layer_id,
                                crate::editing_host::to_ui_graph_target(&target),
                                param_id.clone(),
                            );
                        }
                        ctx.scrub.active = Some(ResolvedScrub::Param {
                            target,
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
                    resolve_graph_target(
                        gpt,
                        ctx.editor_target,
                        effective_tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                    ),
                ) {
                    ctx.project.with_preset_graph_mut(&target, |inst| {
                        inst.set_base_param(param_id.as_ref(), val);
                    });
                    if let Some(ResolvedScrub::Param { live, .. }) = &mut ctx.scrub.active {
                        *live = val;
                    }
                    let pid = param_id.clone();
                    let t = target.clone();
                    ContentCommand::send(
                        ctx.content_tx,
                        ContentCommand::MutateProjectLive(Box::new(move |p| {
                            p.with_preset_graph_mut(&t, |inst| {
                                inst.set_base_param(pid.as_ref(), val);
                            });
                        })),
                    );
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => {
                // Release commits ONE `ChangeGraphParamCommand` through the
                // undo-tracked `Execute` path — one undo unit per gesture, not
                // per motion event. Baseline from the stored gesture; target
                // re-resolved from the wire address (matches the retired trio).
                let baseline = match &ctx.scrub.active {
                    Some(ResolvedScrub::Param { baseline, .. }) => Some(*baseline),
                    _ => None,
                };
                if let Some(old_val) = baseline
                    && let Some(target) = resolve_graph_target(
                        gpt,
                        ctx.editor_target,
                        effective_tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                    )
                {
                    let new_val = ctx
                        .project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.params
                                .contains(param_id.as_ref())
                                .then(|| inst.get_base_param(param_id.as_ref()))
                        })
                        .flatten();
                    if let Some(new_val) = new_val
                        && (old_val - new_val).abs() > f32::EPSILON
                    {
                        let cmd =
                            ChangeGraphParamCommand::new(target, param_id.clone(), old_val, new_val);
                        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
                ctx.scrub.active = None;
                DispatchResult::handled()
            }
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
                    ctx.project.settings.master_opacity = v;
                    if let Some(ResolvedScrub::MasterOpacity { live, .. }) = &mut ctx.scrub.active {
                        *live = v;
                    }
                    ContentCommand::send(
                        ctx.content_tx,
                        ContentCommand::MutateProjectLive(Box::new(move |p| {
                            p.settings.master_opacity = v;
                        })),
                    );
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => {
                if let Some(ResolvedScrub::MasterOpacity { baseline, .. }) = &ctx.scrub.active {
                    let baseline = *baseline;
                    let new_val = ctx.project.settings.master_opacity;
                    if (baseline - new_val).abs() > f32::EPSILON {
                        let cmd = ChangeMasterOpacityCommand::new(baseline, new_val);
                        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
                ctx.scrub.active = None;
                DispatchResult::handled()
            }
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
                    ctx.project.settings.led_brightness = v;
                    if let Some(ResolvedScrub::LedBrightness { live, .. }) = &mut ctx.scrub.active {
                        *live = v;
                    }
                    // LED brightness lands via a plain (non-Live) mutation, as
                    // the retired `LedBrightnessChanged` did.
                    ContentCommand::send(
                        ctx.content_tx,
                        ContentCommand::MutateProject(Box::new(move |p| {
                            p.settings.led_brightness = v;
                        })),
                    );
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => {
                if let Some(ResolvedScrub::LedBrightness { baseline, .. }) = &ctx.scrub.active {
                    let baseline = *baseline;
                    let new_val = ctx.project.settings.led_brightness;
                    if (baseline - new_val).abs() > f32::EPSILON {
                        let cmd = ChangeLedBrightnessCommand::new(baseline, new_val);
                        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
                ctx.scrub.active = None;
                DispatchResult::handled()
            }
        },

        ValueRef::LayerOpacity => match phase {
            ScrubPhase::Begin => {
                if let Some(idx) = super::resolve_active_layer_index(active_layer, ctx.project)
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
                    super::resolve_active_layer_index(active_layer, ctx.project),
                ) {
                    if let Some(layer) = ctx.project.timeline.layers.get_mut(idx) {
                        layer.opacity = v;
                    }
                    if let Some(ResolvedScrub::LayerOpacity { live, .. }) = &mut ctx.scrub.active {
                        *live = v;
                    }
                    let layer_id = active_layer.clone().unwrap_or_default();
                    ContentCommand::send(
                        ctx.content_tx,
                        ContentCommand::MutateProjectLive(Box::new(move |p| {
                            if let Some((_, layer)) = p.timeline.find_layer_by_id_mut(&layer_id) {
                                layer.opacity = v;
                            }
                        })),
                    );
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => {
                let baseline = match &ctx.scrub.active {
                    Some(ResolvedScrub::LayerOpacity { baseline, .. }) => Some(*baseline),
                    _ => None,
                };
                if let Some(old_val) = baseline
                    && let Some(idx) = super::resolve_active_layer_index(active_layer, ctx.project)
                    && let Some(layer) = ctx.project.timeline.layers.get(idx)
                {
                    let layer_id = layer.layer_id.clone();
                    let new_val = layer.opacity;
                    if (old_val - new_val).abs() > f32::EPSILON {
                        let cmd = ChangeLayerOpacityCommand::new(layer_id, old_val, new_val);
                        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
                ctx.scrub.active = None;
                DispatchResult::handled()
            }
        },

        ValueRef::Macro(idx) => {
            let idx = *idx;
            match phase {
                ScrubPhase::Begin => {
                    if idx < manifold_core::macro_bank::MACRO_COUNT {
                        let value = ctx.project.settings.macro_bank.slots[idx].value;
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
                        manifold_core::macro_bank::MacroBank::apply_macro(ctx.project, idx, v);
                        ContentCommand::send(
                            ctx.content_tx,
                            ContentCommand::MutateProjectLive(Box::new(move |p| {
                                manifold_core::macro_bank::MacroBank::apply_macro(p, idx, v);
                            })),
                        );
                    }
                    DispatchResult::handled()
                }
                ScrubPhase::Commit => {
                    let baseline = match &ctx.scrub.active {
                        Some(ResolvedScrub::Macro { baseline, .. }) => Some(*baseline),
                        _ => None,
                    };
                    if let Some(old_val) = baseline
                        && idx < manifold_core::macro_bank::MACRO_COUNT
                    {
                        let new_val = ctx.project.settings.macro_bank.slots[idx].value;
                        if (old_val - new_val).abs() > f32::EPSILON {
                            let cmd = ChangeMacroCommand::new(idx, old_val, new_val);
                            ContentCommand::send(
                                ctx.content_tx,
                                ContentCommand::Execute(Box::new(cmd)),
                            );
                        }
                    }
                    ctx.scrub.active = None;
                    DispatchResult::handled()
                }
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
                    && let Some((_, layer)) = ctx.project.timeline.find_layer_by_id_mut(id)
                {
                    layer.audio_gain_db = v;
                    if let Some(ResolvedScrub::LayerAudioGain { live, .. }) = &mut ctx.scrub.active {
                        *live = v;
                    }
                    let id = id.clone();
                    ContentCommand::send(
                        ctx.content_tx,
                        ContentCommand::MutateProjectLive(Box::new(move |p| {
                            if let Some((_, l)) = p.timeline.find_layer_by_id_mut(&id) {
                                l.audio_gain_db = v;
                            }
                        })),
                    );
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => {
                let baseline = match &ctx.scrub.active {
                    Some(ResolvedScrub::LayerAudioGain { baseline, .. }) => Some(*baseline),
                    _ => None,
                };
                if let Some(old_db) = baseline
                    && let Some((_, layer)) = ctx.project.timeline.find_layer_by_id(id)
                {
                    let new_db = layer.audio_gain_db;
                    if (old_db - new_db).abs() > f32::EPSILON {
                        let cmd = manifold_editing::commands::layer::SetLayerAudioGainCommand::new(
                            layer.layer_id.clone(),
                            old_db,
                            new_db,
                        );
                        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
                ctx.scrub.active = None;
                DispatchResult::handled()
            }
        },

        ValueRef::RelightParam(gpt, field) => {
            // Resolve the ui field to the core relight field once per phase, as
            // the retired trio did.
            let f = crate::ui_translate::relight_field_to_editing(*field);
            match phase {
                ScrubPhase::Begin => {
                    if let Some(target) = resolve_graph_target(
                        gpt,
                        ctx.editor_target,
                        effective_tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                    ) {
                        let baseline = ctx
                            .project
                            .with_preset_graph_mut(&target, |inst| f.get(&inst.relight_params));
                        if let Some(baseline) = baseline {
                            ctx.scrub.active = Some(ResolvedScrub::RelightParam {
                                target,
                                field: f,
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
                        resolve_graph_target(
                            gpt,
                            ctx.editor_target,
                            effective_tab,
                            active_layer,
                            ctx.selection,
                            ctx.project,
                        ),
                    ) {
                        if let Some(ResolvedScrub::RelightParam { live, .. }) = &mut ctx.scrub.active
                        {
                            *live = v;
                        }
                        // Live drag: float knobs are per-frame uniforms — no
                        // structure-version bump (D8/P7).
                        ctx.project.with_preset_graph_mut(&target, |inst| {
                            f.set(&mut inst.relight_params, v);
                        });
                        let t = target.clone();
                        ContentCommand::send(
                            ctx.content_tx,
                            ContentCommand::MutateProjectLive(Box::new(move |p| {
                                p.with_preset_graph_mut(&t, |inst| {
                                    f.set(&mut inst.relight_params, v);
                                });
                            })),
                        );
                    }
                    DispatchResult::handled()
                }
                ScrubPhase::Commit => {
                    let baseline = match &ctx.scrub.active {
                        Some(ResolvedScrub::RelightParam { baseline, .. }) => Some(*baseline),
                        _ => None,
                    };
                    if let Some(old_val) = baseline
                        && let Some(target) = resolve_graph_target(
                            gpt,
                            ctx.editor_target,
                            effective_tab,
                            active_layer,
                            ctx.selection,
                            ctx.project,
                        )
                    {
                        let new_val = ctx
                            .project
                            .with_preset_graph_mut(&target, |inst| f.get(&inst.relight_params));
                        if let Some(new_val) = new_val
                            && (old_val - new_val).abs() > f32::EPSILON
                        {
                            let cmd = SetRelightParamCommand::new(target, f, old_val, new_val);
                            ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                        }
                    }
                    ctx.scrub.active = None;
                    DispatchResult::handled()
                }
            }
        }

        ValueRef::Trim(kind, gpt, param_id) => {
            use manifold_ui::panels::TrimKind;
            match phase {
                ScrubPhase::Begin => {
                    if let Some(target) = resolve_graph_target(
                        gpt,
                        ctx.editor_target,
                        effective_tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                    ) {
                        // The ValueRef→resolved mapping done ONCE, at Begin (FORK
                        // 1): resolve the Ableton mapping target for the restore
                        // payload, and read the pre-gesture range from the kind's
                        // store as the undo baseline.
                        let ableton_target = matches!(kind, TrimKind::Ableton)
                            .then(|| {
                                ableton_mapping_target(
                                    &target,
                                    effective_tab,
                                    active_layer,
                                    ctx.project,
                                    param_id,
                                )
                            })
                            .flatten();
                        let baseline = match kind {
                            TrimKind::Driver => ctx
                                .project
                                .with_preset_graph_mut(&target, |inst| {
                                    inst.drivers
                                        .as_ref()
                                        .and_then(|ds| ds.iter().find(|d| d.param_id == *param_id))
                                        .map(|d| (d.trim_min, d.trim_max))
                                })
                                .flatten(),
                            TrimKind::Audio => ctx
                                .project
                                .with_preset_graph_mut(&target, |inst| {
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
                        resolve_graph_target(
                            gpt,
                            ctx.editor_target,
                            effective_tab,
                            active_layer,
                            ctx.selection,
                            ctx.project,
                        ),
                    ) {
                        if let Some(ResolvedScrub::Trim { live, .. }) = &mut ctx.scrub.active {
                            *live = (mn, mx);
                        }
                        // Each kind keeps the exact live edit it had before the
                        // unification (driver dual-edit, audio dual-edit, Ableton
                        // mapping local + content-sync).
                        match kind {
                            TrimKind::Driver => {
                                graph_driver_dual_edit(
                                    ctx.project,
                                    ctx.content_tx,
                                    &target,
                                    param_id.clone(),
                                    move |d| {
                                        d.trim_min = mn;
                                        d.trim_max = mx;
                                    },
                                );
                            }
                            TrimKind::Audio => {
                                graph_audio_mod_dual_edit(
                                    ctx.project,
                                    ctx.content_tx,
                                    &target,
                                    param_id.clone(),
                                    move |m| {
                                        m.shape.range_min = mn;
                                        m.shape.range_max = mx;
                                    },
                                );
                            }
                            TrimKind::Ableton => {
                                if let Some(mapping_target) = ableton_mapping_target(
                                    &target,
                                    effective_tab,
                                    active_layer,
                                    ctx.project,
                                    param_id,
                                ) {
                                    // Local edit + content sync route through the
                                    // shared locate-fork so they can't split.
                                    if let Some(ms) = ctx
                                        .project
                                        .ableton_param_mappings_mut(&mapping_target)
                                        .and_then(|opt| opt.as_mut())
                                        && let Some(m) =
                                            ms.iter_mut().find(|m| m.param_id == *param_id)
                                    {
                                        m.range_min = mn;
                                        m.range_max = mx;
                                    }
                                    let mt = mapping_target.clone();
                                    let pid = param_id.clone();
                                    ContentCommand::send(
                                        ctx.content_tx,
                                        ContentCommand::MutateProject(Box::new(move |p| {
                                            if let Some(ms) = p
                                                .ableton_param_mappings_mut(&mt)
                                                .and_then(|opt| opt.as_mut())
                                                && let Some(m) =
                                                    ms.iter_mut().find(|m| m.param_id == pid)
                                            {
                                                m.range_min = mn;
                                                m.range_max = mx;
                                            }
                                        })),
                                    );
                                }
                            }
                        }
                    }
                    DispatchResult::handled()
                }
                ScrubPhase::Commit => {
                    let baseline = match &ctx.scrub.active {
                        Some(ResolvedScrub::Trim { baseline, .. }) => Some(*baseline),
                        _ => None,
                    };
                    if let Some((old_min, old_max)) = baseline
                        && let Some(target) = resolve_graph_target(
                            gpt,
                            ctx.editor_target,
                            effective_tab,
                            active_layer,
                            ctx.selection,
                            ctx.project,
                        )
                    {
                        match kind {
                            TrimKind::Driver => {
                                let info = ctx
                                    .project
                                    .with_preset_graph_mut(&target, |inst| {
                                        inst.drivers
                                            .as_ref()
                                            .and_then(|ds| {
                                                ds.iter().position(|d| d.param_id == *param_id)
                                            })
                                            .map(|di| {
                                                let d = &inst.drivers.as_ref().unwrap()[di];
                                                (di, d.trim_min, d.trim_max)
                                            })
                                    })
                                    .flatten();
                                if let Some((di, new_min, new_max)) = info
                                    && ((old_min - new_min).abs() > f32::EPSILON
                                        || (old_max - new_max).abs() > f32::EPSILON)
                                {
                                    let cmd = ChangeTrimCommand::new(
                                        DriverTarget::from(&target),
                                        di,
                                        old_min,
                                        old_max,
                                        new_min,
                                        new_max,
                                    );
                                    ContentCommand::send(
                                        ctx.content_tx,
                                        ContentCommand::Execute(Box::new(cmd)),
                                    );
                                }
                            }
                            TrimKind::Audio => {
                                let new_shape = ctx
                                    .project
                                    .with_preset_graph_mut(&target, |inst| {
                                        inst.audio_mods
                                            .as_ref()
                                            .and_then(|ms| {
                                                ms.iter().find(|a| a.param_id == *param_id)
                                            })
                                            .map(|m| m.shape)
                                    })
                                    .flatten();
                                if let Some(new_shape) = new_shape
                                    && ((old_min - new_shape.range_min).abs() > f32::EPSILON
                                        || (old_max - new_shape.range_max).abs() > f32::EPSILON)
                                {
                                    let mut old_shape = new_shape;
                                    old_shape.range_min = old_min;
                                    old_shape.range_max = old_max;
                                    let cmd = SetAudioModShapeCommand::new(
                                        DriverTarget::from(&target),
                                        param_id.clone(),
                                        old_shape,
                                        new_shape,
                                    );
                                    ContentCommand::send(
                                        ctx.content_tx,
                                        ContentCommand::Execute(Box::new(cmd)),
                                    );
                                }
                            }
                            TrimKind::Ableton => {
                                if let Some(mt) = ableton_mapping_target(
                                    &target,
                                    effective_tab,
                                    active_layer,
                                    ctx.project,
                                    param_id,
                                ) {
                                    let new = ctx
                                        .project
                                        .ableton_param_mappings(&mt)
                                        .and_then(|opt| opt.as_ref())
                                        .and_then(|ms| ms.iter().find(|m| m.param_id == *param_id))
                                        .map(|m| (m.range_min, m.range_max));
                                    if let Some((new_min, new_max)) = new
                                        && ((old_min - new_min).abs() > f32::EPSILON
                                            || (old_max - new_max).abs() > f32::EPSILON)
                                    {
                                        let cmd = ChangeAbletonTrimCommand::new(
                                            mt, old_min, old_max, new_min, new_max,
                                        );
                                        ContentCommand::send(
                                            ctx.content_tx,
                                            ContentCommand::Execute(Box::new(cmd)),
                                        );
                                    }
                                }
                            }
                        }
                    }
                    ctx.scrub.active = None;
                    DispatchResult::handled()
                }
            }
        }

        ValueRef::EnvelopeTarget(gpt, param_id) => match phase {
            ScrubPhase::Begin => {
                if let Some(target) = resolve_graph_target(
                    gpt,
                    ctx.editor_target,
                    effective_tab,
                    active_layer,
                    ctx.selection,
                    ctx.project,
                ) {
                    let baseline = ctx
                        .project
                        .with_preset_graph_mut(&target, |inst| {
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
                    resolve_graph_target(
                        gpt,
                        ctx.editor_target,
                        effective_tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                    ),
                ) {
                    if let Some(ResolvedScrub::EnvelopeTarget { live, .. }) = &mut ctx.scrub.active {
                        *live = v;
                    }
                    graph_env_dual_edit(
                        ctx.project,
                        ctx.content_tx,
                        &target,
                        param_id.clone(),
                        move |env| {
                            env.target_normalized = v;
                        },
                    );
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => {
                let baseline = match &ctx.scrub.active {
                    Some(ResolvedScrub::EnvelopeTarget { baseline, .. }) => Some(*baseline),
                    _ => None,
                };
                if let Some(old_target) = baseline
                    && let Some(target) = resolve_graph_target(
                        gpt,
                        ctx.editor_target,
                        effective_tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                    )
                {
                    let info = ctx
                        .project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.envelopes
                                .as_ref()
                                .and_then(|es| es.iter().position(|e| e.param_id == *param_id))
                                .map(|idx| {
                                    (idx, inst.envelopes.as_ref().unwrap()[idx].target_normalized)
                                })
                        })
                        .flatten();
                    if let Some((env_idx, new_t)) = info
                        && (old_target - new_t).abs() > f32::EPSILON
                    {
                        let cmd =
                            ChangeEnvelopeTargetCommand::new(target, env_idx, old_target, new_t);
                        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
                ctx.scrub.active = None;
                DispatchResult::handled()
            }
        },

        ValueRef::EnvDecay(gpt, param_id) => match phase {
            ScrubPhase::Begin => {
                if let Some(target) = resolve_graph_target(
                    gpt,
                    ctx.editor_target,
                    effective_tab,
                    active_layer,
                    ctx.selection,
                    ctx.project,
                ) {
                    let baseline = ctx
                        .project
                        .with_preset_graph_mut(&target, |inst| {
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
                    resolve_graph_target(
                        gpt,
                        ctx.editor_target,
                        effective_tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                    ),
                ) {
                    if let Some(ResolvedScrub::EnvDecay { live, .. }) = &mut ctx.scrub.active {
                        *live = v;
                    }
                    graph_env_dual_edit(
                        ctx.project,
                        ctx.content_tx,
                        &target,
                        param_id.clone(),
                        move |env| {
                            env.decay_beats = v;
                        },
                    );
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => {
                let baseline = match &ctx.scrub.active {
                    Some(ResolvedScrub::EnvDecay { baseline, .. }) => Some(*baseline),
                    _ => None,
                };
                if let Some(old_decay) = baseline
                    && let Some(target) = resolve_graph_target(
                        gpt,
                        ctx.editor_target,
                        effective_tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                    )
                {
                    let info = ctx
                        .project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.envelopes
                                .as_ref()
                                .and_then(|es| es.iter().position(|e| e.param_id == *param_id))
                                .map(|idx| (idx, inst.envelopes.as_ref().unwrap()[idx].decay_beats))
                        })
                        .flatten();
                    if let Some((env_idx, new_d)) = info
                        && (old_decay - new_d).abs() > f32::EPSILON
                    {
                        let cmd =
                            ChangeEnvelopeDecayCommand::new(target, env_idx, old_decay, new_d);
                        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
                ctx.scrub.active = None;
                DispatchResult::handled()
            }
        },

        ValueRef::AudioModShape(gpt, param_id, which) => match phase {
            ScrubPhase::Begin => {
                if let Some(target) = resolve_graph_target(
                    gpt,
                    ctx.editor_target,
                    effective_tab,
                    active_layer,
                    ctx.selection,
                    ctx.project,
                ) {
                    // Capture the WHOLE pre-drag shape as the undo baseline (the
                    // restore path re-stamps all three scalars, so a stomp on a
                    // one-scalar drag can't revert the others).
                    let baseline = ctx
                        .project
                        .with_preset_graph_mut(&target, |inst| {
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
                    resolve_graph_target(
                        gpt,
                        ctx.editor_target,
                        effective_tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                    ),
                ) {
                    let which = *which;
                    if let Some(ResolvedScrub::AudioModShape { live, .. }) = &mut ctx.scrub.active {
                        match which {
                            AudioShapeParam::Sensitivity => live.sensitivity = v,
                            AudioShapeParam::Attack => live.attack_ms = v,
                            AudioShapeParam::Release => live.release_ms = v,
                        }
                    }
                    graph_audio_mod_dual_edit(
                        ctx.project,
                        ctx.content_tx,
                        &target,
                        param_id.clone(),
                        move |m| match which {
                            AudioShapeParam::Sensitivity => m.shape.sensitivity = v,
                            AudioShapeParam::Attack => m.shape.attack_ms = v,
                            AudioShapeParam::Release => m.shape.release_ms = v,
                        },
                    );
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => {
                // One undo step: baseline (old) → current shape (new). Matches the
                // retired trio, including the local `execute` on the UI project.
                let old_shape = match &ctx.scrub.active {
                    Some(ResolvedScrub::AudioModShape { baseline, .. }) => Some(*baseline),
                    _ => None,
                };
                ctx.scrub.active = None;
                if let Some(old_shape) = old_shape
                    && let Some(target) = resolve_graph_target(
                        gpt,
                        ctx.editor_target,
                        effective_tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                    )
                {
                    let new_shape = ctx
                        .project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.audio_mods
                                .as_ref()
                                .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                                .map(|m| m.shape)
                        })
                        .flatten();
                    if let Some(new_shape) = new_shape
                        && new_shape != old_shape
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(SetAudioModShapeCommand::new(
                                DriverTarget::from(&target),
                                param_id.clone(),
                                old_shape,
                                new_shape,
                            ));
                        boxed.execute(ctx.project);
                        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                    }
                }
                DispatchResult::handled()
            }
        },

        ValueRef::AudioModStepAmount(gpt, param_id) => match phase {
            ScrubPhase::Begin => {
                if let Some(target) = resolve_graph_target(
                    gpt,
                    ctx.editor_target,
                    effective_tab,
                    active_layer,
                    ctx.selection,
                    ctx.project,
                ) {
                    // Capture the WHOLE pre-drag action as the undo baseline (the
                    // commit diffs it against the new action); the restore path
                    // re-stamps only the live amount, preserving wrap.
                    let baseline = ctx
                        .project
                        .with_preset_graph_mut(&target, |inst| {
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
                    resolve_graph_target(
                        gpt,
                        ctx.editor_target,
                        effective_tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                    ),
                ) {
                    if let Some(ResolvedScrub::AudioModStepAmount { live_amount, .. }) =
                        &mut ctx.scrub.active
                    {
                        *live_amount = v;
                    }
                    // Re-stamp the Step action preserving the current wrap — the
                    // same dual-edit the retired `AudioModStepAmountChanged` arm ran.
                    graph_audio_mod_dual_edit(
                        ctx.project,
                        ctx.content_tx,
                        &target,
                        param_id.clone(),
                        move |m| {
                            let wrap = match m.action {
                                TriggerAction::Step { wrap, .. } => wrap,
                                _ => manifold_core::audio_mod::WrapMode::Wrap,
                            };
                            m.action = TriggerAction::Step { amount: v, wrap };
                        },
                    );
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => {
                // One undo step: baseline action (old) → current action (new),
                // via `SetAudioModActionCommand` — matches the retired trio,
                // including the local `execute` on the UI project.
                let old_action = match &ctx.scrub.active {
                    Some(ResolvedScrub::AudioModStepAmount { baseline, .. }) => Some(*baseline),
                    _ => None,
                };
                ctx.scrub.active = None;
                if let Some(old_action) = old_action
                    && let Some(target) = resolve_graph_target(
                        gpt,
                        ctx.editor_target,
                        effective_tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                    )
                {
                    let new_action = ctx
                        .project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.find_audio_mod(param_id.as_ref()).map(|m| m.action)
                        })
                        .flatten();
                    if let Some(new_action) = new_action
                        && new_action != old_action
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(SetAudioModActionCommand::new(
                                DriverTarget::from(&target),
                                param_id.clone(),
                                old_action,
                                new_action,
                            ));
                        boxed.execute(ctx.project);
                        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                    }
                }
                DispatchResult::handled()
            }
        },

        ValueRef::AudioTriggerShape(layer_id, index, which) => match phase {
            ScrubPhase::Begin => {
                // Addressed directly by `(layer_id, index)` — no
                // `resolve_graph_target`. Capture the WHOLE pre-drag shape as the
                // undo baseline (the restore path re-stamps all three scalars).
                let baseline = ctx
                    .project
                    .timeline
                    .find_layer_by_id_mut(layer_id)
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
                    if let Some(ResolvedScrub::AudioTriggerShape { live, .. }) = &mut ctx.scrub.active
                    {
                        match which {
                            AudioShapeParam::Sensitivity => live.sensitivity = v,
                            AudioShapeParam::Attack => live.attack_ms = v,
                            AudioShapeParam::Release => live.release_ms = v,
                        }
                    }
                    clip_trigger_shape_dual_edit(
                        ctx.project,
                        ctx.content_tx,
                        layer_id,
                        *index,
                        move |shape| match which {
                            AudioShapeParam::Sensitivity => shape.sensitivity = v,
                            AudioShapeParam::Attack => shape.attack_ms = v,
                            AudioShapeParam::Release => shape.release_ms = v,
                        },
                    );
                }
                DispatchResult::handled()
            }
            ScrubPhase::Commit => {
                // One undo step: baseline shape (old) → current shape (new), via
                // `SetLayerClipTriggerCommand` diffing the whole `ClipTrigger` —
                // matches the retired trio.
                let old_shape = match &ctx.scrub.active {
                    Some(ResolvedScrub::AudioTriggerShape { baseline, .. }) => Some(*baseline),
                    _ => None,
                };
                ctx.scrub.active = None;
                if let Some(old_shape) = old_shape {
                    let current = ctx
                        .project
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
        },
    }
}
