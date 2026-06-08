use manifold_core::effects::{EffectGroup, PresetInstance};
use manifold_core::project::Project;
use manifold_core::{EffectId, LayerId};

/// Identifies an effect *list* for structural / list operations (add, remove,
/// reorder, grouping). It names a destination list, not an instance — an insert
/// has no instance to address yet — which is why these ops can't use an
/// [`EffectId`]. Single-effect edits (toggle, param, expose, binding) address
/// the instance directly by [`EffectId`] via `Project::find_effect_by_id_mut`
/// and do NOT use this type. See the addressing-model note in `effects.rs`.
#[derive(Debug, Clone)]
pub enum EffectTarget {
    Layer { layer_id: LayerId },
    Master,
}

/// Execute a closure with mutable access to a target's effects and groups.
/// Returns None if the target doesn't exist.
pub fn with_effects_mut<F, R>(project: &mut Project, target: &EffectTarget, f: F) -> Option<R>
where
    F: FnOnce(&mut Vec<PresetInstance>, &mut Vec<EffectGroup>) -> R,
{
    match target {
        EffectTarget::Layer { layer_id } => {
            let (_, layer) = project.timeline.find_layer_by_id_mut(layer_id)?;
            let effects = layer.effects_mut() as *mut Vec<PresetInstance>;
            let groups = layer.effect_groups_mut() as *mut Vec<EffectGroup>;
            Some(f(unsafe { &mut *effects }, unsafe { &mut *groups }))
        }
        EffectTarget::Master => {
            let settings = &mut project.settings;
            let effects = &mut settings.master_effects as *mut Vec<PresetInstance>;
            let groups = settings.master_effect_groups_mut() as *mut Vec<EffectGroup>;
            Some(f(unsafe { &mut *effects }, unsafe { &mut *groups }))
        }
    }
}

/// Execute a closure with read-only access to a target's effects.
pub fn with_effects<F, R>(project: &Project, target: &EffectTarget, f: F) -> Option<R>
where
    F: FnOnce(&[PresetInstance], &[EffectGroup]) -> R,
{
    match target {
        EffectTarget::Layer { layer_id } => {
            let (_, layer) = project.timeline.find_layer_by_id(layer_id)?;
            let effects = layer.effects.as_deref().unwrap_or(&[]);
            let groups = layer.effect_groups.as_deref().unwrap_or(&[]);
            Some(f(effects, groups))
        }
        EffectTarget::Master => {
            let groups = project
                .settings
                .master_effect_groups
                .as_deref()
                .unwrap_or(&[]);
            Some(f(&project.settings.master_effects, groups))
        }
    }
}

/// Routes driver commands to effect drivers vs generator param drivers.
/// The `Effect` arm addresses its instance by stable [`EffectId`] (master /
/// layer / clip), consistent with every other single-effect edit.
#[derive(Debug, Clone)]
pub enum DriverTarget {
    Effect { effect_id: EffectId },
    GeneratorParam { layer_id: LayerId },
}

impl From<&manifold_core::GraphTarget> for DriverTarget {
    fn from(t: &manifold_core::GraphTarget) -> Self {
        match t {
            manifold_core::GraphTarget::Effect(eid) => DriverTarget::Effect {
                effect_id: eid.clone(),
            },
            manifold_core::GraphTarget::Generator(lid) => DriverTarget::GeneratorParam {
                layer_id: lid.clone(),
            },
        }
    }
}
