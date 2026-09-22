//! Guard the already-previewed RGB transaction at the content-thread boundary.
use manifold_core::{GraphTarget, PresetTypeId, effects::ParamId, project::Project};
use manifold_editing::{
    command::{Command, CompositeCommand},
    commands::effects::ChangeGraphParamCommand,
};

#[derive(Debug)]
pub(super) struct ChangeMaterialColourCommand {
    target: GraphTarget,
    ids: [ParamId; 3],
    preset: PresetTypeId,
    baseline: [f32; 3],
    live: [f32; 3],
    expected: [f32; 3],
    inner: CompositeCommand,
    rejected: bool,
}

impl ChangeMaterialColourCommand {
    pub(super) fn new(
        target: GraphTarget,
        ids: [ParamId; 3],
        preset: PresetTypeId,
        baseline: [f32; 3],
        live: [f32; 3],
    ) -> Self {
        let commands = ids
            .iter()
            .cloned()
            .zip(baseline)
            .zip(live)
            .map(|((id, old), new)| {
                Box::new(ChangeGraphParamCommand::new(target.clone(), id, old, new))
                    as Box<dyn Command>
            })
            .collect();
        Self {
            target,
            ids,
            preset,
            baseline,
            live,
            expected: live,
            inner: CompositeCommand::new(commands, "Change material colour".into()),
            rejected: false,
        }
    }
}

/// Revert only our own still-current preview. Never overwrite a newer edit.
pub(super) fn rollback_preview(
    project: &mut Project,
    target: &GraphTarget,
    ids: &[ParamId; 3],
    preset: &PresetTypeId,
    baseline: [f32; 3],
    live: [f32; 3],
) {
    let unchanged = project.preset_instance(target).is_some_and(|inst| {
        inst.effect_type() == preset
            && ids
                .iter()
                .zip(live)
                .all(|(id, value)| inst.params.contains(id) && inst.get_base_param(id) == value)
    });
    if unchanged {
        project.with_preset_graph_mut(target, |inst| {
            for (id, value) in ids.iter().zip(baseline) {
                inst.set_base_param(id, value);
            }
        });
    }
}

impl Command for ChangeMaterialColourCommand {
    fn execute(&mut self, project: &mut Project) {
        self.rejected =
            !project.preset_instance(&self.target).is_some_and(|inst| {
                inst.effect_type() == &self.preset
                    && self.ids.iter().zip(self.expected).all(|(id, value)| {
                        inst.params.contains(id) && inst.get_base_param(id) == value
                    })
            }) || !super::projection::material::rgb_editable(project, &self.target, &self.ids);
        if self.rejected {
            rollback_preview(
                project,
                &self.target,
                &self.ids,
                &self.preset,
                self.baseline,
                self.live,
            );
        } else {
            self.inner.execute(project);
            self.expected = self.baseline;
        }
    }
    fn undo(&mut self, project: &mut Project) {
        self.inner.undo(project);
    }
    fn description(&self) -> &str {
        "Change material colour"
    }
    fn was_applied(&self) -> bool {
        !self.rejected && self.inner.was_applied()
    }
    fn rejection_reason(&self) -> Option<&str> {
        if self.rejected {
            Some("Material colour or its ownership changed during the gesture")
        } else {
            self.inner.rejection_reason()
        }
    }
}
