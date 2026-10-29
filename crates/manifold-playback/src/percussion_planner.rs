// Turns one audio clip's detected hits into beat-domain trigger placements.
// Pure planning logic — does not mutate timeline/project state.

use std::collections::HashMap;

use manifold_core::percussion_analysis::{
    PercussionAnalysisData, PercussionClipPlacement, PercussionImportOptions,
    PercussionPlacementPlan,
};
use manifold_core::percussion_binding::{PercussionBindingResolver, ResolvedBinding};
use manifold_core::units::{Beats, Seconds};

/// Maps each hit through the clip anchor (warp-aware, trimmed to the clip window), gates it by its binding's
/// confidence, quantizes, and keeps the most confident hit per quantized slot.
pub struct PercussionTimelinePlanner<'a> {
    binding_resolver: Box<dyn PercussionBindingResolver + 'a>,
}

impl<'a> PercussionTimelinePlanner<'a> {
    pub fn new(binding_resolver: Box<dyn PercussionBindingResolver + 'a>) -> Self {
        Self { binding_resolver }
    }

    pub fn build_plan(
        &mut self,
        analysis: &PercussionAnalysisData,
        options: &PercussionImportOptions,
    ) -> PercussionPlacementPlan {
        let mut plan = PercussionPlacementPlan::new();
        let events = &analysis.events;
        plan.total_events = events.len() as i32;

        let mut accepted_placements: Vec<PercussionClipPlacement> =
            Vec::with_capacity(events.len());
        let mut placement_index_by_quantized_slot: HashMap<i64, usize> = HashMap::new();

        let quantizing = options.quantize_to_grid && options.quantize_step_beats > Beats::ZERO;
        let quantize_step = if quantizing {
            options.quantize_step_beats
        } else {
            Beats::ZERO
        };

        for percussion_event in events.iter() {
            let ResolvedBinding {
                binding,
                video_clip_id,
            } = match self.binding_resolver.try_resolve(percussion_event) {
                Some(b) => b,
                None => {
                    plan.skipped_unmapped += 1;
                    continue;
                }
            };

            if percussion_event.confidence < binding.minimum_confidence {
                plan.skipped_low_confidence += 1;
                continue;
            }

            let compensated_time =
                Seconds(percussion_event.time_seconds as f64) + options.onset_compensation_seconds;
            let Some(source_beat) = options.clip_anchor.map_source_seconds(compensated_time) else {
                plan.skipped_out_of_clip += 1;
                continue;
            };
            let source_beat = source_beat.max(Beats::ZERO);

            let mut placement_beat = source_beat;
            if quantizing {
                placement_beat = quantize_step * (source_beat / quantize_step).round();
            }
            placement_beat = placement_beat.max(Beats::ZERO);

            let spacing_key =
                ((binding.trigger_type as i32).wrapping_mul(397)) ^ binding.layer_index;

            let duration_beats = if binding.duration_beats.0 > 0.0 {
                binding.duration_beats
            } else {
                options.default_clip_duration_beats
            };

            let placement = PercussionClipPlacement::new(
                binding.trigger_type,
                binding.layer_index,
                video_clip_id,
                binding.generator_type,
                placement_beat,
                duration_beats,
                percussion_event.confidence,
                percussion_event.time_seconds,
            );

            if quantizing {
                let quantized_tick = (placement_beat / quantize_step).round() as i32;
                let slot_key = Self::compose_quantized_slot_key(spacing_key, quantized_tick);
                if let Some(&existing_index) = placement_index_by_quantized_slot.get(&slot_key) {
                    let existing = &accepted_placements[existing_index];
                    if placement.confidence > existing.confidence {
                        accepted_placements[existing_index] = placement;
                    }
                    plan.skipped_by_quantized_dedup += 1;
                    continue;
                }

                placement_index_by_quantized_slot.insert(slot_key, accepted_placements.len());
            }

            accepted_placements.push(placement);
        }

        for placement in accepted_placements {
            plan.add_placement(placement);
        }

        plan.sort_placements();
        plan
    }

    fn compose_quantized_slot_key(spacing_key: i32, quantized_tick: i32) -> i64 {
        ((spacing_key as i64) << 32) | (quantized_tick as u32 as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::percussion_analysis::{
        ClipDetectionAnchor, PercussionClipBinding, PercussionEvent, PercussionTriggerType,
    };
    use manifold_core::preset_type_id::PresetTypeId;
    use manifold_core::units::Bpm;

    struct KickResolver;
    impl PercussionBindingResolver for KickResolver {
        fn try_resolve(&self, event: &PercussionEvent) -> Option<ResolvedBinding> {
            Some(ResolvedBinding {
                binding: PercussionClipBinding::new(
                    event.trigger_type,
                    0,
                    PresetTypeId::from_string("Gen".to_string()),
                    Beats(0.25),
                    0.0,
                ),
                video_clip_id: None,
            })
        }
    }

    #[test]
    fn anchored_plan_places_on_heard_beats_and_trims() {
        // 120-BPM clip on a 128-BPM project, anchored at beat 0, 4 beats long.
        let analysis = PercussionAnalysisData::new(vec![
            // 0.5s into the file = 1 source-beat = timeline beat 1 (inside).
            PercussionEvent::new(PercussionTriggerType::Kick, 0.5, 0.9),
            // Far past the 4-beat clip window: trimmed.
            PercussionEvent::new(PercussionTriggerType::Kick, 50.0, 0.9),
        ]);

        let options = PercussionImportOptions {
            quantize_to_grid: false,
            quantize_step_beats: Beats(0.25),
            default_clip_duration_beats: Beats(0.25),
            onset_compensation_seconds: Seconds::ZERO,
            bindings: vec![PercussionClipBinding::new(
                PercussionTriggerType::Kick,
                0,
                PresetTypeId::from_string("Gen".to_string()),
                Beats(0.25),
                0.0,
            )],
            clip_anchor: ClipDetectionAnchor::new(
                Beats::ZERO,
                Beats(4.0),
                Seconds::ZERO,
                120.0,
                Bpm(128.0),
            ),
        };

        let plan =
            PercussionTimelinePlanner::new(Box::new(KickResolver)).build_plan(&analysis, &options);

        assert_eq!(plan.accepted_events(), 1, "one kick inside the clip window");
        assert_eq!(plan.skipped_out_of_clip, 1, "one kick trimmed past the end");
        assert!((plan.placements()[0].start_beat.0 - 1.0).abs() < 1e-6);
    }
}
