//! Inspector dispatch handlers: the params domain (UI_FUNNEL_DECOMPOSITION
//! P-B, D6) — value edits, trims, and toggles on the inspected effect,
//! generator, layer, and master parameters, plus the effect/generator card
//! and preset-library management actions that ride the same resolve path.
//! One slice of the inspector dispatch, reached by `dispatch_inspector`'s
//! first-non-unhandled chain. Arms are the former `dispatch_inspector` arms
//! VERBATIM (they already read `ctx` fields directly); a `_ => unhandled()`
//! fall-through lets the chain advance.
//!
//! D-11: `effective_tab`/`active_layer` are computed once near the top of
//! `dispatch_inspector` in inspector.rs; this sub-dispatcher cannot see that
//! outer function's locals, so it recomputes them here — the same two
//! lines, byte-exact, as the sanctioned preamble.

use crate::content_command::ContentCommand;
use manifold_core::effects::PresetInstance;
use manifold_editing::command::Command;
use manifold_editing::commands::effect_target::EffectTarget;
use manifold_editing::commands::effects::{
    ChangeGraphParamCommand, RemoveEffectCommand, ReorderEffectCommand, ReorderEffectGroupCommand,
    SetRelightHeightFromCommand, SetRelightParamCommand, ToggleEffectCommand, ToggleRelightCommand,
};
use manifold_editing::commands::settings::{
    ChangeLayerOpacityCommand, ChangeLedBrightnessCommand, ChangeMacroCommand,
    ChangeMasterOpacityCommand, PasteGeneratorCommand,
};
use manifold_ui::{InspectorTab, ParamsAction};

use super::super::DispatchResult;
use super::{resolve_effects_mut, resolve_effects_read};
use super::resolve::{preset_source_def, resolve_graph_target};

pub(crate) fn dispatch_params(action: &ParamsAction, ctx: &mut super::super::DispatchCtx) -> DispatchResult {
    let (effective_tab, effective_active_layer) = super::editor_dispatch_context(ctx.editor_target, &*ctx.project, ctx.ui.inspector.last_effect_tab(), ctx.active_layer);
    let active_layer = &effective_active_layer;
    match action {
        // ── Macros panel collapse ─────────────────────────────────
        ParamsAction::MacrosCollapseToggle => {
            ctx.ui.inspector.macros_panel_mut().toggle_collapsed();
            DispatchResult::structural()
        }

        // ── Macro sliders ─────────────────────────────────────────
        ParamsAction::MacroSnapshot(idx) => {
            let idx = *idx;
            if idx < manifold_core::macro_bank::MACRO_COUNT {
                let value = ctx.project.settings.macro_bank.slots[idx].value;
                ctx.scrub.slider_snapshot = Some(value);
                // Macros ride in every ModulationSnapshot block, so the drag
                // must be guarded or the per-tick apply stomps it (undo-race
                // regression, 2026-07-18).
                ctx.scrub.active_inspector_drag = Some(crate::app::ActiveInspectorDrag::Macro { idx, value });
            }
            DispatchResult::handled()
        }
        ParamsAction::MacroChanged(idx, val) => {
            let idx = *idx;
            let val = *val;
            if let Some(crate::app::ActiveInspectorDrag::Macro { idx: di, value }) =
                &mut ctx.scrub.active_inspector_drag
                && *di == idx
            {
                *value = val;
            }
            manifold_core::macro_bank::MacroBank::apply_macro(ctx.project, idx, val);
            ContentCommand::send(
                ctx.content_tx,
                ContentCommand::MutateProjectLive(Box::new(move |p| {
                    manifold_core::macro_bank::MacroBank::apply_macro(p, idx, val);
                })),
            );
            DispatchResult::handled()
        }
        ParamsAction::MacroCommit(idx) => {
            if let Some(old_val) = ctx.scrub.slider_snapshot.take() {
                let idx = *idx;
                if idx < manifold_core::macro_bank::MACRO_COUNT {
                    let new_val = ctx.project.settings.macro_bank.slots[idx].value;
                    if (old_val - new_val).abs() > f32::EPSILON {
                        let cmd = ChangeMacroCommand::new(idx, old_val, new_val);
                        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                    }
                }
            }
            ctx.scrub.active_inspector_drag = None;
            DispatchResult::handled()
        }
        ParamsAction::MacroReset(idx) => {
            let idx = *idx;
            if idx < manifold_core::macro_bank::MACRO_COUNT {
                let old = ctx.project.settings.macro_bank.slots[idx].value;
                if old.abs() > f32::EPSILON {
                    manifold_core::macro_bank::MacroBank::apply_macro(ctx.project, idx, 0.0);
                    let cmd = ChangeMacroCommand::new(idx, old, 0.0);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }
        ParamsAction::MacroLabelRename(_) => DispatchResult::handled(),

        // ── Master chrome ──────────────────────────────────────────
        ParamsAction::MasterOpacitySnapshot => {
            ctx.scrub.slider_snapshot = Some(ctx.project.settings.master_opacity);
            ctx.scrub.active_inspector_drag = Some(crate::app::ActiveInspectorDrag::MasterOpacity(
                ctx.project.settings.master_opacity,
            ));
            DispatchResult::handled()
        }
        ParamsAction::MasterOpacityChanged(val) => {
            ctx.project.settings.master_opacity = *val;
            if let Some(crate::app::ActiveInspectorDrag::MasterOpacity(v)) = &mut ctx.scrub.active_inspector_drag {
                *v = *val;
            }
            let v = *val;
            ContentCommand::send(
                ctx.content_tx,
                ContentCommand::MutateProjectLive(Box::new(move |p| {
                    p.settings.master_opacity = v;
                })),
            );
            DispatchResult::handled()
        }
        ParamsAction::MasterOpacityCommit => {
            if let Some(old_val) = ctx.scrub.slider_snapshot.take() {
                let new_val = ctx.project.settings.master_opacity;
                if (old_val - new_val).abs() > f32::EPSILON {
                    let cmd = ChangeMasterOpacityCommand::new(old_val, new_val);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            ctx.scrub.active_inspector_drag = None;
            DispatchResult::handled()
        }
        // ── Audio-layer gain slider (layer header) ─────────────────
        ParamsAction::AudioGainSnapshot(id) => {
            ctx.scrub.slider_snapshot = ctx.project
                .timeline
                .find_layer_by_id(id)
                .map(|(_, l)| l.audio_gain_db);
            if let Some(db) = ctx.scrub.slider_snapshot {
                ctx.scrub.active_inspector_drag = Some(crate::app::ActiveInspectorDrag::AudioGain {
                    layer_id: id.clone(),
                    db,
                });
            }
            DispatchResult::handled()
        }
        ParamsAction::AudioGainChanged(id, db) => {
            let db = *db;
            if let Some(crate::app::ActiveInspectorDrag::AudioGain { db: guard, .. }) =
                &mut ctx.scrub.active_inspector_drag
            {
                *guard = db;
            }
            if let Some((_, layer)) = ctx.project.timeline.find_layer_by_id_mut(id) {
                layer.audio_gain_db = db;
                let id = id.clone();
                ContentCommand::send(
                    ctx.content_tx,
                    ContentCommand::MutateProjectLive(Box::new(move |p| {
                        if let Some((_, l)) = p.timeline.find_layer_by_id_mut(&id) {
                            l.audio_gain_db = db;
                        }
                    })),
                );
            }
            DispatchResult::handled()
        }
        ParamsAction::AudioGainCommit(id) => {
            ctx.scrub.active_inspector_drag = None;
            if let Some(old_db) = ctx.scrub.slider_snapshot.take()
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
            DispatchResult::handled()
        }
        ParamsAction::MasterCollapseToggle => {
            ctx.ui.inspector.master_chrome_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        ParamsAction::MasterExitPathClicked => {
            // Handled by try_open_dropdown in ui_root.rs — opens exit path dropdown.
            DispatchResult::handled()
        }
        ParamsAction::SetLedExitIndex(idx) => {
            let idx = *idx;
            ctx.project.settings.led_exit_index = idx;
            // Push to content thread so the LED pipeline picks it up
            ContentCommand::send(
                ctx.content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    p.settings.led_exit_index = idx;
                })),
            );
            DispatchResult::handled()
        }
        // ── LED enabled toggle ───────────────────────────────────
        ParamsAction::LedEnabledToggle => {
            let new_enabled = !ctx.content_state.led_enabled;
            // Persist the new ON/OFF state in project settings so the LED
            // pipeline auto-initialises on project load.
            ctx.project.settings.led_enabled = new_enabled;
            ContentCommand::send(
                ctx.content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    p.settings.led_enabled = new_enabled;
                })),
            );
            if new_enabled {
                let settings = manifold_led::LedSettings {
                    enabled: true,
                    ..Default::default()
                };
                ContentCommand::send(
                    ctx.content_tx,
                    ContentCommand::InitLedOutput(Box::new(settings)),
                );
            } else {
                ContentCommand::send(ctx.content_tx, ContentCommand::ShutdownLedOutput);
            }
            DispatchResult::handled()
        }

        // ── LED brightness ───────────────────────────────────────
        ParamsAction::LedBrightnessSnapshot => {
            ctx.scrub.slider_snapshot = Some(ctx.project.settings.led_brightness);
            ctx.scrub.active_inspector_drag = Some(crate::app::ActiveInspectorDrag::LedBrightness(
                ctx.project.settings.led_brightness,
            ));
            DispatchResult::handled()
        }
        ParamsAction::LedBrightnessChanged(val) => {
            ctx.project.settings.led_brightness = *val;
            if let Some(crate::app::ActiveInspectorDrag::LedBrightness(v)) = &mut ctx.scrub.active_inspector_drag {
                *v = *val;
            }
            let v = *val;
            ContentCommand::send(
                ctx.content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    p.settings.led_brightness = v;
                })),
            );
            DispatchResult::handled()
        }
        ParamsAction::LedBrightnessCommit => {
            if let Some(old_val) = ctx.scrub.slider_snapshot.take() {
                let new_val = ctx.project.settings.led_brightness;
                if (old_val - new_val).abs() > f32::EPSILON {
                    let cmd = ChangeLedBrightnessCommand::new(old_val, new_val);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            ctx.scrub.active_inspector_drag = None;
            DispatchResult::handled()
        }
        // ── Layer chrome ───────────────────────────────────────────
        ParamsAction::LayerOpacitySnapshot => {
            let layer_idx = super::resolve_active_layer_index(active_layer, ctx.project);
            if let Some(idx) = layer_idx
                && let Some(layer) = ctx.project.timeline.layers.get(idx)
            {
                ctx.scrub.slider_snapshot = Some(layer.opacity);
                ctx.scrub.active_inspector_drag = Some(crate::app::ActiveInspectorDrag::LayerOpacity {
                    layer_id: layer.layer_id.clone(),
                    value: layer.opacity,
                });
            }
            DispatchResult::handled()
        }
        ParamsAction::LayerOpacityChanged(val) => {
            let layer_idx = super::resolve_active_layer_index(active_layer, ctx.project);
            if let Some(idx) = layer_idx {
                if let Some(layer) = ctx.project.timeline.layers.get_mut(idx) {
                    layer.opacity = *val;
                }
                if let Some(crate::app::ActiveInspectorDrag::LayerOpacity { value, .. }) =
                    &mut ctx.scrub.active_inspector_drag
                {
                    *value = *val;
                }
                let v = *val;
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
        ParamsAction::LayerOpacityCommit => {
            let layer_idx = super::resolve_active_layer_index(active_layer, ctx.project);
            if let Some(old_val) = ctx.scrub.slider_snapshot.take()
                && let Some(idx) = layer_idx
                && let Some(layer) = ctx.project.timeline.layers.get(idx)
            {
                let layer_id = layer.layer_id.clone();
                let new_val = layer.opacity;
                if (old_val - new_val).abs() > f32::EPSILON {
                    let cmd = ChangeLayerOpacityCommand::new(layer_id, old_val, new_val);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            ctx.scrub.active_inspector_drag = None;
            DispatchResult::handled()
        }
        ParamsAction::LayerChromeCollapseToggle => {
            ctx.ui.inspector.layer_chrome_mut().toggle_collapsed();
            DispatchResult::structural()
        }

        // ── Effect operations ──────────────────────────────────────
        ParamsAction::EffectToggle(fx_idx) => {
            let tab = effective_tab;
            let selected = ctx.ui.inspector.get_selected_effect_indices();
            // If clicked effect is part of multi-selection, apply to all selected
            let indices: Vec<usize> = if selected.len() > 1 && selected.contains(fx_idx) {
                selected
            } else {
                vec![*fx_idx]
            };
            // New state = inverse of the clicked card, applied to every selected.
            let new_enabled = super::resolve_effect_id(
                ctx.editor_target,
                tab,
                active_layer,
                ctx.selection,
                ctx.project,
                *fx_idx,
            )
            .and_then(|eid| ctx.project.find_effect_by_id(&eid).map(|fx| !fx.enabled))
            .unwrap_or(true);
            // Resolve every affected card to its stable id + current state. The
            // editor toggles its single watched effect (id wins over `idx`); the
            // inspector resolves each selected index against its own context.
            let targets: Vec<(manifold_core::EffectId, bool)> = indices
                .iter()
                .filter_map(|&idx| {
                    let eid = super::resolve_effect_id(
                        ctx.editor_target,
                        tab,
                        active_layer,
                        ctx.selection,
                        ctx.project,
                        idx,
                    )?;
                    let enabled = ctx.project.find_effect_by_id(&eid)?.enabled;
                    Some((eid, enabled))
                })
                .collect();
            let mut commands: Vec<Box<dyn manifold_editing::command::Command>> = Vec::new();
            for (eid, old_enabled) in &targets {
                if *old_enabled != new_enabled {
                    commands.push(Box::new(ToggleEffectCommand::new(
                        eid.clone(),
                        *old_enabled,
                        new_enabled,
                    )));
                }
            }
            // Apply locally for immediate visual feedback.
            for (eid, _) in &targets {
                if let Some(fx) = ctx.project.find_effect_by_id_mut(eid) {
                    fx.enabled = new_enabled;
                }
            }
            if !commands.is_empty() {
                ContentCommand::send(
                    ctx.content_tx,
                    ContentCommand::ExecuteBatch(commands, "Toggle effects".into()),
                );
            }
            DispatchResult::handled()
        }
        ParamsAction::EffectCollapseToggle(fx_idx) => {
            let tab = effective_tab;
            let selected = ctx.ui.inspector.get_selected_effect_indices();
            // If clicked effect is part of multi-selection, apply to all selected
            let indices: Vec<usize> = if selected.len() > 1 && selected.contains(fx_idx) {
                selected
            } else {
                vec![*fx_idx]
            };
            let new_collapsed;
            {
                let (effects_mut, _target) =
                    resolve_effects_mut(tab, ctx.project, active_layer, ctx.selection);
                if let Some(effects) = effects_mut {
                    new_collapsed = effects.get(*fx_idx).map(|fx| !fx.collapsed).unwrap_or(true);
                    for &idx in &indices {
                        if let Some(fx) = effects.get_mut(idx) {
                            fx.collapsed = new_collapsed;
                        }
                    }
                } else {
                    new_collapsed = true;
                }
            }
            // Send to content thread so snapshot sync doesn't overwrite
            let target = super::resolve_effect_target(tab, active_layer, ctx.project);
            let indices_owned = indices;
            ContentCommand::send(
                ctx.content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    let effects = match &target {
                        EffectTarget::Master => Some(&mut p.settings.master_effects),
                        EffectTarget::Layer { layer_id } => p
                            .timeline
                            .find_layer_by_id_mut(layer_id)
                            .map(|(_, l)| l.effects_mut()),
                    };
                    if let Some(effects) = effects {
                        for &idx in &indices_owned {
                            if let Some(fx) = effects.get_mut(idx) {
                                fx.collapsed = new_collapsed;
                            }
                        }
                    }
                })),
            );
            DispatchResult::structural()
        }
        ParamsAction::SetAllCardsCollapsed { collapsed } => {
            // Collapse/expand every effect card in the active column at once.
            // Mirrors EffectCollapseToggle's two-write pattern (snapshot now,
            // MutateProject so the content thread's snapshot doesn't overwrite).
            let tab = effective_tab;
            let collapsed = *collapsed;
            {
                let (effects_mut, _target) =
                    resolve_effects_mut(tab, ctx.project, active_layer, ctx.selection);
                if let Some(effects) = effects_mut {
                    for fx in effects.iter_mut() {
                        fx.collapsed = collapsed;
                    }
                }
            }
            let target = super::resolve_effect_target(tab, active_layer, ctx.project);
            ContentCommand::send(
                ctx.content_tx,
                ContentCommand::MutateProject(Box::new(move |p| {
                    let effects = match &target {
                        EffectTarget::Master => Some(&mut p.settings.master_effects),
                        EffectTarget::Layer { layer_id } => p
                            .timeline
                            .find_layer_by_id_mut(layer_id)
                            .map(|(_, l)| l.effects_mut()),
                    };
                    if let Some(effects) = effects {
                        for fx in effects.iter_mut() {
                            fx.collapsed = collapsed;
                        }
                    }
                })),
            );
            DispatchResult::structural()
        }
        ParamsAction::ModConfigTabChanged => {
            // The card already switched its own active-tab UI state in
            // handle_click; this just forces a rebuild so the drawer repaints
            // with the newly-selected config. No model mutation.
            DispatchResult::structural()
        }
        ParamsAction::SectionFoldToggled => {
            // D5 — the card already flipped its own `section_folded` UI-only
            // state in handle_click; this just forces a rebuild so the
            // folded/unfolded rows repaint. No model mutation (fold state is
            // workspace-local, never serialized).
            DispatchResult::structural()
        }
        ParamsAction::ModsCompactToggled => {
            // §6b — the inspector already flipped its own compact flag in
            // route_click; rebuild so every card hides/shows its mod drawers.
            // No model mutation.
            DispatchResult::structural()
        }
        ParamsAction::EffectCardClicked(_) => {
            // Deselect generator card when an effect card is clicked
            if let Some(gp) = ctx.ui.inspector.gen_params_mut() {
                gp.update_selection_visual(&mut ctx.ui.tree, false);
            }
            let tree = &mut ctx.ui.tree;
            let inspector = &mut ctx.ui.inspector;
            inspector.apply_selection_visuals(tree);
            DispatchResult::handled()
        }
        // BUG-061: the old bespoke per-param right-click reset action was
        // deleted — reset now rides the generic `SliderReset` trio
        // (`ParamSnapshot`/`ParamChanged`/`ParamCommit` below), reusing
        // these same handlers instead of a bespoke code path. Known
        // trade-off: this drops the eased "value snap-back" fill animation
        // the old handler drove via `begin_value_snapback` (D15) — the value
        // now jumps to default like any other committed change instead of
        // easing to it. `begin_value_snapback` itself is left in place in
        // manifold-ui (still exercised by its own unit tests) but has no
        // remaining production caller.
        ParamsAction::ParamSnapshot(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
            {
                let val = ctx.project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.params
                            .contains(param_id.as_ref())
                            .then(|| inst.get_base_param(param_id.as_ref()))
                    })
                    .flatten();
                if let Some(val) = val {
                    // Touch-to-select (P5, `docs/AUTOMATION_LANES_DESIGN.md`
                    // §7 addendum): the ONE funnel every param drag fires
                    // through, once per touch. Layer-scoped only (Master/Clip
                    // tabs have no layer for the chooser to live on, per §7's
                    // "automation lives on the layer").
                    if effective_tab.is_layer_scope()
                        && let Some(layer_id) = active_layer.clone()
                    {
                        ctx.selection.set_chosen_automation_param(
                            layer_id,
                            crate::editing_host::to_ui_graph_target(&target),
                            param_id.clone(),
                        );
                    }
                    ctx.scrub.slider_snapshot = Some(val);
                    ctx.scrub.active_inspector_drag = Some(crate::app::ActiveInspectorDrag::Param {
                        target,
                        param_id: param_id.clone(),
                        value: val,
                    });
                }
            }
            DispatchResult::handled()
        }
        ParamsAction::ParamChanged(gpt, param_id, val) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
            {
                ctx.project.with_preset_graph_mut(&target, |inst| {
                    inst.set_base_param(param_id.as_ref(), *val);
                });
                if let Some(crate::app::ActiveInspectorDrag::Param { value, .. }) =
                    &mut ctx.scrub.active_inspector_drag
                {
                    *value = *val;
                }
                let pid = param_id.clone();
                let v = *val;
                let t = target.clone();
                ContentCommand::send(
                    ctx.content_tx,
                    ContentCommand::MutateProjectLive(Box::new(move |p| {
                        p.with_preset_graph_mut(&t, |inst| {
                            inst.set_base_param(pid.as_ref(), v);
                        });
                    })),
                );
            }
            DispatchResult::handled()
        }
        ParamsAction::ParamCommit(gpt, param_id) => {
            // Release commits ONE `ChangeGraphParamCommand` through the
            // undo-tracked `ContentCommand::Execute` path — one undo unit per
            // gesture, not per motion event.
            if let Some(old_val) = ctx.scrub.slider_snapshot.take()
                && let Some(target) =
                    resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
            {
                let new_val = ctx.project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.params
                            .contains(param_id.as_ref())
                            .then(|| inst.get_base_param(param_id.as_ref()))
                    })
                    .flatten();
                if let Some(new_val) = new_val
                    && (old_val - new_val).abs() > f32::EPSILON
                {
                    let cmd = ChangeGraphParamCommand::new(
                        target,
                        param_id.clone(),
                        old_val,
                        new_val,
                    );
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            ctx.scrub.active_inspector_drag = None;
            DispatchResult::handled()
        }
        // BUG-250: an enum dropdown pick — one atomic write, one undo unit,
        // no drag. `ParamToggle`'s read-old/write-new `ChangeGraphParamCommand`
        // shape, exactly as `ParamChanged`/`ParamToggle` already do.
        ParamsAction::ParamEnumSet(gpt, param_id, new_val) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
            {
                let old_val = ctx.project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.params
                            .contains(param_id.as_ref())
                            .then(|| inst.get_base_param(param_id.as_ref()))
                    })
                    .flatten();
                if let Some(old_val) = old_val
                    && (old_val - *new_val).abs() > f32::EPSILON
                {
                    ctx.project.with_preset_graph_mut(&target, |inst| {
                        inst.set_base_param(param_id.as_ref(), *new_val);
                    });
                    let cmd = ChangeGraphParamCommand::new(target, param_id.clone(), old_val, *new_val);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }

        // ── Effect modulation ──────────────────────────────────────
        // ── Effect management ──────────────────────────────────────
        ParamsAction::AddEffectClicked(_tab) => DispatchResult::handled(),
        ParamsAction::BrowserSearchClicked => DispatchResult::handled(),
        ParamsAction::RemoveEffect(fx_idx) => {
            let tab = effective_tab;
            let (effects_ref, target) = resolve_effects_read(tab, ctx.project, active_layer, ctx.selection);
            if let Some(effects) = effects_ref
                && let Some(fx) = effects.get(*fx_idx)
            {
                let cmd = RemoveEffectCommand::new(target, fx.clone(), *fx_idx);
                {
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(ctx.project);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        ParamsAction::EffectReorder(from_idx, to_idx) => {
            let tab = effective_tab;
            let target = super::resolve_effect_target(tab, active_layer, ctx.project);
            let cmd = ReorderEffectCommand::new(target, *from_idx, *to_idx);
            {
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(ctx.project);
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            }
            // Selection follows automatically (ID-based, no remapping needed)
            DispatchResult::structural()
        }
        // `ParamsAction::ToggleNodeParamExpose` is handled in
        // `app_render.rs` alongside the other graph commands so it can
        // access `watched_graph_target` + `watched_catalog_default`
        // directly. No fork on Effect vs Generator at the dispatch
        // layer — the command itself handles both.
        ParamsAction::EffectReorderGroup(source_indices, target_idx) => {
            // Multi-select reorder: move a group of effects to the target position.
            let tab = effective_tab;
            let target = super::resolve_effect_target(tab, active_layer, ctx.project);
            let (effects_mut, _target) = resolve_effects_mut(tab, ctx.project, active_layer, ctx.selection);
            if let Some(effects) = effects_mut {
                // Snapshot before
                let old_effects = effects.clone();

                // Remove selected effects in reverse order (preserving relative order)
                let mut moving: Vec<(usize, PresetInstance)> = Vec::new();
                let mut sorted_sources = source_indices.clone();
                sorted_sources.sort_unstable();
                for &idx in sorted_sources.iter().rev() {
                    if idx < effects.len() {
                        moving.push((idx, effects.remove(idx)));
                    }
                }
                moving.reverse(); // Restore original relative order

                // Compute adjusted insertion point (account for removed items before target)
                let removed_before = sorted_sources.iter().filter(|&&i| i < *target_idx).count();
                let insert_at = target_idx.saturating_sub(removed_before).min(effects.len());

                // Insert the group at the target position
                for (offset, (_, fx)) in moving.into_iter().enumerate() {
                    let pos = (insert_at + offset).min(effects.len());
                    effects.insert(pos, fx);
                }

                // Snapshot after and create undoable command
                let new_effects = effects.clone();
                let cmd = ReorderEffectGroupCommand::new(target, old_effects, new_effects);
                // State already applied — send for undo stack only (don't re-execute)
                let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            }
            // Selection follows automatically (ID-based, no remapping needed)
            DispatchResult::structural()
        }

        // ── Generator card actions ─────────────────────────────────
        ParamsAction::GenStringParamClicked(_) | ParamsAction::GenStringParamDropdownClicked(_) => {
            // Intercepted in app_render.rs to open text input / dropdown.
            DispatchResult::handled()
        }
        ParamsAction::GenStringParamSelected(sp_idx, selected_value) => {
            // A dropdown string param was selected (e.g. font family).
            // Commit it as a SetClipStringParamCommand.
            let layer_idx = super::resolve_active_layer_index(active_layer, ctx.project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = ctx.project.timeline.layers.get(layer_idx)
            {
                let gen_type = layer.generator_type();
                if let Some(def) = manifold_core::preset_definition_registry::try_get(gen_type)
                    && let Some(sp_def) = def.string_param_defs.get(*sp_idx)
                {
                    let key = sp_def.key.to_string();
                    let new_value: Option<String> = if selected_value.is_empty() {
                        None
                    } else {
                        Some(selected_value.clone())
                    };

                    // Find clip: selected clip on this layer, or first clip
                    let clip = ctx.selection
                        .primary_selected_clip_id
                        .as_ref()
                        .and_then(|sel_id| layer.clips.iter().find(|c| c.id == *sel_id))
                        .or_else(|| layer.clips.first());
                    if let Some(c) = clip {
                        let old_value = c.string_params.as_ref().and_then(|m| m.get(&key)).cloned();
                        if old_value != new_value {
                            let clip_id = c.id.clone();
                            let cmd =
                                manifold_editing::commands::clip::SetClipStringParamCommand::new(
                                    clip_id, key, old_value, new_value,
                                );
                            ContentCommand::send(
                                ctx.content_tx,
                                ContentCommand::Execute(Box::new(cmd)),
                            );
                        }
                    }
                }
            }
            DispatchResult::handled()
        }
        ParamsAction::GenCollapseToggle => {
            if let Some(gp) = ctx.ui.inspector.gen_params_mut() {
                let new_val = !gp.is_collapsed();
                gp.set_collapsed(new_val);
            }
            DispatchResult::structural()
        }
        ParamsAction::GenCardClicked => {
            // Select the generator card (blue highlight border), deselect effect cards
            if let Some(gp) = ctx.ui.inspector.gen_params_mut() {
                gp.update_selection_visual(&mut ctx.ui.tree, true);
            }
            // Deselect all effect cards
            ctx.ui.inspector.clear_effect_selection(&mut ctx.ui.tree);
            DispatchResult::handled()
        }
        ParamsAction::CardRightClicked(_) => {
            // Handled by UIRoot::try_open_dropdown (opens the card context menu)
            // — should not reach dispatch.
            DispatchResult::handled()
        }
        ParamsAction::CopyGenerator => {
            let layer_idx = super::resolve_active_layer_index(active_layer, ctx.project);
            if let Some(layer_idx) = layer_idx
                && let Some(layer) = ctx.project.timeline.layers.get(layer_idx)
                && let Some(gp) = layer.gen_params()
            {
                ctx.ui.gen_clipboard.copy_from(gp);
            }
            DispatchResult::handled()
        }
        ParamsAction::PasteGenerator => {
            if let Some(snapshot) = ctx.ui.gen_clipboard.get_paste_snapshot() {
                let layer_idx = super::resolve_active_layer_index(active_layer, ctx.project);
                if let Some(layer_idx) = layer_idx
                    && let Some(layer) = ctx.project.timeline.layers.get(layer_idx)
                    && let Some(gp) = layer.gen_params()
                {
                    let layer_id = layer.layer_id.clone();
                    let old_type = gp.generator_type().clone();
                    let old_params = gp.snapshot_params();
                    let old_drivers = gp.snapshot_drivers();
                    let old_envelopes = gp.snapshot_envelopes();

                    let cmd = PasteGeneratorCommand::new(
                        layer_id,
                        old_type,
                        old_params,
                        old_drivers,
                        old_envelopes,
                        snapshot.generator_type,
                        snapshot.param_values,
                        snapshot.drivers,
                        snapshot.envelopes,
                    );
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(ctx.project);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        ParamsAction::MakePresetUnique(gpt) => {
            // Fork the targeted preset (effect OR generator) into a
            // project-embedded copy and retarget the instance to it. One path
            // for both kinds: resolve the GraphTarget, take its source def
            // (diverged per-instance graph else catalog canonical), fork via
            // the shared command keyed off `target.preset_kind()`.
            use manifold_editing::commands::preset::ForkPresetCommand;
            if let Some(target) = resolve_graph_target(
                gpt,
                ctx.editor_target,
                effective_tab,
                active_layer,
                ctx.selection,
                ctx.project,
            ) && let Some((source_def, _)) = preset_source_def(&target, ctx.project)
            {
                let cmd = ForkPresetCommand::new(target.clone(), target.preset_kind(), source_def);
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(ctx.project);
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        ParamsAction::ExportPreset(gpt) => {
            // Export the targeted preset's graph to a .json via a native save
            // dialog. Source def is the diverged per-instance graph else the
            // catalog canonical; the preset id is the filename stem.
            if let Some(target) = resolve_graph_target(
                gpt,
                ctx.editor_target,
                effective_tab,
                active_layer,
                ctx.selection,
                ctx.project,
            ) && let Some((def, preset_id)) = preset_source_def(&target, ctx.project)
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("MANIFOLD Preset", &["json"])
                    .set_file_name(format!("{}.json", preset_id.as_str()))
                    .save_file()
                && let Err(e) = manifold_io::preset_file::export_preset(&def, &path)
            {
                log::error!("[preset] export failed: {e}");
            }
            DispatchResult::handled()
        }
        ParamsAction::ImportPreset(gpt) => {
            // Import a .json preset and retarget the targeted instance to it
            // (registered as a project-embedded preset via the shared fork
            // command, so it rides undo + the overlay refresh).
            use manifold_editing::commands::preset::ForkPresetCommand;
            if let Some(target) = resolve_graph_target(
                gpt,
                ctx.editor_target,
                effective_tab,
                active_layer,
                ctx.selection,
                ctx.project,
            ) && let Some(path) = rfd::FileDialog::new()
                .add_filter("MANIFOLD Preset", &["json"])
                .pick_file()
            {
                match manifold_io::preset_file::import_preset(&path) {
                    Ok(def) => {
                        let cmd =
                            ForkPresetCommand::importing(target.clone(), target.preset_kind(), def);
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(ctx.project);
                        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                    }
                    Err(e) => log::error!("[preset] import failed: {e}"),
                }
            }
            DispatchResult::structural()
        }
        ParamsAction::SaveToLibrary(gpt) | ParamsAction::SaveToProject(gpt) => {
            // Library doors (PRESET_LIBRARY_DESIGN D4): resolve the target's
            // current effective def (same `preset_source_def` resolution as
            // Make Unique / Export) and hand it back on `DispatchResult` for
            // the caller to open the shared name-prompt text-input session
            // with — this function has no `TextInputState` access (it's
            // UI-thread overlay state, not routed here), so the prompt itself
            // opens one level up.
            let mut result = DispatchResult::handled();
            if let Some(target) = resolve_graph_target(
                gpt,
                ctx.editor_target,
                effective_tab,
                active_layer,
                ctx.selection,
                ctx.project,
            ) && let Some((def, _)) = preset_source_def(&target, ctx.project)
            {
                let destination = if matches!(action, ParamsAction::SaveToLibrary(_)) {
                    crate::text_input::SavePresetDestination::Library
                } else {
                    crate::text_input::SavePresetDestination::Project
                };
                result.begin_save_preset = Some((target.preset_kind(), def, destination));
            }
            result
        }
        ParamsAction::RevertToLibrary(gpt) => {
            // PRESET_LIBRARY_DESIGN D3/P4: clear the per-instance graph
            // override, undoable — but ONLY if the tracked library id still
            // resolves in the catalog. The resolution check happens HERE
            // (app/renderer-aware) rather than inside the command itself:
            // `manifold-editing` cannot depend on `manifold-renderer`
            // (`manifold-playback` already depends on `manifold-editing`,
            // and `manifold-renderer` depends on `manifold-playback` — the
            // reverse dependency would cycle), so the fact is resolved once
            // here and baked into the command, mirroring how
            // `ForkPresetCommand` is handed an already-resolved `source_def`
            // rather than looking the catalog up inside `Command::execute`.
            use manifold_editing::commands::preset::RevertToLibraryCommand;
            if let Some(target) = resolve_graph_target(
                gpt,
                ctx.editor_target,
                effective_tab,
                active_layer,
                ctx.selection,
                ctx.project,
            ) && let Some(preset_id) = ctx.project.instance_preset_id(&target)
            {
                let resolves = manifold_renderer::node_graph::loaded_preset_view_by_id(&preset_id)
                    .is_some();
                let cmd = RevertToLibraryCommand::new(target, resolves);
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(ctx.project);
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }
        ParamsAction::PushToLibrary(gpt) => {
            // Push to Library (D3, P4): overwrite the targeted preset's
            // tracked user-library file with its current (diverged)
            // definition in place — no name prompt (id/filename never
            // change). A factory/stock id has no user file to overwrite;
            // fall back to the same Save-to-Library-as-new prompt the
            // `SaveToLibrary` action opens, via `begin_save_preset` (this
            // function has no `TextInputState` access — see the comment on
            // the `SaveToLibrary`/`SaveToProject` arm above).
            let mut result = DispatchResult::handled();
            if let Some(target) = resolve_graph_target(
                gpt,
                ctx.editor_target,
                effective_tab,
                active_layer,
                ctx.selection,
                ctx.project,
            ) && let Some((def, preset_id)) = preset_source_def(&target, ctx.project)
            {
                let kind = target.preset_kind();
                let lib = crate::user_library::UserLibrary::new();
                if lib.is_user_entry(kind, &preset_id) {
                    if let Err(e) = lib.push(kind, &preset_id, &def) {
                        log::error!("[preset] push to library failed: {e}");
                    }
                } else {
                    result.begin_save_preset =
                        Some((kind, def, crate::text_input::SavePresetDestination::Library));
                }
            }
            result
        }

        // ── Browser: sources, badges, management (PRESET_LIBRARY_DESIGN P5) ──
        // `BrowserCellRightClicked` opens its menu entirely inside
        // `UIRoot::try_open_dropdown` — this arm only keeps the match
        // exhaustive (same pattern as `CardRightClicked` above).
        // ── Generator params ───────────────────────────────────────
        ParamsAction::GenTypeClicked(_) => DispatchResult::handled(),
        // `ParamToggle`/`ParamFire` (§8.4 P3b): unified effect+generator via
        // the same `resolve_graph_target` + `with_preset_graph_mut` path
        // `ParamChanged`/`ParamCommit` already use, rather than the old
        // `GenParamToggle`/`GenParamFire`'s generator-only `gen_params_mut()`
        // lookup — a click is atomic (no drag), so one command captures the
        // old value and writes the new one in the same arm.
        ParamsAction::ParamToggle(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
            {
                let old_val = ctx.project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.params
                            .contains(param_id.as_ref())
                            .then(|| inst.get_base_param(param_id.as_ref()))
                    })
                    .flatten();
                if let Some(old_val) = old_val {
                    let new_val = if old_val > 0.5 { 0.0 } else { 1.0 };
                    ctx.project.with_preset_graph_mut(&target, |inst| {
                        inst.set_base_param(param_id.as_ref(), new_val);
                    });
                    let cmd = ChangeGraphParamCommand::new(target, param_id.clone(), old_val, new_val);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }
        ParamsAction::ParamFire(gpt, param_id) => {
            // Trigger button click: increment the monotonic counter by one.
            // Mirrors ParamToggle's plumbing exactly except the value
            // transform is `+1` instead of `0↔1`.
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
            {
                let old_val = ctx.project
                    .with_preset_graph_mut(&target, |inst| {
                        inst.params
                            .contains(param_id.as_ref())
                            .then(|| inst.get_base_param(param_id.as_ref()))
                    })
                    .flatten();
                if let Some(old_val) = old_val {
                    let new_val = old_val + 1.0;
                    ctx.project.with_preset_graph_mut(&target, |inst| {
                        inst.set_base_param(param_id.as_ref(), new_val);
                    });
                    let cmd = ChangeGraphParamCommand::new(target, param_id.clone(), old_val, new_val);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }

        // ── "3D Shading" relight (docs/DEPTH_RELIGHT_DESIGN.md D8/P7) ─────
        // The toggle and `height_from` change template topology, so they stay
        // structural. The D3 float knobs are now live uniforms written per
        // frame, so a drag updates the local project + the content thread via
        // `MutateProjectLive` and returns `handled()` — no chain rebuild.
        ParamsAction::RelightToggle(gpt) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
            {
                let old = ctx.project.with_preset_graph_mut(&target, |inst| inst.relight).unwrap_or(false);
                let mut cmd = ToggleRelightCommand::new(target, old, !old);
                cmd.execute(ctx.project);
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
            DispatchResult::structural()
        }
        ParamsAction::RelightParamSnapshot(gpt, field) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
            {
                let f = crate::ui_translate::relight_field_to_editing(*field);
                ctx.scrub.slider_snapshot =
                    ctx.project.with_preset_graph_mut(&target, |inst| f.get(&inst.relight_params));
                if let Some(value) = ctx.scrub.slider_snapshot {
                    ctx.scrub.active_inspector_drag = Some(crate::app::ActiveInspectorDrag::RelightParam {
                        target,
                        field: f,
                        value,
                    });
                }
            }
            DispatchResult::handled()
        }
        ParamsAction::RelightParamChanged(gpt, field, val) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
            {
                let f = crate::ui_translate::relight_field_to_editing(*field);
                let v = *val;
                if let Some(crate::app::ActiveInspectorDrag::RelightParam { value, .. }) =
                    &mut ctx.scrub.active_inspector_drag
                {
                    *value = v;
                }
                // Live drag: update the UI-side project immediately so the
                // slider follows the pointer, and mirror to the content thread
                // via `MutateProjectLive`. No `bump_graph_structure_version`
                // — float knobs are per-frame uniforms (D8/P7).
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
        ParamsAction::RelightParamCommit(gpt, field) => {
            ctx.scrub.active_inspector_drag = None;
            if let Some(old_val) = ctx.scrub.slider_snapshot.take()
                && let Some(target) =
                    resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
            {
                let f = crate::ui_translate::relight_field_to_editing(*field);
                let new_val =
                    ctx.project.with_preset_graph_mut(&target, |inst| f.get(&inst.relight_params));
                if let Some(new_val) = new_val
                    && (old_val - new_val).abs() > f32::EPSILON
                {
                    let cmd = SetRelightParamCommand::new(target, f, old_val, new_val);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
        }
        ParamsAction::RelightHeightFromChanged(gpt, height_from) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project)
            {
                let old = ctx.project
                    .with_preset_graph_mut(&target, |inst| inst.relight_params.height_from)
                    .unwrap_or_default();
                let new = crate::ui_translate::relight_height_from_to_core(*height_from);
                let mut cmd = SetRelightHeightFromCommand::new(target, old, new);
                cmd.execute(ctx.project);
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
            DispatchResult::structural()
        }

        ParamsAction::AddEffect(tab, effect_type) => {
            use manifold_core::effects::PresetInstance;
            // The action carries the chosen preset id directly (registry
            // entries AND project-embedded presets), so no index lookup.
            let effect_type = crate::ui_translate::preset_type_id_to_core(effect_type);
            let defaults = manifold_core::preset_definition_registry::get_defaults(&effect_type);
            let mut effect = PresetInstance::new(effect_type.clone());
            effect.params = manifold_core::params::ParamManifest::from_params(defaults);
            let layer_idx = super::resolve_active_layer_index(active_layer, ctx.project);
            let target = match tab {
                InspectorTab::Master => EffectTarget::Master,
                InspectorTab::Layer | InspectorTab::Group => {
                    if let Some(idx) = layer_idx {
                        let layer_id = ctx.project
                            .timeline
                            .layers
                            .get(idx)
                            .map(|l| l.layer_id.clone())
                            .unwrap_or_default();
                        EffectTarget::Layer { layer_id }
                    } else {
                        return DispatchResult::handled();
                    }
                }
                InspectorTab::Clip => {
                    log::debug!("Add effect to clip (clip selection not yet implemented)");
                    return DispatchResult::handled();
                }
            };
            let insert_idx = match &target {
                EffectTarget::Master => ctx.project.settings.master_effects.len(),
                EffectTarget::Layer { layer_id } => ctx.project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| l.layer_id == *layer_id)
                    .and_then(|l| l.effects.as_ref())
                    .map(|e| e.len())
                    .unwrap_or(0),
            };
            let cmd = manifold_editing::commands::effects::AddEffectCommand::new(
                target, effect, insert_idx,
            );
            {
                let mut boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                boxed.execute(ctx.project);
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
            }
            DispatchResult::structural()
        }

        ParamsAction::PasteEffects => DispatchResult::handled(),

        // Label right-clicks are consumed by try_open_dropdown — shouldn't reach here
        ParamsAction::ParamLabelRightClick(..) => {
            DispatchResult::handled()
        }

        // ── Macro mapping ─────────────────────────────────────────
    }
}
