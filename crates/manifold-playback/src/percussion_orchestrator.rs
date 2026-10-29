// Detect on an audio clip: the shipped trained kick and snare detectors run over the clip's whole file on a
// background thread, the hits are cached on the clip, and its trigger clips are planned and placed. Replan
// re-places from the cache when an inspector knob moves; Clear removes the clip's triggers.
//
// The host calls `tick(unscaled_time, project)` each frame. While a Detect runs, `tick(_, None)` polls the
// thread; once `result_ready()`, the next `tick` gets the project and the editing service and applies it.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};

use manifold_audio::beats::{Grid, NoGrid, song_grid};
use manifold_audio::detectors::{EventKind, candidates};
use manifold_core::ClipId;
use manifold_core::audio_clip_detection::{AudioClipDetection, confidence_from_probability};
use manifold_core::clip::TimelineClip;
use manifold_core::percussion_analysis::{
    ClipDetectionAnchor, PercussionAnalysisData, PercussionEvent, PercussionTriggerType,
};
use manifold_core::percussion_binding::ProjectPercussionBindingResolver;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::units::{Beats, Seconds};
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::clip::{ChangeClipRecordedBpmCommand, MoveClipCommand};
use manifold_editing::service::EditingService;

use crate::audio_decoder::decode_audio_to_pcm;
use crate::percussion_import::{
    apply_placement_plan, build_clip_detection_options, get_trigger_layer_name, remove_triggers,
};
use crate::percussion_planner::PercussionTimelinePlanner;

const PERCUSSION_PROGRESS_UNKNOWN: f32 = -1.0;

/// Status message color. [r, g, b, a].
pub type StatusColor = [f32; 4];

const COLOR_GREY: StatusColor = [0.72, 0.72, 0.72, 1.0];
const COLOR_BLUE: StatusColor = [0.7, 0.85, 1.0, 1.0];
const COLOR_GREEN: StatusColor = [0.5, 0.95, 0.6, 1.0];
const COLOR_ORANGE: StatusColor = [1.0, 0.75, 0.35, 1.0];
const COLOR_RED: StatusColor = [1.0, 0.4, 0.35, 1.0];

/// What Detect finds in a song: its kick and snare hits and its beat grid.
struct Analysis {
    hits: PercussionAnalysisData,
    grid: Result<Grid, NoGrid>,
}

/// One Detect in flight: the clip, the file it read, and the thread's answer once it arrives.
struct DetectRun {
    clip_id: ClipId,
    audio_path: String,
    rx: Receiver<Result<Analysis, String>>,
    result: Option<Result<Analysis, String>>,
}

pub struct PercussionImportOrchestrator {
    running: Option<DetectRun>,
    status_message: String,
    status_color: StatusColor,
    status_animate: bool,
    /// Seconds until the status clears: negative while waiting for the next tick to anchor it to the clock,
    /// then the absolute deadline; -1 when the status stays.
    status_clear_time: f32,
    status_progress01: f32,
    status_show_progress: bool,
}

impl Default for PercussionImportOrchestrator {
    fn default() -> Self {
        Self::new()
    }
}

impl PercussionImportOrchestrator {
    pub fn new() -> Self {
        Self {
            running: None,
            status_message: String::new(),
            status_color: COLOR_GREY,
            status_animate: false,
            status_clear_time: -1.0,
            status_progress01: PERCUSSION_PROGRESS_UNKNOWN,
            status_show_progress: false,
        }
    }

    pub fn status_message(&self) -> &str {
        &self.status_message
    }

    pub fn status_color(&self) -> StatusColor {
        self.status_color
    }

    pub fn status_progress01(&self) -> f32 {
        self.status_progress01
    }

    pub fn show_progress_bar(&self) -> bool {
        self.status_show_progress
    }

    /// Called each frame by the host: clears an expired status, collects a finished Detect, and applies it
    /// when the project is handed in.
    pub fn tick(
        &mut self,
        unscaled_time: f32,
        project: Option<(&mut Project, &mut EditingService)>,
    ) {
        if self.status_clear_time < -1.0 {
            self.status_clear_time = unscaled_time - self.status_clear_time;
        }
        if self.running.is_none()
            && self.status_clear_time > 0.0
            && unscaled_time >= self.status_clear_time
        {
            self.clear_status();
        }

        let Some(run) = self.running.as_mut() else {
            return;
        };
        if run.result.is_none() {
            run.result = match run.rx.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => Some(Err("the Detect thread stopped".into())),
            };
        }
        let Some((project, editing_service)) = project else {
            return;
        };
        let run = self.running.take().expect("checked above");
        let source_unchanged = project
            .timeline
            .find_clip_by_id(&run.clip_id)
            .is_some_and(|clip| clip.audio_file_path == run.audio_path);
        match run.result.expect("collected above") {
            Ok(_) if !source_unchanged => {
                self.set_status(
                    "Detect: source changed; result discarded",
                    COLOR_ORANGE,
                    4.0,
                );
            }
            Ok(analysis) => {
                let (tempo, color) = match &analysis.grid {
                    Ok(grid) => {
                        Self::apply_grid(&run.clip_id, grid, project, editing_service);
                        (format!("{:.2} BPM", grid.bpm()), COLOR_GREEN)
                    }
                    Err(no_grid) => (no_grid.to_string(), COLOR_ORANGE),
                };
                Self::auto_route_unset_instruments(&run.clip_id, project);
                let placed = self.plan_and_apply_for_clip(
                    &run.clip_id,
                    &analysis.hits,
                    project,
                    editing_service,
                );
                self.set_status(&format!("Placed {placed} clips, {tempo}"), color, 6.0);
            }
            Err(e) => {
                log::error!("[Detect] {}: {e}", run.audio_path);
                self.set_status("Detect: analysis failed", COLOR_RED, 6.0);
            }
        }
    }

    /// A Detect result waits to be written: the next `tick` needs the project and the editing service.
    pub fn result_ready(&self) -> bool {
        self.running.as_ref().is_some_and(|r| r.result.is_some())
    }

    /// Detect on one audio clip: finds its beat grid and runs the trained kick and snare detectors over its file on
    /// a background thread. The clip must already exist; Detect is a manual action, not automatic on drop.
    pub fn detect_clip(&mut self, clip_id: ClipId, project: &Project) {
        if self.running.is_some() {
            self.set_status("Detection already running", COLOR_ORANGE, 2.0);
            return;
        }
        let audio_path = match clip_in(project, &clip_id) {
            Some(c) if c.is_audio() => c.audio_file_path.clone(),
            _ => {
                self.set_status("Detect: not an audio clip", COLOR_RED, 3.0);
                return;
            }
        };
        if audio_path.trim().is_empty() || !std::path::Path::new(&audio_path).exists() {
            self.set_status("Detect: audio file not found", COLOR_RED, 3.0);
            return;
        }

        let beats_per_bar = project.settings.time_signature_numerator.max(1) as u32;
        let (tx, rx) = mpsc::channel();
        let path = audio_path.clone();
        let spawned = std::thread::Builder::new()
            .name("detect-clip".into())
            .spawn(move || {
                let _ = tx.send(analyse(&path, beats_per_bar));
            });
        if let Err(e) = spawned {
            log::error!("[Detect] could not start the Detect thread: {e}");
            self.set_status("Detect: could not start", COLOR_RED, 4.0);
            return;
        }
        self.running = Some(DetectRun {
            clip_id,
            audio_path,
            rx,
            result: None,
        });
        self.status_message = "Detecting beats, kicks and snares".to_string();
        self.status_color = COLOR_BLUE;
        self.status_animate = true;
        self.status_clear_time = -1.0;
        self.status_progress01 = PERCUSSION_PROGRESS_UNKNOWN;
        self.status_show_progress = true;
    }

    /// For each enabled instrument still routed to "Auto", point it at an existing non-group layer whose name
    /// matches the trigger ("Kick", "Snare") if one exists. Routing already falls back to by-name, so this only
    /// pre-populates the dropdown selection; it never creates layers.
    fn auto_route_unset_instruments(clip_id: &ClipId, project: &mut Project) {
        let name_to_id: Vec<(String, manifold_core::id::LayerId)> = project
            .timeline
            .layers
            .iter()
            .filter(|l| l.layer_type != LayerType::Group)
            .map(|l| (l.name.clone(), l.layer_id.clone()))
            .collect();
        if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id) {
            let detection = clip.detection.get_or_insert_with(AudioClipDetection::new);
            for inst in detection.config.instruments.iter_mut() {
                if !inst.enabled || inst.target_layer.is_some() {
                    continue;
                }
                let want = get_trigger_layer_name(inst.trigger_type);
                if let Some((_, id)) = name_to_id.iter().find(|(n, _)| *n == want) {
                    inst.target_layer = Some(id.clone());
                }
            }
        }
    }

    /// Re-place a clip's triggers from its cached hits, with no detector run. Drives the live inspector knobs
    /// (sensitivity / quantize / onset / routing): they all act at plan time, so a re-plan is instant.
    pub fn replan_clip(
        &mut self,
        clip_id: ClipId,
        project: &mut Project,
        editing_service: &mut EditingService,
    ) {
        if self.running.is_some() {
            self.set_status("Detection running", COLOR_ORANGE, 2.0);
            return;
        }
        let Some(analysis) = project
            .timeline
            .find_clip_by_id_mut(&clip_id)
            .and_then(|c| c.detection.as_ref())
            .and_then(|d| d.analysis.clone())
        else {
            self.set_status("Replan: detect first", COLOR_ORANGE, 3.0);
            return;
        };
        match self.plan_and_apply_for_clip(&clip_id, &analysis, project, editing_service) {
            0 => self.set_status("No accepted triggers", COLOR_ORANGE, 6.0),
            placed => self.set_status(&format!("Placed {placed} clips"), COLOR_GREEN, 5.0),
        }
    }

    /// Remove every trigger this audio clip produced (tagged `detection_source`), as one undoable step. Leaves
    /// other clips' triggers and hand-placed clips.
    pub fn clear_clip_triggers(
        &mut self,
        clip_id: ClipId,
        project: &mut Project,
        editing_service: &mut EditingService,
    ) {
        let commands = remove_triggers(project, &clip_id);
        if commands.is_empty() {
            self.set_status("No triggers to clear", COLOR_GREY, 2.0);
            return;
        }
        let count = commands.len();
        editing_service.record(Box::new(CompositeCommand::new(
            commands,
            "Clear clip triggers".to_string(),
        )));
        self.set_status(&format!("Cleared {count} clips"), COLOR_GREEN, 3.0);
    }

    /// Cache `analysis` on the clip, build the clip-anchored options from its `DetectionConfig`, plan, and place
    /// its triggers in place of the clip's previous ones. Shared by Detect and Replan. Returns how many clips it
    /// placed.
    fn plan_and_apply_for_clip(
        &mut self,
        clip_id: &ClipId,
        analysis: &PercussionAnalysisData,
        project: &mut Project,
        editing_service: &mut EditingService,
    ) -> usize {
        let Some(clip) = clip_in(project, clip_id) else {
            return 0;
        };
        let anchor = ClipDetectionAnchor::from_clip(clip, project);
        let config = {
            let clip = project
                .timeline
                .find_clip_by_id_mut(clip_id)
                .expect("clip resolved above");
            let detection = clip.detection.get_or_insert_with(AudioClipDetection::new);
            detection.analysis = Some(analysis.clone());
            detection.config.clone()
        };

        let options = build_clip_detection_options(project, &config, anchor);
        let plan = PercussionTimelinePlanner::new(Box::new(ProjectPercussionBindingResolver::new(
            project, &options,
        )))
        .build_plan(analysis, &options);

        // Per-instrument trigger counts for the inspector rows ("Kick 64"). Written even when empty so a
        // tightened sensitivity that drops an instrument to zero clears its stale count.
        let mut counts: HashMap<PercussionTriggerType, u32> = HashMap::new();
        for placement in plan.placements() {
            *counts.entry(placement.trigger_type).or_insert(0) += 1;
        }
        if let Some(detection) = project
            .timeline
            .find_clip_by_id_mut(clip_id)
            .and_then(|c| c.detection.as_mut())
        {
            detection.last_counts = counts;
        }

        let (added, undo) = apply_placement_plan(project, &plan, &options, clip_id);
        if let Some(undo) = undo {
            editing_service.record(undo);
        }
        added
    }

    /// Sets the clip's source tempo from `grid` (a manual BPM stays; see `ChangeClipRecordedBpmCommand::new_detected`),
    /// then moves the clip by under half a bar so the song's downbeats land on the project's bar lines. One undo step.
    fn apply_grid(
        clip_id: &ClipId,
        grid: &Grid,
        project: &mut Project,
        editing_service: &mut EditingService,
    ) {
        let mut tempo: Box<dyn Command> = Box::new(ChangeClipRecordedBpmCommand::new_detected(
            clip_id.clone(),
            grid.bpm() as f32,
        ));
        tempo.execute(project);
        let mut steps = vec![tempo];
        let layer = project
            .timeline
            .layers
            .iter()
            .find(|l| l.clips.iter().any(|c| c.id == *clip_id))
            .map(|l| l.layer_id.clone());
        if let (Some(clip), Some(layer)) = (clip_in(project, clip_id).cloned(), layer) {
            let bar = Beats(f64::from(project.settings.time_signature_numerator.max(1)));
            let downbeat = Seconds(grid.downbeat_at_or_after(clip.in_point.0));
            let at = project.source_clock().beat_at_source(&clip, downbeat);
            let mut start = clip.start_beat + bar * (at / bar).round() - at;
            if start < Beats::ZERO {
                start += bar;
            }
            if (start - clip.start_beat).0.abs() > 1e-9 {
                let mut shift: Box<dyn Command> = Box::new(MoveClipCommand::new(
                    clip_id.clone(),
                    clip.start_beat,
                    start,
                    layer.clone(),
                    layer,
                ));
                shift.execute(project);
                steps.push(shift);
            }
        }
        editing_service.record(Box::new(CompositeCommand::new(
            steps,
            "Detect tempo".to_string(),
        )));
    }

    fn set_status(&mut self, message: &str, color: StatusColor, clear_after_seconds: f32) {
        self.status_message = message.to_string();
        self.status_color = color;
        self.status_animate = false;
        // tick() anchors the pending interval to its clock on the next frame.
        self.status_clear_time = -(clear_after_seconds.max(0.0)) - 1.0;
        self.status_progress01 = PERCUSSION_PROGRESS_UNKNOWN;
        self.status_show_progress = false;
    }

    fn clear_status(&mut self) {
        self.status_message.clear();
        self.status_animate = false;
        self.status_clear_time = -1.0;
        self.status_progress01 = PERCUSSION_PROGRESS_UNKNOWN;
        self.status_show_progress = false;
    }
}

/// Finds a clip by scanning the layers: the timeline's id cache needs `&mut` to heal, and these reads hold `&`.
fn clip_in<'p>(project: &'p Project, clip_id: &ClipId) -> Option<&'p TimelineClip> {
    project
        .timeline
        .layers
        .iter()
        .flat_map(|layer| &layer.clips)
        .find(|clip| clip.id == *clip_id)
}

/// Decodes `path`, averages it to mono, and runs the beat tracker beside the shipped kick and snare detectors over
/// the whole file. Each detector candidate becomes a hit at its onset, with the model's probability as a Detect
/// confidence.
fn analyse(path: &str, beats_per_bar: u32) -> Result<Analysis, String> {
    let audio = decode_audio_to_pcm(path)?;
    let mono = audio.mono();
    let (events, grid) = std::thread::scope(|s| {
        let grid = s.spawn(|| song_grid(&mono, audio.sample_rate, beats_per_bar));
        (hits(&mono, audio.sample_rate), grid.join())
    });
    Ok(Analysis {
        hits: PercussionAnalysisData::new(events),
        grid: grid.map_err(|_| "the beat tracker panicked".to_string())?,
    })
}

fn hits(mono: &[f32], rate: u32) -> Vec<PercussionEvent> {
    candidates(mono, rate)
        .into_iter()
        .map(|c| {
            let trigger_type = match c.kind {
                EventKind::Kick => PercussionTriggerType::Kick,
                EventKind::Snare => PercussionTriggerType::Snare,
            };
            PercussionEvent::new(
                trigger_type,
                c.onset_s as f32,
                confidence_from_probability(c.p, c.cutoff),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::Beats;
    use manifold_core::PresetTypeId;
    use manifold_core::units::Seconds;

    #[test]
    fn clear_clip_triggers_removes_only_tagged() {
        let mut project = Project::default();
        let idx = project.timeline.add_layer(
            "Kick",
            LayerType::Generator,
            PresetTypeId::from_string("Gen".into()),
        );
        let audio_a = ClipId::new("audioA");
        {
            let layer = &mut project.timeline.layers[idx];
            let mut a = TimelineClip::new_generator(Beats(1.0), Beats(0.25));
            a.detection_source = Some(audio_a.clone());
            layer.restore_clip(a);
            let mut b = TimelineClip::new_generator(Beats(5.0), Beats(0.25));
            b.detection_source = Some(ClipId::new("audioB"));
            layer.restore_clip(b);
            layer.restore_clip(TimelineClip::new_generator(Beats(9.0), Beats(0.25)));
        }
        project.timeline.mark_clip_lookup_dirty();

        let mut es = EditingService::new();
        PercussionImportOrchestrator::new().clear_clip_triggers(
            audio_a.clone(),
            &mut project,
            &mut es,
        );

        let clips = &project.timeline.layers[idx].clips;
        assert_eq!(clips.len(), 2, "only audioA's trigger removed");
        assert!(
            clips
                .iter()
                .all(|c| c.detection_source.as_ref() != Some(&audio_a))
        );
        assert!(es.undo(&mut project), "clear is one undoable step");
        assert_eq!(
            project.timeline.layers[idx].clips.len(),
            3,
            "undo restores it"
        );
    }

    #[test]
    fn replan_clip_places_from_cache_without_running_detectors() {
        let mut project = Project::default();
        let aidx = project
            .timeline
            .add_layer("Audio", LayerType::Audio, PresetTypeId::NONE);
        let mut clip = TimelineClip::new_audio(
            "song.wav".into(),
            Beats(0.0),
            Beats(64.0),
            Seconds(0.0),
            Seconds(120.0),
        );
        // Seed cached hits as if a prior Detect ran: two kicks and a snare under the default gate.
        clip.detection = Some(AudioClipDetection {
            analysis: Some(PercussionAnalysisData::new(vec![
                PercussionEvent::new(PercussionTriggerType::Kick, 0.5, 0.9),
                PercussionEvent::new(PercussionTriggerType::Kick, 1.5, 0.6),
                PercussionEvent::new(PercussionTriggerType::Snare, 1.0, 0.2),
            ])),
            ..Default::default()
        });
        let clip_id = clip.id.clone();
        project.timeline.layers[aidx].restore_clip(clip);
        project.timeline.mark_clip_lookup_dirty();

        let mut es = EditingService::new();
        PercussionImportOrchestrator::new().replan_clip(clip_id.clone(), &mut project, &mut es);

        let placed = project
            .timeline
            .layers
            .iter()
            .flat_map(|l| l.clips.iter())
            .filter(|c| c.detection_source.as_ref() == Some(&clip_id))
            .count();
        assert_eq!(
            placed, 2,
            "the two kicks pass the default gate, the weak snare does not"
        );
        let detection = project
            .timeline
            .find_clip_by_id(&clip_id)
            .and_then(|c| c.detection.as_ref())
            .expect("detection stays on the clip");
        assert_eq!(detection.count(PercussionTriggerType::Kick), 2);
        assert_eq!(detection.count(PercussionTriggerType::Snare), 0);
    }

    #[test]
    fn replan_clip_without_analysis_is_noop() {
        let mut project = Project::default();
        let aidx = project
            .timeline
            .add_layer("Audio", LayerType::Audio, PresetTypeId::NONE);
        let clip = TimelineClip::new_audio(
            "song.wav".into(),
            Beats(0.0),
            Beats(8.0),
            Seconds(0.0),
            Seconds(8.0),
        );
        let clip_id = clip.id.clone();
        project.timeline.layers[aidx].restore_clip(clip);
        project.timeline.mark_clip_lookup_dirty();

        let before: usize = project.timeline.layers.iter().map(|l| l.clips.len()).sum();
        let mut es = EditingService::new();
        PercussionImportOrchestrator::new().replan_clip(clip_id, &mut project, &mut es);
        let after: usize = project.timeline.layers.iter().map(|l| l.clips.len()).sum();
        assert_eq!(before, after, "no cached analysis -> nothing placed");
    }

    #[test]
    fn grid_sets_tempo_and_puts_downbeats_on_bar_lines() {
        let mut project = Project::default();
        let aidx = project
            .timeline
            .add_layer("Audio", LayerType::Audio, PresetTypeId::NONE);
        // Dropped off the grid, trimmed into the song, with warp on at a guessed tempo.
        let mut clip = TimelineClip::new_audio(
            "song.wav".into(),
            Beats(9.3),
            Beats(32.0),
            Seconds(5.0),
            Seconds(240.0),
        );
        clip.recorded_bpm = 120.0;
        clip.audio_warp_enabled = Some(true);
        clip.audio_bpm_automatic = true;
        let clip_id = clip.id.clone();
        project.timeline.layers[aidx].restore_clip(clip);
        project.timeline.mark_clip_lookup_dirty();
        let grid = Grid {
            period_s: 60.0 / 126.7,
            downbeat_s: 0.83,
            beats_per_bar: 4,
        };

        let mut es = EditingService::new();
        PercussionImportOrchestrator::apply_grid(&clip_id, &grid, &mut project, &mut es);

        let clip = project.timeline.find_clip(&clip_id).unwrap().clone();
        assert!((clip.recorded_bpm - 126.7).abs() < 1e-3);
        assert!(
            (clip.start_beat.0 - 9.3).abs() < 2.0,
            "moved under half a bar"
        );
        let first = Seconds(grid.downbeat_at_or_after(clip.in_point.0));
        for bar in 0..4 {
            let downbeat = first + Seconds(bar as f64 * 4.0 * grid.period_s);
            let at = project.source_clock().beat_at_source(&clip, downbeat).0;
            assert!(
                (at / 4.0 - (at / 4.0).round()).abs() < 1e-6,
                "downbeat at beat {at}"
            );
        }
        assert!(es.undo(&mut project), "tempo and move are one undo step");
        let clip = project.timeline.find_clip(&clip_id).unwrap();
        assert_eq!((clip.recorded_bpm, clip.start_beat), (120.0, Beats(9.3)));
    }
}
