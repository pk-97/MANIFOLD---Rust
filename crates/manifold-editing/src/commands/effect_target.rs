use manifold_core::LayerId;
use manifold_core::effects::{EffectInstance, EffectGroup};
use manifold_core::project::Project;

/// Routes effect commands to layer/master effect lists.
/// Per-clip effects removed (Ableton model: effects on layer/master only).
#[derive(Debug, Clone)]
pub enum EffectTarget {
    Layer { layer_id: LayerId },
    Master,
}

/// Execute a closure with mutable access to a target's effects and groups.
/// Returns None if the target doesn't exist.
pub fn with_effects_mut<F, R>(
    project: &mut Project,
    target: &EffectTarget,
    f: F,
) -> Option<R>
where
    F: FnOnce(&mut Vec<EffectInstance>, &mut Vec<EffectGroup>) -> R,
{
    match target {
        EffectTarget::Layer { layer_id } => {
            let (_, layer) = project.timeline.find_layer_by_id_mut(layer_id)?;
            let effects = layer.effects_mut() as *mut Vec<EffectInstance>;
            let groups = layer.effect_groups_mut() as *mut Vec<EffectGroup>;
            Some(f(unsafe { &mut *effects }, unsafe { &mut *groups }))
        }
        EffectTarget::Master => {
            let settings = &mut project.settings;
            let effects = &mut settings.master_effects as *mut Vec<EffectInstance>;
            let groups = settings.master_effect_groups_mut() as *mut Vec<EffectGroup>;
            Some(f(unsafe { &mut *effects }, unsafe { &mut *groups }))
        }
    }
}

/// Execute a closure with read-only access to a target's effects.
pub fn with_effects<F, R>(
    project: &Project,
    target: &EffectTarget,
    f: F,
) -> Option<R>
where
    F: FnOnce(&[EffectInstance], &[EffectGroup]) -> R,
{
    match target {
        EffectTarget::Layer { layer_id } => {
            let (_, layer) = project.timeline.find_layer_by_id(layer_id)?;
            let effects = layer.effects.as_deref().unwrap_or(&[]);
            let groups = layer.effect_groups.as_deref().unwrap_or(&[]);
            Some(f(effects, groups))
        }
        EffectTarget::Master => {
            let groups = project.settings.master_effect_groups.as_deref().unwrap_or(&[]);
            Some(f(&project.settings.master_effects, groups))
        }
    }
}

/// Routes driver commands to effect drivers vs generator param drivers.
#[derive(Debug, Clone)]
pub enum DriverTarget {
    Effect {
        effect_target: EffectTarget,
        effect_index: usize,
    },
    GeneratorParam {
        layer_id: LayerId,
    },
}
