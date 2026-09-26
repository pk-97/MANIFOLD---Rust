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
use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::GraphTarget;
use manifold_editing::command::Command;
use manifold_editing::commands::effect_target::EffectTarget;
use manifold_editing::commands::effects::{
    ChangeGraphParamCommand, RemoveEffectCommand, ReorderEffectCommand, ReorderEffectGroupCommand,
    SetRelightHeightFromCommand, ToggleEffectCommand, ToggleRelightCommand,
};
use manifold_editing::commands::settings::{ChangeMacroCommand, PasteGeneratorCommand};
use manifold_ui::{InspectorTab, PanelAction, ParamsAction};

use super::super::DispatchResult;
use super::{resolve_effects_mut, resolve_effects_read};
use super::resolve::{resolve_graph_target, resolve_preset_target};

fn find_effect_string_node(
    nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
    stable_id: &manifold_core::NodeId,
    scope: &mut Vec<u32>,
) -> Option<(u32, Vec<u32>)> {
    for node in nodes {
        if node.node_id == *stable_id {
            return Some((node.id, scope.clone()));
        }
        if let Some(group) = &node.group {
            scope.push(node.id);
            let found = find_effect_string_node(&group.nodes, stable_id, scope);
            scope.pop();
            if found.is_some() {
                return found;
            }
        }
    }
    None
}

fn string_binding_target(
    project: &manifold_core::project::Project,
    target: &GraphTarget,
    binding_id: &str,
) -> Option<(u32, String, Vec<u32>, manifold_core::effect_graph_def::EffectGraphDef)> {
    let instance = project.preset_instance(target)?;
    let catalog_default = manifold_renderer::node_graph::bundled_preset_def(instance.effect_type())?.clone();
    let graph = instance.graph.as_ref().unwrap_or(&catalog_default);
    let metadata = graph
        .preset_metadata
        .as_ref()
        .or(catalog_default.preset_metadata.as_ref())?;
    let manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } =
        &metadata.string_bindings.iter().find(|binding| binding.id == binding_id)?.target
    else {
        return None;
    };
    let (node_id, scope_path) = find_effect_string_node(&graph.nodes, node_id, &mut Vec::new())?;
    Some((node_id, param.clone(), scope_path, catalog_default))
}

pub(crate) fn dispatch_params(action: &ParamsAction, ctx: &mut super::super::DispatchCtx) -> DispatchResult {
    let (effective_tab, effective_active_layer) = super::editor_dispatch_context(ctx.editor_target, &*ctx.project, ctx.ui.inspector.last_effect_tab(), ctx.active_layer);
    let active_layer = &effective_active_layer;
    if let ParamsAction::ParamEnumSet(gpt, param_id, _)
        | ParamsAction::ParamToggle(gpt, param_id)
        | ParamsAction::ParamFire(gpt, param_id) = action
        && let Some(target) = resolve_graph_target(gpt, ctx.editor_target, effective_tab,
            active_layer, ctx.selection, ctx.project)
        && let Some(reason) = crate::scene_modifier_edit::macro_parameter_lock_reason(ctx.project, &target, param_id.as_ref())
    {
        ContentCommand::send(ctx.content_tx, ContentCommand::GraphEditRejected(reason.into()));
        return DispatchResult::handled();
    }

    match action {
        ParamsAction::ClearAutomation(gpt, param_id) => {
            if let Some(target) = resolve_graph_target(
                gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project,
            ) {
                let target = crate::editing_host::to_ui_graph_target(&target);
                super::super::project::remove_parameter_automation(
                    ctx.project, ctx.content_tx, ctx.ui, ctx.selection, &target, param_id,
                );
            }
            DispatchResult::structural()
        }
        ParamsAction::ShowAutomation(gpt, param_id) => {
            let Some(target) = resolve_graph_target(
                gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project,
            ) else {
                return DispatchResult::handled();
            };
            show_automation_target(&target, param_id, ctx)
        }
        ParamsAction::ShowAutomationAddress(ui_target, param_id) => {
            let target = crate::editing_host::to_graph_target(ui_target);
            show_automation_target(&target, param_id, ctx)
        }
        ParamsAction::OpenAutomationChooser(_layer_id) => {
            // The root owns the searchable popup session. Keep this action
            // view-only here so opening the chooser can never mutate a lane or
            // a parameter; the root intercept supplies the manifest-backed
            // entries and dispatches ShowAutomation for the chosen address.
            DispatchResult::handled()
        }
        // ── Macros panel collapse ─────────────────────────────────
        ParamsAction::MacrosCollapseToggle => {
            ctx.ui.inspector.macros_panel_mut().toggle_collapsed();
            DispatchResult::structural()
        }

        // ── Macro sliders ─────────────────────────────────────────
        // Macro scrub trio migrated to `PanelAction::Scrub` (`ValueRef::Macro`,
        // P-I / D4). `MacroReset`/`MacroLabelRename` are not scrub gestures and
        // stay here.
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

        // Master-opacity + LED-brightness scrub trios migrated to the unified
        // `PanelAction::Scrub` wire (`ui_bridge/scrub.rs`, P-I / D4).
        // Layer audio-gain scrub trio migrated to `PanelAction::Scrub`
        // (`ValueRef::LayerAudioGain`, P-I / D4).
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

        // ── Layer chrome ───────────────────────────────────────────
        // Layer-opacity scrub trio migrated to `PanelAction::Scrub`
        // (`ValueRef::LayerOpacity`, P-I / D4).
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
            // section 6b — the inspector already flipped its own compact flag in
            // route_click; rebuild so every card hides/shows its mod drawers.
            // No model mutation.
            DispatchResult::structural()
        }
        ParamsAction::EffectCardClicked(_) | ParamsAction::EffectSelectionChanged => {
            // Deselect generator card when an effect card is clicked
            if let Some(gp) = ctx.ui.inspector.gen_params_mut() {
                gp.update_selection_visual(&mut ctx.ui.tree, false);
            }
            let tree = &mut ctx.ui.tree;
            let inspector = &mut ctx.ui.inspector;
            inspector.apply_selection_visuals(tree);
            DispatchResult::handled()
        }
        // The plain-param scrub trio (`ParamSnapshot`/`ParamChanged`/
        // `ParamCommit`) migrated to the unified `PanelAction::Scrub` wire
        // (`ui_bridge/scrub.rs`, P-I / D4). Right-click reset still rides the
        // generic `SliderReset` — its three boxed actions are now `Scrub`
        // gestures. (The old bespoke reset dropped the eased snap-back fill
        // `begin_value_snapback` drove; that helper stays in manifold-ui with
        // no production caller.)
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
        // The browser open is intercepted by `try_open_dropdown` before
        // dispatch; reaching here means the popup couldn't open. The
        // target rides the request/session atomically from the click
        // (PRESET_BROWSER_AUDITION D2), so there is nothing to re-resolve.
        ParamsAction::AddEffectClicked { .. } => DispatchResult::handled(),
        // SCENE_MODIFIER_FRAMEWORK section 3.7 (Inspector card region + picker): the modifier picker opens an
        // app-side overlay (UIRoot::try_open_dropdown); the dispatch layer
        // has nothing to mutate.
        ParamsAction::AddModifierClicked(_layer_id) => DispatchResult::handled(),
        ParamsAction::BrowserSearchClicked => DispatchResult::handled(),
        ParamsAction::RemoveEffect(fx_idx) => {
            let tab = effective_tab;
            let (effects_ref, target) = resolve_effects_read(tab, ctx.project, active_layer, ctx.selection);
            if let Some(effects) = effects_ref
                && let Some(fx) = effects.get(*fx_idx)
            {
                let cmd = RemoveEffectCommand::new(target, fx.clone(), *fx_idx);
                {
                    ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(cmd)));
                }
            }
            DispatchResult::structural()
        }
        ParamsAction::EffectMove { tab, layer_id, ids, before, destination_group, preserve_groups } => {
            let target = match tab {
                InspectorTab::Master => EffectTarget::Master,
                _ => match layer_id { Some(id) => EffectTarget::Layer { layer_id: id.clone() }, None => return DispatchResult::handled() },
            };
            let command = manifold_editing::commands::effect_groups::MoveEffectsToGroupCommand::new(
                target, ids.clone(), before.clone(), destination_group.clone(), *preserve_groups,
            );
            ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(command)));
            DispatchResult::structural()
        }
        ParamsAction::EffectGroupCollapsed { tab, layer_id, group_id, collapsed } => {
            let target = match tab {
                InspectorTab::Master => EffectTarget::Master,
                _ => match layer_id { Some(id) => EffectTarget::Layer { layer_id: id.clone() }, None => return DispatchResult::handled() },
            };
            let command = manifold_editing::commands::effect_groups::SetGroupCollapsedCommand::new(target, group_id.clone(), *collapsed);
            ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(command)));
            DispatchResult::structural()
        }
        ParamsAction::EffectReorder(from_idx, to_idx) => {
            let tab = effective_tab;
            let target = super::resolve_effect_target(tab, active_layer, ctx.project);
            let cmd = ReorderEffectCommand::new(target, *from_idx, *to_idx);
            {
                ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(cmd)));
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
            let (effects_ref, _target) = resolve_effects_read(tab, ctx.project, active_layer, ctx.selection);
            if let Some(original) = effects_ref {
                let mut effects = original.to_vec();
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
                // The content thread validates and applies the proposed order.
                let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(boxed));
            }
            // Selection follows automatically (ID-based, no remapping needed)
            DispatchResult::structural()
        }

        // ── Generator card actions ─────────────────────────────────
        ParamsAction::EffectStringParamDropdownClicked(effect_id, binding_id, index) => {
            let Some(info) = ctx
                .ui
                .inspector
                .effect_string_param(effect_id, binding_id, *index)
                .cloned()
            else {
                return DispatchResult::handled();
            };
            let Some(rect) = ctx.ui.inspector.effect_string_param_rect(
                &ctx.ui.tree,
                effect_id,
                binding_id,
                *index,
            ) else {
                return DispatchResult::handled();
            };
            let items = if info.dropdown_choices.is_empty() {
                vec![manifold_ui::panels::dropdown::DropdownItem::disabled(
                    "No audio sends",
                )]
            } else {
                info.dropdown_choices
                    .iter()
                    .map(|choice| {
                        if choice.disabled {
                            manifold_ui::panels::dropdown::DropdownItem::disabled(&choice.label)
                        } else {
                            manifold_ui::panels::dropdown::DropdownItem::new(&choice.label)
                                .with_action(PanelAction::Params(
                                    ParamsAction::EffectStringParamSelected(
                                        effect_id.clone(),
                                        binding_id.clone(),
                                        choice.value.clone(),
                                    ),
                                ))
                        }
                    })
                    .collect()
            };
            ctx.ui.open_dropdown_typed(
                items,
                manifold_ui::node::Rect::new(rect.x, rect.y, rect.width, rect.height),
            );
            DispatchResult::handled()
        }
        ParamsAction::EffectStringParamSelected(effect_id, binding_id, selected_value) => {
            let target = GraphTarget::Effect(effect_id.clone());
            let Some((node_id, param_name, scope_path, catalog_default)) =
                string_binding_target(ctx.project, &target, binding_id)
            else {
                return DispatchResult::handled();
            };
            let cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                target,
                node_id,
                param_name,
                SerializedParamValue::String {
                    value: selected_value.clone(),
                },
                catalog_default,
            )
            .with_scope(scope_path);
            ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(cmd)));
            DispatchResult::structural()
        }
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
                    if sp_def.key == "audioSend" {
                        let target = GraphTarget::Generator(layer.layer_id.clone());
                        let Some((node_id, param_name, scope_path, catalog_default)) =
                            string_binding_target(ctx.project, &target, sp_def.key)
                        else {
                            return DispatchResult::handled();
                        };
                        let cmd = manifold_editing::commands::graph::SetGraphNodeParamCommand::new(
                            target,
                            node_id,
                            param_name,
                            SerializedParamValue::String {
                                value: selected_value.clone(),
                            },
                            catalog_default,
                        )
                        .with_scope(scope_path);
                        ContentCommand::send(
                            ctx.content_tx,
                            ContentCommand::ExecuteOnContent(Box::new(cmd)),
                        );
                        return DispatchResult::structural();
                    }
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
        ParamsAction::CardRightClicked(_) | ParamsAction::ModifierCardClicked(_)
        | ParamsAction::EffectGroupAddModifierClicked(_) => {
            // Handled by UIRoot::try_open_dropdown (opens the card context menu)
            // — should not reach dispatch.
            DispatchResult::handled()
        }
        ParamsAction::RemoveEffectGroupMask(group_id) => {
            let target = if ctx.project.settings.master_effect_groups.as_ref()
                .is_some_and(|groups| groups.iter().any(|group| group.id == *group_id))
            {
                EffectTarget::Master
            } else if let Some(layer) = ctx.project.timeline.layers.iter().find(|layer| {
                layer.effect_groups.as_ref()
                    .is_some_and(|groups| groups.iter().any(|group| group.id == *group_id))
            }) {
                EffectTarget::Layer { layer_id: layer.layer_id.clone() }
            } else {
                return DispatchResult::handled();
            };
            let cmd = manifold_editing::commands::effect_groups::RemoveGroupMaskCommand::new(
                target,
                group_id.clone(),
            );
            ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(cmd)));
            DispatchResult::structural()
        }
        ParamsAction::AddMask { preset_id, source_layer, .. }
        | ParamsAction::AddEffectGroupMask { preset_id, source_layer, .. } => {
            let target = match action {
                ParamsAction::AddEffectGroupMask { group_id, .. } => {
                    if ctx.project.settings.master_effect_groups.as_ref()
                        .is_some_and(|groups| groups.iter().any(|group| group.id == *group_id))
                    {
                        EffectTarget::Master
                    } else if let Some(layer) = ctx.project.timeline.layers.iter().find(|layer| {
                        layer.effect_groups.as_ref()
                            .is_some_and(|groups| groups.iter().any(|group| group.id == *group_id))
                    }) {
                        EffectTarget::Layer { layer_id: layer.layer_id.clone() }
                    } else {
                        return DispatchResult::handled();
                    }
                }
                ParamsAction::AddMask { target: gpt, .. } => {
                    let Some(GraphTarget::Effect(_)) = resolve_graph_target(
                        gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, ctx.project,
                    ) else {
                        return DispatchResult::handled();
                    };
                    match effective_tab {
                        InspectorTab::Master => EffectTarget::Master,
                        InspectorTab::Layer | InspectorTab::Group => {
                            let Some(layer_id) = active_layer.clone() else {
                                return DispatchResult::handled();
                            };
                            EffectTarget::Layer { layer_id }
                        }
                        InspectorTab::Clip => return DispatchResult::handled(),
                    }
                }
                _ => unreachable!(),
            };
            let effect_type = manifold_core::PresetTypeId::from_string(preset_id.clone());
            let mut mask = manifold_core::preset_definition_registry::create_default(&effect_type);
            if let Some(layer_id) = source_layer {
                let Some(mut graph) = manifold_renderer::node_graph::bundled_preset_def(&effect_type).cloned() else {
                    ContentCommand::send(ctx.content_tx, ContentCommand::GraphEditRejected("Layer mask preset is unavailable".into()));
                    return DispatchResult::handled();
                };
                let Some(node) = graph.nodes.iter_mut().find(|node| node.node_id.as_str() == "layer") else {
                    ContentCommand::send(ctx.content_tx, ContentCommand::GraphEditRejected("Layer mask source is unavailable".into()));
                    return DispatchResult::handled();
                };
                node.params.insert(
                    "layer".to_string(),
                    SerializedParamValue::String { value: layer_id.to_string() },
                );
                mask.graph = Some(graph);
            }
            use manifold_editing::commands::effect_groups::AddGroupMaskCommand;
            let cmd = match action {
                ParamsAction::AddEffectGroupMask { group_id, .. } =>
                    AddGroupMaskCommand::for_group(target, group_id.clone(), mask),
                ParamsAction::AddMask { selected_indices, .. } =>
                    AddGroupMaskCommand::new(target, selected_indices.clone(), mask),
                _ => unreachable!(),
            };
            ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(cmd)));
            DispatchResult::structural()
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
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::MakeUnique, ctx)
        }
        ParamsAction::ExportPreset(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::Export, ctx)
        }
        ParamsAction::ImportPreset(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::Import, ctx)
        }
        ParamsAction::SaveToLibrary(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::SaveToLibrary, ctx)
        }
        ParamsAction::SaveToProject(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::SaveToProject, ctx)
        }
        ParamsAction::RevertToLibrary(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::RevertToLibrary, ctx)
        }
        ParamsAction::PushToLibrary(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::PushToLibrary, ctx)
        }
        ParamsAction::PresetAction(target, kind) => super::presets::dispatch_preset(
            Some(crate::editing_host::to_graph_target(target)), *kind, ctx),

        // ── Browser: sources, badges, management (PRESET_LIBRARY_DESIGN P5) ──
        // `BrowserCellRightClicked` opens its menu entirely inside
        // `UIRoot::try_open_dropdown` — this arm only keeps the match
        // exhaustive (same pattern as `CardRightClicked` above).
        // ── Generator params ───────────────────────────────────────
        ParamsAction::GenTypeClicked(_) => DispatchResult::handled(),
        // `ParamToggle`/`ParamFire` (section 8.4 P3b): unified effect+generator via
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
                    // Connect to Mesh ENABLE on an unsupported Math View chain
                    // is rejected with the same reason the card row shows —
                    // the compiler would silently drop the appearance anyway.
                    // Disabling stays legal so an unsupported chain can be
                    // switched off after the fact.
                    if new_val > 0.5
                        && let Some(reason) =
                            crate::scene_modifier_edit::math_view_connect_mesh_enable_lock_reason(
                                ctx.project,
                                &target,
                                param_id.as_ref(),
                            )
                    {
                        ContentCommand::send(ctx.content_tx, ContentCommand::GraphEditRejected(reason));
                        return DispatchResult::handled();
                    }
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
        // Relight-knob scrub trio migrated to `PanelAction::Scrub`
        // (`ValueRef::RelightParam`, P-I / D4).
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

        ParamsAction::AddEffect {
            tab,
            layer_id,
            preset: effect_type,
        } => {
            use manifold_core::effects::PresetInstance;
            // The action carries the chosen preset id directly (registry
            // entries AND project-embedded presets), so no index lookup.
            let effect_type = crate::ui_translate::preset_type_id_to_core(effect_type);
            let defaults = manifold_core::preset_definition_registry::get_defaults(&effect_type);
            let mut effect = PresetInstance::new(effect_type.clone());
            effect.params = manifold_core::params::ParamManifest::from_params(defaults);
            // PRESET_BROWSER_AUDITION D2 — context-atomic add: the target is
            // the invocation context captured at the open click and carried
            // through the popup session (routing → request → Selected →
            // here), NOT the active layer re-resolved at pick time. The
            // `layer_id` is `Some` exactly when the browser was opened from
            // a layer's "+ Add Effect" (master opens carry `None`).
            let target = match tab {
                InspectorTab::Master => EffectTarget::Master,
                InspectorTab::Layer | InspectorTab::Group => {
                    let Some(layer_id) = layer_id else {
                        return DispatchResult::handled();
                    };
                    EffectTarget::Layer {
                        layer_id: layer_id.clone(),
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

        ParamsAction::PasteEffects | ParamsAction::EditCards(_) | ParamsAction::EffectGroupRightClicked(_) => DispatchResult::handled(),

        // Label right-clicks are consumed by try_open_dropdown — shouldn't reach here
        ParamsAction::ParamLabelRightClick(..) => {
            DispatchResult::handled()
        }

        // ── Macro mapping ─────────────────────────────────────────
    }
}

fn show_automation_target(
    target: &manifold_core::GraphTarget,
    param_id: &manifold_core::effects::ParamId,
    ctx: &mut super::super::DispatchCtx,
) -> DispatchResult {
    let Some(instance) = ctx.project.preset_instance(target) else {
        return DispatchResult::handled();
    };
    if !instance.params.contains(param_id.as_ref()) {
        return DispatchResult::handled();
    }
    // Resolve the owning layer from the stable target, including a scene card
    // bound to a different layer than the active inspector.
    let owner = ctx.project.timeline.layers.iter().find(|layer| match target {
        manifold_core::GraphTarget::Generator(id) => layer.layer_id == *id,
        manifold_core::GraphTarget::Effect(id) => layer.effects.as_ref()
            .is_some_and(|effects| effects.iter().any(|effect| effect.id == *id)),
        manifold_core::GraphTarget::SceneModifier { .. } => false,
    });
    let Some(owner) = owner.filter(|layer| !layer.is_group()) else {
        return DispatchResult::handled();
    };
    let owner_id = owner.layer_id.clone();
    let mut expand = Vec::new();
    let mut next = Some(owner_id.clone());
    while let Some(id) = next {
        let Some((_, layer)) = ctx.project.timeline.find_layer_by_id(&id) else { break; };
        if layer.is_collapsed { expand.push(id); }
        next = layer.parent_layer_id.clone();
    }
    let ui_target = crate::editing_host::to_ui_graph_target(target);
    ctx.selection
        .hidden_automation_lanes
        .remove(&(ui_target.clone(), param_id.clone()));
    let min_height = ctx.ui.layout.track_header_height()
        + manifold_ui::color::AUTOMATION_LANE_STRIP_HEIGHT + 60.0;
    if ctx.ui.layout.timeline_body().height < min_height {
        let content = ctx.ui.layout.content_area();
        ctx.ui.layout.update_split_from_drag(content.y_max() - min_height);
    }
    ctx.ui.pending_automation_reveal = Some((ui_target.clone(), param_id.clone()));
    ctx.selection.set_chosen_automation_param(owner_id, ui_target, param_id.clone());
    ctx.selection.automation_mode_visible = true;
    ctx.selection.clear_automation_selection();
    if !expand.is_empty() {
        ContentCommand::send(ctx.content_tx, ContentCommand::MutateProject(Box::new(move |project| {
            for id in expand {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(&id) {
                    layer.is_collapsed = false;
                }
            }
        })));
    }
    DispatchResult::structural()
}

#[cfg(test)]
mod audio_send_dispatch_tests {
    use super::*;
    use manifold_editing::service::EditingService;
    use manifold_ui::{PanelAction, ParamsAction};

    fn waveform_send(
        project: &manifold_core::project::Project,
        target: &GraphTarget,
    ) -> Option<String> {
        let instance = project.preset_instance(target)?;
        let catalog = manifold_renderer::node_graph::bundled_preset_def(instance.effect_type())?;
        let graph = instance.graph.as_ref().unwrap_or(catalog);
        let node = graph.nodes.iter().find(|node| node.node_id.as_str() == "waveform")?;
        match node.params.get("send") {
            Some(SerializedParamValue::String { value }) => Some(value.clone()),
            _ => None,
        }
    }

    #[test]
    fn generator_audio_send_selection_uses_graph_binding_and_round_trips_undo() {
        let mut project = manifold_core::project::Project::default();
        let layer_id = manifold_core::LayerId::new("oscilloscope-layer");
        let mut layer = manifold_core::layer::Layer::new_generator(
            "Oscilloscope".to_string(),
            manifold_core::PresetTypeId::from_string("Oscilloscope".to_string()),
            0,
        );
        layer.layer_id = layer_id.clone();
        layer.clips.push(manifold_core::clip::TimelineClip::new_generator(
            manifold_core::Beats(0.0),
            manifold_core::Beats(16.0),
        ));
        project.timeline.layers.push(layer);

        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let content_state = crate::content_state::ContentState::default();
        let mut ui = crate::ui_root::UIRoot::new();
        let mut selection = manifold_ui::UIState::new();
        selection.select_layer(layer_id.clone());
        let mut active_layer = Some(layer_id.clone());
        let mut user_prefs = crate::user_prefs::UserPrefs::in_memory();
        let mut scrub = crate::ui_bridge::ScrubState::default();
        let action = PanelAction::Params(ParamsAction::GenStringParamSelected(
            0,
            "send-2".to_string(),
        ));

        let result = {
            let mut ctx = crate::ui_bridge::DispatchCtx {
                project: &mut project,
                content_tx: &content_tx,
                content_state: &content_state,
                ui: &mut ui,
                selection: &mut selection,
                active_layer: &mut active_layer,
                user_prefs: &mut user_prefs,
                editor_target: None,
                scrub: &mut scrub,
            };
            crate::ui_bridge::dispatch(&action, &mut ctx)
        };
        assert!(result.structural_change);

        let command = match content_rx.try_recv().expect("selection queues a command") {
            crate::content_command::ContentCommand::ExecuteOnContent(command) => command,
            _ => panic!("expected ExecuteOnContent"),
        };
        let target = GraphTarget::Generator(layer_id);
        let mut service = EditingService::new();
        service.execute(command, &mut project);
        assert_eq!(waveform_send(&project, &target).as_deref(), Some("send-2"));

        assert!(service.undo(&mut project));
        assert_eq!(waveform_send(&project, &target), None);
        assert!(service.redo(&mut project));
        assert_eq!(waveform_send(&project, &target).as_deref(), Some("send-2"));
    }
}
