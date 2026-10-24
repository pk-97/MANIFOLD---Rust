//! Set slider range… / Reset slider range (SCENE_SURFACES_DESIGN.md D7 (user
//! choices), D15 (slider range and hard limit)).
//!
//! The range is a per-control choice in `ParamChoices.slider`. It moves what
//! the slider spans and what a 0–1 controller sweeps; the declared range on
//! the spec, which the reshape, wraps and the `Slider` limit read, is never
//! touched. `None` clears the choice.

use manifold_core::GraphTarget;
use manifold_core::project::Project;

use crate::command::Command;

#[derive(Debug)]
pub struct SetSliderRangeCommand {
    target: GraphTarget,
    param_id: String,
    range: Option<(f32, f32)>,
    /// The choice before the first execute; `None` until then.
    previous: Option<Option<(f32, f32)>>,
}

impl SetSliderRangeCommand {
    /// Ends given the wrong way round are swapped; a range equal to the
    /// declared one, empty, or not finite is stored as no choice.
    pub fn new(target: GraphTarget, param_id: String, range: Option<(f32, f32)>) -> Self {
        let range = range
            .map(|(a, b)| (a.min(b), a.max(b)))
            .filter(|(lo, hi)| lo.is_finite() && hi.is_finite() && hi > lo);
        Self {
            target,
            param_id,
            range,
            previous: None,
        }
    }

    fn write(
        &self,
        project: &mut Project,
        range: Option<(f32, f32)>,
    ) -> Option<Option<(f32, f32)>> {
        project
            .with_preset_graph_mut(&self.target, |host| {
                host.params.get_mut(&self.param_id).map(|param| {
                    let declared = (param.spec.min, param.spec.max);
                    std::mem::replace(&mut param.choices.slider, range.filter(|r| *r != declared))
                })
            })
            .flatten()
    }
}

impl Command for SetSliderRangeCommand {
    /// The span re-lays the row's slider and its lanes; the value is not moved.
    fn touches(&self) -> manifold_core::change::ChangeSet {
        use manifold_core::change::{ChangeSet, Part};
        ChangeSet::of(&[Part::Effects, Part::Shapes])
    }

    fn do_execute(&mut self, project: &mut Project) {
        let before = self.write(project, self.range);
        if self.previous.is_none() {
            self.previous = before;
        }
    }

    fn do_undo(&mut self, project: &mut Project) {
        if let Some(previous) = self.previous {
            self.write(project, previous);
        }
    }

    fn description(&self) -> &str {
        if self.range.is_some() {
            "Set Slider Range"
        } else {
            "Reset Slider Range"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{project_with_one_generator_layer, slot};
    use super::*;

    fn param(project: &Project, target: &GraphTarget) -> manifold_core::params::Param {
        let GraphTarget::Generator(layer) = target else {
            unreachable!()
        };
        let (_, layer) = project.timeline.find_layer_by_id(layer.as_str()).unwrap();
        layer
            .gen_params()
            .unwrap()
            .params
            .get("position_x")
            .unwrap()
            .clone()
    }

    #[test]
    fn set_slider_range_undo_redo() {
        let (mut project, layer) = project_with_one_generator_layer();
        let target = GraphTarget::Generator(layer);
        project.with_preset_graph_mut(&target, |host| {
            host.params.push(slot("position_x", 0.5, true))
        });
        let declared = {
            let p = param(&project, &target);
            (p.spec.min, p.spec.max)
        };

        let mut set =
            SetSliderRangeCommand::new(target.clone(), "position_x".into(), Some((-1.0, 1.0)));
        set.execute(&mut project);
        let p = param(&project, &target);
        assert_eq!(p.slider_span(), (-1.0, 1.0));
        assert_eq!(
            (p.spec.min, p.spec.max),
            declared,
            "the declared range never moves"
        );

        let mut reset = SetSliderRangeCommand::new(target.clone(), "position_x".into(), None);
        reset.execute(&mut project);
        assert_eq!(param(&project, &target).choices.slider, None);

        reset.undo(&mut project);
        assert_eq!(param(&project, &target).choices.slider, Some((-1.0, 1.0)));
        set.undo(&mut project);
        assert_eq!(param(&project, &target).choices.slider, None);
        set.execute(&mut project);
        assert_eq!(
            param(&project, &target).choices.slider,
            Some((-1.0, 1.0)),
            "redo"
        );

        let mut same =
            SetSliderRangeCommand::new(target.clone(), "position_x".into(), Some(declared));
        same.execute(&mut project);
        assert_eq!(
            param(&project, &target).choices.slider,
            None,
            "the declared range is no choice"
        );
        let mut inverted =
            SetSliderRangeCommand::new(target.clone(), "position_x".into(), Some((2.0, -2.0)));
        inverted.execute(&mut project);
        assert_eq!(
            param(&project, &target).choices.slider,
            Some((-2.0, 2.0)),
            "ends are swapped"
        );

        use manifold_core::change::Part;
        assert!(set.touches().contains(Part::Effects));
        assert!(!set.touches().contains(Part::Values));
    }
}
