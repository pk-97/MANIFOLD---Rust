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

    pub fn unmap(target: AbletonMappingTarget, old_mapping: AbletonParamMapping) -> Self {
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
        Self {
            target,
            old_min,
            old_max,
            new_min,
            new_max,
        }
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
    // MacroSlot stores a single mapping, not a per-param vec — its own arm.
    if let AbletonMappingTarget::MacroSlot { slot_index } = target {
        if let Some(slot) = project.settings.macro_bank.slots.get_mut(*slot_index) {
            slot.ableton_mapping = mapping.clone();
        }
        return;
    }
    // The three host-vec variants (master / layer effect / generator) share
    // one upsert: locate the host's mapping vec, drop any prior mapping for
    // this param, push the new one (or leave the slot cleared when removing).
    let Some(param_id) = target.param_id().cloned() else {
        return;
    };
    if let Some(slot) = project.ableton_param_mappings_mut(target) {
        let m = slot.get_or_insert_with(Vec::new);
        m.retain(|x| x.param_id != param_id);
        if let Some(mapping) = mapping {
            m.push(mapping.clone());
        }
        if m.is_empty() {
            *slot = None;
        }
    }
}

fn set_trim(project: &mut Project, target: &AbletonMappingTarget, min: f32, max: f32) {
    // MacroSlot's single mapping — its own arm.
    if let AbletonMappingTarget::MacroSlot { slot_index } = target {
        if let Some(slot) = project.settings.macro_bank.slots.get_mut(*slot_index)
            && let Some(m) = &mut slot.ableton_mapping
        {
            m.range_min = min;
            m.range_max = max;
        }
        return;
    }
    // The three host-vec variants share one find-by-param-id + set-range.
    let Some(param_id) = target.param_id().cloned() else {
        return;
    };
    if let Some(slot) = project.ableton_param_mappings_mut(target)
        && let Some(ms) = slot.as_mut()
        && let Some(m) = ms.iter_mut().find(|m| m.param_id == param_id)
    {
        m.range_min = min;
        m.range_max = max;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    use manifold_core::ableton_mapping::{
        AbletonDeviceIdentity, AbletonMacroAddress, AbletonMappingStatus,
    };
    use manifold_core::effects::PresetInstance;
    use manifold_core::id::EffectId;
    use manifold_core::PresetTypeId;
    use std::borrow::Cow;

    fn mapping(param: &'static str, min: f32, max: f32) -> AbletonParamMapping {
        AbletonParamMapping {
            param_id: Cow::Borrowed(param),
            address: AbletonMacroAddress {
                track_id: 0,
                device_id: 0,
                param_id: 0,
                device_identity: AbletonDeviceIdentity {
                    device_class_name: "InstrumentGroupDevice".to_string(),
                },
                track_name: "Bass".to_string(),
                device_name: "Rack".to_string(),
                macro_name: "Filter".to_string(),
            },
            range_min: min,
            range_max: max,
            inverted: false,
            legacy_param_index: None,
            last_value: 0.0,
            status: AbletonMappingStatus::Active,
        }
    }

    #[test]
    fn macro_slot_trim_round_trips() {
        let mut project = Project::default();
        project.settings.macro_bank.slots[0].ableton_mapping = Some(mapping("macro", 0.2, 0.8));

        let mut cmd = ChangeAbletonTrimCommand::new(
            AbletonMappingTarget::MacroSlot { slot_index: 0 },
            0.2,
            0.8,
            0.4,
            0.6,
        );
        cmd.execute(&mut project);
        let m = project.settings.macro_bank.slots[0].ableton_mapping.as_ref().unwrap();
        assert!((m.range_min - 0.4).abs() < f32::EPSILON);
        assert!((m.range_max - 0.6).abs() < f32::EPSILON);

        cmd.undo(&mut project);
        let m = project.settings.macro_bank.slots[0].ableton_mapping.as_ref().unwrap();
        assert!((m.range_min - 0.2).abs() < f32::EPSILON);
        assert!((m.range_max - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn master_effect_trim_round_trips() {
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::new("Bloom"));
        fx.id = EffectId::new("fx-1");
        fx.ableton_mappings = Some(vec![mapping("amount", 0.0, 1.0)]);
        project.settings.master_effects.push(fx);

        let target = AbletonMappingTarget::MasterEffect {
            effect_type: PresetTypeId::new("Bloom"),
            param_id: Cow::Borrowed("amount"),
        };
        let mut cmd = ChangeAbletonTrimCommand::new(target, 0.0, 1.0, 0.25, 0.75);

        cmd.execute(&mut project);
        let m = &project.settings.master_effects[0].ableton_mappings.as_ref().unwrap()[0];
        assert!((m.range_min - 0.25).abs() < f32::EPSILON);
        assert!((m.range_max - 0.75).abs() < f32::EPSILON);

        cmd.undo(&mut project);
        let m = &project.settings.master_effects[0].ableton_mappings.as_ref().unwrap()[0];
        assert!((m.range_min - 0.0).abs() < f32::EPSILON);
        assert!((m.range_max - 1.0).abs() < f32::EPSILON);
    }
}
