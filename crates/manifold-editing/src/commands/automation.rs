//! Automation-lane edit commands, addressed by [`GraphTarget`].
//!
//! Mirrors the envelope/driver/audio-mod command shape exactly (see
//! `commands/envelopes.rs`, `commands/drivers.rs`): every command resolves its
//! instance through [`Project::with_preset_graph_mut`] (which auto-inits a
//! generator's `gen_params` if it doesn't exist yet) and edits that
//! instance's `automation_lanes`, keyed by `param_id` — there is no
//! layer-scoped lane pool. See `docs/AUTOMATION_LANES_DESIGN.md` section 6.
//!
//! Lanes are created implicitly: [`AddAutomationPointCommand`] creates the
//! lane if none exists for the param yet (the design's section 6 command set has no
//! separate "AddLaneCommand" — a lane is born from its first point, same as
//! drawing the first breakpoint in Ableton). `points` must stay sorted
//! ascending by beat (section 2's invariant, mirroring `TempoMap::ensure_sorted`);
//! [`AddAutomationPointCommand`] and [`MoveAutomationPointCommand`] both
//! replace collisions and restore snapshots on undo.
//!
//! Point identity across execute/undo: [`AutomationPoint`] carries no id, so
//! commands match the exact stored `(beat, value)` key. This
//! preserves distinct equal-beat breakpoints while allowing an exact duplicate
//! key to be replaced. [`AutomationLane::value_at`] resolves an exact beat to
//! the last point in its stable equal-beat order.

use crate::command::Command;
use manifold_core::effects::{AutomationLane, AutomationPoint};
use manifold_core::project::Project;
use manifold_core::GraphTarget;

/// Insert `point` into `points` at its sorted-by-beat position, maintaining
/// the ascending-beat invariant. Ties insert after existing points at that
/// beat so equal-beat points retain authored order.
fn insert_sorted(points: &mut Vec<AutomationPoint>, point: AutomationPoint) {
    let pos = points
        .iter()
        .position(|p| p.beat.0 > point.beat.0)
        .unwrap_or(points.len());
    points.insert(pos, point);
}

/// Source tuples retain the exact stored value; normalization belongs at the
/// UI projection boundary, never in the editing identity comparison.
fn same_point(a: &AutomationPoint, b: &AutomationPoint) -> bool {
    a.beat == b.beat && a.value == b.value
}

/// Add a breakpoint to the automation lane for `param_id` on the instance
/// addressed by `target`, creating the lane (enabled) if it doesn't exist yet.
#[derive(Debug)]
pub struct AddAutomationPointCommand {
    target: GraphTarget,
    param_id: String,
    point: AutomationPoint,
    previous_lane: Option<AutomationLane>,
    previous_lane_index: Option<usize>,
    previous_lanes_were_none: bool,
    applied: bool,
}

impl AddAutomationPointCommand {
    pub fn new(target: GraphTarget, param_id: impl Into<String>, point: AutomationPoint) -> Self {
        Self {
            target,
            param_id: param_id.into(),
            point,
            previous_lane: None,
            previous_lane_index: None,
            previous_lanes_were_none: false,
            applied: false,
        }
    }
}

impl Command for AddAutomationPointCommand {
    fn execute(&mut self, project: &mut Project) {
        self.previous_lane = None;
        self.previous_lane_index = None;
        self.previous_lanes_were_none = false;
        self.applied = false;
        let param_id = self.param_id.clone();
        let point = self.point;
        let mut prior_lane = None;
        let mut prior_index = None;
        let mut lanes_were_none = false;
        let applied = project.with_preset_graph_mut(&self.target, |inst| {
            let found = inst.automation_lanes.as_ref().and_then(|lanes| {
                lanes
                    .iter()
                    .enumerate()
                    .find(|(_, l)| l.param_id.as_ref() == param_id)
            });
            prior_lane = found.map(|(_, lane)| lane.clone());
            prior_index = found.map(|(idx, _)| idx);
            lanes_were_none = inst.automation_lanes.is_none();
            // A pre-automation slider edit is not an override of a future lane.
            if prior_lane.is_none() && let Some(param) = inst.params.get_mut(&param_id) {
                param.touched = false;
            }
            let lanes = inst.automation_lanes_mut();
            match lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id) {
                Some(lane) => {
                    // Distinct values at one beat are independent breakpoints;
                    // only an exact `(beat, value)` duplicate is replaced.
                    lane.points.retain(|p| {
                        !same_point(p, &point)
                    });
                    insert_sorted(&mut lane.points, point);
                }
                None => lanes.push(AutomationLane { param_id: param_id.into(), enabled: true, points: vec![point] }),
            }
            true
        });
        self.previous_lane = prior_lane;
        self.previous_lane_index = prior_index;
        self.previous_lanes_were_none = lanes_were_none;
        self.applied = applied.is_some();
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied { return; }
        let param_id = self.param_id.clone();
        let previous_lane = self.previous_lane.clone();
        let previous_lane_index = self.previous_lane_index;
        let previous_lanes_were_none = self.previous_lanes_were_none;
        project.with_preset_graph_mut(&self.target, |inst| {
            if let Some(previous_lane) = previous_lane {
                if let Some(lanes) = inst.automation_lanes.as_mut() {
                    let idx = previous_lane_index
                        .filter(|&idx| {
                            lanes
                                .get(idx)
                                .is_some_and(|lane| lane.param_id.as_ref() == param_id)
                        })
                        .or_else(|| lanes.iter().position(|l| l.param_id.as_ref() == param_id));
                    if let Some(idx) = idx {
                        lanes[idx] = previous_lane;
                    }
                }
            } else if let Some(lanes) = inst.automation_lanes.as_mut() {
                lanes.retain(|l| l.param_id.as_ref() != param_id);
                if previous_lanes_were_none {
                    inst.automation_lanes = None;
                }
            }
        });
    }

    fn description(&self) -> &str {
        "Add Automation Point"
    }
}

/// Move existing breakpoints within one lane, identified by source beat and value.
/// Capture the entire lane on execute for exact collision undo. `new` moves one
/// point; `for_group` transforms all selected sources simultaneously so an
/// overlapping phrase cannot overwrite its own points mid-command.
#[derive(Debug)]
pub struct MoveAutomationPointCommand {
    target: GraphTarget,
    param_id: String,
    moves: Vec<(AutomationPoint, AutomationPoint)>,
    previous_lane: Option<AutomationLane>,
    previous_lane_index: Option<usize>,
}

impl MoveAutomationPointCommand {
    pub fn new(
        target: GraphTarget,
        param_id: impl Into<String>,
        old_point: AutomationPoint,
        new_point: AutomationPoint,
    ) -> Self {
        Self::for_group(target, param_id, vec![(old_point, new_point)])
    }

    /// Move all selected points simultaneously, preserving their authored order.
    pub fn for_group(
        target: GraphTarget,
        param_id: impl Into<String>,
        moves: Vec<(AutomationPoint, AutomationPoint)>,
    ) -> Self {
        Self {
            target, param_id: param_id.into(), moves,
            previous_lane: None, previous_lane_index: None,
        }
    }

    fn apply(
        project: &mut Project,
        target: &GraphTarget,
        param_id: &str,
        moves: &[(AutomationPoint, AutomationPoint)],
    ) -> bool {
        project
            .with_preset_graph_mut(target, |inst| {
                let Some(lanes) = inst.automation_lanes.as_mut() else {
                    return false;
                };
                let Some(lane) = lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id) else {
                    return false;
                };
                if moves.is_empty() || !moves.iter().all(|(from, _)| lane.points.iter().any(|p| same_point(p, from))) {
                    return false;
                }
                // Transform in original lane order, then use a stable beat sort.
                // This preserves step order for single and group moves alike.
                lane.points.retain_mut(|point| {
                    if let Some((_, to)) = moves.iter().find(|(from, _)| same_point(point, from)) {
                        *point = *to;
                        true
                    } else {
                        !moves.iter().any(|(_, to)| same_point(point, to))
                    }
                });
                lane.points.sort_by(|a, b| a.beat.0.total_cmp(&b.beat.0));
                true
            })
            .unwrap_or(false)
    }
}

impl Command for MoveAutomationPointCommand {
    fn execute(&mut self, project: &mut Project) {
        self.previous_lane = None;
        self.previous_lane_index = None;
        let Some(inst) = project.preset_instance(&self.target) else {
            return;
        };
        let Some((idx, lane)) = inst.automation_lanes.as_ref().and_then(|lanes| {
            lanes.iter().enumerate().find(|(_, l)| {
                l.param_id.as_ref() == self.param_id
                    && !self.moves.is_empty()
                    && self.moves.iter().all(|(from, _)| l.points.iter().any(|p| same_point(p, from)))
            })
        }) else {
            return;
        };
        self.previous_lane_index = Some(idx);
        self.previous_lane = Some(lane.clone());
        Self::apply(
            project,
            &self.target,
            &self.param_id,
            &self.moves,
        );
    }

    fn undo(&mut self, project: &mut Project) {
        let Some(previous_lane) = self.previous_lane.clone() else {
            return;
        };
        project.with_preset_graph_mut(&self.target, |inst| {
            if let Some(lanes) = inst.automation_lanes.as_mut() {
                let idx = self
                    .previous_lane_index
                    .filter(|&idx| {
                        lanes
                            .get(idx)
                            .is_some_and(|l| l.param_id.as_ref() == self.param_id)
                    })
                    .or_else(|| {
                        lanes
                            .iter()
                            .position(|l| l.param_id.as_ref() == self.param_id)
                    });
                if let Some(idx) = idx {
                    lanes[idx] = previous_lane;
                }
            }
        });
    }

    fn description(&self) -> &str {
        if self.moves.len() > 1 { "Move Automation Points" } else { "Move Automation Point" }
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
    pub fn for_param(target: GraphTarget, param_id: impl Into<String>) -> Self {
        Self::new(target, param_id, 0)
    }

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
        self.removed_lane = None;
        let param_id = self.param_id.clone();
        let removed = project.with_preset_graph_mut(&self.target, |inst| {
            inst.automation_lanes
                .as_mut()
                .and_then(|lanes| {
                    let idx = lanes.iter().position(|lane| lane.param_id.as_ref() == param_id)?;
                    self.removed_index = idx;
                    Some(lanes.remove(idx))
                })
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

/// Commits a completed recording gesture (section 5) as ONE undo entry. By the time
/// this command is built, `manifold-playback::automation`'s gesture-closure
/// pass has already computed the final joined point set (pre-punch-in old
/// points + the recorded segment + post-punch-out old points) — this
/// command's only job is installing that set and registering the undo entry
/// with the explicit pre-gesture reverse, mirroring
/// `EditParamMappingCommand::new_with_reverse`'s drag-commit shape: the
/// reverse is captured by the caller at gesture START (before any recording
/// mutated anything), not self-snapshotted here.
#[derive(Debug)]
pub struct CommitRecordedGestureCommand {
    target: GraphTarget,
    param_id: String,
    /// The full, already-joined point set to install (sorted ascending).
    new_points: Vec<AutomationPoint>,
    /// `None` when the gesture created the lane (no lane existed for this
    /// param before recording started) — undo then removes the whole lane,
    /// mirroring `AddAutomationPointCommand`'s `created_lane` behavior.
    /// `Some(points)` carries the pre-gesture point set to restore exactly.
    old_points: Option<Vec<AutomationPoint>>,
    previous_lanes_were_none: bool,
}

impl CommitRecordedGestureCommand {
    pub fn new(
        target: GraphTarget,
        param_id: impl Into<String>,
        new_points: Vec<AutomationPoint>,
        old_points: Option<Vec<AutomationPoint>>,
    ) -> Self {
        Self {
            target,
            param_id: param_id.into(),
            new_points,
            old_points,
            previous_lanes_were_none: false,
        }
    }
}

impl Command for CommitRecordedGestureCommand {
    fn execute(&mut self, project: &mut Project) {
        let param_id = self.param_id.clone();
        let points = self.new_points.clone();
        project.with_preset_graph_mut(&self.target, |inst| {
            self.previous_lanes_were_none = inst.automation_lanes.is_none();
            let new_lane = !inst.automation_lanes.as_ref().is_some_and(|lanes| lanes.iter().any(|lane| lane.param_id.as_ref() == param_id));
            if new_lane && let Some(param) = inst.params.get_mut(&param_id) {
                param.touched = false;
            }
            let lanes = inst.automation_lanes_mut();
            match lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id) {
                Some(lane) => lane.points = points,
                None => lanes.push(AutomationLane {
                    param_id: param_id.into(),
                    enabled: true,
                    points,
                }),
            }
        });
    }

    fn undo(&mut self, project: &mut Project) {
        let param_id = self.param_id.clone();
        match self.old_points.clone() {
            None => {
                project.with_preset_graph_mut(&self.target, |inst| {
                    if let Some(lanes) = inst.automation_lanes.as_mut() {
                        lanes.retain(|l| l.param_id.as_ref() != param_id);
                        if lanes.is_empty() && self.previous_lanes_were_none { inst.automation_lanes = None; }
                    }
                });
            }
            Some(points) => {
                project.with_preset_graph_mut(&self.target, |inst| {
                    if let Some(lane) = inst.automation_lanes.as_mut().and_then(|lanes| {
                        lanes.iter_mut().find(|l| l.param_id.as_ref() == param_id)
                    }) {
                        lane.points = points;
                    }
                });
            }
        }
    }

    fn description(&self) -> &str {
        "Record Automation"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_registration::EffectMetadata;
    use manifold_core::effects::SegmentShape;
    use manifold_core::generator_registration::ParamSpec;
    use manifold_core::layer::Layer;
    use manifold_core::preset_definition_registry::create_default;
    use manifold_core::Beats;
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

    #[test]
    fn first_authored_automation_does_not_inherit_an_old_slider_touch() {
        let (mut project, fx_id) = project_with_effect();
        project.find_effect_by_id_mut(&fx_id).unwrap().set_base_param("amount", 0.7);
        let mut command = AddAutomationPointCommand::new(GraphTarget::Effect(fx_id.clone()), "amount", point(0.0, 0.2));
        command.execute(&mut project);
        assert!(!project.find_effect_by_id(&fx_id).unwrap().params.get("amount").unwrap().touched);
        // Subsequent authored points must not erase a real manual touch on an existing lane.
        project.find_effect_by_id_mut(&fx_id).unwrap().set_base_param("amount", 0.9);
        AddAutomationPointCommand::new(GraphTarget::Effect(fx_id.clone()), "amount", point(4.0, 0.5)).execute(&mut project);
        assert!(project.find_effect_by_id(&fx_id).unwrap().params.get("amount").unwrap().touched);
    }

    fn point(beat: f64, value: f32) -> AutomationPoint {
        AutomationPoint {
            beat: Beats(beat),
            value,
            shape: SegmentShape::Linear,
        }
    }

    fn lane_points<'a>(
        project: &'a Project,
        fx_id: &manifold_core::EffectId,
    ) -> &'a [AutomationPoint] {
        project
            .find_effect_by_id(fx_id)
            .and_then(|fx| fx.automation_lanes.as_ref())
            .and_then(|lanes| lanes.iter().find(|l| l.param_id.as_ref() == "amount"))
            .map(|l| l.points.as_slice())
            .unwrap_or(&[])
    }

    fn install_lane(
        project: &mut Project,
        fx_id: &manifold_core::EffectId,
        points: Vec<AutomationPoint>,
    ) {
        project
            .find_effect_by_id_mut(fx_id)
            .unwrap()
            .automation_lanes = Some(vec![AutomationLane {
            param_id: "amount".into(),
            enabled: false,
            points,
        }]);
    }

    #[test]
    fn group_move_overlap_and_collision_round_trip() {
        for delta in [4.0, -4.0] {
            let (mut project, fx_id) = project_with_effect();
            let mut original = vec![point(0.0, 0.1), point(4.0, 0.3), point(8.0, 0.6), point(12.0, 0.9)];
            original[1].shape = SegmentShape::Hold;
            original[2].shape = SegmentShape::Curved(0.4);
            install_lane(&mut project, &fx_id, original.clone());
            let shifted = |mut p: AutomationPoint| { p.beat += Beats(delta); p };
            let moves = vec![(original[1], shifted(original[1])), (original[2], shifted(original[2]))];
            let mut command = MoveAutomationPointCommand::for_group(
                GraphTarget::Effect(fx_id.clone()), "amount", moves.clone(),
            );
            command.execute(&mut project);
            let mut expected = vec![original[0], moves[0].1, moves[1].1, original[3]];
            expected.sort_by(|a, b| a.beat.partial_cmp(&b.beat).unwrap());
            assert_eq!(lane_points(&project, &fx_id), expected);
            assert!(!project.find_effect_by_id(&fx_id).unwrap().automation_lanes.as_ref().unwrap()[0].enabled);
            command.undo(&mut project);
            assert_eq!(lane_points(&project, &fx_id), original);
            command.execute(&mut project);
            assert_eq!(lane_points(&project, &fx_id), expected);
        }
    }

    #[test]
    fn group_move_restores_duplicate_destinations_and_rejects_missing_sources() {
        let (mut project, fx_id) = project_with_effect();
        let original = vec![point(0.0, 0.1), point(4.0, 0.4), point(8.0, 0.7), point(8.0, 0.9)];
        install_lane(&mut project, &fx_id, original.clone());
        let mut command = MoveAutomationPointCommand::for_group(
            GraphTarget::Effect(fx_id.clone()), "amount",
            vec![(original[0], point(4.0, 0.1)), (original[1], point(8.0, 0.4))],
        );
        command.execute(&mut project);
        assert_eq!(lane_points(&project, &fx_id), &[point(4.0, 0.1), point(8.0, 0.4), point(8.0, 0.7), point(8.0, 0.9)]);
        command.undo(&mut project);
        assert_eq!(lane_points(&project, &fx_id), original);
        let mut invalid = MoveAutomationPointCommand::for_group(
            GraphTarget::Effect(fx_id.clone()), "amount",
            vec![(original[0], point(4.0, 0.1)), (point(99.0, 0.2), point(12.0, 0.2))],
        );
        invalid.execute(&mut project);
        assert_eq!(lane_points(&project, &fx_id), original);
        invalid.undo(&mut project);
        assert_eq!(lane_points(&project, &fx_id), original);
    }

    #[test]
    fn add_collision_undo_redo_restores_duplicate_points_exactly() {
        let (mut project, fx_id) = project_with_effect();
        let original = vec![
            AutomationPoint {
                beat: Beats(2.0),
                value: 0.1,
                shape: SegmentShape::Hold,
            },
            AutomationPoint {
                beat: Beats(2.0),
                value: 0.2,
                shape: SegmentShape::Curved(0.4),
            },
            point(4.0, 0.8),
        ];
        install_lane(&mut project, &fx_id, original.clone());
        let mut cmd = AddAutomationPointCommand::new(
            GraphTarget::Effect(fx_id.clone()),
            "amount",
            point(2.0, 0.9),
        );
        cmd.execute(&mut project);
        assert_eq!(
            lane_points(&project, &fx_id),
            &[original[0], original[1], point(2.0, 0.9), point(4.0, 0.8)]
        );
        cmd.undo(&mut project);
        assert_eq!(lane_points(&project, &fx_id), original.as_slice());
        cmd.execute(&mut project);
        assert_eq!(
            lane_points(&project, &fx_id),
            &[original[0], original[1], point(2.0, 0.9), point(4.0, 0.8)]
        );
    }

    #[test]
    fn move_collision_undo_redo_restores_entire_lane() {
        let (mut project, fx_id) = project_with_effect();
        let original = vec![
            point(0.0, 0.1),
            point(4.0, 0.4),
            point(4.0, 0.5),
            point(8.0, 0.8),
        ];
        install_lane(&mut project, &fx_id, original.clone());
        let mut cmd = MoveAutomationPointCommand::new(
            GraphTarget::Effect(fx_id.clone()),
            "amount",
            point(0.0, 0.1),
            point(4.0, 0.9),
        );
        cmd.execute(&mut project);
        assert_eq!(
            lane_points(&project, &fx_id),
            &[point(4.0, 0.9), point(4.0, 0.4), point(4.0, 0.5), point(8.0, 0.8)]
        );
        cmd.undo(&mut project);
        assert_eq!(lane_points(&project, &fx_id), original.as_slice());
        cmd.execute(&mut project);
        assert_eq!(
            lane_points(&project, &fx_id),
            &[point(4.0, 0.9), point(4.0, 0.4), point(4.0, 0.5), point(8.0, 0.8)]
        );
    }

    #[test]
    fn move_missing_source_is_noop_and_undo_is_safe() {
        let (mut project, fx_id) = project_with_effect();
        install_lane(&mut project, &fx_id, vec![point(4.0, 0.4)]);
        let mut cmd = MoveAutomationPointCommand::new(
            GraphTarget::Effect(fx_id.clone()), "amount", point(0.0, 0.1), point(4.0, 0.9)
        );
        cmd.execute(&mut project);
        cmd.undo(&mut project);
        assert_eq!(lane_points(&project, &fx_id), &[point(4.0, 0.4)]);
    }

    #[test]
    fn repeated_execute_captures_each_replica_state() {
        let (mut ui, ui_id) = project_with_effect();
        let (mut content, content_id) = project_with_effect();
        install_lane(&mut ui, &ui_id, vec![point(2.0, 0.1)]);
        install_lane(&mut content, &content_id, vec![point(2.0, 0.3)]);
        let mut cmd = AddAutomationPointCommand::new(
            GraphTarget::Effect(ui_id.clone()),
            "amount",
            point(2.0, 0.9),
        );
        cmd.execute(&mut ui);
        cmd.target = GraphTarget::Effect(content_id.clone());
        cmd.execute(&mut content);
        cmd.undo(&mut content);
        assert_eq!(lane_points(&content, &content_id), &[point(2.0, 0.3)]);
    }

    #[test]
    fn add_point_creates_lane_and_undo_removes_it() {
        let (mut project, fx_id) = project_with_effect();
        let target = GraphTarget::Effect(fx_id.clone());
        let mut cmd = AddAutomationPointCommand::new(target, "amount", point(2.0, 0.5));

        cmd.execute(&mut project);
        assert_eq!(lane_points(&project, &fx_id).len(), 1);

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

        // Insert a point BEFORE the existing one — must land first after sort.
        let mut second = AddAutomationPointCommand::new(target.clone(), "amount", point(0.0, 0.2));
        second.execute(&mut project);

        let points = lane_points(&project, &fx_id);
        assert_eq!(points.len(), 2);
        assert_eq!(
            points[0].beat.0, 0.0,
            "sorted-by-beat invariant holds after insert"
        );
        assert_eq!(points[1].beat.0, 4.0);

        // Undo the second add: only that point goes, lane stays.
        second.undo(&mut project);
        let points = lane_points(&project, &fx_id);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].beat.0, 4.0);
    }

    #[test]
    fn add_distinct_values_at_one_beat_and_undo_preserves_both() {
        let (mut project, fx_id) = project_with_effect();
        let target = GraphTarget::Effect(fx_id.clone());
        let mut low = AddAutomationPointCommand::new(target.clone(), "amount", point(4.0, 0.2));
        low.execute(&mut project);
        let mut high = AddAutomationPointCommand::new(target, "amount", point(4.0, 0.8));
        high.execute(&mut project);
        assert_eq!(lane_points(&project, &fx_id), &[point(4.0, 0.2), point(4.0, 0.8)]);
        high.undo(&mut project);
        assert_eq!(lane_points(&project, &fx_id), &[point(4.0, 0.2)]);
    }

    #[test]
    fn moving_across_an_existing_tie_preserves_distinct_points_and_order() {
        let (mut project, fx_id) = project_with_effect();
        install_lane(&mut project, &fx_id, vec![point(0.0, 0.1), point(4.0, 0.4), point(4.0, 0.5)]);
        let mut mv = MoveAutomationPointCommand::new(
            GraphTarget::Effect(fx_id.clone()), "amount", point(0.0, 0.1), point(4.0, 0.9),
        );
        mv.execute(&mut project);
        assert_eq!(lane_points(&project, &fx_id), &[point(4.0, 0.9), point(4.0, 0.4), point(4.0, 0.5)]);
    }

    #[test]
    fn moving_later_point_left_places_it_after_existing_tie() {
        let (mut project, fx_id) = project_with_effect();
        install_lane(&mut project, &fx_id, vec![point(4.0, 0.4), point(4.0, 0.5), point(8.0, 0.9)]);
        let mut mv = MoveAutomationPointCommand::new(
            GraphTarget::Effect(fx_id.clone()), "amount", point(8.0, 0.9), point(4.0, 0.8),
        );
        mv.execute(&mut project);
        assert_eq!(lane_points(&project, &fx_id), &[point(4.0, 0.4), point(4.0, 0.5), point(4.0, 0.8)]);
    }

    #[test]
    fn group_move_preserves_step_order_in_both_directions_and_undo() {
        for destination in [0.0, 8.0] {
            let (mut project, fx_id) = project_with_effect();
            let original = vec![point(4.0, 0.1234567), point(4.0, 0.7654321)];
            install_lane(&mut project, &fx_id, original.clone());
            // Selection order must not change the authored step order.
            let mut command = MoveAutomationPointCommand::for_group(
                GraphTarget::Effect(fx_id.clone()), "amount",
                vec![(original[1], point(destination, original[1].value)),
                     (original[0], point(destination, original[0].value))],
            );
            command.execute(&mut project);
            let moved = vec![point(destination, original[0].value), point(destination, original[1].value)];
            assert_eq!(lane_points(&project, &fx_id), moved);
            command.undo(&mut project);
            assert_eq!(lane_points(&project, &fx_id), original);
            command.execute(&mut project);
            assert_eq!(lane_points(&project, &fx_id), moved);
        }
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
        assert_eq!(
            points[0].beat.0, 0.0,
            "undo restores the original beat/order"
        );
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
        assert_eq!(
            lane_points(&project, &fx_id).len(),
            2,
            "undo restores both points"
        );

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
        assert_eq!(
            lane_points(&project, &fx_id).len(),
            1,
            "undo restores the lane + its points"
        );

        rm.execute(&mut project);
        let fx = project.find_effect_by_id(&fx_id).unwrap();
        assert!(
            fx.automation_lanes.as_ref().is_none_or(|v| v.is_empty()),
            "redo re-applies"
        );
    }

    #[test]
    fn generator_target_creates_gen_params_and_lane() {
        // GraphTarget::Generator auto-inits gen_params via with_preset_graph_mut
        // when the layer has none yet — pin that this path works for automation
        // commands the same way it does for envelopes/drivers.
        let layer = Layer::new_generator("GenLayer".into(), TEST_FX, 0);
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

    #[test]
    fn commit_recorded_gesture_creates_lane_and_undo_removes_it() {
        // No pre-existing lane: `old_points: None` mirrors
        // AddAutomationPointCommand's created_lane path — undo removes the
        // whole lane, not just points.
        let (mut project, fx_id) = project_with_effect();
        let target = GraphTarget::Effect(fx_id.clone());
        let new_points = vec![point(4.0, 0.3), point(6.0, 0.7)];
        let mut cmd = CommitRecordedGestureCommand::new(target, "amount", new_points.clone(), None);

        cmd.execute(&mut project);
        assert_eq!(lane_points(&project, &fx_id), new_points.as_slice());

        cmd.undo(&mut project);
        let fx = project.find_effect_by_id(&fx_id).unwrap();
        assert!(
            fx.automation_lanes.as_ref().is_none_or(|v| v.is_empty()),
            "undo of a gesture that created the lane removes the whole lane"
        );

        cmd.execute(&mut project);
        assert_eq!(
            lane_points(&project, &fx_id),
            new_points.as_slice(),
            "redo re-applies"
        );
    }

    #[test]
    fn composite_group_delete_highest_index_first_survives_execute_and_undo() {
        // P4 Unit B (marquee group-delete): `RemoveAutomationPointCommand`
        // removes BY INDEX at execute time, so deleting several points in one
        // undo entry must build the removals highest-index-first — otherwise
        // an earlier removal shifts every later target index down by one.
        // This pins the exact ordering `CompositeCommand` needs to compose
        // correctly with index-based removal.
        use crate::command::CompositeCommand;

        let (mut project, fx_id) = project_with_effect();
        let target = GraphTarget::Effect(fx_id.clone());
        // Seed 7 points at beats 0..6 (indices 0..6 once sorted).
        for beat in 0..7 {
            let mut add = AddAutomationPointCommand::new(
                target.clone(),
                "amount",
                point(beat as f64, 0.1 * beat as f32),
            );
            add.execute(&mut project);
        }
        assert_eq!(lane_points(&project, &fx_id).len(), 7);

        // Delete the points originally at indices 1, 3, 5 — built HIGH TO LOW
        // per the design's mandatory ordering.
        let commands: Vec<Box<dyn Command>> = vec![
            Box::new(RemoveAutomationPointCommand::new(
                target.clone(),
                "amount",
                5,
            )),
            Box::new(RemoveAutomationPointCommand::new(
                target.clone(),
                "amount",
                3,
            )),
            Box::new(RemoveAutomationPointCommand::new(
                target.clone(),
                "amount",
                1,
            )),
        ];
        let mut group = CompositeCommand::new(commands, "Delete Automation Points".to_string());

        group.execute(&mut project);
        let remaining: Vec<f64> = lane_points(&project, &fx_id)
            .iter()
            .map(|p| p.beat.0)
            .collect();
        assert_eq!(
            remaining,
            vec![0.0, 2.0, 4.0, 6.0],
            "indices 1,3,5 (beats 1,3,5) removed; 0,2,4,6 survive"
        );

        group.undo(&mut project);
        let restored: Vec<f64> = lane_points(&project, &fx_id)
            .iter()
            .map(|p| p.beat.0)
            .collect();
        assert_eq!(
            restored,
            vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            "undo (reverse order) fully restores the original 7-point set"
        );

        group.execute(&mut project);
        let redone: Vec<f64> = lane_points(&project, &fx_id)
            .iter()
            .map(|p| p.beat.0)
            .collect();
        assert_eq!(
            redone,
            vec![0.0, 2.0, 4.0, 6.0],
            "redo re-applies the whole group deletion"
        );
    }

    #[test]
    fn commit_recorded_gesture_over_existing_lane_restores_pre_gesture_points_on_undo() {
        // A lane already exists (pre-gesture curve). The gesture punches over
        // part of it; undo must restore the EXACT pre-gesture point set, not
        // whatever `execute` self-captured (there is nothing to self-capture
        // here — `old_points` is the explicit reverse, the
        // `new_with_reverse` precedent).
        let (mut project, fx_id) = project_with_effect();
        let target = GraphTarget::Effect(fx_id.clone());
        let mut seed = AddAutomationPointCommand::new(target.clone(), "amount", point(0.0, 0.2));
        seed.execute(&mut project);
        let mut seed2 = AddAutomationPointCommand::new(target.clone(), "amount", point(10.0, 0.9));
        seed2.execute(&mut project);
        let pre_gesture_points = lane_points(&project, &fx_id).to_vec();
        assert_eq!(pre_gesture_points.len(), 2);

        // Simulate the gesture-closure join: punch-in at beat 4, recorded up
        // to beat 6, old curve resumes at beat 10 (untouched).
        let joined = vec![
            point(0.0, 0.2),
            point(4.0, 0.4),
            point(6.0, 0.5),
            point(10.0, 0.9),
        ];
        let mut cmd = CommitRecordedGestureCommand::new(
            target,
            "amount",
            joined.clone(),
            Some(pre_gesture_points.clone()),
        );

        cmd.execute(&mut project);
        assert_eq!(lane_points(&project, &fx_id), joined.as_slice());

        cmd.undo(&mut project);
        assert_eq!(
            lane_points(&project, &fx_id),
            pre_gesture_points.as_slice(),
            "undo restores the exact pre-gesture point set"
        );

        cmd.execute(&mut project);
        assert_eq!(
            lane_points(&project, &fx_id),
            joined.as_slice(),
            "redo re-applies the join"
        );
    }
}
