//! Inspector dispatch handlers: the mapping domain (UI_FUNNEL_DECOMPOSITION
//! P-B, D6) — macro mappings and Ableton mappings (params + macro slots),
//! their trim handles, and invert toggles. One slice of the inspector
//! dispatch, reached by `dispatch_inspector`'s first-non-unhandled chain. Arms
//! are the former `dispatch_inspector` arms VERBATIM (they already read `ctx`
//! fields directly); a `_ => unhandled()` fall-through lets the chain advance.
//!
//! D-11: `effective_tab`/`active_layer` are computed once near the top of
//! `dispatch_inspector` in inspector.rs; this sub-dispatcher cannot see that
//! outer function's locals, so it recomputes them here — the same two
//! lines, byte-exact, as the sanctioned preamble.

use manifold_editing::commands::ableton::ChangeAbletonTrimCommand;
use manifold_ui::MappingAction;

use super::super::DispatchResult;
use super::resolve::{
    ableton_mapping_target, macro_mapping_target, resolve_graph_target, resolve_param_range,
};
use crate::content_command::ContentCommand;

pub(crate) fn dispatch_mapping(action: &MappingAction, ctx: &mut super::super::DispatchCtx) -> DispatchResult {
    let (effective_tab, effective_active_layer) = super::editor_dispatch_context(ctx.editor_target, &*ctx.project, ctx.ui.inspector.last_effect_tab(), ctx.active_layer);
    let active_layer = &effective_active_layer;
    match action {
        MappingAction::MapParamToMacro(gpt, param_id, macro_idx) => {
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
        MappingAction::MacroLabelRightClick(_) => DispatchResult::handled(),

        MappingAction::UnmapMacro(macro_idx, mapping_idx) => {
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
        MappingAction::ClearMacroMappings(macro_idx) => {
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
        MappingAction::MapParamToAbleton(gpt, param_id, address) => {
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
        MappingAction::UnmapParamAbleton(gpt, param_id) => {
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

        MappingAction::MapMacroToAbleton(slot_idx, address) => {
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
        MappingAction::UnmapMacroAbleton(slot_idx) => {
            use manifold_core::ableton_mapping::AbletonMappingTarget;
            let target = AbletonMappingTarget::MacroSlot {
                slot_index: *slot_idx,
            };
            ContentCommand::send(ctx.content_tx, ContentCommand::AbletonUnmapParam { target });
            DispatchResult::handled()
        }
        // Picker open is consumed by try_open_dropdown — never reaches dispatch.
        MappingAction::OpenAbletonPickerForMacro(_) => DispatchResult::handled(),

        // Driver / Ableton / audio trim handles are unified into the
        // `Trim{Changed,Snapshot,Commit}(TrimKind, …)` arms above.

        MappingAction::AbletonMacroTrimChanged(slot_idx, min, max) => {
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
        MappingAction::AbletonMacroTrimSnapshot(slot_idx) => {
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
        MappingAction::AbletonMacroTrimCommit(slot_idx) => {
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

        MappingAction::AbletonInvertToggle(gpt, param_id) => {
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

        MappingAction::AbletonMacroInvertToggle(slot_idx) => {
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

    }
}
