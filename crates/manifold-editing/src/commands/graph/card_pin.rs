//! Show on Card / Remove from Card (SCENE_SURFACES_DESIGN.md D7 (user
//! choices), D10 (cross-surface gestures)).
//!
//! A pin is a per-control choice in `ParamChoices.pin`; the card projection
//! reads it, so the command only writes that one field. `None` hands the
//! row back to its role.

use manifold_core::GraphTarget;
use manifold_core::project::Project;
use manifold_core::scene_surface::CardPin;

use crate::command::Command;

#[derive(Debug)]
pub struct SetCardPinCommand {
    target: GraphTarget,
    param_id: String,
    pin: Option<CardPin>,
    /// The pin before the first execute; `None` until then.
    previous: Option<Option<CardPin>>,
}

impl SetCardPinCommand {
    pub fn new(target: GraphTarget, param_id: String, pin: Option<CardPin>) -> Self {
        Self { target, param_id, pin, previous: None }
    }

    fn write(&self, project: &mut Project, pin: Option<CardPin>) -> Option<Option<CardPin>> {
        project
            .with_preset_graph_mut(&self.target, |host| {
                host.params.get_mut(&self.param_id).map(|param| std::mem::replace(&mut param.choices.pin, pin))
            })
            .flatten()
    }
}

impl Command for SetCardPinCommand {
    /// A pin decides which rows the card builds: card shape, never a value.
    fn touches(&self) -> manifold_core::change::ChangeSet {
        manifold_core::change::ChangeSet::of(&[manifold_core::change::Part::Effects])
    }

    fn execute(&mut self, project: &mut Project) {
        let before = self.write(project, self.pin);
        if self.previous.is_none() {
            self.previous = before;
        }
    }

    fn undo(&mut self, project: &mut Project) {
        if let Some(previous) = self.previous {
            self.write(project, previous);
        }
    }

    fn description(&self) -> &str {
        match self.pin {
            Some(CardPin::Shown) => "Show on Card",
            Some(CardPin::Removed) => "Remove from Card",
            None => "Reset Card Pin",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{project_with_one_generator_layer, slot};
    use super::*;

    fn pin_of(project: &Project, target: &GraphTarget) -> Option<CardPin> {
        let GraphTarget::Generator(layer) = target else { unreachable!() };
        let (_, layer) = project.timeline.find_layer_by_id(layer.as_str()).unwrap();
        layer.gen_params().unwrap().params.get("roughness").unwrap().choices.pin
    }

    #[test]
    fn pin_unpin_undo_redo() {
        let (mut project, layer) = project_with_one_generator_layer();
        let target = GraphTarget::Generator(layer);
        project.with_preset_graph_mut(&target, |host| host.params.push(slot("roughness", 0.5, true)));

        let mut show = SetCardPinCommand::new(target.clone(), "roughness".into(), Some(CardPin::Shown));
        show.execute(&mut project);
        assert_eq!(pin_of(&project, &target), Some(CardPin::Shown));
        let mut remove = SetCardPinCommand::new(target.clone(), "roughness".into(), Some(CardPin::Removed));
        remove.execute(&mut project);
        assert_eq!(pin_of(&project, &target), Some(CardPin::Removed));

        remove.undo(&mut project);
        assert_eq!(pin_of(&project, &target), Some(CardPin::Shown));
        show.undo(&mut project);
        assert_eq!(pin_of(&project, &target), None, "undo hands the row back to its role");
        show.execute(&mut project);
        remove.execute(&mut project);
        assert_eq!(pin_of(&project, &target), Some(CardPin::Removed), "redo replays both");
        remove.undo(&mut project);
        show.undo(&mut project);
        assert_eq!(pin_of(&project, &target), None, "a redo never overwrites the captured previous pin");

        use manifold_core::change::Part;
        assert!(show.touches().contains(Part::Effects));
        assert!(!show.touches().contains(Part::Values));
    }

    #[test]
    fn missing_param_is_a_no_op() {
        let (mut project, layer) = project_with_one_generator_layer();
        let target = GraphTarget::Generator(layer);
        let mut command = SetCardPinCommand::new(target, "gone".into(), Some(CardPin::Shown));
        command.execute(&mut project);
        command.undo(&mut project);
    }
}
