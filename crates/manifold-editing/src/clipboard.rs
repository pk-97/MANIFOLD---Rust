use manifold_core::effects::{EffectGroup, PresetInstance};
use manifold_core::{EffectGroupId, EffectId};
use std::collections::{HashMap, HashSet};

/// Static effect clipboard. Port of C# EffectClipboard.
pub struct EffectClipboard {
    clips: Vec<PresetInstance>,
    groups: Vec<EffectGroup>,
}

impl EffectClipboard {
    pub fn new() -> Self {
        Self {
            clips: Vec::new(),
            groups: Vec::new(),
        }
    }

    pub fn has_content(&self) -> bool {
        !self.clips.is_empty()
    }

    pub fn count(&self) -> usize {
        self.clips.len()
    }

    pub fn copy_single(&mut self, effect: &PresetInstance) {
        self.clips.clear();
        self.groups.clear();
        self.clips.push(effect.clone());
    }

    pub fn copy_all(&mut self, effects: &[PresetInstance]) {
        self.clips.clear();
        self.groups.clear();
        self.clips.extend(effects.iter().cloned());
    }

    /// Copy an effect selection together with every group whose members are
    /// fully selected. Partial groups become ordinary, ungrouped effects.
    pub fn copy_selection(
        &mut self,
        effects: &[PresetInstance],
        groups: &[EffectGroup],
        selected: &[EffectId],
    ) {
        self.clips.clear();
        self.groups.clear();

        let selected: HashSet<&EffectId> = selected.iter().collect();
        let complete_groups: HashSet<EffectGroupId> = groups
            .iter()
            .filter(|group| {
                let members: Vec<&PresetInstance> = effects
                    .iter()
                    .filter(|effect| effect.group_id.as_ref() == Some(&group.id))
                    .collect();
                !members.is_empty() && members.iter().all(|effect| selected.contains(&effect.id))
            })
            .map(|group| group.id.clone())
            .collect();

        self.clips = effects
            .iter()
            .filter(|effect| selected.contains(&effect.id))
            .cloned()
            .map(|mut effect| {
                if !effect
                    .group_id
                    .as_ref()
                    .is_some_and(|group_id| complete_groups.contains(group_id))
                {
                    effect.group_id = None;
                }
                effect
            })
            .collect();

        self.groups = groups
            .iter()
            .filter(|group| complete_groups.contains(&group.id))
            .cloned()
            .map(|mut group| {
                if !group
                    .parent_group_id
                    .as_ref()
                    .is_some_and(|parent| complete_groups.contains(parent))
                {
                    group.parent_group_id = None;
                }
                group
            })
            .collect();
    }

    /// Get fresh clones for paste.
    pub fn get_paste_clones(&self) -> Vec<PresetInstance> {
        self.clips.clone()
    }

    /// Return fresh effect and group IDs for one paste operation.
    pub fn paste_payload(&self) -> (Vec<PresetInstance>, Vec<EffectGroup>) {
        let effect_ids: HashMap<EffectId, EffectId> = self
            .clips
            .iter()
            .map(|effect| (effect.id.clone(), EffectId::new(manifold_core::short_id())))
            .collect();
        let group_ids: HashMap<EffectGroupId, EffectGroupId> = self
            .groups
            .iter()
            .map(|group| {
                (
                    group.id.clone(),
                    EffectGroupId::new(manifold_core::short_id()),
                )
            })
            .collect();

        let clips = self
            .clips
            .iter()
            .map(|effect| {
                let mut copy = effect.duplicated();
                copy.id = effect_ids
                    .get(&effect.id)
                    .cloned()
                    .expect("every clipboard effect has a remapped ID");
                copy.group_id = copy
                    .group_id
                    .as_ref()
                    .and_then(|group_id| group_ids.get(group_id).cloned());
                copy
            })
            .collect();
        let groups = self
            .groups
            .iter()
            .map(|group| {
                let mut copy = group.clone();
                copy.id = group_ids
                    .get(&group.id)
                    .cloned()
                    .expect("every clipboard group has a remapped ID");
                copy.parent_group_id = copy
                    .parent_group_id
                    .as_ref()
                    .and_then(|parent| group_ids.get(parent).cloned());
                copy.mask_effect_id = copy
                    .mask_effect_id
                    .as_ref()
                    .and_then(|mask_id| effect_ids.get(mask_id).cloned());
                copy
            })
            .collect();

        (clips, groups)
    }

    pub fn clear(&mut self) {
        self.clips.clear();
        self.groups.clear();
    }
}

impl Default for EffectClipboard {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Generator Clipboard ───

use manifold_core::effects::{ParamEnvelope, ParameterDriver};
use manifold_core::preset_type_id::PresetTypeId;

/// Snapshot of a generator's complete state for copy/paste.
#[derive(Debug, Clone)]
pub struct GeneratorSnapshot {
    pub generator_type: PresetTypeId,
    pub param_values: Vec<f32>,
    pub base_param_values: Option<Vec<f32>>,
    pub drivers: Option<Vec<ParameterDriver>>,
    pub envelopes: Option<Vec<ParamEnvelope>>,
}

/// Generator clipboard — stores one generator setup for paste.
pub struct GeneratorClipboard {
    snapshot: Option<GeneratorSnapshot>,
}

impl GeneratorClipboard {
    pub fn new() -> Self {
        Self { snapshot: None }
    }

    pub fn has_content(&self) -> bool {
        self.snapshot.is_some()
    }

    pub fn copy_from(&mut self, state: &PresetInstance) {
        self.snapshot = Some(GeneratorSnapshot {
            generator_type: state.generator_type().clone(),
            // Clipboard carries effective float values; exposure is host
            // state and doesn't travel with a copy/paste.
            param_values: state.params.iter().map(|p| p.value).collect(),
            // base rides each Param now; snapshot it as the former
            // Option<Vec<f32>> shape, present iff base is tracked.
            base_param_values: state
                .base_tracked
                .then(|| state.params.iter().map(|p| p.base).collect()),
            drivers: state.drivers.clone(),
            envelopes: state.envelopes.clone(),
        });
    }

    pub fn get_paste_snapshot(&self) -> Option<GeneratorSnapshot> {
        self.snapshot.clone()
    }
}

impl Default for GeneratorClipboard {
    fn default() -> Self {
        Self::new()
    }
}
