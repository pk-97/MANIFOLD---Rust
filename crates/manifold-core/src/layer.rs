use crate::clip::TimelineClip;
use crate::color::Color;
use crate::preset_type_id::PresetTypeId;
use crate::effects::{EffectGroup, ParamEnvelope, ParameterDriver, PresetInstance};
use crate::id::{ClipId, EffectGroupId, LayerId};
use crate::types::{BlendMode, ClipDurationMode, LayerType, MidiTriggerMode};
use crate::units::{Beats, Seconds};
use ahash::AHashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ─── Overlap enforcement ───────────────────────────────────────────

/// Describes an overlap resolution action performed by `add_clip` or
/// `enforce_non_overlap_for`.  Callers use these to build undo commands.
#[derive(Clone, Debug)]
pub enum OverlapAction {
    /// A clip was fully covered by the placed clip and removed from the layer.
    Deleted(TimelineClip),
    /// A clip was trimmed (start and/or end) to avoid overlap.
    Trimmed {
        clip_id: ClipId,
        old_start_beat: Beats,
        old_duration_beats: Beats,
        old_in_point: Seconds,
    },
    /// A clip was split: its end was trimmed and a tail piece was added after
    /// the placed clip.
    Split {
        clip_id: ClipId,
        old_duration_beats: Beats,
        tail_clip: TimelineClip,
    },
}

/// A single layer in the timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layer {
    #[serde(default)]
    pub layer_id: LayerId,
    #[serde(default)]
    pub index: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub layer_type: LayerType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_layer_id: Option<LayerId>,
    #[serde(default)]
    pub is_collapsed: bool,

    #[serde(default)]
    pub clips: Vec<TimelineClip>,

    #[serde(default)]
    pub is_solo: bool,
    #[serde(default)]
    pub is_muted: bool,
    #[serde(default)]
    pub blit_to_led: bool,
    #[serde(default)]
    pub default_blend_mode: BlendMode,
    #[serde(default)]
    pub layer_color: Color,

    // ── Effects ──
    #[serde(default = "default_one")]
    pub opacity: f32,

    // ── Audio layer (LayerType::Audio) ──
    /// Per-layer audio output gain in **decibels** (0 dB = unity), applied to
    /// this audio layer's playback. The track fader; meaningless on non-audio
    /// layers. See `docs/AUDIO_LAYER_DESIGN.md`.
    #[serde(default, skip_serializing_if = "is_zero_audio_gain")]
    pub audio_gain_db: f32,
    /// Audio output state: when `true`, this layer is **silent to the master mix
    /// but still feeds its send** (the third state beside Live and Muted). Mute
    /// still wins — a muted layer is silent everywhere. Default `false` (Live).
    /// Stem lanes from Detect-and-Group default this `true`. See
    /// `docs/AUDIO_LAYER_DESIGN.md` §5 / `LAYER_CONTROLS_DESIGN.md` §5.3.
    #[serde(default, skip_serializing_if = "is_false")]
    pub analysis_only: bool,
    /// Set ONLY on a Detect-and-Group **group** layer: the source audio lane this
    /// set was built for. Drives lane-keyed reuse — re-detecting any clip on that
    /// lane reuses this group's stem lanes + sends instead of making a second set.
    /// `None` on every other layer. See `docs/AUDIO_CLIP_DETECTION_DESIGN.md` §8.3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detect_group_source: Option<LayerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<Vec<PresetInstance>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_groups: Option<Vec<EffectGroup>>,
    // Effect envelopes moved onto each effect's `PresetInstance.envelopes`
    // (envelope-home unification, v1.6). The old layer-level `envelopes` array
    // is relocated by the v1.5→v1.6 load migration; there is no layer-scoped
    // envelope home anymore.

    // ── Generator params (V1.1.0+, nested) ──
    // A generator is a `PresetInstance { kind: Generator }`. It serializes
    // through the kind-aware `Serialize` (generator JSON shape, byte-identical
    // to the former `GeneratorParamState`), but must DESERIALIZE via the
    // generator decoder — the default `PresetInstance` deserialize reads the
    // effect shape (`effectType`/`id`), which a generator object lacks.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::effects::deserialize_opt_generator_instance"
    )]
    gen_params: Option<PresetInstance>,

    // The generator's per-instance graph override now lives on the generator
    // `PresetInstance` itself (`gen_params.graph`), exactly like an effect's
    // `graph` — the graph-home unification. Read it through
    // [`Self::generator_graph`] / its version accessors; older project files
    // carried a layer-level `generatorGraph` + version fields, which the load
    // migration relocates into `gen_params`.

    // ── Video/MIDI assignment ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_folder_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_video_folder_path: Option<String>,
    #[serde(default = "default_neg_one")]
    pub midi_note: i32,
    #[serde(default = "default_neg_one")]
    pub midi_channel: i32,
    /// Per-layer MIDI device filter (matches device name case-insensitively).
    /// `None` = accept events from any connected device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_device: Option<String>,
    /// Trigger mode for incoming NoteOn events. See `MidiTriggerMode`.
    #[serde(default, skip_serializing_if = "is_default_trigger_mode")]
    pub midi_trigger_mode: MidiTriggerMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_mode: Option<ClipDurationMode>,
    #[serde(default)]
    pub source_clip_ids: Vec<String>,

    // ── Legacy flat generator fields (V1.0.0 format) ──
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "generatorType"
    )]
    pub legacy_generator_type: Option<PresetTypeId>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genParamValues"
    )]
    pub legacy_gen_param_values: Option<Vec<f32>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genDrivers"
    )]
    pub legacy_gen_drivers: Option<serde_json::Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genParamVersion"
    )]
    pub legacy_gen_param_version: Option<i32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genAnimSpeed"
    )]
    pub legacy_gen_anim_speed: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genAnimateEdges"
    )]
    pub legacy_gen_animate_edges: Option<serde_json::Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genLineThickness"
    )]
    pub legacy_gen_line_thickness: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genProjDistance"
    )]
    pub legacy_gen_proj_distance: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genRotSpeedXY"
    )]
    pub legacy_gen_rot_speed_xy: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genRotSpeedZW"
    )]
    pub legacy_gen_rot_speed_zw: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genRotSpeedXW"
    )]
    pub legacy_gen_rot_speed_xw: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genShowVertices"
    )]
    pub legacy_gen_show_vertices: Option<serde_json::Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genVertexSize"
    )]
    pub legacy_gen_vertex_size: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genWindowSize"
    )]
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
            layer_id: LayerId::new(crate::short_id()),
            index,
            name,
            layer_type,
            layer_color: Self::generate_layer_color(index as usize),
            clips: Vec::new(),
            ..Default::default()
        }
    }

    /// Create a new generator layer with fully initialized params.
    pub fn new_generator(name: String, gen_type: PresetTypeId, index: i32) -> Self {
        let mut layer = Self::new(name, LayerType::Generator, index);
        // MUST be `new_generator`, not `new`: the latter stamps
        // `kind: Effect`, which is invisible in memory (`generator_type()`
        // just reads `effect_type`) but routes serialization through the
        // effect path — the saved `genParams` then carries `effectType`
        // instead of `generatorType`. On reload the generator decoder only
        // reads `generatorType`, so the type drops to `NONE` and the layer
        // renders black ("cleared generator"). `new_generator` also seeds
        // default param values via `init_defaults()`.
        layer.gen_params = Some(PresetInstance::new_generator(gen_type));
        layer
    }

    /// Create a new video layer.
    pub fn new_video(name: String, index: i32) -> Self {
        Self::new(name, LayerType::Video, index)
    }

    /// Create a new audio layer.
    pub fn new_audio(name: String, index: i32) -> Self {
        Self::new(name, LayerType::Audio, index)
    }

    #[inline]
    pub fn is_audio(&self) -> bool {
        self.layer_type == LayerType::Audio
    }

    /// Image clips may only be dropped here.
    #[inline]
    pub fn is_video(&self) -> bool {
        self.layer_type == LayerType::Video
    }

    /// The audio clip active on this layer at `beat`, if any. Layers enforce
    /// non-overlap, so there is at most one. Shared by the modulation-curve path
    /// and the playback path so they agree on "which clip is playing."
    pub fn active_audio_clip_at(&self, beat: Beats) -> Option<&TimelineClip> {
        self.clips
            .iter()
            .find(|c| c.is_audio() && c.is_active_at_beat(beat))
    }

    /// Per-layer audio gain as a linear multiplier (0 dB → 1.0).
    #[inline]
    pub fn audio_gain_linear(&self) -> f32 {
        10f32.powf(self.audio_gain_db / 20.0)
    }

    #[inline]
    pub fn gen_params(&self) -> Option<&PresetInstance> {
        self.gen_params.as_ref()
    }

    #[inline]
    pub fn gen_params_mut(&mut self) -> Option<&mut PresetInstance> {
        self.gen_params.as_mut()
    }

    /// Mutable access to generator params, creating a default if None.
    #[inline]
    pub fn gen_params_or_init(&mut self) -> &mut PresetInstance {
        // Inherit the layer's generator type (from a legacy flat field when
        // `gen_params` hasn't been built yet) so a freshly-initialized instance
        // isn't stranded on `PresetTypeId::NONE` — which would lose the type the
        // moment graph editing forces an init (graph-home unification).
        let ty = self.generator_type().clone();
        self.gen_params
            .get_or_insert_with(|| PresetInstance::new_generator(ty))
    }

    /// The generator's per-instance graph override, or `None` when it renders
    /// the catalog default. Lives on `gen_params.graph` (graph-home
    /// unification) — the generator twin of an effect's `PresetInstance.graph`.
    #[inline]
    pub fn generator_graph(&self) -> Option<&crate::effect_graph_def::EffectGraphDef> {
        self.gen_params.as_ref().and_then(|gp| gp.graph.as_ref())
    }

    /// The generator graph's snapshot version (bumped by every edit). `0` when
    /// the layer has no generator params yet. Runtime-only — resets on load.
    #[inline]
    pub fn generator_graph_version(&self) -> u32 {
        self.gen_params.as_ref().map_or(0, |gp| gp.graph_version)
    }

    /// The generator graph's structure version (bumped only on node/wire
    /// add/remove + revert). `0` when the layer has no generator params yet.
    #[inline]
    pub fn generator_graph_structure_version(&self) -> u32 {
        self.gen_params
            .as_ref()
            .map_or(0, |gp| gp.graph_structure_version)
    }

    /// Generates a distinct color for layer visualization based on index.
    /// Uses golden ratio hue distribution for maximum visual separation.
    /// From Unity Layer.cs line 586-590.
    pub fn generate_layer_color(index: usize) -> crate::color::Color {
        let hue = (index as f32 * 0.618_034) % 1.0;
        crate::color::Color::hsv_to_rgb(hue, 0.6, 0.8)
    }

    pub fn is_group(&self) -> bool {
        self.layer_type == LayerType::Group
    }

    /// Get the generator type for this layer (from genParams or legacy field).
    pub fn generator_type(&self) -> &PresetTypeId {
        if let Some(gp) = &self.gen_params {
            gp.generator_type()
        } else {
            self.legacy_generator_type
                .as_ref()
                .unwrap_or(&PresetTypeId::NONE)
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
            self.clips_by_end_indices
                .sort_by(|&a, &b| Self::compare_by_end_beat_ref(&clips[a], &clips[b]));
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
    pub fn collect_active_clips_at_beat(&self, beat: Beats, results: &mut Vec<usize>) {
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
    fn upper_bound_start_beat(clips: &[TimelineClip], beat: Beats) -> usize {
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
    fn lower_bound_end_beat(clips: &[TimelineClip], indices: &[usize], beat: Beats) -> usize {
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
        a.start_beat
            .partial_cmp(&b.start_beat)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.end_beat()
                    .partial_cmp(&b.end_beat())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Compare by end_beat, tiebreak by start_beat.
    /// From Unity Layer.cs CompareByEndBeat (lines 517-525).
    fn compare_by_end_beat_ref(a: &TimelineClip, b: &TimelineClip) -> std::cmp::Ordering {
        a.end_beat()
            .partial_cmp(&b.end_beat())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.start_beat
                    .partial_cmp(&b.start_beat)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }

    /// Convenience: ensure caches + collect active clips in one &mut self call.
    /// Use when you don't need to split the borrow.
    pub fn collect_active_clips_at_beat_mut(&mut self, beat: Beats, results: &mut Vec<usize>) {
        self.ensure_clip_ordering_caches();
        self.collect_active_clips_at_beat(beat, results);
    }

    /// Add a clip with DaVinci-style overlap enforcement.
    /// Trims or deletes existing clips that collide with the new clip.
    /// Returns the actions taken so callers can build undo commands.
    /// `spb` = seconds per beat (60.0 / bpm), used for video in_point trimming.
    pub fn add_clip(&mut self, mut clip: TimelineClip, spb: f32) -> Vec<OverlapAction> {
        clip.layer_id = self.layer_id.clone();
        let clip_id = clip.id.clone();
        self.clips.push(clip);
        let actions = self.enforce_non_overlap_for(&clip_id, &HashSet::new(), spb);
        self.mark_clips_unsorted();
        actions
    }

    /// Raw clip insertion — no overlap enforcement.
    /// **Only for undo/restore paths** that reinstate a known-good prior state.
    pub fn restore_clip(&mut self, mut clip: TimelineClip) {
        clip.layer_id = self.layer_id.clone();
        self.clips.push(clip);
        self.mark_clips_unsorted();
    }

    /// Enforce non-overlap for a clip that is **already on this layer**.
    /// Used after position/duration changes (e.g. drag).
    /// Returns the actions taken so callers can build undo commands.
    pub fn enforce_non_overlap_for(
        &mut self,
        clip_id: &ClipId,
        ignore_ids: &HashSet<ClipId>,
        spb: f32,
    ) -> Vec<OverlapAction> {
        let mut actions = Vec::new();

        // Get placed clip bounds.
        let (placed_start, placed_end) = match self.clips.iter().find(|c| &c.id == clip_id) {
            Some(c) => (c.start_beat, c.end_beat()),
            None => return actions,
        };
        let placed_id = clip_id.clone();

        let mut to_delete: Vec<ClipId> = Vec::new();
        let mut tails: Vec<TimelineClip> = Vec::new();

        for clip in &mut self.clips {
            if clip.id == placed_id || ignore_ids.contains(&clip.id) {
                continue;
            }

            let clip_start = clip.start_beat;
            let clip_end = clip.end_beat();

            // No overlap
            if clip_end <= placed_start || clip_start >= placed_end {
                continue;
            }

            // Case 1: fully covered → delete
            if placed_start <= clip_start && placed_end >= clip_end {
                to_delete.push(clip.id.clone());
                actions.push(OverlapAction::Deleted(clip.clone()));
                continue;
            }

            // Case 2: covers start → trim start of existing
            if placed_start <= clip_start && placed_end < clip_end {
                let trim_beats = placed_end - clip_start;
                let trim_seconds = Seconds(trim_beats.0 * spb as f64);
                actions.push(OverlapAction::Trimmed {
                    clip_id: clip.id.clone(),
                    old_start_beat: clip.start_beat,
                    old_duration_beats: clip.duration_beats,
                    old_in_point: clip.in_point,
                });
                clip.in_point += trim_seconds;
                clip.start_beat = placed_end;
                clip.duration_beats -= trim_beats;
                continue;
            }

            // Case 3: covers end → trim end of existing
            if placed_start > clip_start && placed_end >= clip_end {
                actions.push(OverlapAction::Trimmed {
                    clip_id: clip.id.clone(),
                    old_start_beat: clip.start_beat,
                    old_duration_beats: clip.duration_beats,
                    old_in_point: clip.in_point,
                });
                clip.duration_beats = placed_start - clip_start;
                continue;
            }

            // Case 4: placed inside existing → split
            if placed_start > clip_start && placed_end < clip_end {
                let beats_elapsed = placed_end - clip_start;
                let tail_in_point = clip.in_point + Seconds(beats_elapsed.0 * spb as f64);

                let mut tail = clip.clone_with_new_id();
                tail.start_beat = placed_end;
                tail.duration_beats = clip_end - placed_end;
                tail.in_point = tail_in_point;
                tail.layer_id = self.layer_id.clone();

                let old_duration = clip.duration_beats;
                clip.duration_beats = placed_start - clip_start;

                actions.push(OverlapAction::Split {
                    clip_id: clip.id.clone(),
                    old_duration_beats: old_duration,
                    tail_clip: tail.clone(),
                });

                tails.push(tail);
            }
        }

        // Remove fully-covered clips.
        if !to_delete.is_empty() {
            self.clips.retain(|c| !to_delete.contains(&c.id));
        }

        // Add tail clips from splits.
        for tail in tails {
            self.clips.push(tail);
        }

        if !actions.is_empty() {
            self.mark_clips_unsorted();
        }

        actions
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

    pub fn find_clip_index(&self, clip_id: &str) -> Option<usize> {
        self.clips.iter().position(|c| c.id == clip_id)
    }

    /// Check whether any clips on this layer overlap in beat range.
    /// O(n log n) — sorts a temporary copy by start_beat, then sweeps.
    pub fn has_overlapping_clips(&self) -> bool {
        if self.clips.len() < 2 {
            return false;
        }
        let mut sorted: Vec<(Beats, Beats)> = self
            .clips
            .iter()
            .map(|c| (c.start_beat, c.end_beat()))
            .collect();
        sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        for w in sorted.windows(2) {
            // If the previous clip's end exceeds the next clip's start, they overlap
            if w[0].1 > w[1].0 {
                return true;
            }
        }
        false
    }

    /// Get the effects list, creating it if None.
    pub fn effects_mut(&mut self) -> &mut Vec<PresetInstance> {
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

    /// Read a generator param value at index (returns 0.0 if out of range or no gen_params).
    pub fn get_gen_param(&self, index: usize) -> f32 {
        self.gen_params
            .as_ref()
            .and_then(|gp| gp.param_values.get(index).map(|s| s.value))
            .unwrap_or(0.0)
    }

    /// Snapshot current generator param values (effective floats).
    pub fn snapshot_gen_params(&self) -> Vec<f32> {
        self.gen_params.as_ref().map_or_else(Vec::new, |gp| {
            gp.param_values.iter().map(|s| s.value).collect()
        })
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
    /// Unity Layer.cs lines 554-559.
    ///
    /// Also clears any per-layer `generator_graph` override and bumps
    /// `generator_graph_version`. The override is shape-specific to the
    /// previous type — leaving it in place caused the renderer's
    /// per-frame override-version sweep to rebuild the new-type generator
    /// from the old type's graph, so a Plasma→Lissajous switch would
    /// keep rendering Plasma (or a Wireframe→BasicShapes switch would
    /// render wireframe polyhedra with the BasicShapes outer-card values
    /// jammed into them, producing huge white blobs). Callers that need
    /// to undo a type change snapshot the old graph alongside the old
    /// params and restore both together.
    pub fn change_generator_type(&mut self, new_type: PresetTypeId) {
        if self.layer_type != LayerType::Generator {
            return;
        }
        let gp = self
            .gen_params
            .get_or_insert_with(|| PresetInstance::new_generator(PresetTypeId::NONE));
        gp.change_type(new_type.clone());
        if gp.graph.take().is_some() {
            // Dropping the override swaps in a different def — structural.
            gp.graph_structure_version = gp.graph_structure_version.wrapping_add(1);
            gp.graph_version = gp.graph_version.wrapping_add(1);
        }
    }

    /// Reconcile a generator's identity with its own graph.
    ///
    /// A generator carries its preset identity in two places: the instance's
    /// `generator_type` (the `effect_type` field) and, when it runs a
    /// per-instance graph override, that graph's `preset_metadata.id`. These
    /// must agree. When they desync — the graph metadata names a real preset
    /// (e.g. `FluidSim3D`) but `generator_type` is `NONE` — the
    /// generator still renders fine (the renderer reads the graph directly),
    /// but every type-keyed consumer breaks: the inspector blanks the
    /// generator card, OSC addressing drops, and the picker shows no
    /// selection. Mirror the graph's id back onto the instance so the two
    /// sources agree. Returns `true` if it changed anything.
    ///
    /// Run on load to repair files saved while the identity was desynced.
    pub fn reconcile_generator_identity(&mut self) -> bool {
        if self.layer_type != LayerType::Generator {
            return false;
        }
        let Some(gp) = self.gen_params.as_mut() else {
            return false;
        };
        if *gp.generator_type() != PresetTypeId::NONE {
            return false;
        }
        let graph_id = gp
            .graph
            .as_ref()
            .and_then(|g| g.preset_metadata.as_ref())
            .map(|m| m.id.clone())
            .filter(|id| *id != PresetTypeId::NONE && !id.as_str().is_empty());
        if let Some(id) = graph_id {
            gp.set_preset_id(id);
            true
        } else {
            false
        }
    }

    /// Restore generator state from snapshot.
    /// Unity Layer.cs lines 561-567.
    pub fn restore_generator_state(
        &mut self,
        old_type: PresetTypeId,
        params: Vec<f32>,
        drivers: Option<Vec<ParameterDriver>>,
        envelopes: Option<Vec<ParamEnvelope>>,
    ) {
        if self.layer_type != LayerType::Generator {
            return;
        }
        let gp = self
            .gen_params
            .get_or_insert_with(|| PresetInstance::new_generator(PresetTypeId::NONE));
        gp.restore(old_type.clone(), params, drivers, envelopes);
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

    /// Set the per-layer MIDI device filter. Empty string → None (any device).
    pub fn set_midi_device(&mut self, name: Option<String>) {
        self.midi_device = match name {
            Some(s) if !s.is_empty() => Some(s),
            _ => None,
        };
    }

    /// Set the MIDI trigger mode (single-note vs all-notes).
    pub fn set_midi_trigger_mode(&mut self, mode: MidiTriggerMode) {
        self.midi_trigger_mode = mode;
    }

    /// Clear all clips. Unity Layer.cs line 445.
    pub fn clear_clips(&mut self) {
        self.clips.clear();
        self.mark_clips_unsorted();
    }

    /// Get duration in beats (max end_beat across all clips). Unity Layer.cs line 530.
    pub fn get_duration_beats(&self) -> Beats {
        self.clips
            .iter()
            .map(|c| c.end_beat())
            .fold(Beats::ZERO, |a, b| a.max(b))
    }

    /// Deep-clone this layer with all nested IDs regenerated.
    /// Used for duplicate-layer: new LayerId, new ClipIds, new EffectIds, remapped EffectGroupIds.
    /// Effects are duplicated via [`PresetInstance::duplicated`], so hardware
    /// bindings (Ableton / audio mods) are dropped on the copy.
    /// `parent_layer_id` is NOT remapped here — callers handle group subtree remapping.
    pub fn clone_with_new_ids(&self) -> Self {
        let mut cloned = self.clone();
        cloned.layer_id = LayerId::new(crate::short_id());

        // Fresh clip IDs.
        cloned.clips = self.clips.iter().map(|c| c.clone_with_new_id()).collect();

        // Remap effect groups: build old→new EffectGroupId map, update group_id refs on effects.
        if let Some(groups) = &self.effect_groups {
            let mut id_map: AHashMap<EffectGroupId, EffectGroupId> = AHashMap::new();
            let new_groups: Vec<EffectGroup> = groups
                .iter()
                .map(|g| {
                    let new_group = g.clone_with_new_id();
                    id_map.insert(g.id.clone(), new_group.id.clone());
                    new_group
                })
                .collect();
            cloned.effect_groups = Some(new_groups);

            if let Some(effects) = &mut cloned.effects {
                for effect in effects.iter_mut() {
                    *effect = effect.duplicated();
                    if let Some(ref old_gid) = effect.group_id.clone()
                        && let Some(new_gid) = id_map.get(old_gid)
                    {
                        effect.group_id = Some(new_gid.clone());
                    }
                }
            }
        } else if let Some(effects) = &mut cloned.effects {
            for effect in effects.iter_mut() {
                *effect = effect.duplicated();
            }
        }

        cloned
    }

    /// Set a generator param base value at index. Routes through the
    /// unified [`crate::effects::PresetInstance::set_base_param`].
    pub fn set_gen_param_base(&mut self, index: usize, value: f32) {
        if let Some(gp) = &mut self.gen_params {
            gp.set_base_param(index, value);
        }
    }
}

impl crate::effects::EffectContainer for Layer {
    fn effects(&self) -> &[crate::effects::PresetInstance] {
        self.effects.as_deref().unwrap_or(&[])
    }
    fn effects_mut(&mut self) -> &mut Vec<crate::effects::PresetInstance> {
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
    fn find_effect(&self, effect_type: &PresetTypeId) -> Option<&crate::effects::PresetInstance> {
        self.effects
            .as_ref()?
            .iter()
            .find(|e| e.effect_type() == effect_type)
    }
    fn find_effect_group(&self, group_id: &str) -> Option<&crate::effects::EffectGroup> {
        self.effect_groups
            .as_ref()?
            .iter()
            .find(|g| g.id == group_id)
    }
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            layer_id: LayerId::default(),
            index: 0,
            name: String::new(),
            layer_type: LayerType::Video,
            parent_layer_id: None,
            is_collapsed: false,
            clips: Vec::new(),
            is_solo: false,
            is_muted: false,
            blit_to_led: false,
            default_blend_mode: BlendMode::Normal,
            layer_color: Color::WHITE,
            opacity: 1.0,
            audio_gain_db: 0.0,
            analysis_only: false,
            detect_group_source: None,
            effects: None,
            effect_groups: None,
            gen_params: None,
            video_folder_path: None,
            relative_video_folder_path: None,
            midi_note: -1,
            midi_channel: -1,
            midi_device: None,
            midi_trigger_mode: MidiTriggerMode::SingleNote,
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

fn default_one() -> f32 {
    1.0
}
fn default_neg_one() -> i32 {
    -1
}
fn is_default_trigger_mode(mode: &MidiTriggerMode) -> bool {
    *mode == MidiTriggerMode::SingleNote
}
fn is_zero_audio_gain(v: &f32) -> bool {
    *v == 0.0
}
fn is_false(v: &bool) -> bool {
    !*v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::TimelineClip;

    #[test]
    fn add_clip_syncs_clip_layer_id() {
        let mut layer = Layer::new("Video 1".into(), LayerType::Video, 0);
        layer.add_clip(TimelineClip::default(), 0.5);

        assert_eq!(layer.clips.len(), 1);
        assert_eq!(layer.clips[0].layer_id, layer.layer_id);
    }

    #[test]
    fn clone_with_new_ids_regenerates_layer_and_clip_effect_ids() {
        // BUG-002/004: duplicating a layer must give fresh EffectIds to both the
        // layer's own effects AND the effects nested inside its clips, and drop
        // hardware bindings on the copies.
        let mut layer = Layer::new("Video 1".into(), LayerType::Video, 0);

        let mut layer_fx = PresetInstance::new(crate::PresetTypeId::new("Blur"));
        layer_fx.ableton_mappings = Some(Vec::new());
        layer.effects = Some(vec![layer_fx]);

        let mut clip = TimelineClip::default();
        clip.effects
            .push(PresetInstance::new(crate::PresetTypeId::new("Bloom")));
        layer.clips.push(clip);

        let cloned = layer.clone_with_new_ids();

        assert_ne!(cloned.layer_id, layer.layer_id, "fresh LayerId");

        let src_fx = &layer.effects.as_ref().unwrap()[0];
        let new_fx = &cloned.effects.as_ref().unwrap()[0];
        assert_ne!(new_fx.id, src_fx.id, "layer effect gets a fresh EffectId");
        assert!(
            new_fx.ableton_mappings.is_none(),
            "layer effect's hardware mappings dropped on duplicate"
        );

        let src_clip_fx = &layer.clips[0].effects[0];
        let new_clip_fx = &cloned.clips[0].effects[0];
        assert_ne!(
            new_clip_fx.id, src_clip_fx.id,
            "clip effect gets a fresh EffectId"
        );
    }

    #[test]
    fn active_audio_clip_at_finds_clip_under_playhead() {
        use crate::units::{Beats, Seconds};
        let mut layer = Layer::new_audio("Drums".into(), 0);
        assert!(layer.is_audio());
        layer.clips.push(TimelineClip::new_audio(
            "/x.wav".into(),
            Beats(4.0),
            Beats(8.0),
            Seconds(0.0),
            Seconds(0.0),
        ));
        assert!(layer.active_audio_clip_at(Beats(6.0)).is_some());
        assert!(layer.active_audio_clip_at(Beats(2.0)).is_none());
        assert!(layer.active_audio_clip_at(Beats(20.0)).is_none());
        // A non-audio clip on the layer is never returned as an audio clip.
        let mut vid = Layer::new_video("V".into(), 1);
        vid.clips
            .push(TimelineClip::new_video("v1".into(), Beats(0.0), Beats(4.0), Seconds(0.0)));
        assert!(vid.active_audio_clip_at(Beats(1.0)).is_none());
    }

    #[test]
    fn add_clip_enforces_non_overlap() {
        let mut layer = Layer::new("Video 1".into(), LayerType::Video, 0);
        // Existing clip at beats 0..4
        layer.restore_clip(TimelineClip {
            start_beat: Beats(0.0),
            duration_beats: Beats(4.0),
            ..TimelineClip::default()
        });
        // Add overlapping clip at beats 2..6 → should trim existing to 0..2
        let actions = layer.add_clip(
            TimelineClip {
                start_beat: Beats(2.0),
                duration_beats: Beats(4.0),
                ..TimelineClip::default()
            },
            0.5,
        );
        assert_eq!(layer.clips.len(), 2);
        assert!(!layer.has_overlapping_clips());
        assert_eq!(actions.len(), 1);
        // Existing clip was trimmed to end at beat 2
        let existing = &layer.clips[0];
        assert!((existing.duration_beats - Beats(2.0)).0.abs() < 0.001);
    }

    #[test]
    fn restore_clip_bypasses_overlap_check() {
        let mut layer = Layer::new("Video 1".into(), LayerType::Video, 0);
        layer.restore_clip(TimelineClip {
            start_beat: Beats(0.0),
            duration_beats: Beats(4.0),
            ..TimelineClip::default()
        });
        layer.restore_clip(TimelineClip {
            start_beat: Beats(2.0),
            duration_beats: Beats(4.0),
            ..TimelineClip::default()
        });
        // restore_clip is raw — overlaps remain (used for undo).
        assert_eq!(layer.clips.len(), 2);
        assert!(layer.has_overlapping_clips());
    }

    /// Build a generator layer whose per-instance graph names `graph_id` but
    /// whose `generator_type` is left `NONE` — the desynced-identity state a
    /// few project files were saved in.
    fn desynced_generator(graph_id: PresetTypeId) -> Layer {
        use crate::effect_graph_def::{EffectGraphDef, PresetMetadata};
        let mut layer = Layer::new_generator("Gen".into(), PresetTypeId::NONE, 0);
        let gp = layer.gen_params_mut().unwrap();
        gp.graph = Some(EffectGraphDef {
            version: 1,
            name: Some("Fluid Sim 3D".into()),
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: graph_id,
                display_name: "Fluid Sim 3D".into(),
                category: String::new(),
                osc_prefix: String::new(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: vec![],
                bindings: vec![],
                skip_mode: Default::default(),
                param_aliases: vec![],
                value_aliases: vec![],
                string_params: vec![],
                string_bindings: vec![],
            }),
            nodes: vec![],
            wires: vec![],
        });
        layer
    }

    #[test]
    fn reconcile_generator_identity_backfills_from_graph() {
        let mut layer = desynced_generator(PresetTypeId::new("FluidSim3D"));
        assert_eq!(*layer.generator_type(), PresetTypeId::NONE);
        assert!(layer.reconcile_generator_identity());
        assert_eq!(
            *layer.generator_type(),
            PresetTypeId::new("FluidSim3D")
        );
        // Idempotent: a second pass is a no-op.
        assert!(!layer.reconcile_generator_identity());
    }

    #[test]
    fn reconcile_leaves_typed_generator_untouched() {
        let mut layer =
            Layer::new_generator("Gen".into(), PresetTypeId::new("Plasma"), 0);
        assert!(!layer.reconcile_generator_identity());
        assert_eq!(*layer.generator_type(), PresetTypeId::new("Plasma"));
    }

    #[test]
    fn reconcile_ignores_graph_without_metadata_id() {
        let mut layer = desynced_generator(PresetTypeId::NONE);
        assert!(!layer.reconcile_generator_identity());
        assert_eq!(*layer.generator_type(), PresetTypeId::NONE);
    }

    #[test]
    fn reconcile_ignores_non_generator_layers() {
        let mut layer = Layer::new("Video".into(), LayerType::Video, 0);
        assert!(!layer.reconcile_generator_identity());
    }
}
