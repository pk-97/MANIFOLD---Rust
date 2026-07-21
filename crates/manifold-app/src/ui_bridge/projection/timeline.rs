//! Timeline projection: structural layer/track/clip sync into the viewport,
//! clip-name/colour/waveform helpers, and lightweight per-frame clip position
//! sync. Moved from state_sync.rs (P-P, UI_FUNNEL_DECOMPOSITION_DESIGN.md).

use manifold_core::project::Project;
use manifold_core::tempo::TempoMapConverter;
use manifold_core::types::LayerType;
use manifold_core::Beats;
use manifold_ui::panels::layer_header::LayerInfo;
use manifold_ui::panels::viewport::TrackInfo;
use crate::app::SelectionState;
use crate::ui_root::UIRoot;

/// Sync structural project data (layers, tracks) into UI panels.
/// Call once at init and whenever the project structure changes.
/// Triggers a full UI rebuild afterward.
pub fn sync_project_data(
    ui: &mut UIRoot,
    project: &Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
) {
    {
        // Rebuild CoordinateMapper Y-layout FIRST so layer headers and viewport share
        // the same Y offsets. Unity: LayerHeaderPanel reads from CoordinateMapper.
        // `_for_layout` (not the plain `layers_to_ui`) resolves
        // `automation_lane_count` from `selection.automation_mode_visible` — the
        // one flag that grows a track when lanes are visible
        // (`docs/AUTOMATION_LANES_DESIGN.md` §7).
        ui.viewport.rebuild_mapper_layout(&crate::ui_translate::layers_to_ui_for_layout(
            &project.timeline.layers,
            selection.automation_mode_visible,
            &selection.chosen_automation_params,
        ));

        // Layer data → LayerHeaderPanel. Y offset/height are NOT copied here —
        // `LayerInfo` no longer carries them; the header panel queries the
        // mapper directly at draw time (`docs/TIMELINE_LAYOUT_P0_SPEC.md` D1),
        // the exact same values the viewport uses for lanes.
        let layers: Vec<LayerInfo> = project
            .timeline
            .layers
            .iter()
            .map(|layer| {
                LayerInfo {
                    name: layer.name.clone(),
                    layer_id: layer.layer_id.to_string(),
                    is_collapsed: layer.is_collapsed,
                    is_group: layer.is_group(),
                    is_generator: layer.layer_type == LayerType::Generator,
                    is_audio: layer.is_audio(),
                    is_muted: layer.is_muted
                        || layer.parent_layer_id.as_ref().is_some_and(|pid| {
                            project
                                .timeline
                                .layers
                                .iter()
                                .any(|l| l.layer_id == *pid && l.is_muted)
                        }),
                    is_solo: layer.is_solo
                        || layer.parent_layer_id.as_ref().is_some_and(|pid| {
                            project
                                .timeline
                                .layers
                                .iter()
                                .any(|l| l.layer_id == *pid && l.is_solo)
                        }),
                    analysis_only: layer.analysis_only,
                    is_led: layer.blit_to_led,
                    parent_layer_id: layer.parent_layer_id.as_ref().map(|id| id.to_string()),
                    blend_mode: format!("{:?}", layer.default_blend_mode),
                    generator_type: layer.gen_params().map(|g| {
                        manifold_core::preset_type_registry::display_name(g.generator_type())
                            .to_string()
                    }),
                    clip_count: layer.clips.len(),
                    video_folder_path: layer.video_folder_path.clone(),
                    source_clip_count: 0,
                    midi_note: layer.midi_note,
                    midi_channel: layer.midi_channel,
                    midi_device: layer.midi_device.clone(),
                    midi_all_notes: matches!(
                        layer.midi_trigger_mode,
                        manifold_core::types::MidiTriggerMode::AllNotes
                    ),
                    audio_gain_db: layer.audio_gain_db,
                    audio_send_name: project
                        .audio_setup
                        .send_for_layer(&layer.layer_id)
                        .map(|s| s.label.clone()),
                    is_selected: selection.is_layer_active(&layer.layer_id),
                    color: manifold_ui::node::Color32::from_f32(
                        layer.layer_color.r,
                        layer.layer_color.g,
                        layer.layer_color.b,
                        layer.layer_color.a,
                    ),
                }
            })
            .collect();
        let active_layer_id = active_layer
            .and_then(|i| project.timeline.layers.get(i))
            .map(|l| l.layer_id.clone());
        ui.layer_headers.set_active_layer(active_layer_id);
        ui.layer_headers.set_layers(layers);

        // Track data → TimelineViewportPanel
        // From Unity ViewportManager.BuildTrack (lines 548-663):
        // - is_muted includes parent group mute (children of muted groups are dimmed)
        // - is_group set correctly for group layers
        let tracks: Vec<TrackInfo> = project
            .timeline
            .layers
            .iter()
            .map(|layer| {
                // Check if muted individually or by parent group
                let parent_muted = layer.parent_layer_id.as_ref().is_some_and(|pid| {
                    project
                        .timeline
                        .layers
                        .iter()
                        .any(|l| l.layer_id == *pid && l.is_muted)
                });
                let is_muted = layer.is_muted || parent_muted;

                // Track height is owned solely by the CoordinateMapper
                // (rebuilt above, read back by the viewport). No copy here.

                // Child layer indices for collapsed group preview
                let child_layer_indices = if layer.is_group() {
                    let layer_id = &layer.layer_id;
                    project
                        .timeline
                        .layers
                        .iter()
                        .enumerate()
                        .filter(|(_, l)| l.parent_layer_id.as_ref() == Some(layer_id))
                        .map(|(j, _)| j)
                        .collect()
                } else {
                    Vec::new()
                };

                TrackInfo {
                    is_muted,
                    is_group: layer.is_group(),
                    is_collapsed: layer.is_collapsed,
                    child_layer_indices,
                }
            })
            .collect();
        ui.viewport.set_tracks(tracks);
        ui.viewport.layer_ids = project
            .timeline
            .layers
            .iter()
            .map(|l| l.layer_id.clone())
            .collect();

        // (CoordinateMapper Y-layout already rebuilt above, before layer headers)

        // Clip data → TimelineViewportPanel
        let mut viewport_clips = Vec::new();
        for (i, layer) in project.timeline.layers.iter().enumerate() {
            for (clip_idx, clip) in layer.clips.iter().enumerate() {
                let is_gen = layer.layer_type == LayerType::Generator;
                let name = clip_display_name(layer, clip, clip_idx + 1, &project.video_library);
                use manifold_ui::panels::viewport::ViewportClip;
                let clip_color = clip_base_color(layer, clip, 1.0);
                viewport_clips.push(ViewportClip {
                    clip_id: clip.id.clone(),
                    layer_index: i,
                    start_beat: clip.start_beat,
                    duration_beats: clip.duration_beats,
                    name: name.into(),
                    color: clip_color,
                    is_muted: clip.is_muted,
                    is_locked: false,
                    is_generator: is_gen,
                    is_audio: layer.is_audio(),
                    waveform: if layer.is_audio() {
                        ui.audio_waveforms.renderer(&clip.id)
                    } else {
                        None
                    },
                    in_point_seconds: clip.in_point.0 as f32,
                    waveform_breakpoints: audio_waveform_breakpoints(clip, project),
                });
            }
        }
        ui.viewport.set_clips(viewport_clips);

        // Automation lane data → viewport (P4, `docs/AUTOMATION_LANES_DESIGN.md`
        // §7). Gated on the same flag `layers_to_ui_for_layout` used above, so
        // the Y-layout and the lane list can never disagree about whether
        // lanes are showing this frame.
        let mut viewport_lanes = Vec::new();
        if selection.automation_mode_visible {
            use manifold_ui::panels::viewport::ViewportAutomationLane;
            for (i, layer) in project.timeline.layers.iter().enumerate() {
                if layer.is_collapsed || layer.is_group() {
                    continue;
                }
                let chosen = selection.chosen_automation_params.get(&layer.layer_id);
                for lane in crate::ui_translate::layer_automation_lanes_to_ui(layer, chosen) {
                    viewport_lanes.push(ViewportAutomationLane { layer_index: i, lane });
                }
            }
        }
        ui.viewport.set_automation_lanes(viewport_lanes);

        // Timeline markers → viewport
        ui.viewport
            .set_markers(crate::ui_translate::markers_to_ui(&project.timeline.markers));
        ui.viewport
            .set_selected_marker_ids(selection.selected_marker_ids.iter().cloned().collect());

        // Beats per bar
        ui.viewport
            .set_beats_per_bar(project.settings.time_signature_numerator as u32);
    }
}

/// Display title for a timeline clip in the viewport: **type · instance ·
/// format**. The type is the generator subtype / "Video" / "Audio", the
/// instance is the clip's 1-based position on its layer (left to right), and
/// the format is the container + resolution (video) or container (audio).
/// Generators carry no file format. Shared by both clip-sync paths so they
/// label clips identically.
///
/// Examples: `Text 1` · `Video 2 · MOV 1080p` · `Audio 1 · WAV`.
fn clip_display_name(
    layer: &manifold_core::layer::Layer,
    clip: &manifold_core::clip::TimelineClip,
    instance: usize,
    video_library: &manifold_core::video::VideoLibrary,
) -> String {
    if layer.layer_type == LayerType::Generator {
        let ty = layer
            .gen_params()
            .map(|gp| {
                manifold_core::preset_type_registry::display_name(gp.generator_type()).to_string()
            })
            .unwrap_or_else(|| "Gen".to_string());
        format!("{ty} {instance}")
    } else if clip.is_audio() {
        match file_ext_upper(&clip.audio_file_path) {
            Some(ext) => format!("Audio {instance} \u{b7} {ext}"),
            None => format!("Audio {instance}"),
        }
    } else if !clip.video_clip_id.is_empty() {
        // Container + resolution from the source clip, when the library knows it.
        let fmt = video_library
            .find_clip_by_id(&clip.video_clip_id)
            .map(|vc| {
                let ext = file_ext_upper(&vc.file_name);
                let res = resolution_label(vc.resolution_width, vc.resolution_height);
                match (ext, res) {
                    (Some(e), Some(r)) => format!(" \u{b7} {e} {r}"),
                    (Some(e), None) => format!(" \u{b7} {e}"),
                    (None, Some(r)) => format!(" \u{b7} {r}"),
                    (None, None) => String::new(),
                }
            })
            .unwrap_or_default();
        format!("Video {instance}{fmt}")
    } else {
        format!("Clip {instance}")
    }
}

/// Uppercased file extension (`flowers.mov` → `MOV`), or `None` when the path
/// has no extension.
fn file_ext_upper(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_uppercase())
}

/// A short resolution tag from pixel dimensions — `1080p`, `4K`, etc. Falls
/// back to `W×H` for non-standard sizes, and `None` when dimensions are unset.
fn resolution_label(width: i32, height: i32) -> Option<String> {
    if width <= 0 || height <= 0 {
        return None;
    }
    Some(match height {
        2160 => "4K".to_string(),
        1440 => "1440p".to_string(),
        1080 => "1080p".to_string(),
        720 => "720p".to_string(),
        480 => "480p".to_string(),
        _ => format!("{width}\u{00d7}{height}"),
    })
}

/// Effective base colour for a clip: its per-clip `color_override` if set,
/// otherwise the owning layer's colour. Alpha is supplied by the caller — the
/// two clip-sync paths use different clip alphas. Shared so both resolve the
/// override identically.
fn clip_base_color(
    layer: &manifold_core::layer::Layer,
    clip: &manifold_core::clip::TimelineClip,
    alpha: f32,
) -> manifold_ui::node::Color32 {
    let c = clip.color_override.unwrap_or(layer.layer_color);
    manifold_ui::node::Color32::from_f32(c.r, c.g, c.b, alpha)
}

/// Lightweight per-frame clip position sync.
/// Refreshes viewport.clips_by_layer from the live project model so that
/// drag mutations (clip move, trim) are visible in the bitmap renderer.
/// Does NOT touch tracks, bitmap renderers, or layer headers — only clip data.
/// The bitmap fingerprint will detect if positions actually changed and skip
/// repaint when nothing moved (cheap no-op outside of drag).
///
/// `automation_visible` also refreshes `viewport`'s automation lane geometry
/// from the live project when true (P4 Unit A automation point drag —
/// `InteractionOverlay::handle_automation_drag` mutates the project directly
/// each frame the same way clip move-drag does, and needs the SAME per-frame
/// resync path so a dragged dot's on-screen position updates live instead of
/// waiting for the next structural sync — `docs/AUTOMATION_LANES_DESIGN.md`
/// §7). Cheap: gated the same way the clip refresh already is (mouse-pressed
/// or structural change), and lane counts are tens, not hundreds.
pub fn sync_clip_positions(
    ui: &mut UIRoot,
    project: &Project,
    automation_visible: bool,
    chosen_automation_params: &std::collections::HashMap<
        manifold_core::LayerId,
        (manifold_ui::view::UiGraphTarget, manifold_core::effects::ParamId),
    >,
) {
    use manifold_ui::panels::viewport::ViewportClip;
    let mut viewport_clips = Vec::new();
    for (i, layer) in project.timeline.layers.iter().enumerate() {
        let is_gen = layer.layer_type == LayerType::Generator;
        for (clip_idx, clip) in layer.clips.iter().enumerate() {
            let name = clip_display_name(layer, clip, clip_idx + 1, &project.video_library);
            let clip_color = clip_base_color(layer, clip, 0.86);
            viewport_clips.push(ViewportClip {
                clip_id: clip.id.clone(),
                layer_index: i,
                start_beat: clip.start_beat,
                duration_beats: clip.duration_beats,
                name: name.into(),
                color: clip_color,
                is_muted: clip.is_muted,
                is_locked: false,
                is_generator: is_gen,
                is_audio: layer.is_audio(),
                waveform: if layer.is_audio() {
                    ui.audio_waveforms.renderer(&clip.id)
                } else {
                    None
                },
                in_point_seconds: clip.in_point.0 as f32,
                waveform_breakpoints: audio_waveform_breakpoints(clip, project),
            });
        }
    }
    ui.viewport.set_clips(viewport_clips);

    if automation_visible {
        use manifold_ui::panels::viewport::ViewportAutomationLane;
        let mut viewport_lanes = Vec::new();
        for (i, layer) in project.timeline.layers.iter().enumerate() {
            if layer.is_collapsed || layer.is_group() {
                continue;
            }
            let chosen = chosen_automation_params.get(&layer.layer_id);
            for lane in crate::ui_translate::layer_automation_lanes_to_ui(layer, chosen) {
                viewport_lanes.push(ViewportAutomationLane { layer_index: i, lane });
            }
        }
        ui.viewport.set_automation_lanes(viewport_lanes);
    }

    // Only sync markers when marker data has changed (avoids re-pushing on every
    // drag frame). Markers are few (dozens), so building the UI view to compare is cheap.
    let ui_markers = crate::ui_translate::markers_to_ui(&project.timeline.markers);
    if ui.viewport.markers_stale(&ui_markers) {
        ui.viewport.set_markers(ui_markers);
    }
}

/// Piecewise beat→file-seconds breakpoints for an audio clip's waveform,
/// mirroring exactly what `AudioLayerPlayback::update` computes for the
/// voice's expected source position (`crates/manifold-playback/src/
/// audio_layer_playback.rs` ~:251-257): `expected = (now - clip_start) *
/// warp_ratio + in_point`, where `now`/`clip_start` are transport seconds
/// from `engine.beat_to_timeline_time_immut`, i.e.
/// `TempoMapConverter::beat_to_seconds_immut` — the project's piecewise tempo
/// map integration, NOT a constant seconds-per-beat. Audio playback is
/// correct; the waveform painter draws a single linear window, so a varying
/// tempo map made the two disagree.
///
/// Reproducing that beat→seconds function pointwise at the clip's start beat,
/// every tempo-map point strictly inside the clip, and its end beat gives a
/// piecewise-*linear* (in beats, matching the pixel x-axis) mapping: constant
/// between tempo points, a new slope at each one. Each pair is `(x_frac,
/// file_secs)` — `x_frac` beat-linear in `[0, 1]` across the clip, `file_secs`
/// the source-file position playback would be at for that beat. A
/// constant-tempo clip (no tempo-map point strictly inside it) yields exactly
/// 2 breakpoints — the old single linear window, reproduced exactly since
/// `warp_ratio` (unaffected by the tempo map) is unchanged and the only
/// segment IS start→end.
///
/// Empty for non-audio clips or non-positive duration. Baked once here per
/// clip per structural sync (not per frame in the renderer) — plain
/// `Vec<(f32, f32)>` data, no core types, per `manifold-ui`'s layering rule
/// (depends on `manifold-foundation` only).
fn audio_waveform_breakpoints(
    clip: &manifold_core::clip::TimelineClip,
    project: &Project,
) -> Vec<(f32, f32)> {
    if !clip.is_audio() {
        return Vec::new();
    }
    let duration_beats = clip.duration_beats.as_f32();
    if duration_beats <= 0.0 {
        return Vec::new();
    }
    let start_beat = clip.start_beat;
    let end_beat = clip.start_beat + clip.duration_beats;
    let project_bpm = project.settings.bpm;
    let ratio = clip.warp_ratio(project_bpm.0);
    let in_point = clip.in_point.0 as f32;
    let start_secs =
        TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, start_beat, project_bpm).0;

    let file_secs_at = |beat: Beats| -> f32 {
        let secs =
            TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, beat, project_bpm).0;
        ((secs - start_secs) as f32) * ratio + in_point
    };

    let mut breakpoints = Vec::with_capacity(2 + project.tempo_map.points().len());
    breakpoints.push((0.0, file_secs_at(start_beat)));
    for point in project.tempo_map.points() {
        if point.beat > start_beat && point.beat < end_beat {
            let x_frac = (point.beat - start_beat).as_f32() / duration_beats;
            breakpoints.push((x_frac, file_secs_at(point.beat)));
        }
    }
    breakpoints.push((1.0, file_secs_at(end_beat)));
    breakpoints
}

#[cfg(test)]
mod audio_waveform_breakpoints_tests {
    use super::*;
    use manifold_core::clip::TimelineClip;
    use manifold_core::project::Project;
    use manifold_core::tempo::TempoMapConverter;
    use manifold_core::types::TempoPointSource;
    use manifold_core::{Bpm, Seconds};

    /// (a) A single-point (i.e. effectively constant-tempo) map must yield
    /// exactly 2 breakpoints — clip start and end — reproducing the old
    /// `dur_beats * warped_secs_per_beat` window exactly: `file_secs_at(end)`
    /// must equal `duration_beats * (60 / bpm) * warp_ratio + in_point`.
    #[test]
    fn single_tempo_point_reproduces_old_constant_mapping() {
        let mut project = Project::default();
        project.settings.bpm = Bpm(140.0);
        project
            .tempo_map
            .add_or_replace_point(Beats::ZERO, Bpm(140.0), TempoPointSource::Manual, 0.001);

        let clip = TimelineClip::new_audio(
            "song.wav".to_string(),
            Beats::from_f32(8.0),
            Beats::from_f32(4.0),
            Seconds(1.5),
            Seconds(120.0),
        );

        let bp = audio_waveform_breakpoints(&clip, &project);
        assert_eq!(bp.len(), 2, "constant tempo must yield exactly 2 breakpoints");
        assert_eq!(bp[0], (0.0, 1.5), "clip start maps to in_point with no elapsed beats");

        let expected_win_secs = 4.0 * (60.0 / 140.0_f32); // warp off (recorded_bpm unset) → project spb
        assert!(
            (bp[1].0 - 1.0).abs() < 1e-6,
            "clip end must be at x_frac 1.0, got {}",
            bp[1].0
        );
        assert!(
            (bp[1].1 - (1.5 + expected_win_secs)).abs() < 1e-4,
            "clip end file_secs {} should equal old in_point + dur*spb window {}",
            bp[1].1,
            1.5 + expected_win_secs
        );
    }

    /// (b) A 3-point tempo map (120 BPM then 60 BPM partway through the clip)
    /// must place a breakpoint exactly at the tempo-change beat, and every
    /// breakpoint's `file_secs` must match
    /// `TempoMapConverter::beat_to_seconds_immut` differences directly — the
    /// same integration playback performs, not a flat per-beat scalar.
    #[test]
    fn three_point_map_matches_tempo_map_converter_directly() {
        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        project
            .tempo_map
            .add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Manual, 0.001);
        // Tempo halves to 60 BPM at beat 10 — inside the clip (beats 8..16).
        project
            .tempo_map
            .add_or_replace_point(Beats::from_f32(10.0), Bpm(60.0), TempoPointSource::Manual, 0.001);

        let clip = TimelineClip::new_audio(
            "song.wav".to_string(),
            Beats::from_f32(8.0),
            Beats::from_f32(8.0), // 8..16
            Seconds(0.0),
            Seconds(120.0),
        );

        let bp = audio_waveform_breakpoints(&clip, &project);
        assert_eq!(bp.len(), 3, "one tempo point strictly inside the clip → 3 breakpoints");

        // x_frac of the tempo-change beat (10) within the clip (8..16): 2/8 = 0.25.
        assert!((bp[1].0 - 0.25).abs() < 1e-6, "tempo point x_frac, got {}", bp[1].0);

        let start_secs =
            TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, Beats::from_f32(8.0), Bpm(120.0)).0;
        let mid_secs =
            TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, Beats::from_f32(10.0), Bpm(120.0)).0;
        let end_secs =
            TempoMapConverter::beat_to_seconds_immut(&project.tempo_map, Beats::from_f32(16.0), Bpm(120.0)).0;

        // recorded_bpm unset → warp_ratio == 1.0, in_point == 0 → file_secs is
        // exactly the tempo-map-integrated elapsed seconds since clip start.
        assert!((bp[0].1 - 0.0).abs() < 1e-6);
        assert!(
            (bp[1].1 - ((mid_secs - start_secs) as f32)).abs() < 1e-4,
            "mid breakpoint file_secs {} vs tempo-map delta {}",
            bp[1].1,
            (mid_secs - start_secs) as f32
        );
        assert!(
            (bp[2].1 - ((end_secs - start_secs) as f32)).abs() < 1e-4,
            "end breakpoint file_secs {} vs tempo-map delta {}",
            bp[2].1,
            (end_secs - start_secs) as f32
        );
    }

    /// (c) `in_point` must offset every breakpoint's `file_secs` uniformly —
    /// it enters the formula as a flat additive term, exactly like
    /// `AudioLayerPlayback`'s `expected = (now - clip_start) * ratio +
    /// clip.in_point`.
    #[test]
    fn in_point_offsets_every_breakpoint() {
        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        project
            .tempo_map
            .add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Manual, 0.001);

        let clip_no_offset = TimelineClip::new_audio(
            "song.wav".to_string(),
            Beats::from_f32(0.0),
            Beats::from_f32(4.0),
            Seconds(0.0),
            Seconds(120.0),
        );
        let clip_with_offset = TimelineClip::new_audio(
            "song.wav".to_string(),
            Beats::from_f32(0.0),
            Beats::from_f32(4.0),
            Seconds(3.0),
            Seconds(120.0),
        );

        let bp_plain = audio_waveform_breakpoints(&clip_no_offset, &project);
        let bp_offset = audio_waveform_breakpoints(&clip_with_offset, &project);
        assert_eq!(bp_plain.len(), bp_offset.len());
        for (plain, offset) in bp_plain.iter().zip(bp_offset.iter()) {
            assert!((plain.0 - offset.0).abs() < 1e-6, "x_frac must be unaffected by in_point");
            assert!(
                (offset.1 - (plain.1 + 3.0)).abs() < 1e-4,
                "file_secs must be shifted by exactly in_point: {} vs {} + 3.0",
                offset.1,
                plain.1
            );
        }
    }

    /// Non-audio clips (no `audio_file_path`) get no breakpoints — the
    /// waveform painter is never invoked for them.
    #[test]
    fn non_audio_clip_yields_no_breakpoints() {
        let project = Project::default();
        let clip = TimelineClip::new_generator(Beats::ZERO, Beats::from_f32(4.0));
        assert!(audio_waveform_breakpoints(&clip, &project).is_empty());
    }
}
