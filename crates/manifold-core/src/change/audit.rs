//! Change audit fingerprints (D10, feature `change-audit`).
//!
//! `fingerprint` hashes every field of a `Project` into the part that owns
//! it (the Part table in docs/UI_INVALIDATION_DESIGN.md section 3). The audit
//! compares fingerprints around an edit and fails when a part changed that
//! the edit did not declare. `Project`, `ProjectSettings`, `Timeline` and
//! `Layer` are destructured without `..`, so a new field does not compile
//! until it is given a part.

use super::{Part, PART_COUNT};
use crate::effects::PresetInstance;
use crate::layer::Layer;
use crate::project::Project;
use std::collections::hash_map::DefaultHasher;
use std::fmt::Debug;
use std::hash::Hasher;

/// One hash per part, plus the structure of every materialized graph, kept
/// apart so materializing an override (a copy of the catalog default, D21)
/// is not read as a structural Effects edit.
pub struct Fingerprint {
    parts: [u64; PART_COUNT],
    graphs: u64,
}

struct Fp {
    parts: [DefaultHasher; PART_COUNT],
    graph: DefaultHasher,
}

impl Fp {
    fn put(&mut self, part: Part, value: &impl Debug) {
        let text = format!("{value:?}");
        let h = &mut self.parts[part as usize];
        h.write(text.as_bytes());
        h.write_u8(0xff);
    }
}

/// Fields owned by no part: runtime caches and load-time bookkeeping that
/// are never serialized and that no consumer reacts to.
fn unowned<T>(_: &T) {}

pub fn fingerprint(project: &Project) -> Fingerprint {
    let mut fp = Fp { parts: std::array::from_fn(|_| DefaultHasher::new()), graph: DefaultHasher::new() };
    let Project {
        project_name,
        project_version,
        timeline,
        video_library,
        midi_config,
        audio_setup,
        settings,
        tempo_map,
        recording_provenance,
        last_saved_path,
        saved_playhead_time,
        load_report,
        embedded_presets,
        session,
        legacy_perc_audio_path,
        legacy_perc_audio_start_beat,
        legacy_perc_clip_placements,
        legacy_perc_energy_envelope,
        legacy_imported_stem_paths,
        legacy_perc_audio_hash,
    } = project;
    fp.put(Part::Settings, project_name);
    fp.put(Part::Settings, project_version);
    fp.put(Part::Media, video_library);
    fp.put(Part::Midi, midi_config);
    fp.put(Part::Settings, tempo_map);
    fp.put(Part::Settings, recording_provenance);
    unowned(last_saved_path);
    fp.put(Part::ViewPrefs, saved_playhead_time);
    unowned(load_report);
    fp.put(Part::EmbeddedPresets, embedded_presets);
    fp.put(Part::Session, session);
    fp.put(Part::Settings, legacy_perc_audio_path);
    fp.put(Part::Settings, legacy_perc_audio_start_beat);
    fp.put(Part::Settings, legacy_perc_clip_placements);
    fp.put(Part::Settings, legacy_perc_energy_envelope);
    fp.put(Part::Settings, legacy_imported_stem_paths);
    fp.put(Part::Settings, legacy_perc_audio_hash);

    let crate::audio_setup::AudioSetup { device, sends, low_hz, mid_hz, .. } = audio_setup;
    fp.put(Part::AudioSetup, device);
    fp.put(Part::Shapes, low_hz);
    fp.put(Part::Shapes, mid_hz);
    for send in sends {
        let crate::audio_setup::AudioSend { id, label, channels, gain_db, floor_db, analysis, triggers, source, .. } = send;
        fp.put(Part::AudioSetup, &(id, label, channels, floor_db, analysis, triggers, source));
        fp.put(Part::Shapes, gain_db);
    }

    settings_fp(&mut fp, settings);

    let crate::timeline::Timeline {
        name,
        layers,
        export_in_beat,
        export_out_beat,
        export_range_enabled,
        markers,
        clip_lookup,
        clip_lookup_dirty,
        layer_id_to_index,
    } = timeline;
    unowned(&(clip_lookup, clip_lookup_dirty, layer_id_to_index));
    fp.put(Part::Layers, name);
    fp.put(Part::Markers, &(export_in_beat, export_out_beat, export_range_enabled, markers));
    for layer in layers {
        layer_fp(&mut fp, layer);
    }

    Fingerprint { parts: std::array::from_fn(|i| fp.parts[i].finish()), graphs: fp.graph.finish() }
}

fn settings_fp(fp: &mut Fp, settings: &crate::settings::ProjectSettings) {
    let crate::settings::ProjectSettings {
        physics,
        output_width,
        output_height,
        frame_rate,
        vsync_enabled,
        export_hdr,
        split_at_markers,
        video_library_paths,
        video_player_pool_size,
        max_layers,
        default_recording_layer,
        bpm,
        time_signature_numerator,
        time_signature_denominator,
        quantize_mode,
        resolution_preset,
        render_scale,
        tonemap_curve,
        tonemap_enabled,
        stage_layout,
        master_opacity,
        master_effects,
        master_effect_groups,
        led_exit_index,
        led_brightness,
        led_gain,
        led_enabled,
        midi_clock_source_name,
        clock_authority,
        osc_send_port,
        osc_sync_mode,
        macro_bank,
        ableton_set_context,
        rt_quality,
        inspector_width,
        timeline_height_percent,
        effect_browser_width,
        effect_browser_open,
        viewport_scroll_x_beats,
        viewport_scroll_y_px,
        viewport_pixels_per_beat,
        macros_collapsed,
        master_chrome_collapsed,
        layer_chrome_collapsed,
        clip_chrome_collapsed,
        audio_triggers_collapsed,
        legacy_bloom_amount,
        legacy_feedback_amount,
        legacy_pixel_sort_amount,
        legacy_kaleidoscope_amount,
        legacy_kaleidoscope_segments,
        legacy_edge_stretch_amount,
        legacy_edge_stretch_source_width,
        legacy_infinite_zoom_amount,
        legacy_infinite_zoom_sharpness,
        legacy_voronoi_prism_amount,
        legacy_voronoi_prism_cell_count,
        legacy_quad_mirror_amount,
        legacy_dither_amount,
        legacy_dither_algorithm,
        legacy_strobe_amount,
        legacy_strobe_rate,
        legacy_strobe_mode,
        legacy_master_effect_order,
    } = settings;
    fp.put(Part::Settings, &(legacy_infinite_zoom_amount, legacy_infinite_zoom_sharpness, legacy_voronoi_prism_amount, legacy_voronoi_prism_cell_count, legacy_quad_mirror_amount));
    fp.put(Part::Settings, &(legacy_dither_amount, legacy_dither_algorithm, legacy_strobe_amount, legacy_strobe_rate, legacy_strobe_mode, legacy_master_effect_order));
    fp.put(Part::Settings, physics);
    fp.put(Part::Settings, &(output_width, output_height, frame_rate, vsync_enabled, export_hdr, split_at_markers));
    fp.put(Part::Media, &(video_library_paths, video_player_pool_size));
    fp.put(Part::Settings, &(max_layers, default_recording_layer, bpm, time_signature_numerator, time_signature_denominator));
    fp.put(Part::Settings, &(quantize_mode, resolution_preset, render_scale, tonemap_curve, tonemap_enabled));
    fp.put(Part::Led, &(stage_layout, led_exit_index, led_enabled));
    fp.put(Part::Values, &(master_opacity, led_brightness, led_gain));
    for fx in master_effects {
        instance_fp(fp, fx);
    }
    fp.put(Part::Effects, master_effect_groups);
    fp.put(Part::Settings, &(midi_clock_source_name, clock_authority, osc_send_port, osc_sync_mode, rt_quality));
    for slot in &macro_bank.slots {
        let crate::macro_bank::MacroSlot { value, label, mappings, ableton_mapping, .. } = slot;
        fp.put(Part::Values, value);
        fp.put(Part::Effects, label);
        for m in mappings {
            fp.put(Part::Effects, &(&m.target, &m.legacy_param_index, &m.legacy_effect_addr));
            fp.put(Part::Shapes, &(m.range_min, m.range_max, &m.curve));
        }
        if let Some(m) = ableton_mapping {
            mapping_fp(fp, m);
        }
    }
    fp.put(Part::Mappings, ableton_set_context);
    fp.put(Part::ViewPrefs, &(inspector_width, timeline_height_percent, effect_browser_width, effect_browser_open));
    fp.put(Part::ViewPrefs, &(viewport_scroll_x_beats, viewport_scroll_y_px, viewport_pixels_per_beat));
    fp.put(Part::ViewPrefs, &(macros_collapsed, master_chrome_collapsed, layer_chrome_collapsed, clip_chrome_collapsed, audio_triggers_collapsed));
    fp.put(Part::Settings, &(legacy_bloom_amount, legacy_feedback_amount, legacy_pixel_sort_amount, legacy_kaleidoscope_amount));
    fp.put(Part::Settings, &(legacy_kaleidoscope_segments, legacy_edge_stretch_amount, legacy_edge_stretch_source_width));
}

fn layer_fp(fp: &mut Fp, layer: &Layer) {
    let Layer {
        layer_id,
        index,
        name,
        layer_type,
        parent_layer_id,
        is_collapsed,
        clips,
        is_solo,
        is_muted,
        blit_to_led,
        default_blend_mode,
        layer_color,
        opacity,
        audio_gain_db,
        analysis_only,
        detect_group_source,
        detect_stem_role,
        effects,
        effect_groups,
        gen_params,
        video_folder_path,
        relative_video_folder_path,
        midi_note,
        midi_channel,
        midi_device,
        midi_trigger_mode,
        duration_mode,
        source_clip_ids,
        clip_triggers,
        legacy_generator_type,
        legacy_gen_param_values,
        legacy_gen_drivers,
        legacy_gen_param_version,
        legacy_gen_anim_speed,
        legacy_gen_animate_edges,
        legacy_gen_line_thickness,
        legacy_gen_proj_distance,
        legacy_gen_rot_speed_xy,
        legacy_gen_rot_speed_zw,
        legacy_gen_rot_speed_xw,
        legacy_gen_show_vertices,
        legacy_gen_vertex_size,
        legacy_gen_window_size,
        clips_sorted,
        clips_by_end_indices,
        clips_by_end_sorted,
    } = layer;
    unowned(&(clips_sorted, clips_by_end_indices, clips_by_end_sorted));
    fp.put(Part::Layers, &(layer_id, index, name, layer_type, parent_layer_id, is_collapsed));
    fp.put(Part::Clips, clips);
    fp.put(Part::Layers, &(is_solo, is_muted, blit_to_led, default_blend_mode, layer_color, analysis_only));
    fp.put(Part::Values, opacity);
    fp.put(Part::Shapes, audio_gain_db);
    fp.put(Part::Layers, &(detect_group_source, detect_stem_role));
    for fx in effects.iter().flatten() {
        instance_fp(fp, fx);
    }
    fp.put(Part::Effects, effect_groups);
    if let Some(gp) = gen_params {
        instance_fp(fp, gp);
    }
    fp.put(Part::Layers, &(video_folder_path, relative_video_folder_path));
    fp.put(Part::Layers, &(midi_note, midi_channel, midi_device, midi_trigger_mode, duration_mode, source_clip_ids));
    for trigger in clip_triggers {
        let crate::audio_trigger::LayerClipTrigger { enabled, source, shape, .. } = trigger;
        fp.put(Part::Effects, &(enabled, source));
        fp.put(Part::Shapes, shape);
    }
    fp.put(Part::Layers, &(legacy_generator_type, legacy_gen_param_values, legacy_gen_drivers, legacy_gen_param_version));
    fp.put(Part::Layers, &(legacy_gen_anim_speed, legacy_gen_animate_edges, legacy_gen_line_thickness, legacy_gen_proj_distance));
    fp.put(Part::Layers, &(legacy_gen_rot_speed_xy, legacy_gen_rot_speed_zw, legacy_gen_rot_speed_xw));
    fp.put(Part::Layers, &(legacy_gen_show_vertices, legacy_gen_vertex_size, legacy_gen_window_size));
}

fn mapping_fp(fp: &mut Fp, m: &crate::ableton_mapping::AbletonParamMapping) {
    fp.put(Part::Mappings, &(&m.param_id, &m.address, m.inverted, &m.legacy_param_index, &m.status));
    fp.put(Part::MappingRanges, &(m.range_min, m.range_max));
    fp.put(Part::Values, &m.last_value);
}

fn instance_fp(fp: &mut Fp, inst: &PresetInstance) {
    fp.put(Part::Effects, &(&inst.kind, &inst.id, inst.effect_type(), inst.enabled, inst.collapsed, &inst.folded_sections));
    // First base write latches base tracking; it rides with the values it tracks.
    fp.put(Part::Values, &inst.base_tracked);
    for p in inst.params.iter() {
        let mut structural = p.spec.clone();
        // Mapping reshape (range, curve, invert, label) is a live scalar.
        fp.put(Part::Shapes, &(&structural.name, structural.min, structural.max, &structural.curve, structural.invert, p.choices.slider, &p.choices.curve, p.choices.invert));
        // Committed choices that decide labels or which rows exist and where are
        // never dragged, and the live Shapes push writes no labels (UI_INVALIDATION D22).
        fp.put(Part::Effects, &(&p.choices.rename, &p.choices.section, &p.choices.pin));
        structural.name = String::new();
        structural.min = 0.0;
        structural.max = 0.0;
        structural.curve = Default::default();
        structural.invert = false;
        fp.put(Part::Effects, &(structural, &p.origin, p.exposed, &p.clip_trigger_source, &p.clip_control_digest));
        fp.put(Part::Values, &(p.value, p.base, p.touched));
    }
    for d in inst.drivers.iter().flatten() {
        fp.put(Part::Effects, &d.param_id);
        fp.put(Part::Shapes, d);
    }
    for e in inst.envelopes.iter().flatten() {
        fp.put(Part::Effects, &e.param_id);
        fp.put(Part::Shapes, e);
    }
    for m in inst.ableton_mappings.iter().flatten() {
        mapping_fp(fp, m);
    }
    for m in inst.audio_mods.iter().flatten() {
        fp.put(Part::Effects, &(&m.param_id, m.enabled, &m.source));
        fp.put(Part::Shapes, &m.shape);
        fp.put(Part::Values, &m.smoothed);
    }
    fp.put(Part::Automation, &inst.automation_lanes);
    fp.put(Part::Effects, &inst.group_id);
    // Overrides is whether a materialized copy exists; what the copy holds
    // is Effects, except unbound node param values, which are Shapes (D21).
    fp.put(Part::Overrides, &inst.graph.is_some());
    if let Some(graph) = &inst.graph {
        let mut structural = graph.clone();
        for node in &mut structural.nodes {
            fp.put(Part::Shapes, &node.params);
            node.params.clear();
        }
        // Binding reshape values (range, curve, invert, scale, offset) are
        // live scalars; which bindings and params exist, and where they point,
        // is membership and stays with the graph contents.
        if let Some(meta) = structural.preset_metadata.as_mut() {
            for spec in &mut meta.params {
                fp.put(Part::Shapes, &(&spec.id, spec.min, spec.max, &spec.curve, spec.invert));
                spec.min = 0.0;
                spec.max = 0.0;
                spec.curve = Default::default();
                spec.invert = false;
            }
            for binding in &mut meta.bindings {
                fp.put(Part::Shapes, &(&binding.id, binding.scale, binding.offset));
                binding.scale = 1.0;
                binding.offset = 0.0;
            }
        }
        let text = format!("{structural:?}");
        fp.graph.write(text.as_bytes());
        fp.graph.write_u8(0xff);
    }
    // Materializing bumps the structure version, so it rides with the graph.
    fp.graph.write_u32(inst.graph_structure_version);
    fp.put(Part::Effects, &(inst.legacy_param0, inst.legacy_param1, inst.legacy_param2, inst.legacy_param3, inst.legacy_param_version));
    fp.put(Part::Effects, &inst.relight);
    fp.put(Part::Shapes, &inst.relight_params);
}

/// The parts whose fingerprint differs. A graph structure change counts as
/// Effects unless an override was materialized or dropped in the same edit.
pub fn changed(before: &Fingerprint, after: &Fingerprint) -> super::ChangeSet {
    let mut parts: Vec<Part> =
        Part::ALL.into_iter().filter(|p| before.parts[*p as usize] != after.parts[*p as usize]).collect();
    let presence_moved = parts.contains(&Part::Overrides);
    if before.graphs != after.graphs && !presence_moved {
        parts.push(Part::Effects);
    }
    super::ChangeSet::of(&parts)
}
