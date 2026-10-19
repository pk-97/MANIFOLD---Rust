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
use manifold_editing::commands::effect_target::EffectTarget;
use manifold_editing::commands::effects::{
    ChangeGraphParamCommand, RemoveEffectCommand, ReorderEffectCommand, ReorderEffectGroupCommand,
    ToggleEffectCommand,
};
use manifold_editing::commands::settings::{ChangeMacroCommand, PasteGeneratorCommand};
use manifold_ui::{InspectorTab, PanelAction, ParamsAction};
use manifold_ui::panels::inspector::InspectorFold;

use super::super::DispatchResult;
use super::resolve_effects_read;
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
    let catalog_default = manifold_nodes::bundled_presets::bundled_preset_def(instance.effect_type())?.as_ref().clone();
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
    let (effective_tab, effective_active_layer) = super::editor_dispatch_context(ctx.editor_target, &ctx.project, ctx.ui.inspector.last_effect_tab(), ctx.active_layer);
    let active_layer = &effective_active_layer;
    if let ParamsAction::ParamEnumSet(gpt, param_id, _)
        | ParamsAction::ParamToggle(gpt, param_id) = action
        && let Some(target) = resolve_graph_target(gpt, ctx.editor_target, effective_tab,
            active_layer, ctx.selection, &ctx.project)
        && let Some(reason) = crate::scene_modifier_edit::macro_parameter_lock_reason(&ctx.project, &target, param_id.as_ref())
    {
        ContentCommand::send(ctx.content_tx, ContentCommand::GraphEditRejected(reason.into()));
        return DispatchResult::handled();
    }

    match action {
        ParamsAction::ShowClipTriggerResponse(target, param_id) => {
            let address = crate::editing_host::to_graph_target(target);
            let Some(owner) = ctx.project.clip_trigger_target_layer(&address) else {
                return DispatchResult::handled();
            };
            if !ctx.project.can_assign_clip_trigger_source(&address, param_id.as_ref(),
                &manifold_core::params::ClipTriggerSource::Disabled)
            {
                return DispatchResult::handled();
            }
            let owner_id = owner.layer_id.clone();
            expand_layer_ancestors(&owner_id, ctx);
            ctx.selection.select_layer(owner_id.clone());
            *ctx.active_layer = Some(owner_id);
            if crate::ui_bridge::projection::trigger_routing::response_uses_scene_panel(&ctx.project, &address, param_id.as_ref()) {
                ctx.selection.pin_scope(InspectorTab::Scene);
            }
            ctx.ui.request_trigger_response_reveal(target.clone(), param_id.clone());
            // Card/rack folding is presentation state, not an undo step; the
            // source and response stay intact.
            ctx.mutate(crate::content_command::touch::EFFECTS, move |project| {
                let group = project.graph_target_owner_mut(&address).and_then(|instance| {
                    instance.collapsed = false;
                    instance.group_id.clone()
                });
                if let Some(group) = group {
                    for layer in &mut project.timeline.layers {
                        for rack in layer.effect_groups.iter_mut().flatten() {
                            if rack.id == group { rack.collapsed = false; }
                        }
                    }
                }
            });
            DispatchResult::handled()
        }
        ParamsAction::OpenClipTriggerSource(..) | ParamsAction::OpenTriggerTargets(_) => {
            DispatchResult::handled()
        }
        ParamsAction::AssignClipTriggerSource(target, param_id, source) => {
            let target = crate::editing_host::to_graph_target(target);
            if let Some(reason) = crate::scene_modifier_edit::macro_parameter_lock_reason(
                &ctx.project, &target, param_id.as_ref(),
            ) {
                ContentCommand::send(ctx.content_tx, ContentCommand::GraphEditRejected(reason.into()));
                return DispatchResult::handled();
            }
            let command = manifold_editing::commands::trigger_source::SetParamClipTriggerSourceCommand::for_assignment(
                target, param_id.clone(),
                super::super::projection::trigger_routing::to_core_source(source),
            );
            if let manifold_ui::view::UiClipTriggerSource::Lane(id) = source {
                expand_layer_ancestors(id, ctx);
                ctx.ui.request_layer_reveal(id.clone());
            }
            ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(command)));
            DispatchResult::handled()
        }
        ParamsAction::CreateTriggerLane { owner, assignment } => {
            let assignment = assignment.as_ref().map(|(target, param)| {
                (crate::editing_host::to_graph_target(target), param.clone())
            });
            if let Some(command) = manifold_editing::service::EditingService::create_trigger_lane(
                &ctx.project, owner, assignment,
            ) {
                expand_layer_ancestors(owner, ctx);
                ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteSelecting(
                    command, crate::edit_selection::SelectAfterEdit::NewLayer,
                ));
            }
            DispatchResult::handled()
        }
        ParamsAction::ClearAutomation(gpt, param_id) => {
            if let Some(target) = resolve_graph_target(
                gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, &ctx.project,
            ) {
                let target = crate::editing_host::to_ui_graph_target(&target);
                super::super::project::remove_parameter_automation(
                    &mut ctx.project, ctx.content_tx, ctx.ui, ctx.selection, &target, param_id,
                );
            }
            DispatchResult::handled()
        }
        ParamsAction::ShowAutomation(gpt, param_id) => {
            let Some(target) = resolve_graph_target(
                gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, &ctx.project,
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
            ctx.ui.inspector.toggle_fold(InspectorFold::Macros);
            DispatchResult::handled()
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
                    ctx.execute(Box::new(ChangeMacroCommand::new(idx, old, 0.0)));
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
            ctx.ui.inspector.toggle_fold(InspectorFold::MasterChrome);
            DispatchResult::handled()
        }
        ParamsAction::MasterExitPathClicked => {
            // Handled by try_open_dropdown in ui_root.rs — opens exit path dropdown.
            DispatchResult::handled()
        }
        ParamsAction::SetLedExitIndex(idx) => {
            let idx = *idx;
            // Locally and on the content thread, so the LED pipeline picks it up.
            ctx.mutate(crate::content_command::touch::LED, move |p: &mut manifold_core::project::Project| {
                p.settings.led_exit_index = idx;
            });
            DispatchResult::handled()
        }
        // ── LED enabled toggle ───────────────────────────────────
        ParamsAction::LedEnabledToggle => {
            let new_enabled = !ctx.content_state.led_enabled;
            // Persist the new ON/OFF state in project settings so the LED
            // pipeline auto-initialises on project load.
            ctx.mutate(crate::content_command::touch::LED, move |p| {
                p.settings.led_enabled = new_enabled;
            });
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
            ctx.ui.inspector.toggle_fold(InspectorFold::LayerChrome);
            DispatchResult::handled()
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
                &ctx.project,
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
                        &ctx.project,
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
            if !commands.is_empty() {
                ctx.execute_batch(commands, "Toggle effects".into());
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
            let new_collapsed = resolve_effects_read(tab, &ctx.project, active_layer, ctx.selection)
                .0
                .and_then(|effects| effects.get(*fx_idx).map(|fx| !fx.collapsed))
                .unwrap_or(true);
            let target = super::resolve_effect_target(tab, active_layer, &ctx.project);
            ctx.mutate(crate::content_command::touch::EFFECTS, move |p| {
                let effects = match &target {
                    EffectTarget::Master => Some(&mut p.settings.master_effects),
                    EffectTarget::Layer { layer_id } => p
                        .timeline
                        .find_layer_by_id_mut(layer_id)
                        .map(|(_, l)| l.effects_mut()),
                };
                if let Some(effects) = effects {
                    for &idx in &indices {
                        if let Some(fx) = effects.get_mut(idx) {
                            fx.collapsed = new_collapsed;
                        }
                    }
                }
            });
            DispatchResult::handled()
        }
        ParamsAction::SetAllCardsCollapsed { collapsed } => {
            // Collapse/expand every effect card in the active column at once.
            let tab = effective_tab;
            let collapsed = *collapsed;
            let target = super::resolve_effect_target(tab, active_layer, &ctx.project);
            ctx.mutate(crate::content_command::touch::EFFECTS, move |p| {
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
            });
            DispatchResult::handled()
        }
        ParamsAction::ModConfigTabChanged | ParamsAction::SectionFoldToggled => {
            // The card or Scene tab already flipped its own UI-only state (D5:
            // fold state is workspace-local, never serialized); the view
            // version rebuilds the column.
            ctx.ui.inspector.note_card_view_actions(&[PanelAction::Params(action.clone())]);
            DispatchResult::handled()
        }
        ParamsAction::CardSectionFolded { target, section, folded } => {
            // A scene card fold is a saved view choice (SCENE_SURFACES_DESIGN.md
            // D7), not an undo step: written to the local project so the
            // rebuild sees it, and through MutateProject so the next snapshot
            // keeps it, like EffectCollapseToggle.
            let Some(target) = resolve_graph_target(target, ctx.editor_target, effective_tab, active_layer, ctx.selection, &ctx.project) else {
                return DispatchResult::handled();
            };
            let (section, folded) = (section.clone(), *folded);
            let write = move |instance: &mut PresetInstance| {
                instance.folded_sections.retain(|title| *title != section);
                instance.unfolded_sections.retain(|title| *title != section);
                if folded {
                    instance.folded_sections.push(section.clone());
                } else {
                    instance.unfolded_sections.push(section.clone());
                }
            };
            ctx.mutate(crate::content_command::touch::EFFECTS, move |project| {
                project.with_preset_graph_mut(&target, write);
            });
            DispatchResult::handled()
        }
        ParamsAction::SetCardPin(gpt, param_id, pin) => {
            let Some(target) = resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, &ctx.project) else {
                return DispatchResult::handled();
            };
            if let (GraphTarget::Generator(layer), Some(manifold_core::scene_surface::CardPin::Shown)) = (&target, pin) {
                ctx.ui.inspector.flash_card_row_when_shown(layer.clone(), param_id.clone());
            }
            let command = manifold_editing::commands::graph::SetCardPinCommand::new(target, param_id.to_string(), *pin);
            ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(command)));
            DispatchResult::handled()
        }
        ParamsAction::SetSliderRange(gpt, param_id, range) => {
            let Some(target) = resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, &ctx.project) else {
                return DispatchResult::handled();
            };
            ctx.execute(Box::new(manifold_editing::commands::graph::SetSliderRangeCommand::new(
                target,
                param_id.to_string(),
                *range,
            )));
            DispatchResult::handled()
        }
        ParamsAction::RevealInSceneTab(gpt, param_id) => {
            reveal_in_scene_tab(gpt, vec![param_id.clone()], effective_tab, active_layer, ctx)
        }
        ParamsAction::RevealSectionInSceneTab(gpt, section) => {
            reveal_in_scene_tab(gpt, section.clone(), effective_tab, active_layer, ctx)
        }
        ParamsAction::ModsCompactToggled => {
            // section 6b — the inspector flipped its compact flag in route_click
            // through its view-version setter.
            DispatchResult::handled()
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
        // gestures.
        // BUG-250: an enum dropdown pick — one atomic write, one undo unit,
        // no drag. `ParamToggle`'s read-old/write-new `ChangeGraphParamCommand`
        // shape, exactly as `ParamChanged`/`ParamToggle` already do.
        ParamsAction::ParamEnumSet(gpt, param_id, new_val) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, &ctx.project)
            {
                let old_val = ctx.project
                    .with_preset_graph(&target, |inst| {
                        inst.params
                            .contains(param_id.as_ref())
                            .then(|| inst.get_base_param(param_id.as_ref()))
                    })
                    .flatten();
                if let Some(old_val) = old_val
                    && (old_val - *new_val).abs() > f32::EPSILON
                {
                    let cmd = ChangeGraphParamCommand::new(target, param_id.clone(), old_val, *new_val);
                    ctx.execute(Box::new(cmd));
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
            let (effects_ref, target) = resolve_effects_read(tab, &ctx.project, active_layer, ctx.selection);
            if let Some(effects) = effects_ref
                && let Some(fx) = effects.get(*fx_idx)
            {
                let cmd = RemoveEffectCommand::new(target, fx.clone(), *fx_idx);
                {
                    ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(cmd)));
                }
            }
            DispatchResult::handled()
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
            DispatchResult::handled()
        }
        ParamsAction::EffectGroupCollapsed { tab, layer_id, group_id, collapsed } => {
            let target = match tab {
                InspectorTab::Master => EffectTarget::Master,
                _ => match layer_id { Some(id) => EffectTarget::Layer { layer_id: id.clone() }, None => return DispatchResult::handled() },
            };
            let command = manifold_editing::commands::effect_groups::SetGroupCollapsedCommand::new(target, group_id.clone(), *collapsed);
            ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(command)));
            DispatchResult::handled()
        }
        ParamsAction::EffectReorder(from_idx, to_idx) => {
            let tab = effective_tab;
            let target = super::resolve_effect_target(tab, active_layer, &ctx.project);
            let cmd = ReorderEffectCommand::new(target, *from_idx, *to_idx);
            {
                ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(cmd)));
            }
            // Selection follows automatically (ID-based, no remapping needed)
            DispatchResult::handled()
        }
        // `ParamsAction::ToggleNodeParamExpose` is handled in
        // `app_render.rs` alongside the other graph commands so it can
        // access `watched_graph_target` + `watched_catalog_default`
        // directly. No fork on Effect vs Generator at the dispatch
        // layer — the command itself handles both.
        ParamsAction::EffectReorderGroup(source_indices, target_idx) => {
            // Multi-select reorder: move a group of effects to the target position.
            let tab = effective_tab;
            let target = super::resolve_effect_target(tab, active_layer, &ctx.project);
            let (effects_ref, _target) = resolve_effects_read(tab, &ctx.project, active_layer, ctx.selection);
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
            DispatchResult::handled()
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
                string_binding_target(&ctx.project, &target, binding_id)
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
            DispatchResult::handled()
        }
        ParamsAction::GenStringParamClicked(_) | ParamsAction::GenStringParamDropdownClicked(_) => {
            // Intercepted in app_render.rs to open text input / dropdown.
            DispatchResult::handled()
        }
        ParamsAction::GenStringParamSelected(sp_idx, selected_value) => {
            // A dropdown string param was selected (e.g. font family).
            // Commit it as a SetClipStringParamCommand.
            let layer_idx = super::resolve_active_layer_index(active_layer, &ctx.project);
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
                            string_binding_target(&ctx.project, &target, sp_def.key)
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
                        return DispatchResult::handled();
                    }
                    let key = sp_def.key.to_string();
                    let new_value: Option<String> = if selected_value.is_empty() {
                        None
                    } else {
                        Some(selected_value.clone())
                    };

                    // Find clip: selected clip on this layer, or first clip
                    let clip = ctx.selection
                        .primary_selected_clip_id()
                        .as_ref()
                        .and_then(|sel_id| layer.clips.iter().find(|c| c.id == **sel_id))
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
            ctx.ui.inspector.toggle_fold(InspectorFold::Generator);
            DispatchResult::handled()
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
            DispatchResult::handled()
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
                        gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, &ctx.project,
                    ) else {
                        return DispatchResult::handled();
                    };
                    match effective_tab {
                        InspectorTab::Master => EffectTarget::Master,
                        InspectorTab::Layer | InspectorTab::Group | InspectorTab::Scene => {
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
                let Some(mut graph) = manifold_nodes::bundled_presets::bundled_preset_def(&effect_type).map(|def| (*def).clone()) else {
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
            DispatchResult::handled()
        }
        ParamsAction::CopyGenerator => {
            let layer_idx = super::resolve_active_layer_index(active_layer, &ctx.project);
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
                let layer_idx = super::resolve_active_layer_index(active_layer, &ctx.project);
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
                    let boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    ctx.execute(boxed);
                }
            }
            DispatchResult::handled()
        }
        ParamsAction::MakePresetUnique(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, &ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::MakeUnique, ctx)
        }
        ParamsAction::ExportPreset(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, &ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::Export, ctx)
        }
        ParamsAction::ImportPreset(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, &ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::Import, ctx)
        }
        ParamsAction::SaveToLibrary(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, &ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::SaveToLibrary, ctx)
        }
        ParamsAction::SaveToProject(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, &ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::SaveToProject, ctx)
        }
        ParamsAction::RevertToLibrary(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, &ctx.project);
            super::presets::dispatch_preset(target, manifold_ui::panels::actions::PresetActionKind::RevertToLibrary, ctx)
        }
        ParamsAction::PushToLibrary(gpt) => {
            let target = resolve_preset_target(gpt, ctx.editor_target, effective_tab,
                active_layer, ctx.selection, &ctx.project);
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
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, &ctx.project)
            {
                let old_val = ctx.project
                    .with_preset_graph(&target, |inst| {
                        inst.params
                            .contains(param_id.as_ref())
                            .then(|| inst.get_base_param(param_id.as_ref()))
                    })
                    .flatten();
                if let Some(old_val) = old_val {
                    let new_val = if old_val > 0.5 { 0.0 } else { 1.0 };
                    let cmd = ChangeGraphParamCommand::new(target, param_id.clone(), old_val, new_val);
                    ctx.execute(Box::new(cmd));
                }
            }
            DispatchResult::handled()
        }
        ParamsAction::ParamFire(gpt, param_id) => {
            if let Some(target) =
                resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, &ctx.project)
            {
                ContentCommand::send(
                    ctx.content_tx,
                    ContentCommand::FireParameter {
                        target,
                        param_id: param_id.clone(),
                    },
                );
            }
            DispatchResult::handled()
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
                InspectorTab::Layer | InspectorTab::Group | InspectorTab::Scene => {
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
                let boxed: Box<dyn manifold_editing::command::Command + Send> = Box::new(cmd);
                ctx.execute(boxed);
            }
            DispatchResult::handled()
        }

        ParamsAction::PasteEffects | ParamsAction::EditCards(_) | ParamsAction::EffectGroupRightClicked(_) => DispatchResult::handled(),

        // Label right-clicks are consumed by try_open_dropdown — shouldn't reach here
        ParamsAction::ParamLabelRightClick(..) => {
            DispatchResult::handled()
        }

        // ── Macro mapping ─────────────────────────────────────────
    }
}

/// Layer that owns an automation target, resolved from the stable target
/// itself so a scene card bound to a non-active layer still files correctly.
/// None for scene modifiers and group layers.
pub(crate) fn automation_owner_layer_id(
    project: &manifold_core::project::Project,
    target: &manifold_core::GraphTarget,
) -> Option<manifold_core::LayerId> {
    project
        .timeline
        .layers
        .iter()
        .find(|layer| match target {
            manifold_core::GraphTarget::Generator(id) => layer.layer_id == *id,
            manifold_core::GraphTarget::Effect(id) => layer
                .effects
                .as_ref()
                .is_some_and(|effects| effects.iter().any(|effect| effect.id == *id)),
            manifold_core::GraphTarget::SceneModifier { .. } => false,
        })
        .filter(|layer| !layer.is_group())
        .map(|layer| layer.layer_id.clone())
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
    let Some(owner_id) = automation_owner_layer_id(&ctx.project, target) else {
        return DispatchResult::handled();
    };
    expand_layer_ancestors(&owner_id, ctx);
    let ui_target = crate::editing_host::to_ui_graph_target(target);
    ctx.selection.unhide_automation_lane(ui_target.clone(), param_id.clone());
    let min_height = ctx.ui.layout.track_header_height()
        + manifold_ui::color::AUTOMATION_LANE_STRIP_HEIGHT + 60.0;
    if ctx.ui.layout.timeline_body().height < min_height {
        let content = ctx.ui.layout.content_area();
        ctx.ui.layout.update_split_from_drag(content.y_max() - min_height);
    }
    ctx.ui.pending_automation_reveal = Some((ui_target.clone(), param_id.clone()));
    ctx.selection.set_chosen_automation_param(owner_id, ui_target, param_id.clone());
    ctx.selection.set_automation_mode_visible(true);
    ctx.selection.clear_automation_selection();
    DispatchResult::handled()
}

/// Revealing a control lane expands its owner chain, using the same
/// presentation state as manual layer expansion.
fn expand_layer_ancestors(id: &manifold_core::LayerId, ctx: &mut super::super::DispatchCtx) {
    let mut expand = Vec::new();
    let mut next = Some(id.clone());
    for _ in 0..ctx.project.timeline.layers.len() {
        let Some(id) = next.take() else { break; };
        let Some((_, layer)) = ctx.project.timeline.find_layer_by_id(&id) else { break; };
        if layer.is_collapsed { expand.push(id); }
        next = layer.parent_layer_id.clone();
    }
    if !expand.is_empty() {
        ctx.mutate(crate::content_command::touch::LAYERS, move |project| {
            for id in &expand {
                if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(id) {
                    layer.is_collapsed = false;
                }
            }
        });
    }
}

/// Switch the inspector to the Scene tab and queue a reveal of `params`
/// (one row, or a card section's rows for the Scene tab to choose from).
fn reveal_in_scene_tab(
    gpt: &manifold_ui::GraphParamTarget,
    params: Vec<manifold_core::effects::ParamId>,
    effective_tab: InspectorTab,
    active_layer: &Option<manifold_core::LayerId>,
    ctx: &mut super::super::DispatchCtx,
) -> DispatchResult {
    let Some(GraphTarget::Generator(layer)) =
        resolve_graph_target(gpt, ctx.editor_target, effective_tab, active_layer, ctx.selection, &ctx.project)
    else {
        return DispatchResult::handled();
    };
    ctx.selection.pin_scope(InspectorTab::Scene);
    ctx.ui.request_scene_reveal(manifold_ui::view::UiGraphTarget::Generator(layer), params);
    DispatchResult::handled()
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
        let catalog = manifold_nodes::bundled_presets::bundled_preset_def(instance.effect_type())?;
        let graph = instance.graph.as_ref().unwrap_or(catalog.as_ref());
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

        {
            let mut edits = crate::gesture_preview::UiEdits::default();
            let mut ctx = crate::ui_bridge::DispatchCtx {
                project: crate::ui_bridge::ProjectRef::new(&mut project, &mut edits, &content_tx),
                content_tx: &content_tx,
                content_state: &content_state,
                ui: &mut ui,
                selection: &mut selection,
                active_layer: &mut active_layer,
                user_prefs: &mut user_prefs,
                editor_target: None,
                scrub: &mut scrub,
                dialogs: Default::default(),
            };
            crate::ui_bridge::dispatch(&action, &mut ctx)
        };

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

#[cfg(test)]
mod view_fold_dispatch_tests {
    use crate::frame_plan::{plan_frame, FrameInputs, MainRegion, Projection};
    use manifold_ui::{PanelAction, ParamsAction};

    /// A fold moves only the inspector view version: the plan rebuilds the
    /// inspector and runs no project or inspector sync.
    #[test]
    fn fold_toggles_are_view_changes_not_structural_syncs() {
        let mut project = manifold_core::project::Project::default();
        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let content_state = crate::content_state::ContentState::default();
        let mut ui = crate::ui_root::UIRoot::new();
        let mut selection = manifold_ui::UIState::new();
        let mut active_layer = None;
        let mut user_prefs = crate::user_prefs::UserPrefs::in_memory();
        let mut scrub = crate::ui_bridge::ScrubState::default();
        let mut edits = crate::gesture_preview::UiEdits::default();
        let mut last = FrameInputs { inspector_view: ui.inspector.view_version(), ..Default::default() };

        for action in [ParamsAction::MacrosCollapseToggle, ParamsAction::MasterCollapseToggle, ParamsAction::SectionFoldToggled] {
            let was_collapsed = ui.inspector.macros_panel().is_collapsed();
            {
                let mut ctx = crate::ui_bridge::DispatchCtx {
                    project: crate::ui_bridge::ProjectRef::new(&mut project, &mut edits, &content_tx),
                    content_tx: &content_tx,
                    content_state: &content_state,
                    ui: &mut ui,
                    selection: &mut selection,
                    active_layer: &mut active_layer,
                    user_prefs: &mut user_prefs,
                    editor_target: None,
                    scrub: &mut scrub,
                    dialogs: Default::default(),
                };
                crate::ui_bridge::dispatch(&PanelAction::Params(action.clone()), &mut ctx)
            };
            assert!(content_rx.try_recv().is_err(), "{action:?} sends nothing");
            if matches!(action, ParamsAction::MacrosCollapseToggle) {
                assert_ne!(ui.inspector.macros_panel().is_collapsed(), was_collapsed);
            }
            let next = FrameInputs { inspector_view: ui.inspector.view_version(), ..Default::default() };
            assert_ne!(next.inspector_view, last.inspector_view, "{action:?} moves the view version");
            let plan = plan_frame(&last, &next);
            assert!(plan.rebuild.contains(MainRegion::Inspector), "{action:?}");
            assert!(!plan.projections.contains(Projection::ProjectSync), "{action:?}");
            assert!(!plan.projections.contains(Projection::InspectorSync), "{action:?}");
            last = next;
        }
    }
}

#[cfg(test)]
mod trigger_dispatch_tests {
    use super::*;
    use manifold_editing::service::EditingService;
    use manifold_ui::view::{UiClipTriggerSource, UiGraphTarget};
    use manifold_ui::{PanelAction, ParamsAction};

    fn assignment_project() -> (manifold_core::project::Project, manifold_core::GraphTarget, manifold_core::LayerId, manifold_core::LayerId) {
        let mut project = manifold_core::project::Project::default();
        let mut owner = manifold_core::layer::Layer::new_video("Owner".into(), 0);
        let owner_id = owner.layer_id.clone();
        let mut effect = manifold_core::effects::PresetInstance::new(manifold_core::PresetTypeId::BLOOM);
        effect.id = manifold_core::EffectId::new("owner-effect");
        effect.params = manifold_core::params::ParamManifest::from_params(vec![
            manifold_core::params::Param::bundled(manifold_core::effect_graph_def::ParamSpecDef {
                id: "amount".into(),
                name: "Amount".into(),
                ..Default::default()
            }),
        ]);
        owner.effects = Some(vec![effect]);
        let lane = manifold_core::layer::Layer::new_trigger("Owner lane".into(), owner_id.clone(), 1);
        let lane_id = lane.layer_id.clone();
        let mut other = manifold_core::layer::Layer::new_video("Other".into(), 2);
        let other_id = other.layer_id.clone();
        other.effects = Some(Vec::new());
        project.timeline.layers.extend([owner, lane, other]);
        (project, manifold_core::GraphTarget::Effect(manifold_core::EffectId::new("owner-effect")), lane_id, other_id)
    }

    #[test]
    fn clip_source_assignment_queues_stable_target_without_mutating_snapshot() {
        let (mut project, target, lane_id, other_id) = assignment_project();
        let before = serde_json::to_value(&project).unwrap();
        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let content_state = crate::content_state::ContentState::default();
        let mut ui = crate::ui_root::UIRoot::new();
        let mut selection = manifold_ui::UIState::default();
        selection.select_layer(other_id.clone());
        let mut active_layer = Some(other_id);
        let mut user_prefs = crate::user_prefs::UserPrefs::in_memory();
        let mut scrub = crate::ui_bridge::ScrubState::default();
        let action = PanelAction::Params(ParamsAction::AssignClipTriggerSource(
            UiGraphTarget::Effect(manifold_core::EffectId::new("owner-effect")),
            "amount".into(),
            UiClipTriggerSource::Lane(lane_id.clone()),
        ));

        crate::ui_bridge::dispatch(
            &action,
            &mut crate::ui_bridge::DispatchCtx {
                project: crate::ui_bridge::ProjectRef::new(&mut project, &mut Default::default(), &content_tx),
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
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert_eq!(ui.pending_layer_reveal(), Some(&lane_id));
        assert_eq!(active_layer, Some(project.timeline.layers[2].layer_id.clone()));

        let ContentCommand::ExecuteOnContent(command) = content_rx.try_recv().unwrap() else {
            panic!("expected content-owned trigger assignment")
        };
        let mut editing = EditingService::new();
        editing.execute(command, &mut project);
        assert!(editing.take_rejection().is_none());
        let owner = project.graph_target_owner(&target).unwrap();
        assert_eq!(owner.params.get("amount").unwrap().clip_trigger_source,
            manifold_core::params::ClipTriggerSource::Lane { layer_id: lane_id });
        assert_eq!(owner.envelopes.as_ref().unwrap().len(), 1);

        assert!(editing.undo(&mut project));
        let owner = project.graph_target_owner(&target).unwrap();
        assert_eq!(owner.params.get("amount").unwrap().clip_trigger_source,
            manifold_core::params::ClipTriggerSource::OwnLayer);
        assert!(owner.envelopes.as_ref().is_none_or(Vec::is_empty));
        assert!(editing.redo(&mut project));
        assert!(project.graph_target_owner(&target).unwrap().envelopes.is_some());
    }

    #[test]
    fn response_navigation_selects_owner_and_only_queues_presentation_changes() {
        let (mut project, target, lane_id, _) = assignment_project();
        let owner_id = project.timeline.layers[0].layer_id.clone();
        project.graph_target_owner_mut(&target).unwrap().collapsed = true;
        let before = serde_json::to_value(&project).unwrap();
        let mut content = project.clone();
        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let content_state = crate::content_state::ContentState::default();
        let mut ui = crate::ui_root::UIRoot::new();
        let mut selection = manifold_ui::UIState::default();
        selection.select_layer(lane_id.clone());
        let mut active_layer = Some(lane_id);
        let mut user_prefs = crate::user_prefs::UserPrefs::in_memory();
        let mut scrub = crate::ui_bridge::ScrubState::default();
        let ui_target = crate::editing_host::to_ui_graph_target(&target);
        crate::ui_bridge::dispatch(
            &PanelAction::Params(ParamsAction::ShowClipTriggerResponse(ui_target.clone(), "amount".into())),
            &mut crate::ui_bridge::DispatchCtx {
                project: crate::ui_bridge::ProjectRef::new(&mut project, &mut Default::default(), &content_tx), content_tx: &content_tx, content_state: &content_state,
                ui: &mut ui, selection: &mut selection, active_layer: &mut active_layer,
                user_prefs: &mut user_prefs, editor_target: None, scrub: &mut scrub, dialogs: Default::default(),
            },
        );
        assert_eq!(active_layer, Some(owner_id.clone()));
        assert_eq!(selection.primary_selected_layer_id(), Some(&owner_id));
        assert_eq!(ui.pending_trigger_response_reveal(), Some(&(ui_target, "amount".into())));
        // The card expands locally, and the content thread gets the same write.
        assert!(!project.graph_target_owner(&target).unwrap().collapsed);
        for command in content_rx.try_iter() {
            let ContentCommand::MutateProject { f: presentation, .. } = command else {
                panic!("navigation must not author an assignment or response");
            };
            presentation(&mut content);
        }
        assert_eq!(serde_json::to_value(&content).unwrap(), serde_json::to_value(&project).unwrap());
        project.graph_target_owner_mut(&target).unwrap().collapsed = true;
        assert_eq!(serde_json::to_value(&project).unwrap(), before, "only presentation state moved");
    }

    #[test]
    fn create_trigger_lane_dispatch_is_one_selected_undoable_edit() {
        let (mut project, target, _lane_id, other_id) = assignment_project();
        project.timeline.layers[0].is_collapsed = true;
        let owner_id = project.timeline.layers[0].layer_id.clone();
        let before = serde_json::to_value(&project).unwrap();
        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let content_state = crate::content_state::ContentState::default();
        let mut ui = crate::ui_root::UIRoot::new();
        let mut selection = manifold_ui::UIState::default();
        selection.select_layer(other_id.clone());
        let mut active_layer = Some(other_id);
        let mut user_prefs = crate::user_prefs::UserPrefs::in_memory();
        let mut scrub = crate::ui_bridge::ScrubState::default();
        let action = PanelAction::Params(ParamsAction::CreateTriggerLane {
            owner: owner_id.clone(),
            assignment: Some((
                UiGraphTarget::Effect(manifold_core::EffectId::new("owner-effect")),
                "amount".into(),
            )),
        });

        crate::ui_bridge::dispatch(
            &action,
            &mut crate::ui_bridge::DispatchCtx {
                project: crate::ui_bridge::ProjectRef::new(&mut project, &mut Default::default(), &content_tx),
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
        // The parent expands locally (a stamped presentation write); the lane
        // itself waits for the content receipt.
        assert!(!project.timeline.layers[0].is_collapsed);
        project.timeline.layers[0].is_collapsed = true;
        assert_eq!(serde_json::to_value(&project).unwrap(), before);

        let ContentCommand::MutateProject { f: expand, .. } = content_rx.try_recv().unwrap() else {
            panic!("expected the same parent expansion on the content thread")
        };
        expand(&mut project);
        assert!(!project.timeline.layers[0].is_collapsed);
        let ContentCommand::ExecuteSelecting(command, request) = content_rx.try_recv().unwrap() else {
            panic!("expected selected trigger-lane insertion")
        };
        assert!(matches!(request, crate::edit_selection::SelectAfterEdit::NewLayer));
        let mut editing = EditingService::new();
        editing.execute(command, &mut project);
        assert!(editing.take_rejection().is_none());
        assert_eq!(project.timeline.layers.len(), 4);
        let lane = project.timeline.layers.iter().find(|layer| layer.is_trigger()).unwrap();
        let lane_id = lane.layer_id.clone();
        assert_eq!(lane.parent_layer_id.as_ref(), Some(&owner_id));
        assert_eq!(project.graph_target_owner(&target).unwrap().params.get("amount").unwrap().clip_trigger_source,
            manifold_core::params::ClipTriggerSource::Lane { layer_id: lane_id.clone() });

        assert!(editing.undo(&mut project));
        assert_eq!(project.timeline.layers.len(), 3);
        assert!(project.timeline.find_layer_by_id(&lane_id).is_none());
        let owner = project.graph_target_owner(&target).unwrap();
        assert_eq!(owner.params.get("amount").unwrap().clip_trigger_source,
            manifold_core::params::ClipTriggerSource::OwnLayer);
        assert!(owner.envelopes.as_ref().is_none_or(Vec::is_empty));
        assert!(editing.redo(&mut project));
        assert_eq!(project.timeline.layers.len(), 4);
        assert_ne!(project.graph_target_owner(&target).unwrap().params.get("amount").unwrap().clip_trigger_source,
            manifold_core::params::ClipTriggerSource::OwnLayer);
    }
}

#[cfg(test)]
mod automation_owner_tests {
    use super::*;
    use manifold_core::layer::Layer;
    use manifold_core::project::Project;
    use manifold_core::PresetTypeId;

    #[test]
    fn owner_is_target_layer_and_modifiers_have_none() {
        let mut project = Project::default();
        let a = Layer::new_generator("A".into(), PresetTypeId::NONE, 0);
        let b = Layer::new_generator("B".into(), PresetTypeId::NONE, 1);
        let b_id = b.layer_id.clone();
        project.timeline.layers.push(a);
        project.timeline.layers.push(b);
        assert_eq!(
            automation_owner_layer_id(&project, &GraphTarget::Generator(b_id.clone())),
            Some(b_id.clone())
        );
        let modifier = GraphTarget::SceneModifier {
            owner: Box::new(GraphTarget::Generator(b_id)),
            modifier_id: "m".into(),
        };
        assert_eq!(automation_owner_layer_id(&project, &modifier), None);
    }
}

#[cfg(test)]
mod reveal_dispatch_tests {
    use crate::frame_plan::{plan_frame, FrameInputs, MainRegion, Projection};
    use manifold_core::types::LayerType;
    use manifold_core::PresetTypeId;
    use manifold_ui::{InspectorTab, PanelAction, ParamsAction};

    /// Show in Scene tab with the inspector already pinned to Scene: the pin
    /// moves nothing, so the reveal's own version is what runs the inspector
    /// projection that takes it. Asking again while it is pending moves
    /// nothing.
    #[test]
    fn scene_reveal_moves_its_version_when_the_tab_is_already_pinned() {
        let mut project = manifold_core::project::Project::default();
        let idx = project.timeline.add_layer("Scene", LayerType::Generator, PresetTypeId::NONE);
        let layer_id = project.timeline.layers[idx].layer_id.clone();
        let (content_tx, content_rx) = crossbeam_channel::unbounded();
        let content_state = crate::content_state::ContentState::default();
        let mut ui = crate::ui_root::UIRoot::new();
        let mut selection = manifold_ui::UIState::new();
        selection.pin_scope(InspectorTab::Scene);
        let mut active_layer = Some(layer_id.clone());
        let mut user_prefs = crate::user_prefs::UserPrefs::in_memory();
        let mut scrub = crate::ui_bridge::ScrubState::default();
        let mut edits = crate::gesture_preview::UiEdits::default();
        let inputs = |ui: &crate::ui_root::UIRoot, selection: &manifold_ui::UIState| FrameInputs {
            inspector_reveal: ui.inspector_reveal_version(),
            selection_identity: selection.selection_identity_version(),
            ..Default::default()
        };
        let action = PanelAction::Params(ParamsAction::RevealInSceneTab(
            manifold_ui::GraphParamTarget::GeneratorOf(layer_id.clone()),
            "amount".into(),
        ));
        let mut last = inputs(&ui, &selection);
        for pass in 0..2 {
            {
                let mut ctx = crate::ui_bridge::DispatchCtx {
                    project: crate::ui_bridge::ProjectRef::new(&mut project, &mut edits, &content_tx),
                    content_tx: &content_tx,
                    content_state: &content_state,
                    ui: &mut ui,
                    selection: &mut selection,
                    active_layer: &mut active_layer,
                    user_prefs: &mut user_prefs,
                    editor_target: None,
                    scrub: &mut scrub,
                    dialogs: Default::default(),
                };
                crate::ui_bridge::dispatch(&action, &mut ctx);
            }
            assert!(content_rx.try_recv().is_err(), "a reveal sends nothing");
            let next = inputs(&ui, &selection);
            assert_eq!(next.selection_identity, last.selection_identity, "already pinned to Scene");
            let plan = plan_frame(&last, &next);
            if pass == 0 {
                assert!(plan.projections.contains(Projection::InspectorSync));
                assert!(plan.rebuild.contains(MainRegion::Inspector));
                assert!(!plan.projections.contains(Projection::ProjectSync));
            } else {
                assert!(plan.is_empty(), "the same reveal still pending moves nothing");
            }
            last = next;
        }
    }
}
