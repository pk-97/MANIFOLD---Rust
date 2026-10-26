//! The one place an edit `ContentCommand` runs through `EditingService`.
//!
//! The content thread, the headless flow harness and the unit tests all call
//! [`run_edit`] with their own service and project, so an edit behaves the
//! same wherever it runs. The caller folds the returned [`EditOutcome`] into
//! whatever it publishes (a toast, a selection receipt, a rejection message).

use crate::content_command::ContentCommand;
use crate::content_state::UndoRedoEvent;
use crate::edit_selection::EditSelection;
use manifold_core::change::ChangeSet;
use manifold_core::project::Project;
use manifold_editing::command::CompositeCommand;
use manifold_editing::edit::ProjectEdit;
use manifold_editing::service::EditingService;

/// What one edit left behind for the caller to publish.
#[derive(Default)]
pub(crate) struct EditOutcome {
    /// Why the edit was refused; the project is unchanged.
    pub rejection: Option<String>,
    /// An undo or redo that moved history.
    pub undo_redo: Option<UndoRedoEvent>,
    /// The thing a `ExecuteSelecting` edit created, found after it ran.
    pub selection: Option<EditSelection>,
    /// The edit changed the output size or render scale.
    pub output_changed: bool,
    /// A non-undoable write moved a part outside the live values. It leaves
    /// the data version alone, and only a project snapshot carries it to the
    /// UI, so the caller publishes one.
    pub publish: bool,
}

fn output_of(project: &Project) -> (i32, i32, f32) {
    (
        project.settings.output_width,
        project.settings.output_height,
        project.settings.render_scale,
    )
}

fn same_output(a: (i32, i32, f32), b: (i32, i32, f32)) -> bool {
    a.0 == b.0 && a.1 == b.1 && (a.2 - b.2).abs() <= 0.01
}

/// Run an edit variant of `ContentCommand` (execute, batch, selecting,
/// mutate, undo, redo, rejection). Any other command gives `None`.
/// `resize` is asked to accept a candidate project only when the edit changed
/// the output size or render scale; an `Err` rolls the edit back and comes
/// back as the rejection.
pub(crate) fn run_edit(
    service: &mut EditingService,
    mut project: ProjectEdit<'_>,
    cmd: ContentCommand,
    mut resize: impl FnMut(&Project) -> Result<(), String>,
) -> Option<EditOutcome> {
    let before = output_of(project.project());
    let mut validate = |candidate: &Project| {
        if same_output(before, output_of(candidate)) {
            Ok(())
        } else {
            resize(candidate)
        }
    };
    let mut outcome = EditOutcome::default();
    let mut pending_selection = None;
    match cmd {
        ContentCommand::GraphEditRejected(message) => {
            outcome.rejection = Some(message);
            return Some(outcome);
        }
        ContentCommand::MutateProject {
            touches,
            f,
            gesture,
        } => {
            // A gesture's moves wait for its commit, which publishes.
            if gesture {
                service.mutate_gesture(touches, project, f);
            } else {
                let wrote = service.mutate(touches, project, f);
                outcome.publish = !wrote.minus(ChangeSet::LIVE).is_empty();
            }
            return Some(outcome);
        }
        ContentCommand::ExecuteSelecting(command, request) => {
            pending_selection = Some(request.capture(project.project()));
            service.execute_with_validation(command, project.reborrow(), &mut validate);
        }
        ContentCommand::Execute(command) => {
            service.execute_with_validation(command, project.reborrow(), &mut validate);
        }
        ContentCommand::ExecuteBatch(cmds, description) => {
            let batch = Box::new(CompositeCommand::new(cmds, description));
            service.execute_with_validation(batch, project.reborrow(), &mut validate);
        }
        ContentCommand::Undo => {
            // The description is read first: the command moves to the redo
            // stack once undone.
            let description = service.peek_undo_description().map(str::to_string);
            if service.undo_with_validation(project.reborrow(), &mut validate) {
                outcome.undo_redo = Some(UndoRedoEvent {
                    is_redo: false,
                    description: description.unwrap_or_else(|| "action".to_string()),
                });
            }
        }
        ContentCommand::Redo => {
            let description = service.peek_redo_description().map(str::to_string);
            if service.redo_with_validation(project.reborrow(), &mut validate) {
                outcome.undo_redo = Some(UndoRedoEvent {
                    is_redo: true,
                    description: description.unwrap_or_else(|| "action".to_string()),
                });
            }
        }
        _ => return None,
    }
    if let Some(message) = service.take_rejection() {
        outcome.rejection = Some(message);
        outcome.undo_redo = None;
        return Some(outcome);
    }
    outcome.output_changed = !same_output(before, output_of(project.project()));
    outcome.selection = pending_selection.and_then(|pending| pending.resolve(project.project()));
    Some(outcome)
}

/// Run every edit in `sent` through `service` over `project`, as the content
/// thread would, and return one outcome per edit. Other commands are skipped.
/// Tests dispatch over a project, then call this with what the dispatch sent
/// and read the project.
#[cfg(test)]
pub(crate) fn apply_sent(
    service: &mut EditingService,
    project: &mut Project,
    sent: impl IntoIterator<Item = ContentCommand>,
) -> Vec<EditOutcome> {
    sent.into_iter()
        .filter_map(|cmd| run_edit(service, ProjectEdit::new(project), cmd, |_| Ok(())))
        .collect()
}

/// `apply_sent` over the app's shown project, then show the result as the
/// next snapshot would, with the selection receipts applied. For tests that
/// drive the `Application` itself.
#[cfg(test)]
pub(crate) fn land_on_app(
    app: &mut crate::app::Application,
    service: &mut EditingService,
    sent: impl IntoIterator<Item = ContentCommand>,
) -> Vec<EditOutcome> {
    let mut project = (*app.local_project).clone();
    let outcomes = apply_sent(service, &mut project, sent);
    app.local_project = crate::ui_bridge::LocalProject::new(project);
    for outcome in &outcomes {
        assert!(outcome.rejection.is_none(), "{:?}", outcome.rejection);
        if let Some(selection) = outcome.selection.clone() {
            let update = crate::edit_selection::EditSelectionUpdate::after(None, selection);
            crate::edit_selection::apply_update(
                &mut app.ws.ui_root,
                &app.local_project,
                Some(&update),
                &mut app.selection,
                &mut app.active_layer_id,
            );
        }
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::change::Part;
    use manifold_editing::commands::settings::ChangeTonemapCurveCommand;

    fn tonemap(enabled: bool) -> Box<dyn manifold_editing::command::Command> {
        Box::new(ChangeTonemapCurveCommand::new(
            manifold_core::TonemapCurve::default(),
            !enabled,
            manifold_core::TonemapCurve::default(),
            enabled,
        ))
    }

    #[test]
    fn execute_undo_redo_round_trip_with_toast_labels() {
        let mut project = Project::default();
        let mut service = EditingService::new();
        let was = project.settings.tonemap_enabled;
        let sent = vec![
            ContentCommand::Execute(tonemap(!was)),
            ContentCommand::Undo,
            ContentCommand::Redo,
        ];
        let out = apply_sent(&mut service, &mut project, sent);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|o| o.rejection.is_none()));
        let undo = out[1].undo_redo.as_ref().unwrap();
        let redo = out[2].undo_redo.as_ref().unwrap();
        assert!(!undo.is_redo && redo.is_redo);
        assert_eq!(undo.description, redo.description);
        assert_eq!(project.settings.tonemap_enabled, !was);
    }

    #[test]
    fn undo_with_empty_history_reports_nothing() {
        let mut project = Project::default();
        let mut service = EditingService::new();
        let out = apply_sent(&mut service, &mut project, [ContentCommand::Undo]);
        assert!(out[0].undo_redo.is_none() && out[0].rejection.is_none());
    }

    #[test]
    fn mutate_runs_without_history_and_gesture_rows_wait() {
        let mut project = Project::default();
        let mut service = EditingService::new();
        let sent = vec![
            ContentCommand::mutate(
                ChangeSet::of(&[Part::Settings]),
                Box::new(|p| p.settings.frame_rate = 24.0),
            ),
            ContentCommand::gesture_move(
                ChangeSet::of(&[Part::Settings]),
                Box::new(|p| p.settings.frame_rate = 30.0),
            ),
        ];
        apply_sent(&mut service, &mut project, sent);
        assert_eq!(project.settings.frame_rate, 30.0);
        assert!(!service.can_undo());
    }

    /// A layer toggle (collapse, mute) leaves the data version alone, so the
    /// outcome asks for a project publish; a live-value write and a gesture
    /// move do not.
    #[test]
    fn a_layer_toggle_asks_for_a_publish_and_live_writes_do_not() {
        let mut project = Project::default();
        for i in 0..3 {
            project
                .timeline
                .layers
                .push(manifold_core::layer::Layer::new_video(format!("Video {i}"), i));
        }
        let id = project.timeline.layers[2].layer_id.clone();
        let mut service = EditingService::new();
        let out = apply_sent(
            &mut service,
            &mut project,
            [
                ContentCommand::mutate(
                    crate::content_command::touch::LAYERS,
                    Box::new(move |p| {
                        let (_, layer) = p.timeline.find_layer_by_id_mut(&id).expect("layer");
                        layer.is_collapsed = true;
                        layer.is_muted = true;
                    }),
                ),
                ContentCommand::mutate(crate::content_command::touch::VALUES, Box::new(|_| {})),
                ContentCommand::gesture_move(
                    crate::content_command::touch::LAYERS,
                    Box::new(|p| p.timeline.layers[0].is_solo = true),
                ),
            ],
        );
        assert!(out[0].publish && !out[1].publish && !out[2].publish);
        assert_eq!(service.data_version(), 0);
        let layers = &project.timeline.layers;
        assert!(layers[2].is_collapsed && layers[2].is_muted);
        assert!(layers[..2].iter().all(|l| !l.is_collapsed && !l.is_muted));
    }

    #[test]
    fn rejection_message_comes_back() {
        let mut project = Project::default();
        let mut service = EditingService::new();
        let out = apply_sent(
            &mut service,
            &mut project,
            [ContentCommand::GraphEditRejected("nope".into())],
        );
        assert_eq!(out[0].rejection.as_deref(), Some("nope"));
    }

    #[test]
    fn a_failed_resize_rolls_the_edit_back() {
        let mut project = Project::default();
        let mut service = EditingService::new();
        let scale = project.settings.render_scale;
        let cmd = ContentCommand::Execute(Box::new(
            manifold_editing::commands::settings::ChangeRenderScaleCommand::new(scale, 0.5),
        ));
        let out = run_edit(&mut service, ProjectEdit::new(&mut project), cmd, |_| {
            Err("no gpu".into())
        })
        .unwrap();
        assert_eq!(out.rejection.as_deref(), Some("no gpu"));
        assert_eq!(project.settings.render_scale, scale);
        assert!(!service.can_undo());

        let cmd = ContentCommand::Execute(Box::new(
            manifold_editing::commands::settings::ChangeRenderScaleCommand::new(scale, 0.5),
        ));
        let out = run_edit(
            &mut service,
            ProjectEdit::new(&mut project),
            cmd,
            |_| Ok(()),
        )
        .unwrap();
        assert!(out.rejection.is_none() && out.output_changed);
        assert_eq!(project.settings.render_scale, 0.5);
    }
}
