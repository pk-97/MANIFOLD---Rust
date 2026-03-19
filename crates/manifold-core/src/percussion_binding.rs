// Port of Unity PercussionBindingResolver.cs (100 lines).
// Resolves trigger bindings against project library/layer state.

use std::collections::HashMap;

use crate::percussion_analysis::{PercussionClipBinding, PercussionEvent, PercussionImportOptions, PercussionTriggerType};
use crate::project::Project;

// ─── IPercussionBindingResolver trait ───

/// Port of Unity IPercussionBindingResolver interface.
pub trait PercussionBindingResolver {
    fn try_resolve(&self, percussion_event: &PercussionEvent) -> Option<PercussionClipBinding>;
}

// ─── ProjectPercussionBindingResolver ───

/// Port of Unity ProjectPercussionBindingResolver class.
/// Resolves trigger bindings against project library/layer state.
/// If a binding has no explicit clip ID, it falls back to layer/library defaults.
/// Clip IDs are pre-resolved at construction time (immutable borrow of Project
/// is scoped to `new()` only) so the resolver itself carries no lifetime parameter.
pub struct ProjectPercussionBindingResolver {
    /// Bindings with clip IDs already resolved against the project library.
    resolved_bindings: HashMap<PercussionTriggerType, PercussionClipBinding>,
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

    fn resolve_binding(project: &Project, binding: &PercussionClipBinding) -> PercussionClipBinding {
        if binding.uses_generator() {
            return binding.clone();
        }

        let mut clip_id = binding.video_clip_id.clone().unwrap_or_default();
        if clip_id.is_empty() || !project.video_library.has_clip(&clip_id) {
            clip_id = Self::resolve_fallback_clip_id(project, binding.layer_index);
        }

        if clip_id.is_empty() {
            binding.clone()
        } else {
            binding.with_video_clip_id(&clip_id)
        }
    }

    /// Port of Unity ProjectPercussionBindingResolver.ResolveFallbackClipId().
    fn resolve_fallback_clip_id(project: &Project, preferred_layer_index: i32) -> String {
        let layers = &project.timeline.layers;
        let idx = preferred_layer_index as usize;

        // 1) Prefer an existing clip already used on the target layer.
        if idx < layers.len() {
            let layer = &layers[idx];
            for clip in &layer.clips {
                if clip.is_generator() {
                    continue;
                }
                if !clip.video_clip_id.is_empty()
                    && project.video_library.has_clip(&clip.video_clip_id)
                {
                    return clip.video_clip_id.clone();
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
    fn try_resolve(&self, percussion_event: &PercussionEvent) -> Option<PercussionClipBinding> {
        let binding = self.resolved_bindings.get(&percussion_event.trigger_type)?;

        if binding.uses_generator() {
            return Some(binding.clone());
        }

        let clip_id = binding.video_clip_id.clone().unwrap_or_default();
        if clip_id.is_empty() {
            return None;
        }

        Some(binding.clone())
    }
}
