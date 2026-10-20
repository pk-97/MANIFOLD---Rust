//! B3: the setup check reads a water's shapes at the values its cards hold,
//! and its warning lands on the card row that edits the shape.

use super::water_family::{add_water, binding, rows, water_project};
use super::*;
use crate::ui_bridge::projection::cards::{SurfaceVisibility, gen_params_to_surface};
use manifold_core::NodeId;
use manifold_core::fluid_domain::FluidDomainSnapshot;
use manifold_core::liquid_setup::SetupFit;
use manifold_editing::service::EditingService;
use manifold_nodes_scene::node_graph::scene_vm::SetupProblemKind;

struct Rig {
    project: Project,
    layer: LayerId,
    domain: NodeId,
    column: NodeId,
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
        Self { project, layer, domain, column }
    }

    fn set(&mut self, node: &NodeId, param: &str, value: f32) {
        let def = effective_def(&self.project, &self.layer);
        let id = binding(&def, node, param);
        let (index, _) = self.project.timeline.find_layer_by_id(&self.layer).unwrap();
        self.project.timeline.layers[index].gen_params_mut().unwrap().set_base_param(&id, value);
    }

    fn problems(&self) -> Vec<SetupProblemKind> {
        self.problems_with(&[])
    }

    fn problems_with(&self, domains: &[(NodeId, FluidDomainSnapshot)]) -> Vec<SetupProblemKind> {
        let def = effective_def(&self.project, &self.layer);
        let (_, layer) = self.project.timeline.find_layer_by_id(&self.layer).unwrap();
        crate::setup_check::problems_of(&def, layer.gen_params().unwrap(), domains).into_iter().map(|problem| problem.kind).collect()
    }

    /// The warning line the card row bound to `node`'s `param` shows.
    fn warning(&self, node: &NodeId, param: &str) -> Option<String> {
        self.warning_with(node, param, &[])
    }

    fn warning_with(&self, node: &NodeId, param: &str, domains: &[(NodeId, FluidDomainSnapshot)]) -> Option<String> {
        let def = effective_def(&self.project, &self.layer);
        let id = binding(&def, node, param);
        let (_, layer) = self.project.timeline.find_layer_by_id(&self.layer).unwrap();
        let inst = layer.gen_params().unwrap();
        let mut surface = gen_params_to_surface(inst, self.layer.as_str(), None, &[], SurfaceVisibility::All, (manifold_core::Bpm(120.0), 4.0));
        let problems = crate::setup_check::problems_of(&def, inst, domains);
        crate::setup_check::mark_rows(&mut surface.rows, &problems, &def);
        surface.rows.iter().find(|row| row.id.as_ref() == id.as_str()).and_then(|row| row.spec.setup_warning.clone())
    }
}

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
    assert_eq!(rig.warning(&rig.domain, "fill_height").as_deref(), Some("Fill is above the top; capped at the top"));
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

/// A ready domain whose solver reported `fit` on a 4-unit box.
fn snapshot(fit: SetupFit) -> FluidDomainSnapshot {
    use manifold_core::fluid_domain::{FluidDomainLayout, FluidDomainState};
    FluidDomainSnapshot {
        epoch: 1,
        state: FluidDomainState::Ready,
        accepted_layout: Some(FluidDomainLayout { min: [-2.0; 3], size: [4.0; 3], cells: [40; 3], cell_size: 0.1 }),
        fit,
        refusal: None,
    }
}

/// A fill driven by a wire or modulator is capped by the solver at run time;
/// the check never sees its value, so the solver's report carries the warning.
#[test]
fn a_driven_fill_capped_live_still_warns() {
    let mut rig = Rig::new();
    let domain = rig.domain.clone();
    // The authored value fits; only the live (driven) value overshoots.
    rig.set(&domain, "fill_height", 0.5);
    assert_eq!(rig.warning(&domain, "fill_height"), None);
    let domains = [(domain.clone(), snapshot(SetupFit { fill_capped: true, ..Default::default() }))];
    assert_eq!(rig.problems_with(&domains), vec![SetupProblemKind::FillCapped]);
    assert_eq!(rig.warning_with(&domain, "fill_height", &domains).as_deref(), Some("Fill is above the top; capped at the top"));
}

/// A fill pinned to the layer card (the inspector's Layer tab) carries the
/// same warning, so deleting the inspector's marking fails this.
#[test]
fn the_layer_card_shows_the_warning() {
    let mut rig = Rig::new();
    let domain = rig.domain.clone();
    rig.set(&domain, "fill_height", 15.0);
    let def = effective_def(&rig.project, &rig.layer);
    let id = binding(&def, &domain, "fill_height");
    let (index, _) = rig.project.timeline.find_layer_by_id(&rig.layer).unwrap();
    // The curated card keeps only pinned rows from the Scene tab's set.
    rig.project.timeline.layers[index]
        .gen_params_mut()
        .unwrap()
        .params
        .get_mut(id.as_str())
        .expect("the fill param")
        .choices
        .pin = Some(manifold_core::scene_surface::CardPin::Shown);
    let mut ui = crate::ui_root::UIRoot::new();
    let mut selection = crate::ui_bridge::SelectionState::default();
    selection.select_layer(rig.layer.clone());
    crate::ui_bridge::projection::inspector::sync_inspector_data(&mut ui, &rig.project, Some(index), &selection, &[], None);
    let card = ui.inspector.gen_params().expect("the layer card");
    let row = card.row(id.as_str()).expect("the pinned fill row on the card");
    assert_eq!(row.spec.setup_warning.as_deref(), Some("Fill is above the top; capped at the top"));
}

/// The planner's input moves with what the check reads and with the solver's
/// report, and with nothing else.
#[test]
fn the_planner_key_follows_the_check_inputs() {
    use manifold_core::change::{ChangeSet, PartStamps};
    let rig = Rig::new();
    let mut parts = PartStamps::default();
    let frames = |fit: SetupFit| {
        let mut out: crate::physics_overlay::PhysicsOverlayFrames = Default::default();
        out[0] = Some(crate::physics_overlay::PhysicsOverlayFrame {
            layer: rig.layer.clone(),
            frame: 1,
            scene_node: rig.domain.clone(),
            camera: manifold_node_engine::scene::camera::Camera::default_perspective(),
            domains: vec![(rig.domain.clone(), snapshot(fit))].into(),
            lights: Vec::new().into(),
            gizmos: Vec::new().into(),
            outline: None,
        });
        out
    };
    let key = |parts: PartStamps, fit| crate::setup_check::input_key(parts, [Some(&rig.layer), None], &frames(fit));
    let fitting = key(parts, SetupFit::default());
    assert_eq!(key(parts, SetupFit::default()), fitting);
    assert_ne!(key(parts, SetupFit { fill_capped: true, ..Default::default() }), fitting, "a live cap moves the key");
    parts.bump(ChangeSet::ALL);
    assert_ne!(key(parts, SetupFit::default()), fitting, "an edit moves the key");
}
