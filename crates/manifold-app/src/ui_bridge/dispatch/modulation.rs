//! Inspector dispatch handlers: the modulation domain (UI_FUNNEL_DECOMPOSITION
//! P-B, D6) — drivers, audio modulation, envelopes, and their shared trim
//! handles across effect and generator params. One slice of the inspector
//! dispatch, reached by `dispatch_inspector`'s first-non-unhandled chain. Arms
//! are the former `dispatch_inspector` arms VERBATIM (they already read `ctx`
//! fields directly); a `_ => unhandled()` fall-through lets the chain advance.
//!
//! D-11: `effective_tab`/`active_layer` are computed once near the top of
//! `dispatch_inspector` in inspector.rs; this sub-dispatcher cannot see that
//! outer function's locals, so it recomputes them here — the same two
//! lines, byte-exact, as the sanctioned preamble.

use manifold_core::effects::{ParamEnvelope, ParameterDriver};
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_editing::commands::drivers::{
    AddDriverCommand, ChangeDriverBeatDivCommand, ChangeDriverWaveformCommand,
    SetDriverFreePeriodCommand, ToggleDriverEnabledCommand, ToggleDriverReversedCommand,
};
use manifold_editing::commands::audio_mod::{
    AddAudioModCommand, RemoveAudioModCommand, SetAudioModActionCommand, SetAudioModShapeCommand,
    SetAudioModSourceCommand, SetAudioModTriggerModeCommand, ToggleAudioModEnabledCommand,
};
use manifold_editing::commands::effect_target::DriverTarget;
use manifold_editing::commands::envelopes::{AddEnvelopeCommand, ToggleEnvelopeEnabledCommand};
use manifold_ui::{DriverConfigAction, InspectorTab, ModulationAction};

use super::super::DispatchResult;
use super::resolve::resolve_mod_target;
use crate::content_command::ContentCommand;

pub(crate) fn dispatch_modulation(action: &ModulationAction, ctx: &mut super::super::DispatchCtx) -> DispatchResult {
    let (effective_tab, effective_active_layer) = super::editor_dispatch_context(ctx.editor_target, &*ctx.project, ctx.ui.inspector.last_effect_tab(), ctx.active_layer);
    let active_layer = &effective_active_layer;
    match action {
        ModulationAction::DriverToggle(gpt, param_id) => {
            // BUG-249: scene rows redirect to their real exposed param
            // (materializing the exposure on first arm) — see
            // `resolve_mod_target`. Non-scene ids resolve exactly as before.
            let Some((target, param_id)) = resolve_mod_target(
                ctx.ui, ctx.project, ctx.content_tx, gpt, param_id, ctx.editor_target, effective_tab, active_layer,
                ctx.selection, true,
            ) else {
                return DispatchResult::structural();
            };
            let param_id = &param_id;
            // Read the driver state off the SAME instance the command targets,
            // by target — never an ambient row index — so an editor-card driver
            // edit can't split (command -> watched instance, di -> another).
            let Some((existing, base_value)) = ctx.project.with_preset_graph_mut(&target, |inst| {
                let existing = inst
                    .drivers
                    .as_ref()
                    .and_then(|ds| ds.iter().position(|d| d.param_id == *param_id))
                    .map(|di| (di, inst.drivers.as_ref().unwrap()[di].enabled));
                let base_value = inst.get_param(param_id.as_ref());
                (existing, base_value)
            }) else {
                return DispatchResult::structural();
            };
            let driver_target = DriverTarget::from(&target);
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                if let Some((di, old)) = existing {
                    Box::new(ToggleDriverEnabledCommand::new(driver_target, di, old, !old))
                } else {
                    let driver = ParameterDriver {
                        param_id: param_id.clone(),
                        beat_division: BeatDivision::Quarter,
                        waveform: DriverWaveform::Sine,
                        enabled: true,
                        phase: 0.0,
                        base_value,
                        trim_min: 0.0,
                        trim_max: 1.0,
                        reversed: false,
                        free_period_beats: None,
                        legacy_param_index: None,
                        is_paused_by_user: false,
                    };
                    Box::new(AddDriverCommand::new(driver_target, driver))
                };
            boxed.execute(ctx.project);
            ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        ModulationAction::AudioModToggle(gpt, param_id) => {
            let Some((target, param_id)) = resolve_mod_target(
                ctx.ui, ctx.project, ctx.content_tx, gpt, param_id, ctx.editor_target, effective_tab, active_layer,
                ctx.selection, true,
            ) else {
                return DispatchResult::structural();
            };
            let param_id = &param_id;
            // Existing mod's enabled state, read off the resolved instance.
            let existing = ctx.project
                .with_preset_graph_mut(&target, |inst| {
                    inst.find_audio_mod(param_id.as_ref()).map(|a| a.enabled)
                })
                .flatten();
            let driver_target = DriverTarget::from(&target);
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                if let Some(old) = existing {
                    Box::new(ToggleAudioModEnabledCommand::new(
                        driver_target,
                        param_id.clone(),
                        old,
                        !old,
                    ))
                } else {
                    // Arm: assign the project's first audio send. No sends → inert
                    // (the audio button stays a no-op until the Audio Setup defines one).
                    let Some(send_id) = ctx.project.audio_setup.sends.first().map(|s| s.id.clone())
                    else {
                        return DispatchResult::structural();
                    };
                    let m = manifold_core::audio_mod::ParameterAudioMod::new(
                        param_id.clone(),
                        send_id,
                        manifold_core::AudioFeature::default(),
                    );
                    Box::new(AddAudioModCommand::new(driver_target, m))
                };
            boxed.execute(ctx.project);
            ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        ModulationAction::AudioModSetSource(gpt, param_id, send_id, feature) => {
            let Some((target, param_id)) = resolve_mod_target(
                ctx.ui, ctx.project, ctx.content_tx, gpt, param_id, ctx.editor_target, effective_tab, active_layer,
                ctx.selection, true,
            ) else {
                return DispatchResult::structural();
            };
            let param_id = &param_id;
            let old_source = ctx.project
                .with_preset_graph_mut(&target, |inst| {
                    inst.find_audio_mod(param_id.as_ref()).map(|a| a.source.clone())
                })
                .flatten();
            let driver_target = DriverTarget::from(&target);
            let new_source = manifold_core::audio_mod::AudioModSource {
                send_id: send_id.clone(),
                feature: crate::ui_translate::audio_feature_to_core(*feature),
            };
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                if let Some(old) = old_source {
                    Box::new(SetAudioModSourceCommand::new(
                        driver_target,
                        param_id.clone(),
                        old,
                        new_source,
                    ))
                } else {
                    let m = manifold_core::audio_mod::ParameterAudioMod::new(
                        param_id.clone(),
                        send_id.clone(),
                        crate::ui_translate::audio_feature_to_core(*feature),
                    );
                    Box::new(AddAudioModCommand::new(driver_target, m))
                };
            boxed.execute(ctx.project);
            ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        ModulationAction::AudioModRemove(gpt, param_id) => {
            let Some((target, param_id)) = resolve_mod_target(
                ctx.ui, ctx.project, ctx.content_tx, gpt, param_id, ctx.editor_target, effective_tab, active_layer,
                ctx.selection, false,
            ) else {
                return DispatchResult::structural();
            };
            let param_id = &param_id;
            let driver_target = DriverTarget::from(&target);
            let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                Box::new(RemoveAudioModCommand::new(driver_target, param_id.clone()));
            boxed.execute(ctx.project);
            ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            DispatchResult::structural()
        }
        ModulationAction::AudioModSetInvert(gpt, param_id) => {
            // Flip the mod's invert flag in one undo step. Reads the current
            // shape, flips `invert`, commits old→new via the shape command.
            if let Some((target, param_id)) = resolve_mod_target(
                ctx.ui, ctx.project, ctx.content_tx, gpt, param_id, ctx.editor_target, effective_tab, active_layer,
                ctx.selection, false,
            ) {
                let param_id = &param_id;
                let old_shape = ctx.project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.audio_mods
                            .as_ref()
                            .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                            .map(|m| m.shape)
                    })
                    .flatten();
                if let Some(old_shape) = old_shape {
                    let mut new_shape = old_shape;
                    new_shape.invert = !old_shape.invert;
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

        ModulationAction::AudioModSetRateOfChange(gpt, param_id) => {
            // Flip the mod's rate-of-change flag in one undo step — same shape
            // path as invert: read the current shape, flip `rate_of_change`,
            // commit old→new via the shape command.
            if let Some((target, param_id)) = resolve_mod_target(
                ctx.ui, ctx.project, ctx.content_tx, gpt, param_id, ctx.editor_target, effective_tab, active_layer,
                ctx.selection, false,
            ) {
                let param_id = &param_id;
                let old_shape = ctx.project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.audio_mods
                            .as_ref()
                            .and_then(|ms| ms.iter().find(|a| a.param_id == *param_id))
                            .map(|m| m.shape)
                    })
                    .flatten();
                if let Some(old_shape) = old_shape {
                    let mut new_shape = old_shape;
                    new_shape.rate_of_change = !old_shape.rate_of_change;
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

        // Audio-mod shaping-slider scrub trio (sensitivity / attack / release)
        // migrated to the unified `PanelAction::Scrub` wire
        // (`ValueRef::AudioModShape`, P-I / D4): the `AudioShapeParam` rides the
        // address, the whole shape is the restore payload.

        // §9 U3: a trigger-gate row's Mode button — set `trigger_mode` on the
        // SAME `ParameterAudioMod` every other drawer edit targets (no
        // separate per-instance config, no separate command family).
        ModulationAction::AudioModSetTriggerMode(gpt, param_id, mode_idx) => {
            if let Some((target, param_id)) = resolve_mod_target(
                ctx.ui, ctx.project, ctx.content_tx, gpt, param_id, ctx.editor_target, effective_tab, active_layer,
                ctx.selection, false,
            ) {
                let param_id = &param_id;
                let old_mode = ctx.project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.find_audio_mod(param_id.as_ref()).and_then(|m| m.trigger_mode)
                    })
                    .flatten();
                let new_mode = Some(match mode_idx {
                    1 => manifold_core::audio_trigger::TriggerFireMode::Transient,
                    2 => manifold_core::audio_trigger::TriggerFireMode::Both,
                    _ => manifold_core::audio_trigger::TriggerFireMode::ClipEdge,
                });
                if new_mode != old_mode {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(SetAudioModTriggerModeCommand::new(
                            DriverTarget::from(&target),
                            param_id.clone(),
                            old_mode,
                            new_mode,
                        ));
                    boxed.execute(ctx.project);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }

        // PARAM_STEP_ACTIONS D8: the Action segmented row. Entering Step from
        // a non-Step action seeds `amount`/`wrap` from the param's own spec
        // (D2's UI-seeding default); re-clicking Step while already Step is a
        // no-op (keeps the user's dialed-in amount/wrap). Structural: the
        // Amount/Wrap/Mode rows and the collapsed "A"→"S"/"R" glyph all
        // depend on which action is armed.
        ModulationAction::AudioModSetActionKind(gpt, param_id, kind_idx) => {
            if let Some((target, param_id)) = resolve_mod_target(
                ctx.ui, ctx.project, ctx.content_tx, gpt, param_id, ctx.editor_target, effective_tab, active_layer,
                ctx.selection, false,
            ) {
                let param_id = &param_id;
                let (old_action, min, max, whole_numbers) = ctx.project
                    .with_preset_graph_mut(&target, |inst| {
                        let action = inst.find_audio_mod(param_id.as_ref()).map(|m| m.action);
                        let spec = inst.params.get(param_id.as_ref());
                        (
                            action,
                            spec.map(|p| p.spec.min).unwrap_or(0.0),
                            spec.map(|p| p.spec.max).unwrap_or(1.0),
                            spec.map(|p| p.whole_numbers()).unwrap_or(false),
                        )
                    })
                    .unwrap_or((None, 0.0, 1.0, false));
                if let Some(old_action) = old_action {
                    let new_action = match kind_idx {
                        1 => match old_action {
                            manifold_core::audio_mod::TriggerAction::Step { .. } => old_action,
                            _ => manifold_core::audio_mod::TriggerAction::Step {
                                amount: manifold_core::audio_mod::default_step_amount(
                                    min,
                                    max,
                                    whole_numbers,
                                ),
                                wrap: manifold_core::audio_mod::WrapMode::Wrap,
                            },
                        },
                        2 => manifold_core::audio_mod::TriggerAction::Random,
                        _ => manifold_core::audio_mod::TriggerAction::Continuous,
                    };
                    if new_action != old_action {
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
            }
            DispatchResult::structural()
        }

        // Audio-mod Step-amount scrub trio migrated to the unified
        // `PanelAction::Scrub` wire (`ValueRef::AudioModStepAmount`, P-I / D4):
        // the dragged amount rides `ScrubValue::Scalar`, the whole pre-drag
        // `TriggerAction` is the undo baseline, and Commit emits
        // `SetAudioModActionCommand`.

        // The Wrap segmented row — only meaningful while Action=Step; a stray
        // click while some other action is armed (shouldn't happen — the row
        // isn't built then) is a harmless no-op.
        ModulationAction::AudioModSetWrap(gpt, param_id, wrap_idx) => {
            if let Some((target, param_id)) = resolve_mod_target(
                ctx.ui, ctx.project, ctx.content_tx, gpt, param_id, ctx.editor_target, effective_tab, active_layer,
                ctx.selection, false,
            ) {
                let param_id = &param_id;
                let old_action = ctx.project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.find_audio_mod(param_id.as_ref()).map(|m| m.action)
                    })
                    .flatten();
                if let Some(manifold_core::audio_mod::TriggerAction::Step { amount, .. }) = old_action {
                    let wrap = match wrap_idx {
                        1 => manifold_core::audio_mod::WrapMode::Bounce,
                        2 => manifold_core::audio_mod::WrapMode::Clamp,
                        _ => manifold_core::audio_mod::WrapMode::Wrap,
                    };
                    let new_action = manifold_core::audio_mod::TriggerAction::Step { amount, wrap };
                    if let Some(old_action) = old_action
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
            }
            DispatchResult::structural()
        }

        ModulationAction::EnvelopeToggle(gpt, param_id) => {
            // Envelope-home unification: the envelope rides on the resolved
            // instance (keyed by param_id) for effects and generators alike.
            // Toggle the existing one's `enabled`, or create a fresh enabled
            // envelope. Effects are clip-timed, so only layer effects get
            // envelopes (master/clip have no trigger — the button is inert
            // there); generators are layer-scoped, always permitted.
            if let Some((target, param_id)) = resolve_mod_target(
                ctx.ui, ctx.project, ctx.content_tx, gpt, param_id, ctx.editor_target, effective_tab, active_layer,
                ctx.selection, true,
            ) {
                let param_id = &param_id;
                let env_allowed = match &target {
                    manifold_core::GraphTarget::Generator(_) => true,
                    manifold_core::GraphTarget::Effect(_) => {
                        matches!(effective_tab, InspectorTab::Layer)
                    }
                };
                if env_allowed {
                    // Undo-tracked like the DriverToggle/AudioModToggle
                    // siblings (was `MutateProject` — arming or flipping an
                    // envelope recorded NO undo entry): existing envelope →
                    // flip `enabled`; none → add a fresh enabled one.
                    let existing = ctx.project
                        .with_preset_graph_mut(&target, |inst| {
                            inst.envelopes
                                .as_ref()
                                .and_then(|envs| {
                                    envs.iter()
                                        .position(|e| e.param_id == *param_id)
                                        .map(|idx| (idx, envs[idx].enabled))
                                })
                        })
                        .flatten();
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        if let Some((idx, old)) = existing {
                            Box::new(ToggleEnvelopeEnabledCommand::new(
                                target.clone(),
                                idx,
                                old,
                                !old,
                            ))
                        } else {
                            Box::new(AddEnvelopeCommand::new(
                                target.clone(),
                                ParamEnvelope::new(param_id.clone()),
                            ))
                        };
                    boxed.execute(ctx.project);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        ModulationAction::DriverConfig(gpt, param_id, cfg) => {
            let Some((target, param_id)) = resolve_mod_target(
                ctx.ui, ctx.project, ctx.content_tx, gpt, param_id, ctx.editor_target, effective_tab, active_layer,
                ctx.selection, false,
            ) else {
                return DispatchResult::structural();
            };
            let param_id = &param_id;
            let driver_target = DriverTarget::from(&target);
            // Read the driver's current config off the same instance the
            // command targets (by GraphTarget), so an editor-card edit can't
            // split command vs row index.
            let info = ctx.project
                .with_preset_graph_mut(&target, |inst| {
                    inst.drivers
                        .as_ref()
                        .and_then(|ds| ds.iter().position(|d| d.param_id == *param_id))
                        .map(|di| {
                            let d = &inst.drivers.as_ref().unwrap()[di];
                            (di, d.beat_division, d.waveform, d.reversed, d.free_period_beats)
                        })
                })
                .flatten();
            if let Some((di, beat_division, waveform, reversed, free)) = info {
                type BoxedCmd = Box<dyn manifold_editing::command::Command + Send>;
                // The feel segment sets the division's modifier from its base; a
                // base without a dotted/triplet variant (e.g. 1/32) keeps the base.
                let base = beat_division.base_division();
                let cmd: Option<BoxedCmd> = match cfg {
                    DriverConfigAction::BeatDiv(idx) => BeatDivision::from_button_index(*idx)
                        .map(|new_div| {
                            Box::new(ChangeDriverBeatDivCommand::new(
                                driver_target,
                                di,
                                beat_division,
                                new_div,
                                free,
                            )) as BoxedCmd
                        }),
                    DriverConfigAction::Wave(idx) => DriverWaveform::from_index(*idx).map(|new_wave| {
                        Box::new(ChangeDriverWaveformCommand::new(
                            driver_target,
                            di,
                            waveform,
                            new_wave,
                        )) as BoxedCmd
                    }),
                    DriverConfigAction::Straight => Some(Box::new(ChangeDriverBeatDivCommand::new(
                        driver_target,
                        di,
                        beat_division,
                        base,
                        free,
                    )) as BoxedCmd),
                    DriverConfigAction::Dotted => Some(Box::new(ChangeDriverBeatDivCommand::new(
                        driver_target,
                        di,
                        beat_division,
                        base.toggle_dotted().unwrap_or(base),
                        free,
                    )) as BoxedCmd),
                    DriverConfigAction::Triplet => Some(Box::new(ChangeDriverBeatDivCommand::new(
                        driver_target,
                        di,
                        beat_division,
                        base.toggle_triplet().unwrap_or(base),
                        free,
                    )) as BoxedCmd),
                    DriverConfigAction::Invert => Some(Box::new(ToggleDriverReversedCommand::new(
                        driver_target,
                        di,
                        reversed,
                        !reversed,
                    )) as BoxedCmd),
                    DriverConfigAction::SetFreePeriod(p) => {
                        Some(Box::new(SetDriverFreePeriodCommand::new(
                            driver_target,
                            di,
                            free,
                            Some(*p),
                        )) as BoxedCmd)
                    }
                };
                if let Some(mut boxed) = cmd {
                    boxed.execute(ctx.project);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        // Trim-range scrub trio (driver / audio / Ableton) migrated to the
        // unified `PanelAction::Scrub` wire (`ValueRef::Trim`, P-I / D4).
        // Envelope-target scrub trio migrated to the unified `PanelAction::Scrub`
        // wire (`ValueRef::EnvelopeTarget`, P-I / D4).
        // ── Modulation undo: snapshot/commit ────────────────────────
        // Trim-range, envelope-target, and envelope-decay scrub trios migrated to
        // the unified `PanelAction::Scrub` wire (`ValueRef::Trim` /
        // `ValueRef::EnvelopeTarget` / `ValueRef::EnvDecay`, P-I / D4): Begin
        // captures the undo baseline, Commit emits the undo command.
    }
}
