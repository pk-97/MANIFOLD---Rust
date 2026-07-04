//! Automation-lane edit commands, addressed by [`GraphTarget`].
//!
//! Mirrors the envelope/driver/audio-mod command shape exactly (see
//! `commands/envelopes.rs`, `commands/drivers.rs`): every command resolves its
//! instance through [`Project::with_preset_graph_mut`] (which auto-inits a
//! generator's `gen_params` if it doesn't exist yet) and edits that
//! instance's `automation_lanes`, keyed by `param_id` — there is no
//! layer-scoped lane pool. See `docs/AUTOMATION_LANES_DESIGN.md` §6.
//!
//! Lanes are created implicitly: [`AddAutomationPointCommand`] creates the
//! lane if none exists for the param yet (the design's §6 command set has no
//! separate "AddLaneCommand" — a lane is born from its first point, same as
//! drawing the first breakpoint in Ableton). `points` must stay sorted
//! ascending by beat (§2's invariant, mirroring `TempoMap::ensure_sorted`);
//! [`AddAutomationPointCommand`] and [`MoveAutomationPointCommand`] both
//! re-sort after mutating.
//!
//! Point identity across execute/undo: [`AutomationPoint`] carries no id, and
//! the model's own sampler (`AutomationLane::value_at`) already assumes
//! unique, ascending beats — a lane can't have two points at the same beat.
//! [`MoveAutomationPointCommand`] therefore locates a point by matching its
//! `beat` (exact `f64` equality is safe here — `Beats` values reaching the
//! arrangement are never NaN, same assumption `value_at`'s binary search
//! already relies on), not by a positional index that a mid-command sort
//! could invalidate.

use crate::command::Command;
use manifold_core::GraphTarget;
use manifold_core::effects::{AutomationLane, AutomationPoint};
use manifold_core::project::Project;

/// Insert `point` into `points` at its sorted-by-beat position, maintaining
/// the ascending-beat invariant. Ties (equal beat) insert after existing
/// points at that beat — shouldn't occur in practice (Ableton-style unique
/// breakpoints) but stays a stable, defined order rather than a panic.
fn insert_sorted(points: &mut Vec<AutomationPoint>, point: AutomationPoint) {
    let pos = points
        .iter()
        .position(|p| p.beat.0 > point.beat.0)
        .unwrap_or(points.len());
    points.insert(pos, point);
}

/// Re-sort `points` ascending by beat. Used after a move changes one point's
/// beat, which can invalidate the invariant at that single position.
fn resort(points: &mut [AutomationPoint]) {
    points.sort_by(|a, b| {
        a.beat
            .partial_cmp(&b.beat)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Add a breakpoint to the automation lane for `param_id` on the instance
/// addressed by `target`, creating the lane (enabled) if it doesn't exist yet.
#[derive(Debug)]
pub struct AddAutomationPointCommand {
    target: GraphTarget,
    param_id: String,
    point: AutomationPoint,
    /// True when this command's `execute()` had to create the lane (no lane
    /// existed for `param_id` yet) — undo then removes the whole lane entry
    /// rather than just the point.
    created_lane: bool,
}

impl AddAutomationPointCommand {
    pub fn new(target: GraphTarget, param_id: impl Into<String>, point: AutomationPoint) -> Self {
        Self {
            target,
            param_id: param_id.into(),
            point,
            created_lane: false,
        }
    }
}

impl Command for AddAutomationPointCommand {
    fn execute(&mut self, project: &mut Project) {
        let existed = project
            .preset_instance(&self.target)
            .and_then(|inst| inst.automation_lanes.as_ref())
            .is_some_and(|lanes| lanes.iter().any(|l| l.param_id.as_ref() == self.param_id));
        self.created_lane = !existed;

        let param_id = self.param_id.clone();
        let point = self.point;
        project.with_preset_graph_mut(&self.target, |inst| {
            let lanes = inst.automation_lanes_mut();
            match lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id) {
                Some(lane) => insert_sorted(&mut lane.points, point),
                None => lanes.push(AutomationLane {
                    param_id: param_id.into(),
                    enabled: true,
                    points: vec![point],
                }),
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let param_id = self.param_id.clone();
        let point = self.point;
        let created_lane = self.created_lane;
        project.with_preset_graph_mut(&self.target, |inst| {
            let Some(lanes) = inst.automation_lanes.as_mut() else {
                return;
            };
            if created_lane {
                lanes.retain(|l| l.param_id.as_ref() != param_id);
            } else if let Some(lane) = lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id)
                && let Some(pos) = lane.points.iter().position(|p| p.beat.0 == point.beat.0)
            {
                lane.points.remove(pos);
            }
        });
    }

    fn description(&self) -> &str {
        "Add Automation Point"
    }
}

/// Move an existing breakpoint (drag commit): carries the explicit pre-drag
/// point so undo restores it exactly, mirroring `EditParamMappingCommand`'s
/// drag-commit reverse. Identifies the point by beat (see module docs), not
/// by array index, since this command's own re-sort can move the point's
/// position within `points`.
#[derive(Debug)]
pub struct MoveAutomationPointCommand {
    target: GraphTarget,
    param_id: String,
    old_point: AutomationPoint,
    new_point: AutomationPoint,
}

impl MoveAutomationPointCommand {
    pub fn new(
        target: GraphTarget,
        param_id: impl Into<String>,
        old_point: AutomationPoint,
        new_point: AutomationPoint,
    ) -> Self {
        Self {
            target,
            param_id: param_id.into(),
            old_point,
            new_point,
        }
    }

    fn apply(project: &mut Project, target: &GraphTarget, param_id: &str, from: AutomationPoint, to: AutomationPoint) {
        project.with_preset_graph_mut(target, |inst| {
            let Some(lanes) = inst.automation_lanes.as_mut() else {
                return;
            };
            let Some(lane) = lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id) else {
                return;
            };
            let Some(pos) = lane.points.iter().position(|p| p.beat.0 == from.beat.0) else {
                return;
            };
            lane.points[pos] = to;
            resort(&mut lane.points);
        });
    }
}

impl Command for MoveAutomationPointCommand {
    fn execute(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, &self.param_id, self.old_point, self.new_point);
    }

    fn undo(&mut self, project: &mut Project) {
        Self::apply(project, &self.target, &self.param_id, self.new_point, self.old_point);
    }

    fn description(&self) -> &str {
        "Move Automation Point"
    }
}

/// Remove a breakpoint (by its index within the lane's `points` at the time
/// of removal) from the automation lane for `param_id`. Mirrors
/// `RemoveEnvelopeCommand`'s index-capture-and-reinsert shape exactly.
#[derive(Debug)]
pub struct RemoveAutomationPointCommand {
    target: GraphTarget,
    param_id: String,
    point_index: usize,
    removed_point: Option<AutomationPoint>,
}

impl RemoveAutomationPointCommand {
    pub fn new(target: GraphTarget, param_id: impl Into<String>, point_index: usize) -> Self {
        Self {
            target,
            param_id: param_id.into(),
            point_index,
            removed_point: None,
        }
    }
}

impl Command for RemoveAutomationPointCommand {
    fn execute(&mut self, project: &mut Project) {
        let param_id = self.param_id.clone();
        let idx = self.point_index;
        let removed = project.with_preset_graph_mut(&self.target, |inst| {
            inst.automation_lanes
                .as_mut()
                .and_then(|lanes| lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id))
                .filter(|lane| idx < lane.points.len())
                .map(|lane| lane.points.remove(idx))
        });
        if let Some(Some(point)) = removed {
            self.removed_point = Some(point);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(point) = self.removed_point else {
            return;
        };
        let param_id = self.param_id.clone();
        let idx = self.point_index;
        project.with_preset_graph_mut(&self.target, |inst| {
            if let Some(lane) = inst
                .automation_lanes
                .as_mut()
                .and_then(|lanes| lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id))
            {
                let at = idx.min(lane.points.len());
                lane.points.insert(at, point);
            }
        });
    }

    fn description(&self) -> &str {
        "Remove Automation Point"
    }
}

/// Toggle a lane's `enabled` flag. Mirrors `ToggleDriverEnabledCommand`.
#[derive(Debug)]
pub struct SetLaneEnabledCommand {
    target: GraphTarget,
    param_id: String,
    old_enabled: bool,
    new_enabled: bool,
}

impl SetLaneEnabledCommand {
    pub fn new(
        target: GraphTarget,
        param_id: impl Into<String>,
        old_enabled: bool,
        new_enabled: bool,
    ) -> Self {
        Self {
            target,
            param_id: param_id.into(),
            old_enabled,
            new_enabled,
        }
    }
}

impl Command for SetLaneEnabledCommand {
    fn execute(&mut self, project: &mut Project) {
        let param_id = self.param_id.clone();
        let val = self.new_enabled;
        project.with_preset_graph_mut(&self.target, |inst| {
            if let Some(lane) = inst
                .automation_lanes
                .as_mut()
                .and_then(|lanes| lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id))
            {
                lane.enabled = val;
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let param_id = self.param_id.clone();
        let val = self.old_enabled;
        project.with_preset_graph_mut(&self.target, |inst| {
            if let Some(lane) = inst
                .automation_lanes
                .as_mut()
                .and_then(|lanes| lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id))
            {
                lane.enabled = val;
            }
        });
    }

    fn description(&self) -> &str {
        "Toggle Automation Lane"
    }
}

/// Clear all points from a lane, keeping the (now-empty) lane and its
/// `enabled` state. Undo restores the full point list.
#[derive(Debug)]
pub struct ClearLaneCommand {
    target: GraphTarget,
    param_id: String,
    removed_points: Vec<AutomationPoint>,
}

impl ClearLaneCommand {
    pub fn new(target: GraphTarget, param_id: impl Into<String>) -> Self {
        Self {
            target,
            param_id: param_id.into(),
            removed_points: Vec::new(),
        }
    }
}

impl Command for ClearLaneCommand {
    fn execute(&mut self, project: &mut Project) {
        let param_id = self.param_id.clone();
        let taken = project.with_preset_graph_mut(&self.target, |inst| {
            inst.automation_lanes
                .as_mut()
                .and_then(|lanes| lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id))
                .map(|lane| std::mem::take(&mut lane.points))
        });
        if let Some(Some(points)) = taken {
            self.removed_points = points;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let param_id = self.param_id.clone();
        let points = self.removed_points.clone();
        project.with_preset_graph_mut(&self.target, |inst| {
            if let Some(lane) = inst
                .automation_lanes
                .as_mut()
                .and_then(|lanes| lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id))
            {
                lane.points = points;
            }
        });
    }

    fn description(&self) -> &str {
        "Clear Automation Lane"
    }
}

/// Remove an entire lane (by its index within `automation_lanes` at the time
/// of removal). Mirrors `RemoveEnvelopeCommand`'s index-capture-and-reinsert
/// shape.
#[derive(Debug)]
pub struct RemoveLaneCommand {
    target: GraphTarget,
    param_id: String,
    removed_index: usize,
    removed_lane: Option<AutomationLane>,
}

impl RemoveLaneCommand {
    pub fn new(target: GraphTarget, param_id: impl Into<String>, removed_index: usize) -> Self {
        Self {
            target,
            param_id: param_id.into(),
            removed_index,
            removed_lane: None,
        }
    }
}

impl Command for RemoveLaneCommand {
    fn execute(&mut self, project: &mut Project) {
        let param_id = self.param_id.clone();
        let idx = self.removed_index;
        let removed = project.with_preset_graph_mut(&self.target, |inst| {
            inst.automation_lanes
                .as_mut()
                .filter(|lanes| idx < lanes.len() && lanes[idx].param_id.as_ref() == param_id)
                .map(|lanes| lanes.remove(idx))
        });
        if let Some(Some(lane)) = removed {
            self.removed_lane = Some(lane);
        }
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(lane) = self.removed_lane.clone() else {
            return;
        };
        let idx = self.removed_index;
        project.with_preset_graph_mut(&self.target, |inst| {
            let lanes = inst.automation_lanes_mut();
            let at = idx.min(lanes.len());
            lanes.insert(at, lane);
        });
    }

    fn description(&self) -> &str {
        "Remove Automation Lane"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::Beats;
    use manifold_core::effect_registration::EffectMetadata;
    use manifold_core::effects::SegmentShape;
    use manifold_core::generator_registration::ParamSpec;
    use manifold_core::layer::Layer;
    use manifold_core::preset_definition_registry::create_default;
    use manifold_core::PresetTypeId;

    const TEST_FX: PresetTypeId = PresetTypeId::new("TestAutomationEditFx");

    inventory::submit! {
        EffectMetadata {
            id: PresetTypeId::new("TestAutomationEditFx"),
            display_name: "Test Automation Edit Fx",
            category: "Test",
            available: true,
            osc_prefix: "testAutomationEditFx",
            legacy_discriminant: None,
            params: &[ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", "")],
        }
    }

    fn project_with_effect() -> (Project, manifold_core::EffectId) {
        let mut layer = Layer::new_video("FxLayer".into(), 0);
        let fx = create_default(&TEST_FX);
        let fx_id = fx.id.clone();
        layer.effects = Some(vec![fx]);
        let mut project = Project::default();
        project.timeline.layers = vec![layer];
        (project, fx_id)
    }

    fn point(beat: f64, value: f32) -> AutomationPoint {
        AutomationPoint {
            beat: Beats(beat),
            value,
            shape: SegmentShape::Linear,
        }
    }

    fn lane_points<'a>(project: &'a Project, fx_id: &manifold_core::EffectId) -> &'a [AutomationPoint] {
        project
            .find_effect_by_id(fx_id)
            .and_then(|fx| fx.automation_lanes.as_ref())
            .and_then(|lanes| lanes.iter().find(|l| l.param_id.as_ref() == "amount"))
            .map(|l| l.points.as_slice())
            .unwrap_or(&[])
    }

    #[test]
    fn add_point_creates_lane_and_undo_removes_it() {
        let (mut project, fx_id) = project_with_effect();
        let target = GraphTarget::Effect(fx_id.clone());
        let mut cmd = AddAutomationPointCommand::new(target, "amount", point(2.0, 0.5));

        cmd.execute(&mut project);
        assert_eq!(lane_points(&project, &fx_id).len(), 1);
        assert!(cmd.created_lane);

        cmd.undo(&mut project);
        let fx = project.find_effect_by_id(&fx_id).unwrap();
        assert!(
            fx.automation_lanes.as_ref().is_none_or(|v| v.is_empty()),
            "undo of the lane-creating add removes the whole lane"
        );

        cmd.execute(&mut project);
        assert_eq!(lane_points(&project, &fx_id).len(), 1, "redo re-applies");
    }

    #[test]
    fn add_point_to_existing_lane_keeps_sorted_order() {
        let (mut project, fx_id) = project_with_effect();
        let target = GraphTarget::Effect(fx_id.clone());

        let mut first = AddAutomationPointCommand::new(target.clone(), "amount", point(4.0, 0.8));
        first.execute(&mut project);
        assert!(first.created_lane);

        // Insert a point BEFORE the existing one — must land first after sort.
        let mut second = AddAutomationPointCommand::new(target.clone(), "amount", point(0.0, 0.2));
        second.execute(&mut project);
        assert!(!second.created_lane, "lane already existed");

        let points = lane_points(&project, &fx_id);
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].beat.0, 0.0, "sorted-by-beat invariant holds after insert");
        assert_eq!(points[1].beat.0, 4.0);

        // Undo the second add: only that point goes, lane stays.
        second.undo(&mut project);
        let points = lane_points(&project, &fx_id);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].beat.0, 4.0);
    }

    #[test]
    fn move_point_updates_value_and_resorts() {
        let (mut project, fx_id) = project_with_effect();
        let target = GraphTarget::Effect(fx_id.clone());
        let mut add_a = AddAutomationPointCommand::new(target.clone(), "amount", point(0.0, 0.2));
        add_a.execute(&mut project);
        let mut add_b = AddAutomationPointCommand::new(target.clone(), "amount", point(4.0, 0.8));
        add_b.execute(&mut project);

        // Move the beat-0 point to beat 8 — it must now sort AFTER the beat-4 point.
        let mut mv = MoveAutomationPointCommand::new(
            target.clone(),
            "amount",
            point(0.0, 0.2),
            point(8.0, 0.9),
        );
        mv.execute(&mut project);
        let points = lane_points(&project, &fx_id);
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].beat.0, 4.0, "the un-moved point now sorts first");
        assert_eq!(points[1].beat.0, 8.0);
        assert_eq!(points[1].value, 0.9);

        mv.undo(&mut project);
        let points = lane_points(&project, &fx_id);
        assert_eq!(points[0].beat.0, 0.0, "undo restores the original beat/order");
        assert_eq!(points[0].value, 0.2);
        assert_eq!(points[1].beat.0, 4.0);

        mv.execute(&mut project);
        let points = lane_points(&project, &fx_id);
        assert_eq!(points[1].beat.0, 8.0, "redo re-applies the move");
    }

    #[test]
    fn remove_point_reinserts_at_same_index_on_undo() {
        let (mut project, fx_id) = project_with_effect();
        let target = GraphTarget::Effect(fx_id.clone());
        let mut add_a = AddAutomationPointCommand::new(target.clone(), "amount", point(0.0, 0.2));
        add_a.execute(&mut project);
        let mut add_b = AddAutomationPointCommand::new(target.clone(), "amount", point(4.0, 0.8));
        add_b.execute(&mut project);

        let mut rm = RemoveAutomationPointCommand::new(target.clone(), "amount", 0);
        rm.execute(&mut project);
        let points = lane_points(&project, &fx_id);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].beat.0, 4.0);

        rm.undo(&mut project);
        let points = lane_points(&project, &fx_id);
        assert_eq!(points.len(), 2, "undo restores the removed point");
        assert_eq!(points[0].beat.0, 0.0);

        rm.execute(&mut project);
        assert_eq!(lane_points(&project, &fx_id).len(), 1, "redo re-applies");
    }

    #[test]
    fn set_lane_enabled_roundtrips() {
        let (mut project, fx_id) = project_with_effect();
        let target = GraphTarget::Effect(fx_id.clone());
        let mut add = AddAutomationPointCommand::new(target.clone(), "amount", point(0.0, 0.2));
        add.execute(&mut project);

        let mut toggle = SetLaneEnabledCommand::new(target.clone(), "amount", true, false);
        toggle.execute(&mut project);
        let enabled = |p: &Project| {
            p.find_effect_by_id(&fx_id)
                .and_then(|fx| fx.automation_lanes.as_ref())
                .and_then(|lanes| lanes.iter().find(|l| l.param_id.as_ref() == "amount"))
                .map(|l| l.enabled)
        };
        assert_eq!(enabled(&project), Some(false));

        toggle.undo(&mut project);
        assert_eq!(enabled(&project), Some(true));

        toggle.execute(&mut project);
        assert_eq!(enabled(&project), Some(false));
    }

    #[test]
    fn clear_lane_removes_points_and_undo_restores_them() {
        let (mut project, fx_id) = project_with_effect();
        let target = GraphTarget::Effect(fx_id.clone());
        let mut add_a = AddAutomationPointCommand::new(target.clone(), "amount", point(0.0, 0.2));
        add_a.execute(&mut project);
        let mut add_b = AddAutomationPointCommand::new(target.clone(), "amount", point(4.0, 0.8));
        add_b.execute(&mut project);

        let mut clear = ClearLaneCommand::new(target.clone(), "amount");
        clear.execute(&mut project);
        assert!(lane_points(&project, &fx_id).is_empty());
        // The lane itself (enabled bit) survives a clear.
        let fx = project.find_effect_by_id(&fx_id).unwrap();
        assert_eq!(fx.automation_lanes.as_ref().unwrap().len(), 1);

        clear.undo(&mut project);
        assert_eq!(lane_points(&project, &fx_id).len(), 2, "undo restores both points");

        clear.execute(&mut project);
        assert!(lane_points(&project, &fx_id).is_empty(), "redo re-applies");
    }

    #[test]
    fn remove_lane_undo_reinserts_the_whole_lane() {
        let (mut project, fx_id) = project_with_effect();
        let target = GraphTarget::Effect(fx_id.clone());
        let mut add = AddAutomationPointCommand::new(target.clone(), "amount", point(0.0, 0.2));
        add.execute(&mut project);

        let mut rm = RemoveLaneCommand::new(target.clone(), "amount", 0);
        rm.execute(&mut project);
        let fx = project.find_effect_by_id(&fx_id).unwrap();
        assert!(fx.automation_lanes.as_ref().is_none_or(|v| v.is_empty()));

        rm.undo(&mut project);
        assert_eq!(lane_points(&project, &fx_id).len(), 1, "undo restores the lane + its points");

        rm.execute(&mut project);
        let fx = project.find_effect_by_id(&fx_id).unwrap();
        assert!(fx.automation_lanes.as_ref().is_none_or(|v| v.is_empty()), "redo re-applies");
    }

    #[test]
    fn generator_target_creates_gen_params_and_lane() {
        // GraphTarget::Generator auto-inits gen_params via with_preset_graph_mut
        // when the layer has none yet — pin that this path works for automation
        // commands the same way it does for envelopes/drivers.
        let mut layer = Layer::new_generator("GenLayer".into(), TEST_FX, 0);
        layer.gen_params_or_init().init_defaults_for_type(TEST_FX);
        let layer_id = layer.layer_id.clone();
        let mut project = Project::default();
        project.timeline.layers = vec![layer];

        let target = GraphTarget::Generator(layer_id.clone());
        let mut add = AddAutomationPointCommand::new(target, "amount", point(1.0, 0.4));
        add.execute(&mut project);

        let gp = project
            .timeline
            .layers
            .iter()
            .find(|l| l.layer_id == layer_id)
            .and_then(|l| l.gen_params())
            .unwrap();
        assert_eq!(
            gp.automation_lanes.as_ref().unwrap()[0].points[0].value,
            0.4
        );
    }
}
