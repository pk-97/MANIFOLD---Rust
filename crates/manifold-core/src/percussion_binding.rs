// Port of Unity PercussionBindingResolver.cs (100 lines).
// Resolves trigger bindings against project library/layer state.

use std::collections::HashMap;

use crate::percussion_analysis::{
    PercussionClipBinding, PercussionEvent, PercussionImportOptions, PercussionTriggerType,
};
use crate::project::Project;

// ─── IPercussionBindingResolver trait ───

/// Port of Unity IPercussionBindingResolver interface.
pub trait PercussionBindingResolver {
    fn try_resolve(&self, percussion_event: &PercussionEvent) -> Option<ResolvedBinding>;
}

/// A binding plus the library clip it resolved to (`None` for generator bindings).
#[derive(Debug, Clone)]
pub struct ResolvedBinding {
    pub binding: PercussionClipBinding,
    pub video_clip_id: Option<String>,
}

// ─── ProjectPercussionBindingResolver ───

/// Port of Unity ProjectPercussionBindingResolver class.
/// Resolves trigger bindings against project library/layer state.
/// A video binding takes the first clip on its layer, else the first library clip.
/// Clip IDs are pre-resolved at construction time (immutable borrow of Project
/// is scoped to `new()` only) so the resolver itself carries no lifetime parameter.
pub struct ProjectPercussionBindingResolver {
    /// Bindings with clip IDs already resolved against the project library.
    resolved_bindings: HashMap<PercussionTriggerType, ResolvedBinding>,
}

impl ProjectPercussionBindingResolver {
    pub fn new(project: &Project, options: &PercussionImportOptions) -> Self {
        let mut resolved_bindings = HashMap::new();
        for binding in &options.bindings {
            let resolved = Self::resolve_binding(project, binding);
            resolved_bindings.insert(binding.trigger_type, resolved);
        }
        Self { resolved_bindings }
    }

    fn resolve_binding(project: &Project, binding: &PercussionClipBinding) -> ResolvedBinding {
        let video_clip_id = if binding.uses_generator() {
            None
        } else {
            Some(Self::resolve_clip_id(project, binding.layer_index)).filter(|id| !id.is_empty())
        };
        ResolvedBinding {
            binding: binding.clone(),
            video_clip_id,
        }
    }

    /// Port of Unity ProjectPercussionBindingResolver.ResolveFallbackClipId().
    fn resolve_clip_id(project: &Project, preferred_layer_index: i32) -> String {
        let layers = &project.timeline.layers;
        let idx = preferred_layer_index as usize;

        // 1) Prefer an existing clip already used on the target layer.
        if idx < layers.len() {
            let layer = &layers[idx];
            if layer.layer_type != crate::types::LayerType::Generator {
                for clip in &layer.clips {
                    if !clip.video_clip_id.is_empty()
                        && project.video_library.has_clip(&clip.video_clip_id)
                    {
                        return clip.video_clip_id.clone();
                    }
                }
            }

            // 2) Fall back to scanned source clips configured on the layer.
            for candidate in &layer.source_clip_ids {
                if project.video_library.has_clip(candidate) {
                    return candidate.clone();
                }
            }
        }

        // 3) Final fallback: first clip in project library.
        if let Some(clip) = project.video_library.clips.first() {
            return clip.id.clone();
        }

        String::new()
    }
}

impl PercussionBindingResolver for ProjectPercussionBindingResolver {
    fn try_resolve(&self, percussion_event: &PercussionEvent) -> Option<ResolvedBinding> {
        let resolved = self.resolved_bindings.get(&percussion_event.trigger_type)?;
        if !resolved.binding.uses_generator() && resolved.video_clip_id.is_none() {
            return None;
        }
        Some(resolved.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::percussion_analysis::ClipDetectionAnchor;
    use crate::percussion_settings::PercussionImportOptionsFactory;
    use crate::preset_type_id::PresetTypeId;
    use crate::types::LayerType;
    use crate::units::{Beats, Bpm, Seconds};
    use crate::video::VideoClip;

    fn clip(id: &str) -> VideoClip {
        serde_json::from_str(&format!(r#"{{"id":"{id}"}}"#)).unwrap()
    }

    #[test]
    fn video_binding_resolves_to_first_clip_on_its_layer() {
        let mut project = Project::default();
        let layer_idx = project
            .timeline
            .add_layer("Kick", LayerType::Video, PresetTypeId::NONE);
        project.video_library.add_clip(clip("lib_first"));
        project.video_library.add_clip(clip("on_layer"));
        project.timeline.layers[layer_idx]
            .source_clip_ids
            .push("on_layer".to_string());

        let anchor =
            ClipDetectionAnchor::new(Beats::ZERO, Beats(4.0), Seconds::ZERO, 120.0, Bpm(120.0));
        let mut options = PercussionImportOptionsFactory::create_default(&project, anchor);
        options.bindings = vec![PercussionClipBinding::new(
            PercussionTriggerType::Kick,
            layer_idx as i32,
            PresetTypeId::NONE,
            Beats(0.25),
            0.0,
        )];
        let resolver = ProjectPercussionBindingResolver::new(&project, &options);
        let event = PercussionEvent::new(PercussionTriggerType::Kick, 0.5, 0.9);
        let resolved = resolver.try_resolve(&event).expect("resolves");
        assert_eq!(resolved.video_clip_id.as_deref(), Some("on_layer"));
    }
}
