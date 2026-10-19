//! Rack header controls, app side: the bypass switch and the wet/dry scrub
//! become `ToggleGroupCommand` and `ChangeGroupWetDryCommand` through the
//! content thread. A wet/dry drag writes the live value with gesture moves and
//! commits one command, so the whole drag is one undo step.

use super::scrub::ResolvedScrub;
use super::{DispatchCtx, DispatchResult};
use crate::content_command::ContentCommand;
use manifold_core::project::Project;
use manifold_core::{EffectGroupId, LayerId};
use manifold_editing::commands::effect_groups::{ChangeGroupWetDryCommand, ToggleGroupCommand};
use manifold_editing::commands::effect_target::{EffectTarget, with_effects, with_effects_mut};
use manifold_ui::{InspectorTab, ScrubPhase};

/// The effect list a rack header action addresses: master, or the layer the
/// header was drawn for.
pub(crate) fn rack_target(tab: InspectorTab, layer_id: &Option<LayerId>) -> Option<EffectTarget> {
    match tab {
        InspectorTab::Master => Some(EffectTarget::Master),
        _ => layer_id.clone().map(|layer_id| EffectTarget::Layer { layer_id }),
    }
}

fn group_state(project: &Project, target: &EffectTarget, group_id: &EffectGroupId) -> Option<(bool, f32)> {
    with_effects(project, target, |_, groups| {
        groups.iter().find(|g| g.id == *group_id).map(|g| (g.enabled, g.wet_dry))
    })
    .flatten()
}

/// Bypass switch: set the rack's `enabled` flag (undoable).
pub(crate) fn dispatch_bypass(
    tab: InspectorTab,
    layer_id: &Option<LayerId>,
    group_id: &EffectGroupId,
    enabled: bool,
    ctx: &mut DispatchCtx,
) -> DispatchResult {
    if let Some(target) = rack_target(tab, layer_id)
        && let Some((current, _)) = group_state(&ctx.project, &target, group_id)
        && current != enabled
    {
        let cmd = ToggleGroupCommand::new(target, group_id.clone(), current, enabled);
        ContentCommand::send(ctx.content_tx, ContentCommand::ExecuteOnContent(Box::new(cmd)));
    }
    DispatchResult::handled()
}

/// Write the rack's wet/dry in place, the same write a drag move and the
/// mid-gesture restore both use.
pub(crate) fn set_wet_dry(project: &mut Project, target: &EffectTarget, group_id: &EffectGroupId, value: f32) {
    with_effects_mut(project, target, |_, groups| {
        if let Some(group) = groups.iter_mut().find(|g| g.id == *group_id) {
            group.wet_dry = value;
        }
    });
}

/// Wet/dry scrub: Begin captures the baseline, Move writes live, Commit sends
/// the one undo command.
pub(crate) fn dispatch_wet_dry(
    tab: InspectorTab,
    layer_id: &Option<LayerId>,
    group_id: &EffectGroupId,
    phase: &ScrubPhase,
    ctx: &mut DispatchCtx,
) -> DispatchResult {
    let Some(target) = rack_target(tab, layer_id) else {
        return DispatchResult::handled();
    };
    match phase {
        ScrubPhase::Begin => {
            if let Some((_, wet_dry)) = group_state(&ctx.project, &target, group_id) {
                ctx.scrub.active = Some(ResolvedScrub::RackWetDry {
                    target,
                    group_id: group_id.clone(),
                    baseline: wet_dry,
                    live: wet_dry,
                });
            }
        }
        ScrubPhase::Move(sv) => {
            if let Some(v) = sv.scalar().map(|v| v.clamp(0.0, 1.0)) {
                if let Some(ResolvedScrub::RackWetDry { live, .. }) = &mut ctx.scrub.active {
                    *live = v;
                }
                let group_id = group_id.clone();
                ctx.gesture_move(crate::content_command::touch::VALUES, move |p| {
                    set_wet_dry(p, &target, &group_id, v);
                });
            }
        }
        ScrubPhase::Commit => {
            if let Some(ResolvedScrub::RackWetDry { baseline, live, .. }) = ctx.scrub.active.take()
                && (baseline - live).abs() > f32::EPSILON
            {
                let cmd = ChangeGroupWetDryCommand::new(target, group_id.clone(), baseline, live);
                ContentCommand::send(ctx.content_tx, ContentCommand::Execute(Box::new(cmd)));
            }
        }
    }
    DispatchResult::handled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SelectionState;
    use crate::ui_root::UIRoot;
    use manifold_core::effects::EffectGroup;
    use manifold_core::types::LayerType;
    use manifold_core::PresetTypeId;
    use manifold_editing::service::EditingService;
    use manifold_ui::{PanelAction, ParamsAction, ScrubValue, ValueRef};

    struct Rig {
        tx: crossbeam_channel::Sender<ContentCommand>,
        rx: crossbeam_channel::Receiver<ContentCommand>,
        ui: UIRoot,
        selection: SelectionState,
        active_layer: Option<LayerId>,
        prefs: crate::user_prefs::UserPrefs,
        scrub: crate::ui_bridge::ScrubState,
        content: crate::content_state::ContentState,
    }

    impl Rig {
        fn new(layer: &LayerId) -> Self {
            let (tx, rx) = crossbeam_channel::unbounded();
            Self {
                tx,
                rx,
                ui: UIRoot::new(),
                selection: manifold_ui::UIState::new(),
                active_layer: Some(layer.clone()),
                prefs: crate::user_prefs::UserPrefs::in_memory(),
                scrub: crate::ui_bridge::ScrubState::default(),
                content: crate::content_state::ContentState::default(),
            }
        }

        fn dispatch(&mut self, action: &PanelAction, project: &mut Project) {
            let mut edits = crate::gesture_preview::UiEdits::default();
            let mut ctx = crate::ui_bridge::DispatchCtx {
                project: crate::ui_bridge::ProjectRef::new(project, &mut edits, &self.tx),
                content_tx: &self.tx,
                content_state: &self.content,
                ui: &mut self.ui,
                selection: &mut self.selection,
                active_layer: &mut self.active_layer,
                user_prefs: &mut self.prefs,
                editor_target: None,
                scrub: &mut self.scrub,
                dialogs: Default::default(),
            };
            crate::ui_bridge::dispatch(action, &mut ctx);
        }

        /// Apply what reached the content thread, as it would: live moves
        /// and executed commands. Returns the undoable commands that landed.
        fn apply(&self, service: &mut EditingService, project: &mut Project) -> usize {
            let mut landed = 0;
            for cmd in self.rx.try_iter() {
                match cmd {
                    ContentCommand::MutateProject { f, .. } => f(project),
                    ContentCommand::Execute(cmd) | ContentCommand::ExecuteOnContent(cmd) => {
                        service.execute(cmd, &mut *project);
                        landed += 1;
                    }
                    _ => {}
                }
            }
            landed
        }
    }

    fn rack_project() -> (Project, LayerId, EffectGroupId) {
        let mut project = Project::default();
        let idx = project.timeline.add_layer(
            "Rack",
            LayerType::Generator,
            PresetTypeId::from_string("Plasma".to_string()),
        );
        let layer_id = project.timeline.layers[idx].layer_id.clone();
        let group = EffectGroup::new("Rack".to_string());
        let group_id = group.id.clone();
        project.timeline.layers[idx].effect_groups = Some(vec![group]);
        (project, layer_id, group_id)
    }

    fn group_of(project: &Project, layer: &LayerId) -> EffectGroup {
        let (_, layer) = project.timeline.find_layer_by_id(layer).unwrap();
        layer.effect_groups.as_ref().unwrap()[0].clone()
    }

    #[test]
    fn bypass_switch_is_one_undoable_toggle() {
        let (mut project, layer, group_id) = rack_project();
        let mut rig = Rig::new(&layer);
        let mut content = project.clone();
        let mut service = EditingService::new();
        rig.dispatch(
            &PanelAction::Params(ParamsAction::EffectGroupBypass {
                tab: InspectorTab::Layer,
                layer_id: Some(layer.clone()),
                group_id: group_id.clone(),
                enabled: false,
            }),
            &mut project,
        );
        assert_eq!(rig.apply(&mut service, &mut content), 1);
        assert!(!group_of(&content, &layer).enabled);
        assert!(service.undo(&mut content));
        assert!(group_of(&content, &layer).enabled);
    }

    #[test]
    fn wet_dry_drag_is_one_undo_step_back_to_the_start() {
        let (mut project, layer, group_id) = rack_project();
        let mut rig = Rig::new(&layer);
        let mut content = project.clone();
        let mut service = EditingService::new();
        let address = ValueRef::RackWetDry {
            tab: InspectorTab::Layer,
            layer_id: Some(layer.clone()),
            group_id: group_id.clone(),
        };
        let scrub = |phase| PanelAction::Scrub(address.clone(), phase);
        rig.dispatch(&scrub(ScrubPhase::Begin), &mut project);
        for v in [0.9, 0.6, 0.3] {
            rig.dispatch(&scrub(ScrubPhase::Move(ScrubValue::Scalar(v))), &mut project);
        }
        assert_eq!(group_of(&project, &layer).wet_dry, 0.3, "the UI copy tracks the cursor");
        rig.dispatch(&scrub(ScrubPhase::Commit), &mut project);
        assert_eq!(rig.apply(&mut service, &mut content), 1, "the whole drag is one command");
        assert_eq!(group_of(&content, &layer).wet_dry, 0.3);
        assert!(service.undo(&mut content));
        assert_eq!(group_of(&content, &layer).wet_dry, 1.0);
        assert!(!service.undo(&mut content), "nothing else was recorded");
    }
}
