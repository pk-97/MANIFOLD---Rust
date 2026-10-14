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
use super::{sync_build, LOGICAL_H, LOGICAL_W};
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

const REBUILD: [Projection; 4] =
    [Projection::ProjectSync, Projection::InspectorSync, Projection::ClipPositions, Projection::LaneGeometry];
const PUSHES: [Projection; 3] = [Projection::SelectionPush, Projection::ChromePush, Projection::LiveValues];

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

fn fresh_ui(data: &SceneData) -> UIRoot {
    let mut ui = UIRoot::new();
    ui.resize(LOGICAL_W, LOGICAL_H);
    sync_build(&mut ui, data, 24.0);
    ui
}

fn active_id(data: &SceneData) -> Option<LayerId> {
    data.active.and_then(|i| data.project.timeline.layers.get(i)).map(|l| l.layer_id.clone())
}

fn fingerprint(ui: &UIRoot) -> String {
    let tree = super::dump::dump_tree_ex(&ui.tree, &[]);
    let mut clips = Vec::new();
    ui.viewport.visible_clip_rects(&mut clips);
    format!("{tree}\n{clips:?}\n{:?}", ui.viewport.automation_lane_screens())
}

fn push(ui: &mut UIRoot, data: &SceneData, which: Projection) {
    let mut cache = TransportDisplayCache::new();
    match which {
        Projection::SelectionPush => crate::ui_bridge::push_selection(ui, &data.project, data.active, &data.selection),
        Projection::ChromePush => {
            crate::ui_bridge::push_chrome(ui, &data.project, &data.content, &data.selection, false, None, &mut cache)
        }
        Projection::LiveValues => crate::ui_bridge::push_live(ui, &data.project, &data.content, data.active),
        other => unreachable!("{other:?} is not a push"),
    }
}

fn push_all(ui: &mut UIRoot, data: &SceneData) {
    crate::ui_bridge::drain_events(ui, &data.content);
    for which in PUSHES {
        push(ui, data, which);
    }
    ui.update();
}

fn project(ui: &mut UIRoot, data: &mut SceneData, which: Projection) {
    match which {
        Projection::ProjectSync => {
            crate::ui_bridge::sync_automation_lane_order(&data.project, &mut data.selection);
            crate::ui_bridge::sync_project_data(ui, &data.project, data.active, &data.selection);
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
        other => unreachable!("{other:?} is not a projection"),
    }
}

/// The output of `which` run on `data`, with every push from `base`.
fn rebuild_output(base: &SceneData, data: &SceneData, which: Projection) -> String {
    let mut ui = fresh_ui(base);
    let mut data = clone_data(data);
    project(&mut ui, &mut data, which);
    ui.build();
    push_all(&mut ui, base);
    fingerprint(&ui)
}

fn push_output(base: &SceneData, data: &SceneData, which: Projection) -> String {
    let mut ui = fresh_ui(base);
    push(&mut ui, data, which);
    ui.update();
    fingerprint(&ui)
}

fn mirror_param(data: &SceneData) -> (manifold_core::EffectId, ParamId) {
    let flowers = &data.project.timeline.layers[1];
    let mirror = &flowers.effects.as_ref().expect("flowers carries Mirror")[0];
    let lane = &mirror.automation_lanes.as_ref().expect("Mirror is automated")[0];
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
                d.project.timeline.layers[0].clips.push(manifold_core::clip::TimelineClip::new_video(
                    "EXTRA".into(),
                    Beats(48.0),
                    Beats(4.0),
                    manifold_core::Seconds::ZERO,
                ));
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
                let automated: Vec<String> =
                    mirror.automation_lanes.iter().flatten().map(|l| l.param_id.as_ref().to_string()).collect();
                let pid = mirror.params.iter().map(|p| p.spec.id.to_string()).find(|id| !automated.contains(id));
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
                let mut fx = manifold_core::effects::PresetInstance::new(manifold_core::PresetTypeId::new("Mirror"));
                fx.init_defaults();
                d.project.timeline.layers[1].effects.as_mut().unwrap().push(fx);
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
                d.project.timeline.markers.push(manifold_core::marker::TimelineMarker::new(Beats(8.0)));
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
                d.selection.select_clip(clip, d.project.timeline.layers[0].layer_id.clone());
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
        Perturbation {
            name: "point selected",
            parts: ChangeSet::NONE,
            apply: |d| {
                let (fx, pid) = mirror_param(d);
                d.selection.selected_automation_point = Some(manifold_ui::view::UiAutomationPointRef {
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

#[test]
fn projection_input_matrix() {
    let base = base();
    let mut observed: BTreeSet<(Kind, String, &'static str)> = BTreeSet::new();
    let mut failures = Vec::new();
    let mut seen_rows: BTreeSet<String> = BTreeSet::new();

    let base_rebuild: Vec<String> = REBUILD.iter().map(|&p| rebuild_output(&base, &base, p)).collect();
    let base_push: Vec<String> = PUSHES.iter().map(|&p| push_output(&base, &base, p)).collect();
    // The harness is deterministic: the same state twice gives the same output.
    assert_eq!(base_rebuild[0], rebuild_output(&base, &base, Projection::ProjectSync));

    for perturbation in perturbations() {
        let mut data = clone_data(&base);
        (perturbation.apply)(&mut data);

        // What the planner sees: one planner, base then perturbed.
        let mut planner = FramePlanner::default();
        let mut base_sel = base.selection.clone();
        let mut base_edits = UiEdits::default();
        let ui = fresh_ui(&base);
        let last = crate::ui_frame::gather_frame_inputs(
            &mut planner,
            &ui,
            &mut base_sel,
            &active_id(&base),
            &base_edits,
            &base.content,
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
        );
        let plan = crate::frame_plan::plan_frame(&last, &next);

        for (i, &p) in REBUILD.iter().enumerate() {
            if rebuild_output(&base, &data, p) != base_rebuild[i] {
                observed.insert((Kind::Rebuild, format!("{p:?}"), perturbation.name));
                seen_rows.insert(format!("{p:?}"));
                if !plan.projections.contains(p) {
                    failures.push(format!("rebuild: {} changes {p:?}, the plan does not run it", perturbation.name));
                }
                if perturbation.parts.intersects(ChangeSet::LIVE) {
                    failures.push(format!("live scalar {} is a rebuild dependency of {p:?}", perturbation.name));
                }
            }
        }
        for (i, &p) in PUSHES.iter().enumerate() {
            if push_output(&base, &data, p) != base_push[i] {
                observed.insert((Kind::InPlace, format!("{p:?}"), perturbation.name));
                seen_rows.insert(format!("{p:?}"));
                if !plan.projections.contains(p) {
                    failures.push(format!("in place: {} changes {p:?}, the plan does not run it", perturbation.name));
                }
            }
        }
    }

    for (kind, projection, name) in &observed {
        println!("observed {kind:?} {projection} <- {name}");
    }
    for p in REBUILD.iter().chain(PUSHES.iter()) {
        if !seen_rows.contains(&format!("{p:?}")) {
            println!("row never observed: {p:?}");
        }
    }
    assert!(failures.is_empty(), "dependencies missing from the plan:\n{}", failures.join("\n"));
}
