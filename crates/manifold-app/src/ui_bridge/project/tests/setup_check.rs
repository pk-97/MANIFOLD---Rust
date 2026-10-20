//! B3: the setup check reads a water's shapes at the values its cards hold
//! and what its solver reported, and its warning lands on the card row that
//! edits the shape.

use std::sync::Arc;

use super::water_family::{add_water, binding, rows, water_project};
use super::*;
use crate::setup_check::{CardCheck, GizmoProblems, SetupFitEntry, SetupFitReport};
use crate::ui_bridge::projection::cards::{SurfaceVisibility, gen_params_to_surface};
use manifold_core::NodeId;
use manifold_core::change::{ChangeSet, Part, PartStamps};
use manifold_core::fluid_domain::FluidDomainSnapshot;
use manifold_core::liquid_setup::{ColumnOutcome, SetupFit};
use manifold_editing::service::EditingService;
use manifold_nodes_scene::node_graph::scene_vm::{SceneVm, SetupProblemKind};

const CAPPED: &str = "Fill is above the top; capped at the top";

struct Rig {
    project: Project,
    layer: LayerId,
    domain: NodeId,
    column: NodeId,
    /// The UI's part stamps, bumped with every edit the rig makes.
    parts: PartStamps,
}

impl Rig {
    fn new() -> Self {
        let (mut project, layer, render) = water_project();
        let mut editing = EditingService::new();
        add_water(&mut project, &layer, render, &mut editing);
        let def = effective_def(&project, &layer);
        let water = rows(&def).into_iter().filter(|row| row.is_group && row.liquid_domain.is_some()).last().expect("the added water");
        let family = def.nodes.iter().find(|node| Some(node.id) == water.group_node_id).and_then(|node| node.group.as_deref()).unwrap();
        let column = family
            .nodes
            .iter()
            .find(|node| node.node_id.as_str().starts_with("initial_column"))
            .expect("the water's initial column")
            .node_id
            .clone();
        let domain = water.liquid_domain.expect("domain").node;
        Self { project, layer, domain, column, parts: PartStamps::default() }
    }

    fn set(&mut self, node: &NodeId, param: &str, value: f32) {
        let def = effective_def(&self.project, &self.layer);
        let id = binding(&def, node, param);
        let (index, _) = self.project.timeline.find_layer_by_id(&self.layer).unwrap();
        self.project.timeline.layers[index].gen_params_mut().unwrap().set_base_param(&id, value);
        self.parts.bump(ChangeSet::of(&[Part::Values]));
    }

    fn problems(&self) -> Vec<SetupProblemKind> {
        self.problems_with(&[])
    }

    fn problems_with(&self, report: &[SetupFitEntry]) -> Vec<SetupProblemKind> {
        let def = effective_def(&self.project, &self.layer);
        let (_, layer) = self.project.timeline.find_layer_by_id(&self.layer).unwrap();
        let runtime = crate::setup_check::runtime_fits(report, &self.layer);
        crate::setup_check::problems_of(&def, layer.gen_params().unwrap(), runtime).into_iter().map(|problem| problem.kind).collect()
    }

    /// The warning line the card row bound to `node`'s `param` shows.
    fn warning(&self, node: &NodeId, param: &str) -> Option<String> {
        let def = effective_def(&self.project, &self.layer);
        let id = binding(&def, node, param);
        let (_, layer) = self.project.timeline.find_layer_by_id(&self.layer).unwrap();
        let inst = layer.gen_params().unwrap();
        let mut surface = gen_params_to_surface(inst, self.layer.as_str(), None, &[], SurfaceVisibility::All, (manifold_core::Bpm(120.0), 4.0));
        let problems = crate::setup_check::problems_of(&def, inst, []);
        crate::setup_check::mark_rows(&mut surface.rows, &problems, &def);
        surface.rows.iter().find(|row| row.id.as_ref() == id.as_str()).and_then(|row| row.spec.setup_warning.clone())
    }

    /// Pin `node`'s `param` to the curated layer card, which keeps only
    /// pinned rows from the Scene tab's set; its row id.
    fn pin(&mut self, node: &NodeId, param: &str) -> String {
        let def = effective_def(&self.project, &self.layer);
        let id = binding(&def, node, param);
        let (index, _) = self.project.timeline.find_layer_by_id(&self.layer).unwrap();
        self.project.timeline.layers[index]
            .gen_params_mut()
            .unwrap()
            .params
            .get_mut(id.as_str())
            .expect("the param")
            .choices
            .pin = Some(manifold_core::scene_surface::CardPin::Shown);
        id
    }

    /// The warning the layer card's row `id` shows after the inspector
    /// projects `ui` against the rig's project.
    fn layer_card_warning(&self, ui: &mut crate::ui_root::UIRoot, id: &str) -> Option<String> {
        let (index, _) = self.project.timeline.find_layer_by_id(&self.layer).unwrap();
        let mut selection = crate::ui_bridge::SelectionState::default();
        selection.select_layer(self.layer.clone());
        crate::ui_bridge::projection::inspector::sync_inspector_data(ui, &self.project, Some(index), &selection, &[], None);
        let card = ui.inspector.gen_params().expect("the layer card");
        card.row(id).expect("the pinned row on the card").spec.setup_warning.clone()
    }

    /// The solver's report for this water's domain.
    fn report(&self, fit: SetupFit) -> SetupFitReport {
        Arc::from([SetupFitEntry { layer: self.layer.clone(), node: self.domain.clone(), fit, refusal: None }])
    }
}

const CAPPED_FIT: SetupFit = SetupFit { fill_capped: true, column: ColumnOutcome::Fits };

#[test]
fn a_fresh_water_has_no_problems() {
    let rig = Rig::new();
    assert_eq!(rig.problems(), Vec::new());
    assert_eq!(rig.warning(&rig.domain, "fill_height"), None);
}

#[test]
fn fill_above_the_top_warns_on_the_fill_card() {
    let mut rig = Rig::new();
    rig.set(&rig.domain.clone(), "fill_height", 15.0);
    assert_eq!(rig.problems(), vec![SetupProblemKind::FillCapped]);
    assert_eq!(rig.warning(&rig.domain, "fill_height").as_deref(), Some(CAPPED));
}

#[test]
fn a_column_past_a_wall_is_clipped_and_wholly_outside_is_ignored() {
    let mut rig = Rig::new();
    let column = rig.column.clone();
    let def = effective_def(&rig.project, &rig.layer);
    let (size, centre) = {
        let id = |param| binding(&def, &column, param);
        let (_, layer) = rig.project.timeline.find_layer_by_id(&rig.layer).unwrap();
        let get = |param| layer.gen_params().unwrap().get_base_param(&id(param));
        (get("scale_x"), get("pos_x"))
    };
    // Slide the column's centre to the domain's +X wall: half of it leaves.
    let wall = centre.abs() + size * 0.5;
    rig.set(&column, "pos_x", wall.max(2.0) + size * 0.25);
    assert_eq!(rig.problems(), vec![SetupProblemKind::ColumnClipped]);
    assert_eq!(rig.warning(&column, "scale_x").as_deref(), Some("Column leaves the domain; clipped to it"));
    rig.set(&column, "pos_x", 40.0);
    assert_eq!(rig.problems(), vec![SetupProblemKind::ColumnOutside]);
}

/// A fill pinned to the layer card (the inspector's Layer tab) carries the
/// same warning, so deleting the inspector's marking fails this.
#[test]
fn the_layer_card_shows_the_warning() {
    let mut rig = Rig::new();
    let domain = rig.domain.clone();
    rig.set(&domain, "fill_height", 15.0);
    let id = rig.pin(&domain, "fill_height");
    let mut ui = crate::ui_root::UIRoot::new();
    assert_eq!(rig.layer_card_warning(&mut ui, &id).as_deref(), Some(CAPPED));
}

/// A fill driven by a wire or a modulator is capped by the solver at run
/// time; the check never sees that value, so the solver's report carries the
/// warning. The report rides every content state, so the card warns with
/// Show gizmos off and in perform mode, which forces it off.
#[test]
fn a_driven_fill_capped_live_warns_on_the_card_with_gizmos_off_and_in_perform_mode() {
    let mut rig = Rig::new();
    let domain = rig.domain.clone();
    // The authored value fits; only the live (driven) value overshoots.
    rig.set(&domain, "fill_height", 0.5);
    let id = rig.pin(&domain, "fill_height");
    let mut ui = crate::ui_root::UIRoot::new();
    assert_eq!(rig.layer_card_warning(&mut ui, &id), None);

    let mut perform = crate::perform_mode::PerformModeState::new();
    let mut text = crate::text_input::TextInputState::new();
    crate::perform_mode::frame::enter(&mut perform, &mut ui, &mut text, None);
    assert!(ui.scene_setup_panel.gizmo_layer().is_none(), "no gizmo overlay is recorded");
    let state = crate::content_state::ContentState { setup_fits: rig.report(CAPPED_FIT), ..Default::default() };
    assert!(state.physics_overlay_frames.iter().all(Option::is_none));
    crate::ui_bridge::intake_overlay_state(&mut ui, &state);
    assert_eq!(rig.problems_with(&ui.setup_fits), vec![SetupProblemKind::FillCapped]);
    assert_eq!(rig.layer_card_warning(&mut ui, &id).as_deref(), Some(CAPPED));

    // The planner input moves with the live cap, so the cards re-project.
    let mut check = CardCheck::default();
    let fitting = check.input(&rig.project, rig.parts, [Some(&rig.layer), None], &Arc::from([]));
    let capped = check.input(&rig.project, rig.parts, [Some(&rig.layer), None], &ui.setup_fits);
    assert_ne!(capped, fitting, "a live cap moves the cards' input");
}

/// The planner input is the problem list itself: a scrub that leaves the
/// list alone, on this layer or another, never schedules InspectorSync
/// (UI_INVALIDATION_DESIGN.md D22); a scrub that adds or clears a warning
/// does. Only a value the setup shapes read re-checks, and nothing short of a
/// graph edit rebuilds the scene model. A layer with no water is 0.
#[test]
fn a_scrub_that_leaves_the_list_alone_never_syncs_the_inspector() {
    use crate::frame_plan::{FrameInputs, Projection, plan_frame};
    let mut rig = Rig::new();
    let domain = rig.domain.clone();
    let other = rig.project.timeline.add_layer("Other", manifold_core::types::LayerType::Generator, manifold_core::PresetTypeId::new("WaterDamBreakGpuFlip"));
    let none: SetupFitReport = Arc::from([]);
    let mut check = CardCheck::default();
    let mut last = FrameInputs::default();
    let layer = rig.layer.clone();
    let mut frame = |check: &mut CardCheck, rig: &Rig| {
        let next = FrameInputs { setup_check: check.input(&rig.project, rig.parts, [Some(&layer), Some(&layer)], &none), ..Default::default() };
        let synced = plan_frame(&last, &next).projections.contains(Projection::InspectorSync);
        last = next;
        synced
    };
    frame(&mut check, &rig);
    assert_eq!(check.input(&rig.project, rig.parts, [Some(&rig.layer), None], &none), 0, "a fitting water has an empty list");
    assert_eq!(check.work(), (1, 1), "the Scene panel on the same layer is checked once");

    // A knob tick on another layer.
    let first = rig.project.timeline.layers[other].gen_params().unwrap().params.iter().next().unwrap().id().to_owned();
    rig.project.timeline.layers[other].gen_params_mut().unwrap().set_base_param(&first, 0.123);
    rig.parts.bump(ChangeSet::of(&[Part::Values]));
    assert!(!frame(&mut check, &rig), "a scrub elsewhere");
    // A knob on this water that places no setup shape.
    let def = effective_def(&rig.project, &rig.layer);
    let placing = ["fill_height", "domain_size", "resolution", "pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z", "billboard", "role"];
    let unrelated = def.preset_metadata.as_ref().unwrap().bindings.iter().find_map(|binding| match &binding.target {
        manifold_core::effect_graph_def::BindingTarget::Node { param, .. } if !placing.contains(&param.as_str()) => Some(binding.id.clone()),
        _ => None,
    });
    let unrelated = unrelated.expect("a water card param that places nothing");
    let (index, _) = rig.project.timeline.find_layer_by_id(&rig.layer).unwrap();
    let base = rig.project.timeline.layers[index].gen_params().unwrap().get_base_param(&unrelated);
    rig.project.timeline.layers[index].gen_params_mut().unwrap().set_base_param(&unrelated, base + 0.01);
    rig.parts.bump(ChangeSet::of(&[Part::Values]));
    assert!(!frame(&mut check, &rig), "a scrub of a knob that places nothing");
    assert_eq!(check.work(), (1, 1), "neither scrub re-checked or rebuilt");
    // A scrub of this water's fill that still fits.
    rig.set(&domain, "fill_height", 0.6);
    assert!(!frame(&mut check, &rig), "a fitting scrub");
    assert_eq!(check.work(), (1, 2), "a Fill Height scrub re-checks without a rebuild");

    // Past the top: the warning appears, once.
    rig.set(&domain, "fill_height", 15.0);
    assert!(frame(&mut check, &rig), "a warning appears");
    let fresh = |rig: &Rig| CardCheck::default().input(&rig.project, rig.parts, [Some(&rig.layer), None], &none);
    assert_eq!(check.input(&rig.project, rig.parts, [Some(&rig.layer), None], &none), fresh(&rig), "the re-check finds what a rebuild finds");
    rig.set(&domain, "fill_height", 16.0);
    assert!(!frame(&mut check, &rig), "still capped: the same list");
    rig.set(&domain, "fill_height", 0.5);
    assert!(frame(&mut check, &rig), "the warning clears");
    // A column scrub past a wall, re-checked, matches a rebuild too.
    let column = rig.column.clone();
    rig.set(&column, "pos_x", 40.0);
    assert!(frame(&mut check, &rig), "the column warning appears");
    assert_eq!(check.input(&rig.project, rig.parts, [Some(&rig.layer), None], &none), fresh(&rig));
    assert_eq!(check.work().0, 1, "no scrub rebuilt the scene model");

    let forces = crate::ui_bridge::force_project("RadialForce", &["force_a"]);
    let scene = LayerId::new("scene");
    assert_eq!(CardCheck::default().input(&forces, PartStamps::default(), [Some(&scene), None], &none), 0, "no water, no list");
}

/// A driven fill sweeping in and out of the top four times a second latches
/// its warning like a clip light: the cards re-project once, not on every
/// crossing, and an edit to the layer releases it.
#[test]
fn a_swept_driven_fill_syncs_the_inspector_once_until_an_edit() {
    use crate::frame_plan::{FrameInputs, Projection, plan_frame};
    let mut rig = Rig::new();
    let domain = rig.domain.clone();
    rig.set(&domain, "fill_height", 0.5);
    let snapshot = |fit: SetupFit| FluidDomainSnapshot {
        epoch: 1,
        state: manifold_core::fluid_domain::FluidDomainState::Ready,
        accepted_layout: None,
        fit,
        column_box: None,
        refusal: None,
    };
    let mut observations = crate::setup_check::SetupFitObservations::default();
    let mut check = CardCheck::default();
    let mut last = FrameInputs::default();
    let mut syncs = 0;
    let mut run = |rig: &Rig, frames: std::ops::Range<usize>, syncs: &mut usize| -> SetupFitReport {
        for frame in frames {
            // 4 Hz at 60 frames a second: capped for 7 or 8 frames, then not.
            let capped = (frame * 8 / 60) % 2 == 1;
            let fit = if capped { CAPPED_FIT } else { SetupFit::default() };
            let domains = [(rig.domain.clone(), snapshot(fit))];
            observations.record(Some(&rig.project), |_, each| each(&rig.layer, &domains));
            let next = FrameInputs { setup_check: check.input(&rig.project, rig.parts, [Some(&rig.layer), None], &observations.report), ..Default::default() };
            *syncs += usize::from(plan_frame(&last, &next).projections.contains(Projection::InspectorSync));
            last = next;
        }
        observations.report.clone()
    };
    let report = run(&rig, 0..120, &mut syncs);
    assert_eq!(syncs, 1, "one sync for the first cap, none for the sweep");
    assert_eq!(rig.problems_with(&report), vec![SetupProblemKind::FillCapped], "the warning holds through the dips");

    // An edit releases the latch; the sweep latches again at its next cap.
    rig.set(&domain, "fill_height", 0.4);
    assert!(run(&rig, 120..121, &mut syncs).is_empty(), "released while the sweep fits");
    assert_eq!(syncs, 2, "the warning clears");
    run(&rig, 121..240, &mut syncs);
    assert_eq!(syncs, 3, "and latches once more");
}

/// A gizmo host rebuilds its list only when the scene or a domain's fit or
/// refusal changes. A steady frame, or a driven column moving inside its
/// warning, keeps the same list; the box is read from the frame.
#[test]
fn a_steady_gizmo_frame_rebuilds_nothing() {
    let mut rig = Rig::new();
    let domain = rig.domain.clone();
    rig.set(&domain, "fill_height", 15.0);
    let target = manifold_core::GraphTarget::Generator(rig.layer.clone());
    let scene = Arc::new(crate::scene_gizmo::authored_def(&rig.project, &target).as_ref().and_then(SceneVm::from_def).expect("the scene"));
    let snapshot = |fit: SetupFit, column_box| FluidDomainSnapshot {
        epoch: 1,
        state: manifold_core::fluid_domain::FluidDomainState::Ready,
        accepted_layout: None,
        fit,
        column_box,
        refusal: None,
    };
    let clipped = SetupFit { fill_capped: true, column: ColumnOutcome::Clipped };
    let at = |x: f32| Some(([x, 0.0, 0.0], [x + 1.0, 1.0, 1.0]));
    let domains: crate::physics_overlay::SharedDomains = Arc::from([(domain.clone(), snapshot(clipped, at(1.5)))]);
    let mut host = GizmoProblems::default();
    assert!(host.update(Some(&scene), Some(&domains)), "the first frame builds");
    let list = (host.problems().as_ptr(), host.problems().len());
    assert_eq!(list.1, 2, "the capped fill and the clipped column");
    for step in 0..100 {
        let moving: crate::physics_overlay::SharedDomains = Arc::from([(domain.clone(), snapshot(clipped, at(1.5 + step as f32 * 0.01)))]);
        assert!(!host.update(Some(&scene), Some(&moving)), "a moving column inside its warning rebuilds nothing");
        assert!(!host.update(Some(&scene), Some(&domains)), "a steady frame rebuilds nothing");
        let column = host.problems().iter().find(|problem| problem.kind == SetupProblemKind::ColumnClipped).unwrap();
        assert_eq!(column.live_bounds(&moving), at(1.5 + step as f32 * 0.01), "the box is where the column is now");
    }
    assert_eq!((host.problems().as_ptr(), host.problems().len()), list, "the same list, never reallocated");
    let fitting: crate::physics_overlay::SharedDomains = Arc::from([(domain, snapshot(SetupFit::default(), None))]);
    assert!(host.update(Some(&scene), Some(&fitting)), "a new fit rebuilds");
    assert!(host.update(None, None), "no scene clears");
    assert!(host.problems().is_empty());
}

/// The content thread publishes a new report only when the latched set
/// changes: domains arriving in another order, or a capped domain fitting
/// again, change nothing until an edit releases it.
#[test]
fn the_fit_report_is_reallocated_only_on_change() {
    use manifold_core::fluid_domain::FluidDomainState;
    let mut rig = Rig::new();
    let snapshot = |fit: SetupFit| FluidDomainSnapshot {
        epoch: 1,
        state: FluidDomainState::Ready,
        accepted_layout: None,
        fit,
        column_box: None,
        refusal: None,
    };
    let (water, other, calm) = (NodeId::new("water"), NodeId::new("other"), NodeId::new("calm"));
    let mut observations = crate::setup_check::SetupFitObservations::default();
    let mut record = |rig: &Rig, domains: &[(NodeId, FluidDomainSnapshot)]| {
        observations.record(Some(&rig.project), |_, each| each(&rig.layer, domains));
        observations.report.clone()
    };
    let capped = |node: &NodeId| (node.clone(), snapshot(CAPPED_FIT));
    let first = record(&rig, &[capped(&water), capped(&other), (calm.clone(), snapshot(SetupFit::default()))]);
    assert_eq!(first.len(), 2, "one entry per capped domain, none for a fitting one");
    assert!(Arc::ptr_eq(&first, &record(&rig, &[capped(&other), capped(&water)])), "order is not a change");
    assert!(Arc::ptr_eq(&first, &record(&rig, &[(water.clone(), snapshot(SetupFit::default()))])), "a latched cap holds");
    let column = SetupFit { fill_capped: false, column: ColumnOutcome::Outside };
    let both = record(&rig, &[(water.clone(), snapshot(column))]);
    assert!(!Arc::ptr_eq(&first, &both), "a new kind joins the latch");
    let clipped = SetupFit { fill_capped: false, column: ColumnOutcome::Clipped };
    let swept = record(&rig, &[(water.clone(), snapshot(clipped))]);
    assert_eq!(swept.iter().find(|entry| entry.node == water).unwrap().fit.column, ColumnOutcome::Clipped, "outside then clipped latches as clipped");
    assert!(Arc::ptr_eq(&swept, &record(&rig, &[(water.clone(), snapshot(column))])), "clipped and outside share one latched line");
    let domain = rig.domain.clone();
    rig.set(&domain, "fill_height", 0.3);
    let released = record(&rig, &[(water, snapshot(SetupFit::default()))]);
    assert!(released.is_empty(), "an edit releases the latch");
}
