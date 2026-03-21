use manifold_core::ClipId;
use manifold_core::effects::{EffectInstance, EffectGroup};
use manifold_core::project::Project;

/// Routes effect commands to clip/layer/master effect lists.
/// Replaces C#'s `IList<EffectInstance>` parameter pattern.
#[derive(Debug, Clone)]
pub enum EffectTarget {
    Clip { clip_id: ClipId },
    Layer { layer_index: usize },
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
        EffectTarget::Clip { clip_id } => {
            let clip = project.timeline.find_clip_by_id_mut(clip_id)?;
            let groups = clip.effect_groups.get_or_insert_with(Vec::new);
            // effects and groups are different fields of the same struct — no alias.
            // We split the borrow manually.
            let effects = &mut clip.effects as *mut Vec<EffectInstance>;
            let groups = groups as *mut Vec<EffectGroup>;
            // SAFETY: effects and groups are non-overlapping fields of TimelineClip.
            Some(f(unsafe { &mut *effects }, unsafe { &mut *groups }))
        }
        EffectTarget::Layer { layer_index } => {
            let layer = project.timeline.layers.get_mut(*layer_index)?;
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
        EffectTarget::Clip { clip_id } => {
            // Linear search for immutable access (no self-healing cache)
            for layer in &project.timeline.layers {
                if let Some(clip) = layer.find_clip(clip_id) {
                    let groups = clip.effect_groups.as_deref().unwrap_or(&[]);
                    return Some(f(&clip.effects, groups));
                }
            }
            None
        }
        EffectTarget::Layer { layer_index } => {
            let layer = project.timeline.layers.get(*layer_index)?;
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
        layer_index: usize,
    },
}
