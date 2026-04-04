//! Undo/redo commands for Ableton Live OSC bridge mappings.

use crate::command::Command;
use manifold_core::ableton_mapping::{AbletonMappingTarget, AbletonParamMapping};
use manifold_core::project::Project;

// ── Map / Unmap ──────────────────────────────────────────────────

/// Undoable command for adding or removing an Ableton macro mapping.
#[derive(Debug)]
pub struct ChangeAbletonMappingCommand {
    target: AbletonMappingTarget,
    old_mapping: Option<AbletonParamMapping>,
    new_mapping: Option<AbletonParamMapping>,
    /// For MacroSlot: old label to restore on undo.
    old_macro_label: Option<String>,
    new_macro_label: Option<String>,
}

impl ChangeAbletonMappingCommand {
    pub fn map(
        target: AbletonMappingTarget,
        mapping: AbletonParamMapping,
        old_mapping: Option<AbletonParamMapping>,
        old_macro_label: Option<String>,
        new_macro_label: Option<String>,
    ) -> Self {
        Self {
            target,
            old_mapping,
            new_mapping: Some(mapping),
            old_macro_label,
            new_macro_label,
        }
    }

    pub fn unmap(
        target: AbletonMappingTarget,
        old_mapping: AbletonParamMapping,
    ) -> Self {
        Self {
            target,
            old_mapping: Some(old_mapping),
            new_mapping: None,
            old_macro_label: None,
            new_macro_label: None,
        }
    }
}

impl Command for ChangeAbletonMappingCommand {
    fn execute(&mut self, project: &mut Project) {
        apply_mapping(project, &self.target, &self.new_mapping);
        if let AbletonMappingTarget::MacroSlot { slot_index } = &self.target
            && let Some(ref label) = self.new_macro_label
            && let Some(slot) = project.settings.macro_bank.slots.get_mut(*slot_index)
        {
            slot.label = label.clone();
        }
    }

    fn undo(&mut self, project: &mut Project) {
        apply_mapping(project, &self.target, &self.old_mapping);
        if let AbletonMappingTarget::MacroSlot { slot_index } = &self.target
            && let Some(ref label) = self.old_macro_label
            && let Some(slot) = project.settings.macro_bank.slots.get_mut(*slot_index)
        {
            slot.label = label.clone();
        }
    }

    fn description(&self) -> &str {
        if self.new_mapping.is_some() {
            "Map Ableton Macro"
        } else {
            "Unmap Ableton Macro"
        }
    }
}

// ── Trim ─────────────────────────────────────────────────────────

/// Undoable command for changing Ableton trim range (range_min/range_max).
#[derive(Debug)]
pub struct ChangeAbletonTrimCommand {
    target: AbletonMappingTarget,
    old_min: f32,
    old_max: f32,
    new_min: f32,
    new_max: f32,
}

impl ChangeAbletonTrimCommand {
    pub fn new(
        target: AbletonMappingTarget,
        old_min: f32,
        old_max: f32,
        new_min: f32,
        new_max: f32,
    ) -> Self {
        Self { target, old_min, old_max, new_min, new_max }
    }
}

impl Command for ChangeAbletonTrimCommand {
    fn execute(&mut self, project: &mut Project) {
        set_trim(project, &self.target, self.new_min, self.new_max);
    }

    fn undo(&mut self, project: &mut Project) {
        set_trim(project, &self.target, self.old_min, self.old_max);
    }

    fn description(&self) -> &str {
        "Change Ableton Trim"
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn apply_mapping(
    project: &mut Project,
    target: &AbletonMappingTarget,
    mapping: &Option<AbletonParamMapping>,
) {
    match target {
        AbletonMappingTarget::MasterEffect { effect_type, param_index } => {
            if let Some(fx) = project
                .settings
                .master_effects
                .iter_mut()
                .find(|f| f.effect_type() == effect_type)
            {
                let m = fx.ableton_mappings.get_or_insert_with(Vec::new);
                m.retain(|x| x.param_index != *param_index);
                if let Some(mapping) = mapping {
                    m.push(mapping.clone());
                }
                if m.is_empty() {
                    fx.ableton_mappings = None;
                }
            }
        }
        AbletonMappingTarget::LayerEffect { layer_id, effect_type, param_index } => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str())
                && let Some(effects) = &mut layer.effects
                && let Some(fx) = effects.iter_mut().find(|f| f.effect_type() == effect_type)
            {
                let m = fx.ableton_mappings.get_or_insert_with(Vec::new);
                m.retain(|x| x.param_index != *param_index);
                if let Some(mapping) = mapping {
                    m.push(mapping.clone());
                }
                if m.is_empty() {
                    fx.ableton_mappings = None;
                }
            }
        }
        AbletonMappingTarget::GenParam { layer_id, param_index } => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str())
                && let Some(gp) = layer.gen_params_mut()
            {
                let m = gp.ableton_mappings.get_or_insert_with(Vec::new);
                m.retain(|x| x.param_index != *param_index);
                if let Some(mapping) = mapping {
                    m.push(mapping.clone());
                }
                if m.is_empty() {
                    gp.ableton_mappings = None;
                }
            }
        }
        AbletonMappingTarget::MacroSlot { slot_index } => {
            if let Some(slot) = project.settings.macro_bank.slots.get_mut(*slot_index) {
                slot.ableton_mapping = mapping.clone();
            }
        }
    }
}

fn set_trim(project: &mut Project, target: &AbletonMappingTarget, min: f32, max: f32) {
    match target {
        AbletonMappingTarget::MasterEffect { effect_type, param_index } => {
            if let Some(fx) = project
                .settings
                .master_effects
                .iter_mut()
                .find(|f| f.effect_type() == effect_type)
                && let Some(ms) = &mut fx.ableton_mappings
                && let Some(m) = ms.iter_mut().find(|m| m.param_index == *param_index)
            {
                m.range_min = min;
                m.range_max = max;
            }
        }
        AbletonMappingTarget::LayerEffect { layer_id, effect_type, param_index } => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str())
                && let Some(effects) = &mut layer.effects
                && let Some(fx) = effects.iter_mut().find(|f| f.effect_type() == effect_type)
                && let Some(ms) = &mut fx.ableton_mappings
                && let Some(m) = ms.iter_mut().find(|m| m.param_index == *param_index)
            {
                m.range_min = min;
                m.range_max = max;
            }
        }
        AbletonMappingTarget::GenParam { layer_id, param_index } => {
            if let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str())
                && let Some(gp) = layer.gen_params_mut()
                && let Some(ms) = &mut gp.ableton_mappings
                && let Some(m) = ms.iter_mut().find(|m| m.param_index == *param_index)
            {
                m.range_min = min;
                m.range_max = max;
            }
        }
        AbletonMappingTarget::MacroSlot { slot_index } => {
            if let Some(slot) = project.settings.macro_bank.slots.get_mut(*slot_index)
                && let Some(m) = &mut slot.ableton_mapping
            {
                m.range_min = min;
                m.range_max = max;
            }
        }
    }
}
