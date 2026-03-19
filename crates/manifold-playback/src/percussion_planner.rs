// Port of Unity PercussionTimelinePlanner.cs (150 lines).
// Converts parsed seconds-domain percussion events into beat-domain timeline placements.
// Pure planning logic — does not mutate timeline/project state.

use std::collections::HashMap;

use manifold_core::percussion_analysis::{
    BeatTimeConverter, PercussionAnalysisData, PercussionClipPlacement, PercussionImportOptions,
    PercussionPlacementPlan, PercussionTriggerType,
};
use manifold_core::percussion_binding::PercussionBindingResolver;

/// Port of Unity PercussionTimelinePlanner.
/// Converts parsed seconds-domain percussion events into beat-domain timeline placements.
/// Pure planning logic; does not mutate timeline/project state.
pub struct PercussionTimelinePlanner<'a> {
    beat_time_converter: Box<dyn BeatTimeConverter + 'a>,
    binding_resolver: Box<dyn PercussionBindingResolver + 'a>,
}

impl<'a> PercussionTimelinePlanner<'a> {
    pub fn new(
        beat_time_converter: Box<dyn BeatTimeConverter + 'a>,
        binding_resolver: Box<dyn PercussionBindingResolver + 'a>,
    ) -> Self {
        Self {
            beat_time_converter,
            binding_resolver,
        }
    }

    pub fn build_plan(
        &mut self,
        analysis: Option<&PercussionAnalysisData>,
        options: Option<&PercussionImportOptions>,
    ) -> PercussionPlacementPlan {
        let mut plan = PercussionPlacementPlan::new();

        let analysis = match analysis {
            Some(a) => a,
            None => return plan,
        };
        let options = match options {
            Some(o) => o,
            None => return plan,
        };

        let events = &analysis.events;
        plan.total_events = events.len() as i32;

        let mut accepted_placements: Vec<PercussionClipPlacement> = Vec::with_capacity(events.len());
        let mut placement_index_by_quantized_slot: HashMap<i64, usize> = HashMap::new();

        let quantizing = options.quantize_to_grid && options.quantize_step_beats > 0.0;
        let quantize_step = if quantizing { options.quantize_step_beats } else { 0.0 };
        let energy_gate = options.minimum_energy_gate;
        let energy_gate_enabled = energy_gate > 0.0 && analysis.has_energy_envelope();

        for percussion_event in events.iter() {
            if percussion_event.trigger_type == PercussionTriggerType::Unknown {
                plan.skipped_unknown_type += 1;
                continue;
            }

            let binding = match self.binding_resolver.try_resolve(percussion_event) {
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

            let compensated_time = percussion_event.time_seconds + options.onset_compensation_seconds;
            let mapped_beat = match analysis.try_map_seconds_to_beat(
                compensated_time,
                Some(self.beat_time_converter.as_mut()),
            ) {
                Some(b) => b,
                None => {
                    plan.skipped_invalid_timing += 1;
                    continue;
                }
            };

            let mut source_beat = mapped_beat + options.start_beat_offset;
            source_beat = source_beat.max(0.0);

            if energy_gate_enabled {
                let energy_at_beat = analysis.energy_at_beat(source_beat);
                if energy_at_beat < energy_gate {
                    plan.skipped_low_energy += 1;
                    continue;
                }
            }

            let mut placement_beat = source_beat;
            if quantizing {
                placement_beat = (source_beat / quantize_step).round() * quantize_step;
            }
            placement_beat = placement_beat.max(0.0);

            let spacing_key = ((binding.trigger_type as i32).wrapping_mul(397)) ^ binding.layer_index;

            // Duration priority: per-event (from model) > binding (from SO) > default.
            let duration_beats: f32;
            if percussion_event.has_duration() && analysis.bpm > 0.0 {
                let seconds_per_beat = 60.0 / analysis.bpm;
                let raw = percussion_event.duration_seconds / seconds_per_beat;
                duration_beats = raw.clamp(0.0625, 32.0);
            } else {
                duration_beats = if binding.duration_beats > 0.0 {
                    binding.duration_beats
                } else {
                    options.default_clip_duration_beats
                };
            }

            let placement = PercussionClipPlacement::new(
                binding.trigger_type,
                binding.layer_index,
                binding.video_clip_id.clone(),
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
