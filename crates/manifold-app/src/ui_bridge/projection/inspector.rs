//! Inspector projection: the big `sync_inspector_data` selection sync, the
//! per-card modulation builders, single-param modulation lookups, and audio/
//! modifier label helpers. Moved from state_sync.rs (P-P,
//! UI_FUNNEL_DECOMPOSITION_DESIGN.md).

use manifold_core::PresetTypeId;
use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::types::{BeatDivision, LayerType};
use manifold_ui::panels::param_card::RowMod;
use manifold_ui::panels::param_slider_shared::AudioRowState;
use manifold_ui::panels::inspector::RackGroupConfig;
use manifold_ui::param_surface::ParamSurface;
use manifold_ui::panels::scene_setup_panel::SceneSetupState;
use manifold_ui::view::UiGraphTarget;
use crate::app::SelectionState;
use crate::ui_root::UIRoot;

use super::cards::{
    OscScope, SurfaceVisibility, attach_project_sources, audio_send_choices, effects_to_surfaces,
    gen_params_to_surface, modifier_surfaces, scene_card_folds,
};

fn mark_selected_automation_row(surface: &mut ParamSurface, layer_id: &manifold_core::LayerId, target: &UiGraphTarget, param_id: &manifold_core::effects::ParamId) {
    let surface_target = match (surface.kind, surface.modifier.is_some()) {
        (manifold_ui::panels::param_card::ParamCardKind::Effect, false) => UiGraphTarget::Effect(surface.effect_id.clone()),
        (manifold_ui::panels::param_card::ParamCardKind::Effect, true)
        | (manifold_ui::panels::param_card::ParamCardKind::Generator, _) => {
            UiGraphTarget::Generator(layer_id.clone())
        }
    };
    if surface_target == *target {
        for row in &mut surface.rows {
            row.modulation.automation_selected = row.id.as_ref() == param_id.as_ref();
        }
    }
}

fn rack_groups(
    groups: &[manifold_core::effects::EffectGroup],
    effects: &[PresetInstance],
) -> Vec<RackGroupConfig> {
    groups
        .iter()
        .filter_map(|group| {
            let member_ids = effects
                .iter()
                .filter(|effect| effect.group_id.as_ref() == Some(&group.id))
                .map(|effect| effect.id.clone())
                .collect::<Vec<_>>();
            (!member_ids.is_empty()).then(|| RackGroupConfig {
                id: group.id.clone(),
                collapsed: group.collapsed,
                name: match group.name.as_str() {
                    "Group" | "Masked Group" => "Modifier Group".to_string(),
                    _ => group.name.clone(),
                },
                member_ids,
                mask_effect_id: group.mask_effect_id.clone(),
                enabled: group.enabled,
                wet_dry: group.wet_dry,
            })
        })
        .collect()
}

/// Map the content thread's `LedPreview` across the projection boundary for
/// the card (LED_STRIPS_DESIGN MVP-P4, D24). The gate is the same on both
/// call sites (structural sync + per-frame push): only a DMX lane's generator
/// card carries the band (D22 — the whole composite, never a per-layer view),
/// and the app-side `None`/`LedPreview::None` states render no band at all.
pub(crate) fn map_led_preview(
    led_preview: Option<&crate::content_state::LedPreview>,
    is_dmx: bool,
) -> Option<manifold_ui::panels::param_card::LedPreviewBand> {
    if !is_dmx {
        return None;
    }
    match led_preview {
        Some(crate::content_state::LedPreview::Frame { pixels, version }) => {
            Some(manifold_ui::panels::param_card::LedPreviewBand::Frame {
                pixels: std::sync::Arc::clone(pixels),
                version: *version,
            })
        }
        Some(crate::content_state::LedPreview::Black) => {
            Some(manifold_ui::panels::param_card::LedPreviewBand::Black)
        }
        _ => None,
    }
}

/// Sync inspector content for the active selection.
/// Called when the active layer changes or after structural mutations.
pub fn sync_inspector_data(
    ui: &mut UIRoot,
    project: &Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
    led_preview: Option<&crate::content_state::LedPreview>,
) {
    manifold_core::work_count::record_work(manifold_core::work_count::WorkKind::InspectorSync);
    let driver_timing = (ui.driver_bpm.unwrap_or(project.settings.bpm), project.settings.frame_rate);
    // The same stable layer projection feeds the mask-source picker and the
    // audio clip detector. Keep groups out: layer_source resolves visual
    // surfaces, while group compositing is not a source texture.
    ui.set_clip_detect_layers(
        project
            .timeline
            .layers
            .iter()
            .enumerate()
            .filter(|(_, layer)| matches!(layer.layer_type, LayerType::Video | LayerType::Generator))
            .map(|(index, layer)| (layer.layer_id.clone(), format!("{} · {}", index + 1, layer.name)))
            .collect(),
    );
    let selected_automation = |layer_id: &manifold_core::LayerId| {
        if !selection.automation_mode_visible() { return None; }
        selection.selected_automation_point.as_ref().map(|point| (&point.target, &point.param_id))
            .or_else(|| selection.chosen_automation_params().get(layer_id).map(|(target, param_id)| (target, param_id)))
    };
    ui.inspector
        .macros_panel_mut()
        .set_ableton_ranges(&super::live_shapes::macro_ableton_ranges(project));
    // Tabs first: the Scene tab decides whether the scene panel is open.
    configure_inspector_tabs(ui, project, active_layer, selection);

    // ── Scene tab (SCENE_SURFACES D16) ──
    // Rebuilt from scratch every sync while the tab is active — no cached/
    // staged copy anywhere (D1: "no rotting, no staleness"). Bound to the
    // same layer as the tab rungs: the selection's own layer, falling back
    // to `active_layer`.
    if ui.scene_setup_panel.is_open() {
        let sel_layer_idx = selection
            .selected_layer_id_for_clip()
            .or(selection.primary_selected_layer_id())
            .and_then(|id| project.timeline.find_layer_index_by_id(id))
            .or(active_layer);
        let layer = sel_layer_idx.and_then(|i| project.timeline.layers.get(i));
        let state = super::scene::scene_state(project, layer);
        // The layer's FULL generator `ParamSurface`, built by the same
        // `gen_params_to_surface` the main inspector's generator card uses,
        // for THIS panel's bound layer (never `active_layer`). `All`: the
        // panel filters by item, so it must see every param.
        let full_params = match (&state, layer) {
            (SceneSetupState::Live(_), Some(layer)) => layer.gen_params().map(|gp| {
                let mut surface = gen_params_to_surface(
                    gp,
                    layer.layer_id.as_str(),
                    None,
                    automation_latched,
                    SurfaceVisibility::All,
                    driver_timing,
                );
                attach_project_sources(std::slice::from_mut(&mut surface), project);
                surface
            }),
            _ => None,
        };
        ui.scene_setup_panel.configure(state);
        ui.scene_setup_panel.configure_params(full_params);
    }

    // Master effects → inspector (envelopes ride on each instance)
    let mut master_configs = effects_to_surfaces(
        &project.settings.master_effects,
        OscScope::Master,
        automation_latched,
        driver_timing,
    );
    attach_project_sources(&mut master_configs, project);
    ui.inspector.configure_master_effects(&master_configs);
    ui.inspector.configure_rack_groups(
        manifold_ui::InspectorTab::Master,
        &rack_groups(
            project.settings.master_effect_groups.as_deref().unwrap_or(&[]),
            &project.settings.master_effects,
        ),
    );

    // LED composite preview (LED_STRIPS_DESIGN MVP-P4): the band lives only on
    // a DMX lane's generator card (D22). Presence is structural — set before
    // configure_gen_params below so the card build sees it this same sync.
    {
        let is_dmx = active_layer
            .and_then(|i| project.timeline.layers.get(i))
            .is_some_and(|l| l.layer_type == LayerType::Dmx);
        ui.inspector
            .set_led_preview(map_led_preview(led_preview, is_dmx));
    }

    // A failed effect-chain build shows under its layer's or the master's header.
    {
        let failures = std::sync::Arc::clone(&ui.chain_failures);
        let failure = |owner: Option<&manifold_core::LayerId>| {
            failures.iter().find(|(o, _)| o.as_ref() == owner).map(|(_, message)| &**message)
        };
        let layer_id = active_layer.and_then(|i| project.timeline.layers.get(i)).map(|l| &l.layer_id);
        ui.inspector.layer_chrome_mut().set_chain_error(layer_id.and_then(|id| failure(Some(id))));
        ui.inspector.master_chrome_mut().set_chain_error(failure(None));
    }

    // Active layer effects + gen params → inspector
    if let Some(idx) = active_layer {
        if let Some(layer) = project.timeline.layers.get(idx) {
            // AUDIO TRIGGERS (P3b, AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_
            // DESIGN.md) — the layer's own `clip_triggers`, structurally
            // configured alongside gen params/layer effects below. state_sync
            // stays the sole panel data boundary: the inspector never reads
            // `Project` directly. Row label is "{band} → {feature kind}"
            // (the design's own example, "Low → Kick").
            {
                use manifold_ui::panels::audio_trigger_section::{
                    AudioTriggerRowConfig, AudioTriggerSectionConfig,
                };
                let sends = audio_send_choices(&project.audio_setup);
                let rows: Vec<AudioTriggerRowConfig> = layer
                    .clip_triggers
                    .iter()
                    .map(|t| {
                        let feature = t.source.feature;
                        AudioTriggerRowConfig {
                            enabled: t.enabled,
                            label: format!("{} \u{2192} {}", feature.band.label(), feature.kind.label()),
                            kind_idx: feature.kind.index() as i32,
                            band_idx: feature.band.index() as i32,
                            sensitivity: t.shape.sensitivity,
                            send_id: Some(t.source.send_id.clone()),
                            one_shot_beats: t.one_shot_beats.0 as f32,
                        }
                    })
                    .collect();
                ui.inspector.audio_trigger_section_mut().configure(
                    Some(layer.layer_id.clone()),
                    &AudioTriggerSectionConfig { rows, sends },
                );
            }

            // Layer effects — envelopes ride on each effect instance now.
            let lid = layer.layer_id.as_str();
            let mut layer_effects = layer
                .effects
                .as_ref()
                .map(|e| effects_to_surfaces(e, OscScope::Layer(lid), automation_latched, driver_timing))
                .unwrap_or_default();
            if let Some((target, param_id)) = selected_automation(&layer.layer_id) {
                for surface in &mut layer_effects { mark_selected_automation_row(surface, &layer.layer_id, target, param_id); }
            }
            attach_project_sources(&mut layer_effects, project);
            ui.inspector
                .configure_layer_effects(&layer_effects, Some(&layer.layer_id));
            ui.inspector.configure_rack_groups(
                manifold_ui::InspectorTab::Layer,
                &rack_groups(layer.effect_groups.as_deref().unwrap_or(&[]), layer.effects.as_deref().unwrap_or(&[])),
            );

            // Generator params — find clip's string_params for text fields.
            // Use selected clip if on this layer, otherwise first clip.
            let clip_string_params = selection
                .primary_selected_clip_id()
                .as_ref()
                .and_then(|sel_id| layer.clips.iter().find(|c| c.id == **sel_id))
                .or_else(|| layer.clips.first())
                .and_then(|c| c.string_params.as_ref());
            let mut gen_config = layer
                .gen_params()
                .filter(|gp| *gp.generator_type() != PresetTypeId::NONE)
                .map(|gp| {
                    gen_params_to_surface(
                        gp,
                        lid,
                        clip_string_params,
                        automation_latched,
                        // The main inspector's generator CARD is curated.
                        SurfaceVisibility::CuratedCard,
                        driver_timing,
                    )
                });
            if let Some(c) = gen_config.as_mut() {
                if let Some((target, param_id)) = selected_automation(&layer.layer_id) {
                    mark_selected_automation_row(c, &layer.layer_id, target, param_id);
                }
                attach_project_sources(std::slice::from_mut(c), project);
                crate::setup_check::mark_layer_card(c, layer, &ui.setup_fits);
            }
            let layer_id = layer.layer_id.clone();
            let scene_folds = gen_config.as_ref().zip(layer.gen_params()).and_then(|(card, gp)| scene_card_folds(gp, card));
            ui.inspector
                .configure_gen_params(gen_config.as_ref(), Some(layer_id));
            ui.inspector.set_gen_scene_sections(scene_folds.as_deref());

            // Stable instances own cards; the catalog supplies the picker.
            {
                let modifier_region = (|| {
                    let def = layer
                        .generator_graph()
                        .cloned()
                        .or_else(|| {
                            manifold_nodes::bundled_presets::bundled_preset_def(
                                &layer.generator_type().clone(),
                            ).map(|def| (*def).clone())
                        })?;
                    // Only a graph with a scene root takes modifiers.
                    if super::cards::scene_roots(&def).is_empty() {
                        return None;
                    }
                    let gp = layer.gen_params()?;
                    // The picker's model rides the same configure call — one
                    // entry per registry kind, applicability read off the
                    // SAME def (the UI never reads the graph).
                    let picker = super::cards::modifier_picker_entries(&def);
                    let mut surfaces = modifier_surfaces(gp, &def, lid, automation_latched, driver_timing);
                    surfaces.retain(|surface| !super::cards::is_force_surface(surface, &def));
                    attach_project_sources(&mut surfaces, project);
                    if let Some((target, param_id)) = selected_automation(&layer.layer_id) {
                        for surface in &mut surfaces { mark_selected_automation_row(surface, &layer.layer_id, target, param_id); }
                    }
                    Some((surfaces, picker))
                })();
                match modifier_region {
                    Some((surfaces, picker)) => ui.inspector.configure_modifier_cards(
                        &surfaces,
                        Some(&layer.layer_id),
                        true,
                        picker,
                    ),
                    None => {
                        ui.inspector.configure_modifier_cards(&[], None, false, Vec::new())
                    }
                }
            }
        } else {
            ui.inspector.configure_layer_effects(&[], None);
            ui.inspector.configure_rack_groups(manifold_ui::InspectorTab::Layer, &[]);
            ui.inspector.configure_gen_params(None, None);
            ui.inspector.configure_modifier_cards(&[], None, false, Vec::new());
            ui.inspector
                .audio_trigger_section_mut()
                .configure(None, &manifold_ui::panels::audio_trigger_section::AudioTriggerSectionConfig::default());
        }
    } else {
        ui.inspector.configure_layer_effects(&[], None);
        ui.inspector.configure_rack_groups(manifold_ui::InspectorTab::Layer, &[]);
        ui.inspector.configure_gen_params(None, None);
        ui.inspector.configure_modifier_cards(&[], None, false, Vec::new());
        ui.inspector
            .audio_trigger_section_mut()
            .configure(None, &manifold_ui::panels::audio_trigger_section::AudioTriggerSectionConfig::default());
    }

    // Clip chrome → inspector (per-clip effects removed)
    if let Some(clip_id) = selection.primary_selected_clip_id() {
        let clip = project
            .timeline
            .layers
            .iter()
            .flat_map(|l| l.clips.iter())
            .find(|c| c.id == *clip_id);
        if let Some(clip) = clip {
            // Sync clip chrome MODE before build so the tree layout is correct.
            // Value sync (name, bpm, etc.) happens in push_state after build.
            let is_video = !clip.video_clip_id.is_empty();
            let is_gen = clip.generator_type != PresetTypeId::NONE;
            let is_audio = clip.is_audio();
            ui.inspector
                .clip_chrome_mut()
                .set_mode(true, is_video, is_gen, is_audio, clip.is_looping);
            // Feed the detection rows before build so the row count drives layout.
            if is_audio {
                use manifold_core::audio_clip_detection::{
                    quantize_grid_label, DetectionConfig,
                };
                use manifold_core::types::LayerType;
                use manifold_ui::panels::clip_chrome::{DetectInstrumentRow, DetectionView};

                let default_cfg;
                let (cfg, detection) = match clip.audio_detection.as_ref() {
                    Some(d) => (&d.config, Some(d)),
                    None => {
                        default_cfg = DetectionConfig::default();
                        (&default_cfg, None)
                    }
                };

                // Candidate routing layers (non-group) for the per-row dropdowns.
                let candidates: Vec<(manifold_core::LayerId, String)> = project
                    .timeline
                    .layers
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.layer_type != LayerType::Group)
                    .map(|(index, l)| (l.layer_id.clone(), format!("{} · {}", index + 1, l.name)))
                    .collect();

                let instruments = cfg
                    .instruments
                    .iter()
                    .map(|inst| {
                        let count =
                            detection.map_or(0, |d| d.count(inst.trigger_type));
                        let layer_label = inst
                            .target_layer
                            .as_ref()
                            .and_then(|id| {
                                candidates.iter().find(|(lid, _)| lid == id).map(|(_, n)| n.clone())
                            })
                            .unwrap_or_else(|| "Auto".to_string());
                        DetectInstrumentRow {
                            label: format!("{:?}", inst.trigger_type),
                            enabled: inst.enabled,
                            sensitivity: inst.sensitivity,
                            count,
                            layer_label,
                        }
                    })
                    .collect();

                let view = DetectionView {
                    quantize_label: quantize_grid_label(cfg.quantize_on, cfg.quantize_step_beats),
                    onset_ms: (cfg.onset_compensation.0 * 1000.0) as f32,
                    has_analysis: detection.is_some_and(|d| d.analysis.is_some()),
                    instruments,
                };
                ui.inspector.clip_chrome_mut().set_detection(&view);
                ui.set_clip_detect_layers(candidates);
            }
        } else {
            ui.inspector
                .clip_chrome_mut()
                .set_mode(false, false, false, false, false);
        }
    } else {
        ui.inspector
            .clip_chrome_mut()
            .set_mode(false, false, false, false, false);
    }
    if let Some((target, param)) = ui.take_trigger_response_reveal()
        && !ui.inspector.reveal_trigger_response(&target, &param)
    {
        ui.scene_setup_panel.reveal_param(&target, &param);
    }
    if let Some((target, params)) = ui.take_scene_reveal() {
        ui.scene_setup_panel.reveal_section(&target, &params);
    }
}

/// Convert a slice of `PresetInstance` into [`ParamSurface`]s for the UI.
/// Unity: EffectCardState.SyncFromDataModel — populates all data-derived visual state.
///
/// Iterates BOTH the def-declared static block AND the per-instance
/// user-tail bindings, producing one [`ParamRow`] per slot in
/// `effect.param_values` order. The card renders a slider for every
/// exposed entry; hidden static slots and unchecked user-tail entries
/// (the latter are removed from `user_param_bindings` rather than
/// hidden, so they never reach this loop) are filtered at build time.
/// Build the per-row driver + envelope + automation modulation facts for one
/// preset instance's card, one [`RowMod`] per row (D3), all sized to `n` (the
/// card's param count). Shared by the effect and generator card builders —
/// the only thing that differs between them is `resolve`, the `param_id → slot
/// index` mapping (an effect resolves via `param_id_to_value_index`, a generator
/// via its graph/registry `row_index_of`). The rows are identical; the
/// per-card `has_drv` / `has_env` summary flags stay with each caller (the
/// generator card intentionally forces them false).
///
/// `resolve` maps a modulation row's `param_id` to its card slot index.
/// `latched` is `ContentState::automation_latched_params` — checked against
/// `(inst.id, lane.param_id)` for the overridden-gray state (P4 section 7's dot).
/// Always `inst.id`, which (fixed 2026-07-11) is now also the card's own
/// DISPLAYED `effect_id` for both kinds — `preset_to_config` used to blank
/// the generator arm's `effect_id` to `EffectId::new("")`, so this function
/// and the card disagreed about a generator's identity even though both
/// ultimately read the same real, freshly-synthesized `EffectId` (see
/// `manifold_playback::automation`'s `AutomationLatches` doc comment).
/// Configure the inspector's tab rungs and open the scene panel exactly when
/// the Scene tab is the active one (SCENE_SURFACES D16).
fn configure_inspector_tabs(
    ui: &mut UIRoot,
    project: &Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
) {
    let (tabs, active) = inspector_tabs(project, active_layer, selection, ui.inspector.active_tab());
    ui.inspector.configure_tabs(&tabs, active);
    let scene = active == manifold_ui::InspectorTab::Scene;
    if scene {
        ui.scene_setup_panel.open();
    } else {
        ui.scene_setup_panel.close();
    }
    let audio = ui.audio_setup_panel.is_open();
    ui.header.set_audio_dock_open(audio);
}

/// The selection's ownership rungs (local→global) and the active one.
///
/// The rung set is derived from the SELECTION's own layer (the clip's layer
/// or the selected layer), NOT `active_layer` — which follows the active tab
/// (it points at the group when the Group rung is pinned). Deriving from the
/// stable selection keeps the full chain available no matter which rung you
/// are viewing. The active rung is the pin if one is set (a tab click), else
/// the selection-derived default. The Scene rung exists only when that layer
/// is a generator whose graph holds a `node.render_scene`; once showing, it
/// stays across a selection change to another scene layer, as the old dock did.
pub(crate) fn inspector_tabs(
    project: &Project,
    active_layer: Option<usize>,
    selection: &SelectionState,
    current: manifold_ui::InspectorTab,
) -> (Vec<manifold_ui::InspectorTab>, manifold_ui::InspectorTab) {
    use manifold_ui::InspectorTab;
    let has_clip = selection.primary_selected_clip_id().is_some();
    let sel_layer_idx = selection
        .selected_layer_id_for_clip()
        .or(selection.primary_selected_layer_id())
        .and_then(|id| project.timeline.find_layer_index_by_id(id))
        .or(active_layer);
    let layer = sel_layer_idx.and_then(|i| project.timeline.layers.get(i));
    let layer_is_group = layer.is_some_and(|l| l.layer_type == LayerType::Group);
    let has_group_parent =
        sel_layer_idx.is_some_and(|i| project.timeline.find_group_parent(i).is_some());

    let mut tabs: Vec<InspectorTab> = Vec::new();
    if has_clip {
        tabs.push(InspectorTab::Clip);
    }
    if let Some(l) = layer {
        if l.layer_type == LayerType::Group {
            tabs.push(InspectorTab::Group);
        } else {
            tabs.push(InspectorTab::Layer);
            if layer_has_scene(l) {
                tabs.push(InspectorTab::Scene);
            }
            if has_group_parent {
                tabs.push(InspectorTab::Group);
            }
        }
    }
    tabs.push(InspectorTab::Master);

    let active = selection
        .pinned_scope()
        .filter(|t| tabs.contains(t))
        .unwrap_or_else(|| {
            // Default to the LAYER scope on any selection — the layer (its
            // generator, effects, macros) is the persistent thing you tune,
            // so landing there is less jarring than the per-clip view. The
            // Clip tab is still one click away whenever a clip is selected.
            if current == InspectorTab::Scene && tabs.contains(&InspectorTab::Scene) {
                InspectorTab::Scene
            } else if layer_is_group {
                InspectorTab::Group
            } else if layer.is_some() {
                InspectorTab::Layer
            } else if has_clip {
                InspectorTab::Clip
            } else {
                InspectorTab::Master
            }
        });
    (tabs, active)
}

/// A generator layer whose graph (its own, or its bundled preset's) holds a
/// `node.render_scene` (SCENE_SURFACES D16).
fn layer_has_scene(layer: &manifold_core::layer::Layer) -> bool {
    use manifold_core::effect_graph_def::render_scene_count;
    if layer.layer_type != LayerType::Generator {
        return false;
    }
    match layer.generator_graph() {
        Some(def) => render_scene_count(&def.nodes) > 0,
        None => manifold_nodes::bundled_presets::bundled_preset_def(layer.generator_type())
            .is_some_and(|def| render_scene_count(&def.nodes) > 0),
    }
}

pub(crate) fn build_card_modulation(
    inst: &PresetInstance,
    n: usize,
    resolve: impl Fn(&str) -> Option<usize>,
    latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
    timing: (manifold_core::Bpm, f32),
) -> Vec<RowMod> {
    let mut rows = vec![RowMod::default(); n];
    if let Some(ref drivers) = inst.drivers {
        for d in drivers {
            if !d.enabled {
                continue;
            }
            let Some(pi) = resolve(d.param_id.as_ref()).filter(|&pi| pi < n) else {
                continue;
            };
            let row = &mut rows[pi];
            row.driver_active = true;
            row.trim_min = d.trim_min;
            row.trim_max = d.trim_max;
            row.driver_beat_div_idx = beat_div_to_button_index(d.beat_division.base_division());
            row.driver_waveform_idx = d.waveform as i32;
            row.driver_reversed = d.reversed;
            row.driver_dotted = d.beat_division.is_dotted();
            row.driver_triplet = d.beat_division.is_triplet();
            row.driver_free_period = d.free_period_beats;
            row.driver_frame_aligned = d.frame_aligned;
            row.driver_frame_rate = if d.frame_aligned {
                d.effective_period_frames(timing.0, timing.1)
                    .map(|frames| (frames, timing.1 / frames as f32))
            } else { None };
        }
    }
    if let Some(ref envelopes) = inst.envelopes {
        for env in envelopes {
            if !env.enabled {
                continue;
            }
            let Some(pi) = resolve(env.param_id.as_ref()).filter(|&pi| pi < n) else {
                continue;
            };
            let row = &mut rows[pi];
            row.envelope_active = true;
            row.target_norm = env.target_normalized;
            row.env_decay = env.decay_beats;
            row.envelope_action_idx = match env.action {
                manifold_core::audio_mod::TriggerAction::Continuous => 0,
                manifold_core::audio_mod::TriggerAction::Step { .. } => 1,
                manifold_core::audio_mod::TriggerAction::Random => 2,
            };
            if let manifold_core::audio_mod::TriggerAction::Step { amount, wrap } = env.action {
                row.envelope_step_amount = amount;
                row.envelope_wrap_idx = match wrap {
                    manifold_core::audio_mod::WrapMode::Wrap => 0,
                    manifold_core::audio_mod::WrapMode::Bounce => 1,
                    manifold_core::audio_mod::WrapMode::Clamp => 2,
                };
            }
        }
    }
    if let Some(ref lanes) = inst.automation_lanes {
        for lane in lanes {
            // Enabled + non-empty only (section 7: "an empty/disabled lane shows no
            // dot") — matches the sampler's own `has_lanes` gate in
            // `manifold_playback::automation`.
            if !lane.enabled || lane.points.is_empty() {
                continue;
            }
            let Some(pi) = resolve(lane.param_id.as_ref()).filter(|&pi| pi < n) else {
                continue;
            };
            let row = &mut rows[pi];
            row.automation_active = true;
            row.automation_overridden = latched
                .iter()
                .any(|(eid, pid)| *eid == inst.id && *pid == lane.param_id);
        }
    }
    rows
}

/// Translate one enabled `ParameterAudioMod` into the row-owned display state.
/// The projection assigns this scalar by stable parameter id while it builds the
/// manifest rows, so filtering or reordering a modifier card preserves the
/// complete audio configuration without a second indexed vector.
pub(crate) fn audio_row_state(
    am: &manifold_core::audio_mod::ParameterAudioMod,
    is_fire: bool,
) -> AudioRowState {
    let mut row = AudioRowState {
        active: true,
        send_id: (!am.source.send_id.is_empty()).then(|| am.source.send_id.clone()),
        range_min: am.shape.range_min,
        range_max: am.shape.range_max,
        invert: am.shape.invert,
        rate: am.shape.rate_of_change,
        sensitivity: am.shape.sensitivity,
        attack_ms: am.shape.attack_ms,
        release_ms: am.shape.release_ms,
        kind_idx: am.source.feature.kind.index() as i32,
        band_idx: am.source.feature.band.index() as i32,
        ..AudioRowState::default()
    };
    // PARAM_STEP_ACTIONS D3: an unset `trigger_mode`'s effective default
    // depends on the row and the mod's action — ordinary Fire rows and
    // Step/Random defaults use audio transients, while gate/continuous rows
    // retain the evaluator's Both fallback.
    let default_mode = if is_fire
        || !matches!(am.action, manifold_core::audio_mod::TriggerAction::Continuous)
    {
        manifold_core::audio_trigger::TriggerFireMode::Transient
    } else {
        manifold_core::audio_trigger::TriggerFireMode::Both
    };
    row.trigger_mode_idx = match am.trigger_mode.unwrap_or(default_mode) {
        manifold_core::audio_trigger::TriggerFireMode::ClipEdge => 0,
        manifold_core::audio_trigger::TriggerFireMode::Transient => 1,
        manifold_core::audio_trigger::TriggerFireMode::Both => 2,
    };
    match am.action {
        manifold_core::audio_mod::TriggerAction::Continuous => row.action_idx = 0,
        manifold_core::audio_mod::TriggerAction::Step { amount, wrap } => {
            row.action_idx = 1;
            row.step_amount = amount;
            row.wrap_idx = match wrap {
                manifold_core::audio_mod::WrapMode::Wrap => 0,
                manifold_core::audio_mod::WrapMode::Bounce => 1,
                manifold_core::audio_mod::WrapMode::Clamp => 2,
            };
        }
        manifold_core::audio_mod::TriggerAction::Random => row.action_idx = 2,
    }
    row
}

/// The driver, envelope and audio-mod facts of ONE param id on a
/// [`PresetInstance`], through the same chain every card row walks
/// ([`build_card_modulation`] + [`audio_row_state`]). The tests read a single
/// param's modulation through it.
#[cfg(test)]
pub(crate) fn lookup_param_mod_for_id(
    inst: &PresetInstance,
    param_id: &str,
    automation_latched: &[(manifold_core::EffectId, manifold_core::effects::ParamId)],
    timing: (manifold_core::Bpm, f32),
) -> (RowMod, AudioRowState) {
    let resolve = |id: &str| (id == param_id).then_some(0);
    let is_fire = inst
        .params
        .get(param_id)
        .is_some_and(|param| param.spec.is_trigger && !param.spec.is_trigger_gate);
    let audio = inst
        .audio_mods
        .iter()
        .flatten()
        .rfind(|am| am.enabled && am.param_id.as_ref() == param_id)
        .map(|am| audio_row_state(am, is_fire))
        .unwrap_or_default();
    let modulation = build_card_modulation(inst, 1, resolve, automation_latched, timing)
        .pop().expect("one requested modulation row");
    (modulation, audio)
}

#[cfg(test)]
mod param_mod_lookup_tests {
    use super::*;
    use manifold_core::effects::ParameterDriver;
    use manifold_core::types::{BeatDivision, DriverWaveform};

    fn driver_for(param_id: &str) -> ParameterDriver {
        ParameterDriver {
            param_id: std::borrow::Cow::Owned(param_id.to_string()),
            beat_division: BeatDivision::Quarter,
            waveform: DriverWaveform::Sine,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.1,
            trim_max: 0.9,
            reversed: false,
            free_period_beats: None,
            frame_aligned: false,
            legacy_param_index: None,
            is_paused_by_user: false,
        }
    }

    /// UX-P3b (SCENE_PANEL_UX_DESIGN.md sizing amendment): the reusable
    /// single-param query must find the SAME driver `preset_to_config`'s
    /// `build_card_modulation` would find for that id at its real card
    /// position — it just doesn't need that position, because it always
    /// reports at index 0.
    #[test]
    fn frame_align_readout_tracks_clock_without_rebuilding_for_small_bpm_changes() {
        let mut project = Project::default();
        project.settings.bpm = manifold_core::Bpm(164.0);
        project.settings.frame_rate = 24.0;
        let mut inst = PresetInstance::new(PresetTypeId::new("digital_plants"));
        let mut d = driver_for("intensity");
        d.frame_aligned = true;
        d.beat_division = BeatDivision::Sixteenth;
        d.waveform = DriverWaveform::Square;
        inst.drivers = Some(vec![d]);
        project.settings.master_effects.push(inst);
        let mut ui = UIRoot::new();
        assert!(!ui.sync_driver_bpm(&project, manifold_core::Bpm(164.0)));
        assert!(!ui.sync_driver_bpm(&project, manifold_core::Bpm(165.0)));
        assert!(ui.sync_driver_bpm(&project, manifold_core::Bpm(100.0)));
        let (rows, _) = lookup_param_mod_for_id(&project.settings.master_effects[0],
            "intensity", &[], (ui.driver_bpm.unwrap(), 24.0));
        assert_eq!(rows.driver_frame_rate, Some((4, 6.0)));
    }

    #[test]
    fn lookup_finds_the_named_params_driver_regardless_of_manifest_position() {
        let mut inst = PresetInstance::new(PresetTypeId::new("digital_plants"));
        inst.drivers = Some(vec![driver_for("intensity")]);

        let (modulation, _audio) = lookup_param_mod_for_id(&inst, "intensity", &[], (manifold_core::Bpm::DEFAULT, 60.0));
        assert!(modulation.driver_active);
        assert_eq!(modulation.trim_min, 0.1);
        assert_eq!(modulation.trim_max, 0.9);
    }

    /// A driver on a DIFFERENT param id must not leak into this param's slot
    /// — the query is scoped to the exact id it was asked about, not "any
    /// driver on the instance."
    #[test]
    fn lookup_ignores_drivers_on_other_param_ids() {
        let mut inst = PresetInstance::new(PresetTypeId::new("digital_plants"));
        inst.drivers = Some(vec![driver_for("fill")]);

        let (modulation, audio) = lookup_param_mod_for_id(&inst, "intensity", &[], (manifold_core::Bpm::DEFAULT, 60.0));
        assert!(!modulation.driver_active);
        assert!(!audio.active);
    }

    /// No drivers/envelopes/audio-mods at all → an idle single-slot result,
    /// not a panic (the scene panel calls this for every exposed row on
    /// every rebuild, including ones with no modulation yet).
    #[test]
    fn lookup_on_unmodulated_param_returns_idle_slot() {
        let inst = PresetInstance::new(PresetTypeId::new("digital_plants"));
        let (modulation, audio) = lookup_param_mod_for_id(&inst, "intensity", &[], (manifold_core::Bpm::DEFAULT, 60.0));
        assert!(!modulation.driver_active);
        assert!(!modulation.envelope_active);
        assert!(!audio.active);
    }
}

/// Map a base BeatDivision to its button index (0-10).
/// Reverse of BeatDivision::from_button_index.
fn beat_div_to_button_index(div: BeatDivision) -> i32 {
    match div {
        BeatDivision::ThirtySecond => 0,
        BeatDivision::Sixteenth => 1,
        BeatDivision::Eighth | BeatDivision::EighthDotted | BeatDivision::EighthTriplet => 2,
        BeatDivision::Quarter | BeatDivision::QuarterDotted | BeatDivision::QuarterTriplet => 3,
        BeatDivision::Half | BeatDivision::HalfDotted | BeatDivision::HalfTriplet => 4,
        BeatDivision::Whole | BeatDivision::WholeDotted | BeatDivision::WholeTriplet => 5,
        BeatDivision::TwoWhole | BeatDivision::TwoWholeDotted => 6,
        BeatDivision::FourWhole => 7,
        BeatDivision::EightWhole => 8,
        BeatDivision::SixteenWhole => 9,
        BeatDivision::ThirtyTwoWhole => 10,
    }
}

#[cfg(test)]
mod audio_row_state_trigger_mode_tests {
    use super::*;
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
    use manifold_core::audio_trigger::TriggerFireMode;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::effects::PresetInstance;
    use manifold_core::id::AudioSendId;
    use manifold_core::params::{Param, ParamManifest};

    /// Trigger mode shares the scalar projection used by parameter cards.
    #[test]
    fn trigger_mode_reads_off_the_mod_alongside_every_other_field() {
        let mut inst = PresetInstance::new(manifold_core::PresetTypeId::new("Strobe"));
        let mut m = ParameterAudioMod::new(
            "clip_trigger".into(),
            AudioSendId::new("send-kick"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
        );
        m.trigger_mode = Some(TriggerFireMode::Both);
        inst.audio_mods = Some(vec![m]);

        let (_, idle) = lookup_param_mod_for_id(&inst, "amount", &[], (manifold_core::Bpm::DEFAULT, 60.0));
        let (_, row) = lookup_param_mod_for_id(&inst, "clip_trigger", &[], (manifold_core::Bpm::DEFAULT, 60.0));

        assert!(!idle.active);
        assert!(row.active);
        assert_eq!(idle.send_id, None);
        assert_eq!(row.send_id, Some(AudioSendId::new("send-kick")));
        assert_eq!(row.band_idx, AudioBand::Low.index() as i32);
        assert_eq!(row.trigger_mode_idx, 2); // Both
    }

    /// A disabled mod (armed once, then disarmed via the "A" button, which
    /// flips `enabled` without clearing the rest) reads as fully inactive —
    /// the standard per-param "skip if !enabled" rule covers a trigger-gate
    /// row automatically now; no trigger-specific gate to keep in sync.
    #[test]
    fn disabled_mod_reads_as_inactive() {
        let mut inst = PresetInstance::new(manifold_core::PresetTypeId::new("Strobe"));
        let mut m = ParameterAudioMod::new(
            "clip_trigger".into(),
            AudioSendId::new("send-kick"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        m.enabled = false;
        m.trigger_mode = Some(TriggerFireMode::ClipEdge);
        inst.audio_mods = Some(vec![m]);

        let (_, row) = lookup_param_mod_for_id(&inst, "clip_trigger", &[], (manifold_core::Bpm::DEFAULT, 60.0));
        assert!(!row.active);
    }

    /// No `trigger_mode` set on the mod (defensive — section 9 U3 always arms with
    /// `Some(Both)`, but a hand-built or pre-section 9-migrated mod could carry
    /// `None`) reads the SAME `Both` fallback the evaluator uses
    /// (`unwrap_or(TriggerFireMode::Both)`), so the badge/drawer never
    /// disagrees with what actually fires.
    #[test]
    fn missing_trigger_mode_defaults_to_both() {
        let mut inst = PresetInstance::new(manifold_core::PresetTypeId::new("Strobe"));
        let m = ParameterAudioMod::new(
            "clip_trigger".into(),
            AudioSendId::new("send-kick"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        assert_eq!(m.trigger_mode, None);
        inst.audio_mods = Some(vec![m]);

        let (_, row) = lookup_param_mod_for_id(&inst, "clip_trigger", &[], (manifold_core::Bpm::DEFAULT, 60.0));
        assert_eq!(row.trigger_mode_idx, 2); // Both
    }

    #[test]
    fn ordinary_fire_missing_trigger_mode_defaults_to_transient() {
        let mut inst = PresetInstance::new(manifold_core::PresetTypeId::new("FireFixture"));
        inst.params = ParamManifest::from_params(vec![Param::user_added(ParamSpecDef {
            id: "fire".into(),
            name: "Fire".into(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            is_trigger: true,
            ..ParamSpecDef::default()
        })]);
        let m = ParameterAudioMod::new(
            "fire".into(),
            AudioSendId::new("send-kick"),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        assert_eq!(m.trigger_mode, None);
        inst.audio_mods = Some(vec![m]);

        // ParameterAudioMod::new starts Continuous. The ordinary Fire row's
        // projection still defaults to Audio/Transient so its badge matches firing.
        let (_, row) = lookup_param_mod_for_id(
            &inst,
            "fire",
            &[],
            (manifold_core::Bpm::DEFAULT, 60.0),
        );
        assert_eq!(row.trigger_mode_idx, 1); // Transient
    }

    #[test]
    fn unassigned_clip_fire_source_does_not_request_an_audio_scope() {
        let mut modulation = ParameterAudioMod::new(
            "fire".into(), AudioSendId::default(), AudioFeature::default(),
        );
        modulation.trigger_mode = Some(TriggerFireMode::ClipEdge);
        let row = audio_row_state(&modulation, true);
        assert!(row.active);
        assert_eq!(row.send_id, None);
        assert_eq!(row.trigger_mode_idx, 0);
    }
}


/// End-to-end round-trip: every fire-meter key the UI's `update_fire_meters`
/// requests must resolve in the SAME `FireMeterCapture` the content-thread
/// evaluators (`evaluate_all_audio_mods` + `LiveTriggerState::evaluate`)
/// produce for the SAME `Project`. Producer (`manifold-playback`) and
/// consumer (`ParamCardPanel`/`AudioTriggerSection` via this module's
/// `sync_inspector_data`) each independently recompute a fire-meter key from
/// an instance's identity — nothing previously verified they agree. That's
/// exactly the bug class the 2026-07-11 generator-card fix closed:
/// `preset_to_config`'s Generator arm used to display `EffectId::new("")`
/// instead of the real `inst.id`, so a generator's audio-mod meter asked for
/// a key the content thread never pushed anything under.
///
/// Builds ONE project carrying all four fire-meter-hosting shapes on a
/// single layer (so one `sync_inspector_data` + `build_in_rect` pass reaches
/// every one of them — `sync_inspector_data` only configures the ACTIVE
/// layer's effects/gen-params/clip-triggers, so spreading the shapes across
/// separate layers like `ui_snapshot::fixtures::inspector_scene` does would
/// mean only one shape's layer is ever active at a time):
///   (a) a GENERATOR instance's `is_trigger_gate` audio mod (Plasma's
///       `clip_trigger`) — the exact shape the generator-card bug shipped
///       under;
///   (b) an EFFECT's plain continuous audio mod on a non-gate param (Bloom)
///       — the shape the 2026-07-11 widening (128→512 `MAX_FIRE_METERS`,
///       `fire_meters.push` moved above the gate-mode fork) started
///       metering;
///   (c) an EFFECT's `is_trigger_gate` audio mod (Strobe's `clip_trigger`);
///   (d) a `Layer.clip_triggers` row.
/// Bloom/Strobe/Plasma are real shipping presets, chosen to mirror
/// `ui_snapshot::fixtures::inspector_scene` (the BUG-082/P3c gate scene) —
/// folded onto one layer instead of three so a single active-layer build
/// surfaces all four.
#[cfg(test)]
mod fire_meter_roundtrip_tests {
    use super::*;
    use manifold_core::audio_mod::{
        AudioBand, AudioFeature, AudioFeatureKind, AudioModSource, ParameterAudioMod,
    };
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::audio_trigger::{
        FireMeterCapture, LayerClipTrigger, TriggerFireMode, fire_meter_key_for_clip_trigger,
        fire_meter_key_for_param,
    };
    use manifold_core::layer::Layer;
    use manifold_core::project::Project;
    use manifold_core::{EffectId, LayerId, PresetTypeId, Seconds};
    use manifold_playback::live_trigger::LiveTriggerState;
    use manifold_playback::modulation::{TriggerPulse, evaluate_all_audio_mods};
    use manifold_ui::node::Rect;

    /// Zero attack/release so a mod's conditioned level snaps instantly to
    /// its raw input within ONE evaluation tick — the same pattern
    /// `manifold-playback::modulation`'s own tests use for deterministic
    /// single-tick assertions (`attach_full_range_low_mod`).
    fn instant_mod(
        param_id: &str,
        send_id: &manifold_core::AudioSendId,
        feature: AudioFeature,
    ) -> ParameterAudioMod {
        let mut m = ParameterAudioMod::new(param_id.to_string().into(), send_id.clone(), feature);
        m.shape.attack_ms = 0.0;
        m.shape.release_ms = 0.0;
        m
    }

    /// The fixture project plus the identity facts the test needs to compute
    /// each shape's expected fire-meter key.
    struct Fixture {
        project: Project,
        layer_id: LayerId,
        bloom_id: EffectId,
        bloom_param: String,
        strobe_id: EffectId,
        plasma_id: EffectId,
    }

    /// One layer carrying all four fire-meter-hosting shapes at once: a
    /// generator (Plasma) with a gate mod, an effects chain (Bloom
    /// continuous + Strobe gate) riding on top of it, and its own
    /// clip-trigger row. A generator layer carrying an effects chain is the
    /// normal MANIFOLD shape (post-processing over a procedural source), so
    /// this isn't a contrived overlap — it's just all on one layer instead
    /// of `inspector_scene`'s three, specifically so ONE `active_layer`
    /// build reaches all four.
    fn build_fixture() -> Fixture {
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();

        let mut layer = Layer::new_generator("PLASMA".into(), PresetTypeId::PLASMA, 0);
        let layer_id = layer.layer_id.clone();

        // (a) GENERATOR instance, `is_trigger_gate` mod on `clip_trigger`.
        let mut gate_on_gen = instant_mod(
            "clip_trigger",
            &send_id,
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        gate_on_gen.trigger_mode = Some(TriggerFireMode::Both);
        layer.gen_params_mut().unwrap().audio_mods = Some(vec![gate_on_gen]);
        let plasma_id = layer.gen_params().unwrap().id.clone();

        // (b) EFFECT, plain continuous mod on a non-gate param — the
        // newly-metered shape.
        let mut bloom = PresetInstance::new(PresetTypeId::BLOOM);
        bloom.init_defaults();
        let bloom_param = manifold_core::preset_definition_registry::try_get(bloom.effect_type())
            .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()))
            .expect("Bloom has at least one param");
        bloom.audio_mods = Some(vec![instant_mod(
            &bloom_param,
            &send_id,
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
        )]);
        let bloom_id = bloom.id.clone();

        // (c) EFFECT, `is_trigger_gate` mod on `clip_trigger`.
        let mut strobe = PresetInstance::new(PresetTypeId::new("Strobe"));
        strobe.init_defaults();
        let mut gate_on_fx = instant_mod(
            "clip_trigger",
            &send_id,
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
        );
        gate_on_fx.trigger_mode = Some(TriggerFireMode::Transient);
        strobe.audio_mods = Some(vec![gate_on_fx]);
        let strobe_id = strobe.id.clone();

        layer.effects = Some(vec![bloom, strobe]);

        // (d) `Layer.clip_triggers` row.
        let mut trigger = LayerClipTrigger::new(AudioModSource {
            send_id: send_id.clone(),
            feature: AudioFeature::new(AudioFeatureKind::Kick, AudioBand::Low),
        });
        trigger.enabled = true;
        trigger.shape.attack_ms = 0.0;
        trigger.shape.release_ms = 0.0;
        layer.clip_triggers.push(trigger);

        let mut project = Project::default();
        project.audio_setup.sends.push(send);
        project.timeline.layers.push(layer);

        Fixture { project, layer_id, bloom_id, bloom_param, strobe_id, plasma_id }
    }

    /// One send ("Kick") with a nonzero level on every band/feature the
    /// fixture's four mods read, so a real evaluation tick leaves every one
    /// of them in the producer's `FireMeterCapture`.
    fn hot_snapshot() -> manifold_core::audio_features::AudioFeatureSnapshot {
        let mut bands = [manifold_core::BandFeatures::default(); 4];
        bands[AudioBand::Full.index()].amplitude = 0.8; // (b) Bloom
        bands[AudioBand::Full.index()].transients = 0.8; // (a) Plasma gate
        bands[AudioBand::Low.index()].transients = 0.8; // (c) Strobe gate
        bands[AudioBand::Low.index()].kick = 0.8; // (d) clip-trigger row (Kick always reads Low)
        manifold_core::audio_features::AudioFeatureSnapshot {
            sends: vec![manifold_core::SendFeatures { bands, ..Default::default() }], ..Default::default()
        }
    }

    #[test]
    fn every_ui_requested_fire_meter_key_resolves_in_the_producer_capture() {
        let Fixture { mut project, layer_id, bloom_id, bloom_param, strobe_id, plasma_id } =
            build_fixture();

        // ── Producer: the SAME two evaluators the content thread runs every
        // tick (`manifold_playback::modulation::evaluate_all_audio_mods` for
        // every enabled param audio mod, `LiveTriggerState::evaluate` for
        // clip triggers) — see `crates/manifold-playback/tests/engine_tick.rs`
        // and `live_trigger.rs`'s own tests for the same two-call pattern.
        let snapshot = hot_snapshot();
        let dt = Seconds(1.0 / 60.0);
        let mut fire_meters = FireMeterCapture::default();
        let mut pulses: Vec<TriggerPulse> = Vec::new();
        evaluate_all_audio_mods(&mut project, &snapshot, dt, Seconds::ZERO,
            &manifold_playback::clip_controls::ClipControlFrame::default(), &mut pulses, &mut fire_meters);
        let mut live_trigger = LiveTriggerState::default();
        live_trigger.evaluate(&snapshot, &project.audio_setup, &project.timeline.layers, dt, &mut fire_meters);

        // ── Consumer: the real UI build — `sync_inspector_data` (the same
        // function `ui_snapshot::render_ui_scene` calls) configures the
        // inspector's cards + AUDIO TRIGGERS section from this SAME project,
        // then `build_in_rect` (what the graph-editor window's inspector
        // column, and the main window via `UIRoot::build`, both call)
        // materializes them into a real `UITree`.
        let mut ui = UIRoot::new();
        let mut selection = SelectionState::default();
        selection.select_layer(layer_id.clone());
        sync_inspector_data(&mut ui, &project, Some(0), &selection, &[], None);

        // Open every fixture drawer. (a)/(b)/(c) need no explicit "open": an
        // armed audio mod's drawer builds automatically — a toggle/trigger
        // row's drawer whenever its mod is enabled (`build_button_row`),
        // a slider row's whenever Audio is its only (or resolved) active
        // mod-tab (`build_param_row`), both in `param_slider_shared.rs`. Only
        // the AUDIO TRIGGERS section's clip-trigger row (d) is gated behind
        // its OWN collapse/expand UI state (`AudioTriggerSection`), which
        // `configure()` never touches — so it must be opened explicitly here.
        ui.inspector.audio_trigger_section_mut().set_collapsed(Some(false));
        ui.inspector.audio_trigger_section_mut().toggle_row_expanded(0);

        ui.build_inspector_in_rect(Rect::new(0.0, 0.0, 640.0, 4000.0));

        // Record every key the UI requests while pushing levels. `fire_level`
        // is `&dyn Fn`, not `FnMut`, so the recorder needs interior
        // mutability.
        let requested = std::cell::RefCell::new(Vec::<u64>::new());
        let record = |key: u64| -> Option<f32> {
            requested.borrow_mut().push(key);
            fire_meters.get(key)
        };
        ui.inspector.update_fire_meters(&mut ui.tree, &record, dt.0 as f32);
        let requested = requested.into_inner();

        // The four keys the PRODUCER computed for the exact same instances —
        // the ground truth the UI's own keys must agree with.
        let expected: Vec<(&str, u64)> = vec![
            (
                "(a) Plasma generator gate mod (clip_trigger)",
                fire_meter_key_for_param(plasma_id.as_str(), "clip_trigger"),
            ),
            (
                "(b) Bloom continuous mod",
                fire_meter_key_for_param(bloom_id.as_str(), &bloom_param),
            ),
            (
                "(c) Strobe gate mod (clip_trigger)",
                fire_meter_key_for_param(strobe_id.as_str(), "clip_trigger"),
            ),
            (
                "(d) layer clip-trigger row 0",
                fire_meter_key_for_clip_trigger(layer_id.as_str(), 0),
            ),
        ];

        let requested_set: std::collections::HashSet<u64> = requested.iter().copied().collect();

        // (i) every fixture drawer we opened must have had ITS EXACT expected
        // key requested — catches three distinct failure shapes in one
        // check: "meter built but never updated" and "drawer missing its
        // meter" (an audio_configs slot that resolved but whose `DrawerIds`
        // carries no Meter widget, so `update_fire_meters` silently skips
        // it) both mean the key is simply absent from `requested_set`; a
        // THIRD shape — the UI's card computed a DIFFERENT identity for the
        // same instance than the producer did (a blanked/stale `EffectId`,
        // e.g. the pre-2026-07-11 generator-card bug this test targets) —
        // also fails here: the row's drawer still opens and still requests
        // *some* key, just not this one, so `expected`'s real key is still
        // absent from `requested_set`.
        for (label, key) in &expected {
            assert!(
                requested_set.contains(key),
                "the UI never requested the expected fire-meter key for {label}. Either its \
                 drawer never built (the audio_configs slot stayed None), its DrawerIds carries \
                 no Meter widget (update_fire_meters silently skips both), OR — the producer/\
                 consumer divergence class this test exists to catch — the card computed a \
                 DIFFERENT identity key for this same instance than the content thread did (a \
                 blanked or stale EffectId/LayerId), so it requested some other key instead"
            );
        }
        assert_eq!(
            requested_set.len(),
            expected.len(),
            "expected exactly {} open fire-meter rows ({:?}) but the UI requested {} distinct \
             keys — an extra drawer built somewhere this fixture didn't arm",
            expected.len(),
            expected.iter().map(|(l, _)| *l).collect::<Vec<_>>(),
            requested_set.len(),
        );

        // (ii) every key the UI requested must resolve against the SAME
        // `FireMeterCapture` the content thread produced — the round-trip
        // proof, and the exact bug class the 2026-07-11 generator-card fix
        // closed: `preset_to_config`'s Generator arm once displayed
        // `EffectId::new("")` instead of the real `inst.id`, so a generator
        // card's audio-mod meter computed a key the content thread never
        // pushed anything under.
        for (label, key) in &expected {
            assert!(
                fire_meters.get(*key).is_some(),
                "producer/consumer key divergence for {label}: the UI's fire-meter key {key} \
                 was never recorded by the content-thread FireMeterCapture — the UI and the \
                 evaluators disagree about this instance's identity"
            );
        }
    }
}

#[cfg(test)]
mod scene_tab_tests {
    use super::sync_inspector_data;
    use crate::ui_root::UIRoot;
    use manifold_core::{LayerId, PresetTypeId, layer::Layer, project::Project};
    use manifold_ui::InspectorTab;

    /// Layer 0 holds the bundled Scene preset (a `node.render_scene`); layer 1
    /// is a plain Plasma generator; layer 2 is a second scene layer.
    fn project() -> (Project, LayerId, LayerId, LayerId) {
        let mut project = Project::default();
        let scene = Layer::new_generator("Scene".into(), PresetTypeId::new("Scene"), 0);
        let plain = Layer::new_generator("Plasma".into(), PresetTypeId::new("Plasma"), 1);
        let other = Layer::new_generator("Scene 2".into(), PresetTypeId::new("Scene"), 2);
        let ids = (scene.layer_id.clone(), plain.layer_id.clone(), other.layer_id.clone());
        project.timeline.layers.extend([scene, plain, other]);
        (project, ids.0, ids.1, ids.2)
    }

    fn sync(ui: &mut UIRoot, project: &Project, selection: &manifold_ui::UIState) {
        sync_inspector_data(ui, project, None, selection, &[], None);
    }

    #[test]
    fn scene_tab_present_only_for_scene_layer() {
        let (project, scene, plain, _) = project();
        let mut selection = manifold_ui::UIState::new();
        selection.select_layer(scene);
        let (tabs, _) = super::inspector_tabs(&project, None, &selection, InspectorTab::Layer);
        assert_eq!(tabs, [InspectorTab::Layer, InspectorTab::Scene, InspectorTab::Master]);

        selection.select_layer(plain);
        let (tabs, _) = super::inspector_tabs(&project, None, &selection, InspectorTab::Layer);
        assert_eq!(tabs, [InspectorTab::Layer, InspectorTab::Master]);
    }

    #[test]
    fn scene_tab_falls_back_to_layer() {
        let (project, scene, plain, other) = project();
        let mut ui = UIRoot::new();
        let mut selection = manifold_ui::UIState::new();
        selection.select_layer(scene);
        selection.pin_scope(InspectorTab::Scene);
        sync(&mut ui, &project, &selection);
        assert_eq!(ui.inspector.active_tab(), InspectorTab::Scene);
        assert!(ui.scene_setup_panel.is_open(), "the Scene tab hosts the scene panel");

        // Another scene layer keeps the Scene tab, as the old dock stayed open.
        selection.select_layer(other.clone());
        sync(&mut ui, &project, &selection);
        assert_eq!(ui.inspector.active_tab(), InspectorTab::Scene);
        assert_eq!(ui.scene_setup_panel.live_layer_id(), Some(&other));

        // A layer without a scene falls back to the Layer tab and closes the panel.
        selection.select_layer(plain);
        sync(&mut ui, &project, &selection);
        assert_eq!(ui.inspector.active_tab(), InspectorTab::Layer);
        assert!(!ui.scene_setup_panel.is_open());

        // A pinned Scene tab with no scene on the layer is ignored.
        selection.pin_scope(InspectorTab::Scene);
        sync(&mut ui, &project, &selection);
        assert_eq!(ui.inspector.active_tab(), InspectorTab::Layer);
        assert!(!ui.scene_setup_panel.is_open());
    }
}

/// The Audio Setup dock: current device, send rows and the reliability
/// warning, while it is open. Queries the audio directory, so it runs only
/// when the planner keys it (AudioSetup, Layers, Effects, the directory
/// version, or the dock opening).
pub fn sync_audio_dock(ui: &mut UIRoot, project: &Project) {
    if !ui.audio_setup_panel.is_open() {
        return;
    }
    use manifold_core::AudioSourceKind;
    use manifold_ui::panels::audio_setup_panel::AudioSendRow;
    manifold_core::work_count::record_work(manifold_core::work_count::WorkKind::DeviceQuery);
    let dir = manifold_audio::directory::system_directory();
    // Only a hardware input device resolves to a DeviceInfo. Tap sources
    // (system / app output) don't live in the input-device list, and "none"
    // (no capture) has nothing to resolve — a tap's liveness is checked
    // separately below.
    let device = match &project.audio_setup.device {
        Some(d) if d.kind == AudioSourceKind::InputDevice => {
            dir.resolve(d.uid_opt(), Some(&d.name))
        }
        Some(_) => None, // a tap or "none" — no DeviceInfo
        None => dir.list_input_devices().into_iter().find(|d| d.is_default),
    };
    let sends = project
        .audio_setup
        .sends
        .iter()
        .map(|s| {
            // Read-only source view: the full routing lines (capture device +
            // each feeding layer) for the Inputs section. Routing is edited
            // elsewhere — layers from the layer header; channels are always
            // stereo now, so there is no per-send channel control to resolve.
            let cap = s.has_capture();
            let layer_name = |lid: &manifold_core::LayerId| {
                project
                    .timeline
                    .layers
                    .iter()
                    .find(|l| &l.layer_id == lid)
                    .map(|l| l.name.clone())
                    .unwrap_or_else(|| {
                        manifold_ui::panels::audio_setup_panel::MISSING_LAYER_LABEL.to_string()
                    })
            };
            // Full routing lines for the read-only Inputs section. A
            // capture-fed send is always stereo, so its line reads
            // "Capture • Stereo" rather than naming individual channels.
            let mut routings: Vec<String> = Vec::new();
            if cap {
                routings.push("Capture \u{2022} Stereo".to_string());
            }
            for lid in s.layers() {
                routings.push(format!("Layer \u{2022} {}", layer_name(lid)));
            }
            // Consumers section: named audio mods (param gate/continuous
            // cards) plus enabled layer-owned `LayerClipTrigger` configs
            // (P3, D2 — the matrix's per-band route walk is gone; clip
            // triggers are authored on the layer only). Both are purely
            // navigational rows (D3): click selects the owning layer.
            let clip_triggers = project.clip_trigger_consumers(&s.id);
            let has_clip_triggers = !clip_triggers.is_empty();
            let consumers: Vec<manifold_ui::panels::audio_setup_panel::SendConsumerRow> =
                project
                    .audio_mod_consumers(&s.id)
                    .into_iter()
                    .chain(clip_triggers)
                    .map(|(layer_id, label)| {
                        manifold_ui::panels::audio_setup_panel::SendConsumerRow { label, layer_id }
                    })
                    .collect();
            // Inputs section: audio layers feeding this send (id + name).
            let feeding_layers: Vec<(manifold_core::LayerId, String)> =
                s.layers().iter().map(|lid| (lid.clone(), layer_name(lid))).collect();

            AudioSendRow {
                id: s.id.clone(),
                label: s.label.clone(),
                channels: s.channels.clone(),
                gain_db: s.gain_db,
                floor_db: s.floor_db,
                driven_count: project.audio_send_usage_count(&s.id),
                routings,
                has_clip_triggers,
                feeding_layers,
                consumers,
            }
        })
        .collect();

    // Surface a reliability warning: a chosen source that can't capture right
    // now — a device that won't resolve / reads offline, a tap on an OS that
    // can't tap, an app that isn't running — else a blocked mic permission.
    let status_warning = match &project.audio_setup.device {
        Some(d) => match d.kind {
            AudioSourceKind::InputDevice => match &device {
                None => Some(format!("\u{26A0} \"{}\" is offline or unplugged", d.name)),
                Some(info) if !info.is_alive => {
                    Some(format!("\u{26A0} \"{}\" is offline", info.name))
                }
                _ => None,
            },
            // No capture is a deliberate choice, not a warning.
            AudioSourceKind::None => None,
            AudioSourceKind::SystemAudio => (!dir.tap_capabilities().system_audio)
                .then(|| "\u{26A0} System audio capture needs macOS 14.4+".to_string()),
            AudioSourceKind::App => dir
                .resolve_app(d.uid_opt().unwrap_or(""))
                .is_none()
                .then(|| format!("\u{26A0} \"{}\" isn't running", d.name)),
        },
        None => None,
    }
    .or_else(|| {
        (!manifold_audio::permission::status().is_usable())
            .then(|| "\u{26A0} Microphone access blocked — check System Settings".to_string())
    });

    ui.audio_setup_panel.configure(
        project
            .audio_setup
            .device
            .as_ref()
            .map(crate::ui_translate::audio_device_ref_to_ui),
        sends,
        status_warning,
    );
}
