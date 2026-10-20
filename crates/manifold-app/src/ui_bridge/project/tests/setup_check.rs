//! B3: the setup check reads a water's shapes at the values its cards hold,
//! and its warning lands on the card row that edits the shape.

use super::water_family::{add_water, binding, rows, water_project};
use super::*;
use crate::ui_bridge::projection::cards::{SurfaceVisibility, gen_params_to_surface};
use manifold_core::NodeId;
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
        let (_, layer) = self.project.timeline.find_layer_by_id(&self.layer).unwrap();
        crate::setup_check::layer_problems(layer).into_iter().map(|problem| problem.kind).collect()
    }

    /// The warning line the card row bound to `node`'s `param` shows.
    fn warning(&self, node: &NodeId, param: &str) -> Option<String> {
        let def = effective_def(&self.project, &self.layer);
        let id = binding(&def, node, param);
        let (_, layer) = self.project.timeline.find_layer_by_id(&self.layer).unwrap();
        let inst = layer.gen_params().unwrap();
        let mut surface = gen_params_to_surface(inst, self.layer.as_str(), None, &[], SurfaceVisibility::All, (manifold_core::Bpm(120.0), 4.0));
        let problems = crate::setup_check::problems_of(&def, inst);
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

/// The planner's input moves with the list and is not recomputed while
/// nothing it reads moved.
#[test]
fn the_planner_key_follows_the_problem_list() {
    use manifold_core::change::{ChangeSet, PartStamps};
    let mut rig = Rig::new();
    let mut planner = crate::frame_plan::FramePlanner::default();
    let mut parts = PartStamps::default();
    let mut key = |rig: &Rig, parts: PartStamps| planner.setup_check_key(&rig.project, [Some(&rig.layer), None], parts);
    let fitting = key(&rig, parts);
    rig.set(&rig.domain.clone(), "fill_height", 15.0);
    assert_eq!(key(&rig, parts), fitting, "the same stamps answer from the last check");
    parts.bump(ChangeSet::ALL);
    let capped = key(&rig, parts);
    assert_ne!(capped, fitting);
    rig.set(&rig.domain.clone(), "fill_height", 0.5);
    parts.bump(ChangeSet::ALL);
    assert_eq!(key(&rig, parts), fitting, "back inside the domain, back to the first key");
}
