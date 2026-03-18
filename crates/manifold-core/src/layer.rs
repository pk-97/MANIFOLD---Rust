use serde::{Deserialize, Serialize};
use crate::types::{BlendMode, ClipDurationMode, GeneratorType, LayerType};
use crate::clip::TimelineClip;
use crate::color::Color;
use crate::effects::{EffectInstance, EffectGroup, ParamEnvelope, ParameterDriver};
use crate::generator::GeneratorParamState;

/// A single layer in the timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layer {
    #[serde(default)]
    pub layer_id: String,
    #[serde(default)]
    pub index: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub layer_type: LayerType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_layer_id: Option<String>,
    #[serde(default)]
    pub is_collapsed: bool,

    #[serde(default)]
    pub clips: Vec<TimelineClip>,

    #[serde(default)]
    pub is_solo: bool,
    #[serde(default)]
    pub is_muted: bool,
    #[serde(default)]
    pub default_blend_mode: BlendMode,
    #[serde(default)]
    pub layer_color: Color,

    // ── Effects ──
    #[serde(default = "default_one")]
    pub opacity: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<Vec<EffectInstance>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_groups: Option<Vec<EffectGroup>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelopes: Option<Vec<ParamEnvelope>>,

    // ── Generator params (V1.1.0+, nested) ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gen_params: Option<GeneratorParamState>,

    // ── Video/MIDI assignment ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_folder_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_video_folder_path: Option<String>,
    #[serde(default = "default_neg_one")]
    pub midi_note: i32,
    #[serde(default = "default_neg_one")]
    pub midi_channel: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_mode: Option<ClipDurationMode>,
    #[serde(default)]
    pub source_clip_ids: Vec<String>,

    // ── Legacy flat generator fields (V1.0.0 format) ──
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "generatorType")]
    pub legacy_generator_type: Option<GeneratorType>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genParamValues")]
    pub legacy_gen_param_values: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genDrivers")]
    pub legacy_gen_drivers: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genParamVersion")]
    pub legacy_gen_param_version: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genAnimSpeed")]
    pub legacy_gen_anim_speed: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genAnimateEdges")]
    pub legacy_gen_animate_edges: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genLineThickness")]
    pub legacy_gen_line_thickness: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genProjDistance")]
    pub legacy_gen_proj_distance: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genRotSpeedXY")]
    pub legacy_gen_rot_speed_xy: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genRotSpeedZW")]
    pub legacy_gen_rot_speed_zw: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genRotSpeedXW")]
    pub legacy_gen_rot_speed_xw: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genShowVertices")]
    pub legacy_gen_show_vertices: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genVertexSize")]
    pub legacy_gen_vertex_size: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genWindowSize")]
    pub legacy_gen_window_size: Option<f32>,

    // ── Runtime caches (not serialized) ──
    #[serde(skip)]
    clips_sorted: bool,
    /// Indices into `clips`, sorted by end_beat. From Unity Layer.cs clipsByEndBeat.
    #[serde(skip)]
    clips_by_end_indices: Vec<usize>,
    #[serde(skip)]
    clips_by_end_sorted: bool,
}

impl Layer {
    pub fn new(name: String, layer_type: LayerType, index: i32) -> Self {
        Self {
            layer_id: crate::short_id(),
            index,
            name,
            layer_type,
            layer_color: Self::generate_layer_color(index as usize),
            clips: Vec::new(),
            ..Default::default()
        }
    }

    /// Generates a distinct color for layer visualization based on index.
    /// Uses golden ratio hue distribution for maximum visual separation.
    /// From Unity Layer.cs line 586-590.
    pub fn generate_layer_color(index: usize) -> crate::color::Color {
        let hue = (index as f32 * 0.618033988749895) % 1.0;
        crate::color::Color::hsv_to_rgb(hue, 0.6, 0.8)
    }

    pub fn is_group(&self) -> bool {
        self.layer_type == LayerType::Group
    }

    /// Get the generator type for this layer (from genParams or legacy field).
    pub fn generator_type(&self) -> GeneratorType {
        if let Some(gp) = &self.gen_params {
            gp.generator_type
        } else {
            self.legacy_generator_type.unwrap_or(GeneratorType::None)
        }
    }

    /// Ensure both clip ordering caches are up-to-date.
    /// From Unity Layer.cs EnsureClipOrderingCaches (lines 457-473).
    pub fn ensure_clip_ordering_caches(&mut self) {
        if !self.clips_sorted {
            self.clips.sort_by(Self::compare_by_start_beat);
            self.clips_sorted = true;
        }

        if !self.clips_by_end_sorted || self.clips_by_end_indices.len() != self.clips.len() {
            self.clips_by_end_indices = (0..self.clips.len()).collect();
            let clips = &self.clips;
            self.clips_by_end_indices.sort_by(|&a, &b| {
                Self::compare_by_end_beat_ref(&clips[a], &clips[b])
            });
            self.clips_by_end_sorted = true;
        }
    }

    pub fn mark_clips_unsorted(&mut self) {
        self.clips_sorted = false;
        self.clips_by_end_sorted = false;
    }

    /// Eagerly sort clip caches. Call after mutations so queries can be &self.
    pub fn ensure_sorted(&mut self) {
        self.ensure_clip_ordering_caches();
    }

    /// Collect clips active at a given beat using dual sorted indexes.
    /// From Unity Layer.cs CollectActiveClipsAtBeat (lines 388-431).
    /// Uses the smaller of two candidate sets (started-by-beat vs ending-after-beat)
    /// to minimize per-frame work.
    ///
    /// IMPORTANT: Caches must be up-to-date before calling. Either call
    /// `ensure_sorted()` first, or use `collect_active_clips_at_beat_mut()`.
    pub fn collect_active_clips_at_beat(&self, beat: f32, results: &mut Vec<usize>) {
        if self.clips.is_empty() {
            return;
        }
        // Caches must already be sorted (caller's responsibility via ensure_sorted)

        // Count of clips with start_beat <= beat (sorted by start)
        let started_count = Self::upper_bound_start_beat(&self.clips, beat);
        if started_count == 0 {
            return;
        }

        // Index into clips_by_end_indices where end_beat > beat starts
        let end_idx = Self::lower_bound_end_beat(&self.clips, &self.clips_by_end_indices, beat);
        let ending_after_count = self.clips_by_end_indices.len() - end_idx;

        // Iterate the smaller candidate set
        if started_count <= ending_after_count {
            // Scan the start-sorted prefix: clips 0..started_count where start_beat <= beat
            for i in 0..started_count {
                let clip = &self.clips[i];
                if clip.is_muted {
                    continue;
                }
                if beat < clip.end_beat() {
                    results.push(i);
                }
            }
        } else {
            // Scan the end-sorted suffix: clips where end_beat > beat
            // Collect into scratch, sort by start_beat for deterministic ordering
            let mut scratch: Vec<usize> = Vec::new();
            for i in end_idx..self.clips_by_end_indices.len() {
                let ci = self.clips_by_end_indices[i];
                let clip = &self.clips[ci];
                if clip.is_muted {
                    continue;
                }
                if clip.start_beat <= beat {
                    scratch.push(ci);
                }
            }
            // Preserve deterministic per-layer ordering (sort by clip index in start-sorted order)
            scratch.sort_unstable();
            results.extend(scratch);
        }
    }

    /// Binary search: count of clips with start_beat <= beat.
    /// From Unity Layer.cs UpperBoundStartBeat (lines 475-489).
    fn upper_bound_start_beat(clips: &[TimelineClip], beat: f32) -> usize {
        let mut lo = 0;
        let mut hi = clips.len();
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            if clips[mid].start_beat <= beat {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// Binary search: first index in clips_by_end_indices where end_beat > beat.
    /// From Unity Layer.cs LowerBoundEndBeat (lines 491-505).
    fn lower_bound_end_beat(clips: &[TimelineClip], indices: &[usize], beat: f32) -> usize {
        let mut lo = 0;
        let mut hi = indices.len();
        while lo < hi {
            let mid = lo + ((hi - lo) >> 1);
            if clips[indices[mid]].end_beat() <= beat {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// Compare by start_beat, tiebreak by end_beat.
    /// From Unity Layer.cs CompareByStartBeat (lines 507-515).
    fn compare_by_start_beat(a: &TimelineClip, b: &TimelineClip) -> std::cmp::Ordering {
        a.start_beat.partial_cmp(&b.start_beat)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.end_beat().partial_cmp(&b.end_beat()).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// Compare by end_beat, tiebreak by start_beat.
    /// From Unity Layer.cs CompareByEndBeat (lines 517-525).
    fn compare_by_end_beat_ref(a: &TimelineClip, b: &TimelineClip) -> std::cmp::Ordering {
        a.end_beat().partial_cmp(&b.end_beat())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.start_beat.partial_cmp(&b.start_beat).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// Convenience: ensure caches + collect active clips in one &mut self call.
    /// Use when you don't need to split the borrow.
    pub fn collect_active_clips_at_beat_mut(&mut self, beat: f32, results: &mut Vec<usize>) {
        self.ensure_clip_ordering_caches();
        self.collect_active_clips_at_beat(beat, results);
    }

    pub fn add_clip(&mut self, clip: TimelineClip) {
        self.clips.push(clip);
        self.mark_clips_unsorted();
    }

    pub fn remove_clip(&mut self, clip_id: &str) -> Option<TimelineClip> {
        if let Some(idx) = self.clips.iter().position(|c| c.id == clip_id) {
            let clip = self.clips.remove(idx);
            self.mark_clips_unsorted();
            Some(clip)
        } else {
            None
        }
    }

    pub fn find_clip(&self, clip_id: &str) -> Option<&TimelineClip> {
        self.clips.iter().find(|c| c.id == clip_id)
    }

    pub fn find_clip_mut(&mut self, clip_id: &str) -> Option<&mut TimelineClip> {
        self.clips.iter_mut().find(|c| c.id == clip_id)
    }

    /// Find clip index by ID.
    pub fn find_clip_index(&self, clip_id: &str) -> Option<usize> {
        self.clips.iter().position(|c| c.id == clip_id)
    }

    /// Insert a clip at a specific index.
    pub fn insert_clip_at(&mut self, index: usize, clip: TimelineClip) {
        let idx = index.min(self.clips.len());
        self.clips.insert(idx, clip);
        self.mark_clips_unsorted();
    }

    /// Get the effects list, creating it if None.
    pub fn effects_mut(&mut self) -> &mut Vec<EffectInstance> {
        if self.effects.is_none() {
            self.effects = Some(Vec::new());
        }
        self.effects.as_mut().unwrap()
    }

    /// Get the effect groups list, creating it if None.
    pub fn effect_groups_mut(&mut self) -> &mut Vec<EffectGroup> {
        if self.effect_groups.is_none() {
            self.effect_groups = Some(Vec::new());
        }
        self.effect_groups.as_mut().unwrap()
    }

    /// Get the envelopes list, creating it if None.
    pub fn envelopes_mut(&mut self) -> &mut Vec<ParamEnvelope> {
        if self.envelopes.is_none() {
            self.envelopes = Some(Vec::new());
        }
        self.envelopes.as_mut().unwrap()
    }

    /// Read a generator param value at index (returns 0.0 if out of range or no gen_params).
    pub fn get_gen_param(&self, index: usize) -> f32 {
        self.gen_params
            .as_ref()
            .and_then(|gp| gp.param_values.get(index).copied())
            .unwrap_or(0.0)
    }

    /// Snapshot current generator param values.
    pub fn snapshot_gen_params(&self) -> Vec<f32> {
        self.gen_params.as_ref().map_or_else(Vec::new, |gp| gp.param_values.clone())
    }

    /// Snapshot current generator drivers.
    pub fn snapshot_gen_drivers(&self) -> Option<Vec<ParameterDriver>> {
        self.gen_params.as_ref().and_then(|gp| gp.drivers.clone())
    }

    /// Snapshot current generator envelopes.
    pub fn snapshot_gen_envelopes(&self) -> Option<Vec<ParamEnvelope>> {
        self.gen_params.as_ref().and_then(|gp| gp.envelopes.clone())
    }

    /// Change generator type, clearing params/drivers/envelopes.
    pub fn change_generator_type(&mut self, new_type: GeneratorType) {
        let gp = self.gen_params.get_or_insert_with(GeneratorParamState::default);
        gp.change_type(new_type);
    }

    /// Restore generator state from snapshot.
    pub fn restore_generator_state(
        &mut self,
        old_type: GeneratorType,
        params: Vec<f32>,
        drivers: Option<Vec<ParameterDriver>>,
        envelopes: Option<Vec<ParamEnvelope>>,
    ) {
        let gp = self.gen_params.get_or_insert_with(GeneratorParamState::default);
        gp.generator_type = old_type;
        gp.param_values = params.clone();
        gp.base_param_values = Some(params);
        gp.drivers = drivers;
        gp.envelopes = envelopes;
    }

    /// Set opacity with clamp. Unity Layer.cs line 140.
    pub fn set_opacity(&mut self, v: f32) {
        self.opacity = v.clamp(0.0, 1.0);
    }

    /// Set MIDI note. Unity Layer.cs lines 264-265.
    pub fn set_midi_note(&mut self, v: i32) {
        self.midi_note = if v < 0 { -1 } else { v.clamp(0, 127) };
    }

    /// Set MIDI channel. Unity Layer.cs line 271.
    pub fn set_midi_channel(&mut self, v: i32) {
        self.midi_channel = if v < 0 { -1 } else { v.clamp(0, 15) };
    }

    /// Clear all clips. Unity Layer.cs line 445.
    pub fn clear_clips(&mut self) {
        self.clips.clear();
        self.mark_clips_unsorted();
    }

    /// Get duration in beats (max end_beat across all clips). Unity Layer.cs line 530.
    pub fn get_duration_beats(&self) -> f32 {
        self.clips.iter().map(|c| c.end_beat()).fold(0.0f32, f32::max)
    }

    /// Set a generator param base value at index.
    pub fn set_gen_param_base(&mut self, index: usize, value: f32) {
        if let Some(gp) = &mut self.gen_params {
            gp.ensure_base_values();
            if let Some(base) = &mut gp.base_param_values {
                while base.len() <= index {
                    base.push(0.0);
                }
                base[index] = value;
            }
            while gp.param_values.len() <= index {
                gp.param_values.push(0.0);
            }
            gp.param_values[index] = value;
        }
    }
}

impl crate::effects::EffectContainer for Layer {
    fn effects(&self) -> &[crate::effects::EffectInstance] {
        self.effects.as_deref().unwrap_or(&[])
    }
    fn effects_mut(&mut self) -> &mut Vec<crate::effects::EffectInstance> {
        Layer::effects_mut(self)
    }
    fn effect_groups(&self) -> &[crate::effects::EffectGroup] {
        self.effect_groups.as_deref().unwrap_or(&[])
    }
    fn effect_groups_mut(&mut self) -> &mut Vec<crate::effects::EffectGroup> {
        Layer::effect_groups_mut(self)
    }
    fn has_modular_effects(&self) -> bool {
        self.effects.as_ref().is_some_and(|e| !e.is_empty())
    }
    fn find_effect(&self, effect_type: crate::types::EffectType) -> Option<&crate::effects::EffectInstance> {
        self.effects.as_ref()?.iter().find(|e| e.effect_type == effect_type)
    }
    fn find_effect_group(&self, group_id: &str) -> Option<&crate::effects::EffectGroup> {
        self.effect_groups.as_ref()?.iter().find(|g| g.id == group_id)
    }
    fn envelopes(&self) -> &[crate::effects::ParamEnvelope] {
        self.envelopes.as_deref().unwrap_or(&[])
    }
    fn envelopes_mut(&mut self) -> &mut Vec<crate::effects::ParamEnvelope> {
        Layer::envelopes_mut(self)
    }
    fn has_envelopes(&self) -> bool {
        self.envelopes.as_ref().is_some_and(|e| !e.is_empty())
    }
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            layer_id: String::new(),
            index: 0,
            name: String::new(),
            layer_type: LayerType::Video,
            parent_layer_id: None,
            is_collapsed: false,
            clips: Vec::new(),
            is_solo: false,
            is_muted: false,
            default_blend_mode: BlendMode::Normal,
            layer_color: Color::WHITE,
            opacity: 1.0,
            effects: None,
            effect_groups: None,
            envelopes: None,
            gen_params: None,
            video_folder_path: None,
            relative_video_folder_path: None,
            midi_note: -1,
            midi_channel: -1,
            duration_mode: None,
            source_clip_ids: Vec::new(),
            legacy_generator_type: None,
            legacy_gen_param_values: None,
            legacy_gen_drivers: None,
            legacy_gen_param_version: None,
            legacy_gen_anim_speed: None,
            legacy_gen_animate_edges: None,
            legacy_gen_line_thickness: None,
            legacy_gen_proj_distance: None,
            legacy_gen_rot_speed_xy: None,
            legacy_gen_rot_speed_zw: None,
            legacy_gen_rot_speed_xw: None,
            legacy_gen_show_vertices: None,
            legacy_gen_vertex_size: None,
            legacy_gen_window_size: None,
            clips_sorted: false,
            clips_by_end_indices: Vec::new(),
            clips_by_end_sorted: false,
        }
    }
}

fn default_one() -> f32 { 1.0 }
fn default_neg_one() -> i32 { -1 }
