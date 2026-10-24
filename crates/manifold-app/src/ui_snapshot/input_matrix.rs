//! `projection_input_matrix` (UI_INVALIDATION_DESIGN.md D10, INV-16).
//!
//! Loads a fixture, perturbs one project part or one `FrameInputs` field at a
//! time, runs each projection and push on the base and the perturbed state,
//! and records which outputs changed. A rebuild dependency is a projection
//! whose built output changed (every push re-run from the base state on both
//! sides, so a value the push owns never counts); an in-place dependency is a
//! push whose output changed. Every observed dependency must be a row in
//! `plan_frame`; Values, Shapes or MappingRanges as a rebuild dependency
//! fails; a row no perturbation observed is printed for review.

use std::collections::BTreeSet;

use manifold_core::change::{ChangeSet, Part};
use manifold_core::effects::ParamId;
use manifold_core::{Beats, LayerId};

use super::fixtures::{self, SceneData};
use super::{LOGICAL_H, LOGICAL_W, sync_build};
use crate::frame_plan::{FrameInputs, FramePlanner, Projection};
use crate::gesture_preview::UiEdits;
use crate::ui_bridge::TransportDisplayCache;
use crate::ui_root::UIRoot;

/// One state change and the parts it declares (empty for a UI-only change).
struct Perturbation {
    name: &'static str,
    parts: ChangeSet,
    apply: fn(&mut SceneData),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Rebuild,
    InPlace,
}

const REBUILD: [Projection; 5] = [
    Projection::ProjectSync,
    Projection::InspectorSync,
    Projection::ClipPositions,
    Projection::LaneGeometry,
    Projection::AudioDevices,
];
/// `LiveShapes` stands for the live push's Shapes group, which the planner
/// runs on the Shapes or the MappingRanges stamp.
const PUSHES: [Projection; 6] = [
    Projection::SelectionPush,
    Projection::ChromePush,
    Projection::ClockPush,
    Projection::LiveValues,
    Projection::LiveShapes,
    Projection::SceneEyes,
];

fn planned(plan: &crate::frame_plan::FramePlan, p: Projection) -> bool {
    plan.projections.contains(p)
        || (p == Projection::LiveShapes && plan.projections.contains(Projection::LiveRanges))
}

fn clone_data(data: &SceneData) -> SceneData {
    SceneData {
        project: data.project.clone(),
        content: data.content.clone(),
        active: data.active,
        selection: data.selection.clone(),
    }
}

fn base() -> SceneData {
    let mut data = fixtures::build("automation").expect("automation scene");
    // The inspector shows the flowers layer's Mirror and Bloom cards.
    data.active = Some(1);
    data
}

/// How the harness UI opens: plain, or with every live-scalar display on
/// screen (the Audio Setup dock open, the first clip trigger's drawer open).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shown {
    Plain,
    LiveDisplays,
}

fn fresh_ui(data: &SceneData, shown: Shown) -> UIRoot {
    let mut ui = UIRoot::new();
    ui.resize(LOGICAL_W, LOGICAL_H);
    if shown == Shown::LiveDisplays {
        // Unfolds the macros and master chrome (`live_base` settings).
        ui.apply_project_layout(&data.project.settings);
        ui.audio_setup_panel.open();
        ui.layout.audio_setup_width = manifold_ui::color::DEFAULT_AUDIO_SETUP_WIDTH;
        ui.inspector.audio_trigger_section_mut().expand_row(0);
    }
    sync_build(&mut ui, data, 24.0);
    ui
}

fn active_id(data: &SceneData) -> Option<LayerId> {
    data.active
        .and_then(|i| data.project.timeline.layers.get(i))
        .map(|l| l.layer_id.clone())
}

fn fingerprint(ui: &UIRoot) -> String {
    let tree = super::dump::dump_tree_ex(&ui.tree, &[]);
    let mut clips = Vec::new();
    ui.viewport.visible_clip_rects(&mut clips);
    format!(
        "{tree}\n{clips:?}\n{:?}",
        ui.viewport.automation_lane_screens()
    )
}

fn push(ui: &mut UIRoot, data: &SceneData, which: Projection) {
    use crate::ui_bridge::LiveGroups;
    let mut cache = TransportDisplayCache::new();
    match which {
        Projection::SelectionPush => {
            crate::ui_bridge::push_selection(ui, &data.project, data.active, &data.selection)
        }
        // As `apply_frame_plan` runs it: the push, then the bars lay out.
        Projection::ChromePush => {
            crate::ui_bridge::push_chrome(
                ui,
                &data.project,
                &data.content,
                &data.selection,
                false,
                None,
                &mut cache,
            );
            assert!(
                ui.lay_out_chrome().is_empty(),
                "a chrome push changed a bar's structure"
            );
        }
        Projection::ClockPush => {
            crate::ui_bridge::push_clock(ui, &data.project, &data.content, &mut cache)
        }
        Projection::LiveValues => crate::ui_bridge::push_live(
            ui,
            &data.project,
            &data.content,
            data.active,
            LiveGroups {
                values: true,
                shapes: false,
            },
        ),
        Projection::LiveShapes => crate::ui_bridge::push_live(
            ui,
            &data.project,
            &data.content,
            data.active,
            LiveGroups {
                values: false,
                shapes: true,
            },
        ),
        Projection::SceneEyes => crate::ui_bridge::sync_scene_row_values(ui, &data.project),
        other => unreachable!("{other:?} is not a push"),
    }
}

fn push_all(ui: &mut UIRoot, data: &SceneData) {
    crate::ui_bridge::intake_overlay_state(ui, &data.content);
    crate::ui_bridge::drain_events(ui, &data.project, &data.content);
    for which in PUSHES {
        push(ui, data, which);
    }
    ui.update();
}

fn project(ui: &mut UIRoot, data: &mut SceneData, which: Projection) {
    match which {
        Projection::ProjectSync => {
            crate::ui_bridge::sync_automation_lane_order(&data.project, &mut data.selection);
            crate::ui_bridge::sync_project_all(ui, &data.project, &data.selection);
        }
        Projection::InspectorSync => crate::ui_bridge::sync_inspector_data(
            ui,
            &data.project,
            data.active,
            &data.selection,
            &data.content.automation_latched_params,
            data.content.led_preview.as_ref(),
        ),
        Projection::ClipPositions => crate::ui_bridge::sync_clip_positions(ui, &data.project),
        Projection::LaneGeometry => crate::ui_bridge::sync_automation_lane_geometry(
            ui,
            &data.project,
            data.selection.automation_mode_visible(),
            data.selection.chosen_automation_params(),
            data.selection.automation_lane_order(),
            data.selection.hidden_automation_lanes(),
            data.selection.pinned_automation_lanes(),
        ),
        Projection::AudioDevices => crate::ui_bridge::sync_audio_dock(ui, &data.project),
        other => unreachable!("{other:?} is not a projection"),
    }
}

/// The output of `which` run on `data`, with every push from `base`.
fn rebuild_output(base: &SceneData, data: &SceneData, which: Projection, shown: Shown) -> String {
    let mut ui = fresh_ui(base, shown);
    let mut data = clone_data(data);
    project(&mut ui, &mut data, which);
    ui.build();
    push_all(&mut ui, base);
    fingerprint(&ui)
}

fn push_output(base: &SceneData, data: &SceneData, which: Projection, shown: Shown) -> String {
    let mut ui = fresh_ui(base, shown);
    push(&mut ui, data, which);
    ui.update();
    fingerprint(&ui)
}

fn mirror_param(data: &SceneData) -> (manifold_core::EffectId, ParamId) {
    let flowers = &data.project.timeline.layers[1];
    let mirror = &flowers.effects.as_ref().expect("flowers carries Mirror")[0];
    let lane = &mirror
        .automation_lanes
        .as_ref()
        .expect("Mirror is automated")[0];
    (mirror.id.clone(), lane.param_id.clone())
}

fn perturbations() -> Vec<Perturbation> {
    vec![
        Perturbation {
            name: "time signature",
            parts: ChangeSet::of(&[Part::Settings]),
            apply: |d| d.project.settings.time_signature_numerator = 3,
        },
        Perturbation {
            name: "bpm",
            parts: ChangeSet::of(&[Part::Settings]),
            apply: |d| d.project.settings.bpm = manifold_core::Bpm(97.0),
        },
        Perturbation {
            name: "frame rate",
            parts: ChangeSet::of(&[Part::Settings]),
            apply: |d| d.project.settings.frame_rate = 24.0,
        },
        Perturbation {
            name: "clip moved",
            parts: ChangeSet::of(&[Part::Clips]),
            apply: |d| {
                let clip = &mut d.project.timeline.layers[1].clips[1];
                clip.start_beat += Beats(4.0);
            },
        },
        Perturbation {
            name: "clip added",
            parts: ChangeSet::of(&[Part::Clips]),
            apply: |d| {
                d.project.timeline.layers[0].clips.push(
                    manifold_core::clip::TimelineClip::new_video(
                        "EXTRA".into(),
                        Beats(48.0),
                        Beats(4.0),
                        manifold_core::Seconds::ZERO,
                    ),
                );
            },
        },
        Perturbation {
            name: "layer renamed",
            parts: ChangeSet::of(&[Part::Layers]),
            apply: |d| d.project.timeline.layers[1].name = "PETALS".into(),
        },
        Perturbation {
            name: "layer muted",
            parts: ChangeSet::of(&[Part::Layers]),
            apply: |d| d.project.timeline.layers[1].is_muted = true,
        },
        Perturbation {
            name: "automation lane added",
            parts: ChangeSet::of(&[Part::Automation]),
            apply: |d| {
                let mirror = &mut d.project.timeline.layers[1].effects.as_mut().unwrap()[0];
                let automated: Vec<String> = mirror
                    .automation_lanes
                    .iter()
                    .flatten()
                    .map(|l| l.param_id.as_ref().to_string())
                    .collect();
                let pid = mirror
                    .params
                    .iter()
                    .map(|p| p.spec.id.to_string())
                    .find(|id| !automated.contains(id));
                let lanes = mirror.automation_lanes.get_or_insert_with(Vec::new);
                if let Some(pid) = pid {
                    lanes.push(manifold_core::effects::AutomationLane {
                        param_id: ParamId::from(pid),
                        enabled: true,
                        points: vec![manifold_core::effects::AutomationPoint {
                            beat: Beats(0.0),
                            value: 0.0,
                            shape: manifold_core::effects::SegmentShape::Linear,
                        }],
                    });
                }
            },
        },
        Perturbation {
            name: "automation point moved",
            parts: ChangeSet::of(&[Part::Automation]),
            apply: |d| {
                let mirror = &mut d.project.timeline.layers[1].effects.as_mut().unwrap()[0];
                mirror.automation_lanes.as_mut().unwrap()[0].points[1].value = 0.2;
            },
        },
        Perturbation {
            name: "effect added",
            parts: ChangeSet::of(&[Part::Effects]),
            apply: |d| {
                let mut fx = manifold_core::effects::PresetInstance::new(
                    manifold_core::PresetTypeId::new("Mirror"),
                );
                fx.init_defaults();
                d.project.timeline.layers[1]
                    .effects
                    .as_mut()
                    .unwrap()
                    .push(fx);
            },
        },
        Perturbation {
            name: "effect param value",
            parts: ChangeSet::of(&[Part::Values]),
            apply: |d| {
                let (_, pid) = mirror_param(d);
                let mirror = &mut d.project.timeline.layers[1].effects.as_mut().unwrap()[0];
                if let Some(param) = mirror.params.get_mut(pid.as_ref()) {
                    let value = (param.base + 0.37).min(param.spec.max);
                    param.base = value;
                    param.value = value;
                }
            },
        },
        Perturbation {
            name: "marker added",
            parts: ChangeSet::of(&[Part::Markers]),
            apply: |d| {
                d.project
                    .timeline
                    .markers
                    .push(manifold_core::marker::TimelineMarker::new(Beats(8.0)));
            },
        },
        Perturbation {
            name: "master opacity",
            parts: ChangeSet::of(&[Part::Settings]),
            apply: |d| d.project.settings.master_opacity = 0.4,
        },
        Perturbation {
            name: "select a clip (identity)",
            parts: ChangeSet::NONE,
            apply: |d| {
                let clip = d.project.timeline.layers[0].clips[1].id.clone();
                d.selection
                    .select_clip(clip, d.project.timeline.layers[0].layer_id.clone());
            },
        },
        Perturbation {
            name: "insert cursor",
            parts: ChangeSet::NONE,
            apply: |d| {
                let layer = d.project.timeline.layers[2].layer_id.clone();
                d.selection.set_insert_cursor(Beats(12.0), layer);
            },
        },
        Perturbation {
            name: "active layer",
            parts: ChangeSet::NONE,
            apply: |d| d.active = Some(2),
        },
        Perturbation {
            name: "lanes hidden",
            parts: ChangeSet::NONE,
            apply: |d| d.selection.set_automation_mode_visible(false),
        },
        // The playhead moved, as the content clock moves it while playing.
        Perturbation {
            name: "playhead",
            parts: ChangeSet::NONE,
            apply: |d| {
                d.content.current_beat = Beats(9.5);
                d.content.current_time = manifold_core::Seconds(4.75);
            },
        },
        // The transport's DRAW toggle, as dispatch and the shortcut apply it.
        Perturbation {
            name: "draw mode",
            parts: ChangeSet::NONE,
            apply: |d| {
                d.selection.automation_draw_mode = !d.selection.automation_draw_mode;
                d.selection.set_automation_mode_visible(true);
            },
        },
        Perturbation {
            name: "point selected",
            parts: ChangeSet::NONE,
            apply: |d| {
                let (fx, pid) = mirror_param(d);
                d.selection.selected_automation_point =
                    Some(manifold_ui::view::UiAutomationPointRef {
                        target: manifold_ui::view::UiGraphTarget::Effect(fx),
                        param_id: pid,
                        beat: Beats(16.0),
                        value_norm: 0.9,
                    });
                d.selection.note_automation_selection_change();
            },
        },
    ]
}

/// What one matrix run saw.
struct MatrixRun {
    observed: BTreeSet<(Kind, String, &'static str)>,
    failures: Vec<String>,
    seen_rows: BTreeSet<String>,
}

fn run_matrix(base: &SceneData, perturbations: Vec<Perturbation>, shown: Shown) -> MatrixRun {
    let mut run = MatrixRun {
        observed: BTreeSet::new(),
        failures: Vec::new(),
        seen_rows: BTreeSet::new(),
    };

    let base_rebuild: Vec<String> = REBUILD
        .iter()
        .map(|&p| rebuild_output(base, base, p, shown))
        .collect();
    let base_push: Vec<String> = PUSHES
        .iter()
        .map(|&p| push_output(base, base, p, shown))
        .collect();
    // The harness is deterministic: the same state twice gives the same output.
    assert_eq!(
        base_rebuild[0],
        rebuild_output(base, base, Projection::ProjectSync, shown)
    );

    for perturbation in perturbations {
        let mut data = clone_data(base);
        (perturbation.apply)(&mut data);

        // What the planner sees: one planner, base then perturbed.
        let mut planner = FramePlanner::default();
        let mut base_sel = base.selection.clone();
        let mut base_edits = UiEdits::default();
        let ui = fresh_ui(base, shown);
        let last = crate::ui_frame::gather_frame_inputs(
            &mut planner,
            &ui,
            &mut base_sel,
            &active_id(base),
            &base_edits,
            &base.content,
            crate::frame_plan::ChromeSource {
                project: &base.project,
                is_dirty: false,
                project_path: None,
                automation_draw_mode: base.selection.automation_draw_mode,
            },
        );
        base_edits.stamps.bump(perturbation.parts);
        let mut pert_sel = data.selection.clone();
        let next: FrameInputs = crate::ui_frame::gather_frame_inputs(
            &mut planner,
            &ui,
            &mut pert_sel,
            &active_id(&data),
            &base_edits,
            &data.content,
            crate::frame_plan::ChromeSource {
                project: &data.project,
                is_dirty: false,
                project_path: None,
                automation_draw_mode: data.selection.automation_draw_mode,
            },
        );
        let plan = crate::frame_plan::plan_frame(&last, &next);

        for (i, &p) in REBUILD.iter().enumerate() {
            if rebuild_output(base, &data, p, shown) != base_rebuild[i] {
                run.observed
                    .insert((Kind::Rebuild, format!("{p:?}"), perturbation.name));
                run.seen_rows.insert(format!("{p:?}"));
                if !planned(&plan, p) {
                    run.failures.push(format!(
                        "rebuild: {} changes {p:?}, the plan does not run it",
                        perturbation.name
                    ));
                }
                if perturbation.parts.intersects(ChangeSet::LIVE) {
                    run.failures.push(format!(
                        "live scalar {} is a rebuild dependency of {p:?}",
                        perturbation.name
                    ));
                }
            }
        }
        for (i, &p) in PUSHES.iter().enumerate() {
            if push_output(base, &data, p, shown) != base_push[i] {
                run.observed
                    .insert((Kind::InPlace, format!("{p:?}"), perturbation.name));
                run.seen_rows.insert(format!("{p:?}"));
                if !planned(&plan, p) {
                    run.failures.push(format!(
                        "in place: {} changes {p:?}, the plan does not run it",
                        perturbation.name
                    ));
                }
            }
        }
    }

    for (kind, projection, name) in &run.observed {
        println!("observed {kind:?} {projection} <- {name}");
    }
    run
}

#[test]
fn projection_input_matrix() {
    let run = run_matrix(&base(), perturbations(), Shown::Plain);
    assert!(
        run.observed
            .contains(&(Kind::InPlace, "ChromePush".to_string(), "draw mode")),
        "the DRAW button repaints from the chrome push"
    );
    assert!(
        run.observed
            .contains(&(Kind::InPlace, "ClockPush".to_string(), "playhead")),
        "the header clock follows the playhead through the clock push"
    );
    for p in REBUILD.iter().chain(PUSHES.iter()) {
        if !run.seen_rows.contains(&format!("{p:?}")) {
            println!("row never observed: {p:?}");
        }
    }
    assert!(
        run.failures.is_empty(),
        "dependencies missing from the plan:\n{}",
        run.failures.join("\n")
    );
}

// ── Live scalars (D22, P4d batch 0) ───────────────────────────────

/// The Audio Setup fixture with one of every live-scalar display on screen:
/// the active layer's Bloom card carries an audio mod, a Color Grade card
/// below it carries a driver, an envelope and an Ableton mapping on three
/// rows, the layer has a clip trigger, macro 1 has an Ableton mapping, and
/// the KICK audio layer shows its gain.
fn live_base() -> SceneData {
    use manifold_core::effects::{ParamEnvelope, ParameterDriver, PresetInstance};
    use manifold_core::types::{BeatDivision, DriverWaveform};
    let mut data = fixtures::build("audiosends").expect("audiosends scene");
    data.active = Some(1);
    let send = data.project.audio_setup.sends[0].id.clone();
    let bloom_layer = &mut data.project.timeline.layers[1];
    let mut grade = PresetInstance::new(manifold_core::PresetTypeId::from_string(
        "ColorGrade".to_string(),
    ));
    grade.init_defaults();
    let ids: Vec<String> = grade.params.iter().map(|p| p.id().to_string()).collect();
    assert!(ids.len() >= 3, "Color Grade has three rows to arm");
    grade.drivers = Some(vec![ParameterDriver::new(
        ids[0].clone(),
        BeatDivision::Quarter,
        DriverWaveform::Sine,
    )]);
    grade.envelopes = Some(vec![ParamEnvelope::new(ids[1].clone())]);
    grade.ableton_mappings = Some(vec![ableton_mapping(&ids[2])]);
    bloom_layer
        .effects
        .as_mut()
        .expect("the Bloom layer has Bloom")
        .push(grade);
    bloom_layer
        .clip_triggers
        .push(manifold_core::audio_trigger::LayerClipTrigger::new(
            manifold_core::audio_mod::AudioModSource {
                send_id: send,
                feature: manifold_core::audio_mod::AudioFeature::new(
                    manifold_core::audio_mod::AudioFeatureKind::Amplitude,
                    manifold_core::audio_mod::AudioBand::Low,
                ),
            },
        ));
    data.project.settings.macro_bank.slots[0].ableton_mapping = Some(ableton_mapping(""));
    data.project.settings.macros_collapsed = false;
    data.project.settings.master_chrome_collapsed = false;
    data
}

fn ableton_mapping(param: &str) -> manifold_core::ableton_mapping::AbletonParamMapping {
    use manifold_core::ableton_mapping::*;
    AbletonParamMapping {
        param_id: param.to_string().into(),
        address: AbletonMacroAddress {
            track_id: 0,
            device_id: 0,
            param_id: 1,
            device_identity: AbletonDeviceIdentity {
                device_class_name: "AudioEffectGroupDevice".into(),
            },
            track_name: "1-Audio".into(),
            device_name: "Rack".into(),
            macro_name: "Macro 1".into(),
        },
        range_min: 0.2,
        range_max: 0.8,
        inverted: false,
        legacy_param_index: None,
        last_value: 0.0,
        status: AbletonMappingStatus::Active,
    }
}

fn bloom(d: &mut SceneData) -> &mut manifold_core::effects::PresetInstance {
    &mut d.project.timeline.layers[1].effects.as_mut().unwrap()[0]
}

fn grade(d: &mut SceneData) -> &mut manifold_core::effects::PresetInstance {
    &mut d.project.timeline.layers[1].effects.as_mut().unwrap()[1]
}

fn live_perturbations() -> Vec<Perturbation> {
    let shapes = ChangeSet::of(&[Part::Shapes]);
    let ranges = ChangeSet::of(&[Part::MappingRanges]);
    let values = ChangeSet::of(&[Part::Values]);
    vec![
        Perturbation {
            name: "driver trim",
            parts: shapes,
            apply: |d| {
                let driver = &mut grade(d).drivers.as_mut().unwrap()[0];
                (driver.trim_min, driver.trim_max) = (0.25, 0.6);
            },
        },
        Perturbation {
            name: "envelope target",
            parts: shapes,
            apply: |d| grade(d).envelopes.as_mut().unwrap()[0].target_normalized = 0.3,
        },
        Perturbation {
            name: "envelope decay",
            parts: shapes,
            apply: |d| grade(d).envelopes.as_mut().unwrap()[0].decay_beats = 3.5,
        },
        Perturbation {
            name: "audio range",
            parts: shapes,
            apply: |d| {
                let shape = &mut bloom(d).audio_mods.as_mut().unwrap()[0].shape;
                (shape.range_min, shape.range_max) = (0.1, 0.7);
            },
        },
        Perturbation {
            name: "audio sensitivity",
            parts: shapes,
            apply: |d| bloom(d).audio_mods.as_mut().unwrap()[0].shape.sensitivity = 2.5,
        },
        Perturbation {
            name: "audio attack",
            parts: shapes,
            apply: |d| bloom(d).audio_mods.as_mut().unwrap()[0].shape.attack_ms = 120.0,
        },
        Perturbation {
            name: "audio release",
            parts: shapes,
            apply: |d| bloom(d).audio_mods.as_mut().unwrap()[0].shape.release_ms = 400.0,
        },
        Perturbation {
            name: "audio invert",
            parts: shapes,
            apply: |d| {
                let shape = &mut bloom(d).audio_mods.as_mut().unwrap()[0].shape;
                shape.invert = !shape.invert;
            },
        },
        Perturbation {
            name: "ableton range",
            parts: ranges,
            apply: |d| {
                let mapping = &mut grade(d).ableton_mappings.as_mut().unwrap()[0];
                (mapping.range_min, mapping.range_max) = (0.4, 0.9);
            },
        },
        Perturbation {
            name: "macro ableton range",
            parts: ranges,
            apply: |d| {
                let mapping = d.project.settings.macro_bank.slots[0]
                    .ableton_mapping
                    .as_mut()
                    .unwrap();
                (mapping.range_min, mapping.range_max) = (0.1, 0.5);
            },
        },
        Perturbation {
            name: "layer audio gain",
            parts: shapes,
            apply: |d| d.project.timeline.layers[0].audio_gain_db = -6.0,
        },
        Perturbation {
            name: "send gain",
            parts: shapes,
            apply: |d| d.project.audio_setup.sends[0].gain_db = 6.0,
        },
        Perturbation {
            name: "clip trigger sensitivity",
            parts: shapes,
            apply: |d| {
                d.project.timeline.layers[1].clip_triggers[0]
                    .shape
                    .sensitivity = 3.0
            },
        },
        Perturbation {
            name: "card param value",
            parts: values,
            apply: |d| {
                let fx = bloom(d);
                let id = fx.params.iter().next().unwrap().id().to_string();
                let param = fx.params.get_mut(&id).unwrap();
                let value = (param.base + 0.37).min(param.spec.max);
                (param.base, param.value) = (value, value);
            },
        },
        Perturbation {
            name: "macro value",
            parts: values,
            apply: |d| d.project.settings.macro_bank.slots[0].value = 0.66,
        },
    ]
}

/// D22: every live-scalar display updates in place from a Values, Shapes or
/// MappingRanges change alone. Each perturbation must change a live push
/// group the planner runs for it, and must not be a rebuild dependency, so
/// a command that touches only live scalars repaints with no structural sync.
#[test]
fn live_scalars_update_in_place() {
    let run = run_matrix(&live_base(), live_perturbations(), Shown::LiveDisplays);
    let mut failures = run.failures;
    for perturbation in live_perturbations() {
        let pushed = run.observed.iter().any(|(kind, projection, name)| {
            *kind == Kind::InPlace
                && *name == perturbation.name
                && (projection == "LiveValues" || projection == "LiveShapes")
        });
        if !pushed {
            failures.push(format!(
                "{} shows nowhere the live push writes",
                perturbation.name
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "live scalars not updated in place:\n{}",
        failures.join("\n")
    );
}

#[test]
fn renaming_an_authored_control_updates_its_label() {
    use manifold_core::GraphTarget;
    use manifold_editing::command::Command;
    use manifold_editing::commands::effects::{BindingMappingEdit, EditParamMappingCommand};
    const NAME: &str = "Glintness";
    let base = base();
    let fx = &base.project.timeline.layers[base.active.unwrap()]
        .effects
        .as_ref()
        .expect("the active layer has effects")[0];
    let param = fx
        .params
        .iter()
        .next()
        .expect("the card has a control")
        .id()
        .to_string();
    let seed = manifold_nodes::bundled_presets::bundled_preset_def(fx.effect_type())
        .map(|d| d.as_ref().clone());
    let mut cmd = EditParamMappingCommand::new(
        GraphTarget::Effect(fx.id.clone()),
        param,
        BindingMappingEdit {
            label: Some(NAME.into()),
            ..Default::default()
        },
        seed,
    );
    let mut renamed = clone_data(&base);
    cmd.execute(&mut renamed.project);
    let touches = cmd.touches();
    assert!(
        touches.contains(Part::Effects),
        "a rename declares Effects: {touches:?}"
    );

    let last = FrameInputs::default();
    let mut next = last;
    let mut parts = next.parts;
    parts.bump(touches);
    next.parts = parts;
    assert!(
        crate::frame_plan::plan_frame(&last, &next)
            .projections
            .contains(Projection::InspectorSync)
    );

    assert!(!rebuild_output(&base, &base, Projection::InspectorSync, Shown::Plain).contains(NAME));
    assert!(
        rebuild_output(&base, &renamed, Projection::InspectorSync, Shown::Plain).contains(NAME),
        "the rebuilt card shows the new label"
    );
}
