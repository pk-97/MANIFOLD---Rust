//! Clip domain dispatch: warp/BPM toggle, audio-detection edits, clip loop
//! toggle, and clip chrome collapse (UI_FUNNEL_DECOMPOSITION P-B, D6). One
//! slice of the inspector dispatch, reached by `dispatch_inspector`'s
//! first-non-unhandled chain. Arms are the former `dispatch_inspector` arms
//! VERBATIM (they already read `ctx` fields directly); a `_ => unhandled()`
//! fall-through lets the chain advance. `apply_detection_edit` is a clip-only
//! helper moved alongside its callers.

use manifold_core::audio_clip_detection::DetectionConfig;
use manifold_core::project::Project;
use manifold_editing::commands::clip::{ChangeClipLoopCommand, ChangeClipRecordedBpmCommand};
use manifold_editing::commands::clip_detection::SetClipDetectionConfigCommand;
use manifold_ui::PanelAction;

use super::super::DispatchResult;
use crate::content_command::ContentCommand;

/// Apply an edit to a clip's `DetectionConfig` and re-place its triggers from the
/// cached analysis. Reads the current config (or default), mutates it, records the
/// config change (local + content thread), then asks the orchestrator to re-plan
/// — instant, no backend run. See `docs/AUDIO_CLIP_DETECTION_DESIGN.md`.
fn apply_detection_edit(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<crate::content_command::ContentCommand>,
    clip_id: &manifold_core::ClipId,
    mutate: impl FnOnce(&mut DetectionConfig),
) {
    use crate::content_command::ContentCommand;
    let mut config = project
        .timeline
        .find_clip_by_id(clip_id)
        .and_then(|c| c.audio_detection.as_ref())
        .map(|d| d.config.clone())
        .unwrap_or_default();
    mutate(&mut config);

    let mut cmd: Box<dyn manifold_editing::command::Command + Send> =
        Box::new(SetClipDetectionConfigCommand::new(clip_id.clone(), config));
    cmd.execute(project);
    ContentCommand::send(content_tx, ContentCommand::Execute(cmd));
    ContentCommand::send(content_tx, ContentCommand::ReplanClip(clip_id.clone()));
}

pub(crate) fn dispatch_clip(action: &PanelAction, ctx: &mut super::super::DispatchCtx) -> DispatchResult {
    match action {
        // ── Clip chrome ────────────────────────────────────────────
        PanelAction::ClipChromeCollapseToggle => {
            ctx.ui.inspector.clip_chrome_mut().toggle_collapsed();
            DispatchResult::structural()
        }
        PanelAction::ClipBpmClicked => DispatchResult::handled(),
        PanelAction::ClipWarpToggled => {
            // Audio warp toggle: off (recorded_bpm 0, native speed) ⇄ on (lock to
            // the project tempo as a sensible default). One BPM command, which
            // also rescales the clip's timeline length to hold the audio span.
            if let Some(clip_id) = &ctx.selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                let project_bpm = ctx.project.settings.bpm.0;
                if let Some(clip) = ctx.project.timeline.find_clip_by_id(&clip_id) {
                    let old_bpm = clip.recorded_bpm;
                    let new_bpm = if old_bpm > 0.0 { 0.0 } else { project_bpm };
                    let cmd = ChangeClipRecordedBpmCommand::new(clip_id, old_bpm, new_bpm);
                    let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                        Box::new(cmd);
                    boxed.execute(ctx.project);
                    ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                }
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectClicked => {
            // Per-clip detection: analyze the selected audio clip's file and place
            // its triggers. The orchestrator (content thread) does the work and the
            // result syncs back; status shows via the global percussion status.
            if let Some(clip_id) = &ctx.selection.primary_selected_clip_id {
                ContentCommand::send(ctx.content_tx, ContentCommand::DetectClip(clip_id.clone()));
            }
            DispatchResult::handled()
        }
        PanelAction::ClipClearTriggersClicked => {
            if let Some(clip_id) = &ctx.selection.primary_selected_clip_id {
                ContentCommand::send(
                    ctx.content_tx,
                    ContentCommand::ClearClipTriggers(clip_id.clone()),
                );
            }
            DispatchResult::handled()
        }
        PanelAction::ClipReplaceAudioClicked => {
            // Replace the clip's source file (TIMELINE_INGEST_DESIGN D6/D7): a
            // native file dialog picks the new file, `ReplaceAudioFileCommand`
            // swaps path/duration/in_point/BPM and clears the cached analysis
            // while keeping the detection config, and every clip this audio clip
            // generated (tagged `detection_source`) is deleted in the same
            // undoable step — a stale trigger for a song that no longer plays is
            // worse than none. Detection is never re-run here; it stays manual.
            use manifold_editing::command::{Command, CompositeCommand};
            use manifold_editing::commands::clip::{DeleteClipCommand, ReplaceAudioFileCommand};
            if let Some(clip_id) = ctx.selection.primary_selected_clip_id.clone()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        "Audio",
                        &["wav", "mp3", "flac", "aif", "aiff", "ogg", "m4a", "aac"],
                    )
                    .pick_file()
                && let Some(clip) = ctx.project.timeline.find_clip_by_id(&clip_id)
            {
                let new_path = path.to_string_lossy().into_owned();
                let new_source_duration = crate::project_io::audio_source_duration(&new_path);
                let replace = ReplaceAudioFileCommand::new(
                    clip_id.clone(),
                    clip.audio_file_path.clone(),
                    new_path,
                    clip.source_duration,
                    new_source_duration,
                    clip.in_point,
                    clip.recorded_bpm,
                    clip.audio_detection.clone(),
                );
                let mut commands: Vec<Box<dyn Command>> = vec![Box::new(replace)];
                for layer in ctx.project.timeline.layers.iter() {
                    let layer_id = layer.layer_id.clone();
                    for generated in layer
                        .clips
                        .iter()
                        .filter(|c| c.detection_source.as_ref() == Some(&clip_id))
                    {
                        commands.push(Box::new(DeleteClipCommand::new(
                            generated.clone(),
                            layer_id.clone(),
                        )));
                    }
                }
                // Always composite (even for just the replace) so the undo
                // stack always sees one "Replace Audio File" step regardless
                // of how many generated clips came along.
                let mut cmd: Box<dyn Command + Send> = Box::new(CompositeCommand::new(
                    commands,
                    "Replace Audio File".to_string(),
                ));
                cmd.execute(ctx.project);
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(cmd));
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectInstrumentToggled(idx) => {
            let idx = *idx;
            if let Some(clip_id) = ctx.selection.primary_selected_clip_id.clone() {
                apply_detection_edit(ctx.project, ctx.content_tx, &clip_id, |c| {
                    if let Some(inst) = c.instruments.get_mut(idx) {
                        inst.enabled = !inst.enabled;
                    }
                });
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectSensitivityChanged(idx, value) => {
            let (idx, value) = (*idx, *value);
            if let Some(clip_id) = ctx.selection.primary_selected_clip_id.clone() {
                apply_detection_edit(ctx.project, ctx.content_tx, &clip_id, |c| {
                    if let Some(inst) = c.instruments.get_mut(idx) {
                        inst.sensitivity = value.clamp(0.0, 1.0);
                    }
                });
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectOnsetChanged(ms) => {
            let secs = manifold_core::Seconds((*ms / 1000.0) as f64);
            if let Some(clip_id) = ctx.selection.primary_selected_clip_id.clone() {
                apply_detection_edit(ctx.project, ctx.content_tx, &clip_id, |c| {
                    c.onset_compensation = secs;
                });
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectSetQuantize(step) => {
            let step = *step;
            if let Some(clip_id) = ctx.selection.primary_selected_clip_id.clone() {
                apply_detection_edit(ctx.project, ctx.content_tx, &clip_id, |c| match step {
                    Some(beats) => {
                        c.quantize_on = true;
                        c.quantize_step_beats = beats;
                    }
                    None => c.quantize_on = false,
                });
            }
            DispatchResult::structural()
        }
        PanelAction::ClipDetectSetLayer(idx, layer) => {
            let (idx, layer) = (*idx, layer.clone());
            if let Some(clip_id) = ctx.selection.primary_selected_clip_id.clone() {
                apply_detection_edit(ctx.project, ctx.content_tx, &clip_id, |c| {
                    if let Some(inst) = c.instruments.get_mut(idx) {
                        inst.target_layer = layer;
                    }
                });
            }
            DispatchResult::structural()
        }
        // The open actions are consumed by UIRoot::try_open_dropdown before
        // dispatch; these arms are defensive no-ops.
        PanelAction::ClipDetectQuantizeClicked | PanelAction::ClipDetectLayerClicked(_) => {
            DispatchResult::handled()
        }
        PanelAction::ClipLoopToggle => {
            if let Some(clip_id) = &ctx.selection.primary_selected_clip_id {
                let clip_id = clip_id.clone();
                if let Some(clip) = ctx.project.timeline.find_clip_by_id(&clip_id) {
                    let old_loop = clip.is_looping;
                    let old_dur = clip.loop_duration_beats;
                    let cmd =
                        ChangeClipLoopCommand::new(clip_id, old_loop, !old_loop, old_dur, old_dur);
                    {
                        let mut boxed: Box<dyn manifold_editing::command::Command + Send> =
                            Box::new(cmd);
                        boxed.execute(ctx.project);
                        ContentCommand::send(ctx.content_tx, ContentCommand::Execute(boxed));
                    }
                }
            }
            DispatchResult::structural()
        }
        // BUG-061: the clip in-point ("slip") slider and its right-click reset
        // were removed (`ClipSlipSnapshot`/`Changed`/`Commit`/`RightClick`) —
        // dead code with no emitter (the slip UI itself was already gone;
        // `clip_chrome.rs`'s `set_slip_range`/`sync_slip` were empty stubs).
        // The clip LOOP-DURATION trio (`ClipLoopSnapshot`/`Changed`/`Commit`)
        // was dead for the same reason and removed alongside it.
        // `ClipLoopToggle` is a real, live toggle (is_looping) — kept above.
        _ => DispatchResult::unhandled(),
    }
}
