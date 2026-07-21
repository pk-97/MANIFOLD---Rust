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
use manifold_core::GraphTarget;
use manifold_editing::commands::effects::ChangeGraphParamCommand;
use manifold_ui::{ScrubPhase, ValueRef};

use super::dispatch::resolve::resolve_graph_target;
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
    /// Envelope target-handle drag snapshot for undo.
    pub target_snapshot: Option<f32>,
    /// Envelope decay-slider drag snapshot for undo.
    pub decay_snapshot: Option<f32>,
    /// Audio-mod shaping-slider drag snapshot (whole shape) for undo.
    pub audio_shape_snapshot: Option<AudioModShape>,
    /// Step-Amount drag snapshot (PARAM_STEP_ACTIONS D8) for undo.
    pub audio_action_snapshot: Option<TriggerAction>,
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
    }
}
