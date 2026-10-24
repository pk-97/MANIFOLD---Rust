use manifold_core::GraphTarget;
use manifold_core::change::ChangeSet;
use manifold_core::project::Project;
use std::fmt::Debug;

/// An undoable edit. Implement `do_execute` and `do_undo`; everyone calls
/// `execute` and `undo`, which under test and `change-audit` hold every write
/// to `touches` (docs/UI_INVALIDATION_DESIGN.md D10). Never override them and
/// never call the `do_` methods directly: that is how a command escapes the
/// audit (`command_runs_only_through_the_audit` enforces both).
pub trait Command: Debug + Send {
    /// The edit itself. Callers use `execute`.
    fn do_execute(&mut self, project: &mut Project);
    /// The inverse edit. Callers use `undo`.
    fn do_undo(&mut self, project: &mut Project);
    /// Run the edit. Returns the parts it wrote: `touches`, plus Effects
    /// when a value it wrote changed which rows exist ([`rows_written`]).
    /// Stamp this set, not `touches`.
    fn execute(&mut self, project: &mut Project) -> ChangeSet {
        let window = crate::audit::open(project);
        let ((), rows) = rows_written(project, |project| self.do_execute(project));
        // Read after execute: a set that depends on the data is recorded there.
        let wrote = self.touches().union(rows);
        crate::audit::close(window, project, wrote, self.description());
        wrote
    }
    /// Run the inverse edit. Returns the parts it wrote, as `execute` does.
    fn undo(&mut self, project: &mut Project) -> ChangeSet {
        let window = crate::audit::open(project);
        let declared = self.touches();
        let ((), rows) = rows_written(project, |project| self.do_undo(project));
        let wrote = declared.union(rows);
        crate::audit::close(window, project, wrote, self.description());
        wrote
    }
    fn description(&self) -> &str;
    /// Append graph owners whose structural edits require canonical admission.
    /// Live value writes intentionally leave this empty.
    fn graph_admission_targets(&self, _targets: &mut Vec<GraphTarget>) {}
    /// Clip-scoped source edits resolve their generator owner at execution.
    fn graph_admission_clips(&self, _clips: &mut Vec<manifold_core::ClipId>) {}
    /// Append where in a graph this edit writes. The undo manager refuses an
    /// edit that would write inside a scene layer's sealed Scene node before
    /// it runs (`crate::commands::graph::sealed_refusal`).
    fn graph_edit_sites(&self, _sites: &mut Vec<crate::commands::graph::GraphEditSite>) {}
    /// Optional stable diagnostic when execution or admission was rejected.
    fn rejection_reason(&self) -> Option<&str> {
        None
    }
    /// Commands that can reject a stale prepared edit report whether execute
    /// actually changed the project. Existing commands retain their behavior.
    fn was_applied(&self) -> bool {
        true
    }
    /// A scene document command's document edit, so a composite of them
    /// compiles once. `None` for every other command.
    fn scene_edit(&mut self) -> Option<&mut dyn crate::commands::scene_doc::SceneEdit> {
        None
    }
    /// The project parts this command writes, on execute and on undo
    /// (docs/UI_INVALIDATION_DESIGN.md D2, INV-1). Declare from what execute
    /// and undo write, never from the name. Over-declaring only costs work;
    /// under-declaring is a stale-UI bug the change audit catches. A set that
    /// depends on the data is read after execute, so execute may record it.
    fn touches(&self) -> ChangeSet;
}

/// Run a project write and return Effects when it changed which rows exist
/// (`manifold_core::change::row_facts_key`), else nothing. The row walk
/// lives here and only here: every stamp site writes through it (commands,
/// `EditingService::mutate`, the UI's `ProjectRef`), so a value write that
/// flips a feature switch moves Effects whoever made it: a command, OSC,
/// Ableton or MIDI (UI_INVALIDATION D21). A gesture's live moves skip it
/// ([`rows_deferred`]) and its commit reports the move ([`value_moves_rows`]).
///
/// The key covers every card param in the project, so only the outermost
/// write walks it: a write nested inside another (a composite's children, a
/// batch, a command run inside a `mutate` closure) returns nothing, and the
/// outer write's one walk covers it.
pub fn rows_written<R>(project: &mut Project, f: impl FnOnce(&mut Project) -> R) -> (R, ChangeSet) {
    let Some(_outermost) = RowWalk::open() else {
        return (f(project), ChangeSet::NONE);
    };
    let before = manifold_core::change::row_facts_key(project);
    let out = f(project);
    let rows = if manifold_core::change::row_facts_key(project) == before {
        ChangeSet::NONE
    } else {
        ChangeSet::of(&[manifold_core::change::Part::Effects])
    };
    (out, rows)
}

thread_local! {
    static ROW_WALK_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    #[cfg(test)]
    static ROW_WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The open outermost row walk on this thread; closes on drop, so a panic
/// inside the write cannot leave later writes unwalked.
struct RowWalk;

impl RowWalk {
    /// `None` when a walk is already open: the write is nested.
    fn open() -> Option<Self> {
        if ROW_WALK_OPEN.replace(true) {
            return None;
        }
        #[cfg(test)]
        ROW_WALKS.set(ROW_WALKS.get() + 1);
        Some(RowWalk)
    }
}

/// A gesture's live move: run the write with the row walk held open and
/// never walked, so nothing inside it walks either. Its commit reports the
/// rows ([`value_moves_rows`]).
pub fn rows_deferred<R>(project: &mut Project, f: impl FnOnce(&mut Project) -> R) -> R {
    let _held = (!ROW_WALK_OPEN.replace(true)).then_some(RowWalk);
    f(project)
}

impl Drop for RowWalk {
    fn drop(&mut self) {
        ROW_WALK_OPEN.set(false);
    }
}

/// Effects when moving card param `param_id` from `old` to `new` changes
/// which rows exist. A gesture's live moves skip the row walk, so the value
/// is already in the project when its commit runs and the walk sees
/// nothing; the commit measures its own move with the same rule
/// (`ParamSpecDef::row_fact`). Rows then change on mouse-up, never under the
/// cursor.
pub fn value_moves_rows(
    project: &Project,
    target: &GraphTarget,
    param_id: &str,
    old: f32,
    new: f32,
) -> ChangeSet {
    let moves = project
        .preset_instance(target)
        .and_then(|inst| inst.params.get(param_id))
        .is_some_and(|param| param.spec.row_fact(old) != param.spec.row_fact(new));
    if moves {
        ChangeSet::of(&[manifold_core::change::Part::Effects])
    } else {
        ChangeSet::NONE
    }
}

/// Composite command that groups multiple commands.
/// Execute all in order, undo all in reverse.
#[derive(Debug)]
pub struct CompositeCommand {
    commands: Vec<Box<dyn Command>>,
    desc: String,
    rejection: Option<String>,
    /// Runs of scene document commands on one layer that edited the
    /// document in turn and compiled once, with what undo restores.
    scene_runs: Vec<SceneRun>,
}

/// Commands `range` edited `layer`'s document together; `before` is what
/// their one install replaced, until undone.
#[derive(Debug)]
struct SceneRun {
    range: std::ops::Range<usize>,
    layer: manifold_core::LayerId,
    before: Option<crate::commands::scene_doc::Before>,
}

impl CompositeCommand {
    pub fn new(commands: Vec<Box<dyn Command>>, description: String) -> Self {
        Self {
            commands,
            desc: description,
            rejection: None,
            scene_runs: Vec::new(),
        }
    }

    /// The commands, to run again on another copy of the project.
    pub fn into_commands(self) -> Vec<Box<dyn Command>> {
        self.commands
    }

    /// The end of the run of scene document commands on one layer that
    /// starts at `start`; `start + 1` when none starts there.
    fn scene_run_end(&mut self, start: usize) -> usize {
        let Some(layer) = self.commands[start]
            .scene_edit()
            .map(|edit| edit.layer().clone())
        else {
            return start + 1;
        };
        let mut end = start + 1;
        while end < self.commands.len()
            && self.commands[end]
                .scene_edit()
                .is_some_and(|edit| *edit.layer() == layer)
        {
            end += 1;
        }
        end
    }

    /// Edit the document with each command of `range` in turn, compile and
    /// install once, then let each write what needs the new params.
    fn run_scene_edits(
        &mut self,
        project: &mut Project,
        range: std::ops::Range<usize>,
    ) -> Result<(), String> {
        let run = &mut self.commands[range.clone()];
        let layer = run[0].scene_edit().expect("a scene run").layer().clone();
        let mut before = crate::commands::scene_doc::apply(project, &layer, |doc, values| {
            run.iter_mut().try_for_each(|command| {
                command
                    .scene_edit()
                    .expect("a scene run")
                    .edit_doc(doc, values)
            })
        })?;
        for command in run.iter_mut() {
            before.note_written(
                command
                    .scene_edit()
                    .expect("a scene run")
                    .after_install(project),
            );
        }
        self.scene_runs.push(SceneRun {
            range,
            layer,
            before: Some(before),
        });
        Ok(())
    }

    /// Undo what ran before command `upto`, last first.
    fn undo_before(&mut self, project: &mut Project, upto: usize) {
        let mut index = upto;
        while index > 0 {
            if let Some(run) = self
                .scene_runs
                .iter_mut()
                .find(|run| run.range.end == index)
            {
                if let Some(before) = run.before.take() {
                    crate::commands::scene_doc::restore(project, &run.layer, before);
                }
                index = run.range.start;
                continue;
            }
            index -= 1;
            if self.commands[index].was_applied() {
                self.commands[index].undo(project);
            }
        }
    }
}

impl Command for CompositeCommand {
    fn do_execute(&mut self, project: &mut Project) {
        self.rejection = None;
        self.scene_runs.clear();
        let mut index = 0;
        while index < self.commands.len() {
            let end = self.scene_run_end(index);
            if end - index >= 2 {
                if let Err(reason) = self.run_scene_edits(project, index..end) {
                    self.rejection = Some(reason);
                    self.undo_before(project, index);
                    return;
                }
                index = end;
                continue;
            }
            self.commands[index].execute(project);
            if let Some(reason) = self.commands[index].rejection_reason() {
                self.rejection = Some(reason.to_string());
                self.undo_before(project, index);
                return;
            }
            index += 1;
        }
    }

    fn do_undo(&mut self, project: &mut Project) {
        if self.rejection.is_some() {
            return;
        }
        let len = self.commands.len();
        self.undo_before(project, len);
    }

    fn description(&self) -> &str {
        &self.desc
    }

    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        for command in &self.commands {
            command.graph_admission_targets(targets);
        }
    }

    fn graph_admission_clips(&self, clips: &mut Vec<manifold_core::ClipId>) {
        for command in &self.commands {
            command.graph_admission_clips(clips);
        }
    }

    fn graph_edit_sites(&self, sites: &mut Vec<crate::commands::graph::GraphEditSite>) {
        for command in &self.commands {
            command.graph_edit_sites(sites);
        }
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }

    fn was_applied(&self) -> bool {
        self.rejection.is_none()
            && (self.commands.iter().any(|command| command.was_applied())
                || self.scene_runs.iter().any(|run| run.before.is_some()))
    }

    fn touches(&self) -> ChangeSet {
        self.commands
            .iter()
            .fold(ChangeSet::NONE, |set, c| set.union(c.touches()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::change::Part;

    #[derive(Debug)]
    struct TargetCommand(GraphTarget);

    impl Command for TargetCommand {
        fn touches(&self) -> ChangeSet {
            ChangeSet::NONE
        }
        fn do_execute(&mut self, _project: &mut Project) {}
        fn do_undo(&mut self, _project: &mut Project) {}
        fn description(&self) -> &str {
            "Target"
        }
        fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
            targets.push(self.0.clone());
        }
    }

    #[derive(Debug)]
    struct ReasonCommand(&'static str);

    impl Command for ReasonCommand {
        fn touches(&self) -> ChangeSet {
            ChangeSet::NONE
        }
        fn do_execute(&mut self, _project: &mut Project) {}
        fn do_undo(&mut self, _project: &mut Project) {}
        fn description(&self) -> &str {
            "Reason"
        }
        fn rejection_reason(&self) -> Option<&str> {
            Some(self.0)
        }
    }

    /// Appends `to_append` to `Project::settings.video_library_paths` (a
    /// scratch `Vec<String>` field good enough to observe ordering) on
    /// execute, pops it back off on undo — cheap enough to prove
    /// `CompositeCommand`'s execute-forward/undo-reverse composition without
    /// any domain types.
    #[derive(Debug)]
    struct AppendCommand {
        to_append: String,
    }

    impl Command for AppendCommand {
        fn touches(&self) -> ChangeSet {
            ChangeSet::of(&[Part::Media])
        }
        fn do_execute(&mut self, project: &mut Project) {
            project
                .settings
                .video_library_paths
                .push(self.to_append.clone());
        }
        fn do_undo(&mut self, project: &mut Project) {
            project.settings.video_library_paths.pop();
        }
        fn description(&self) -> &str {
            "Append"
        }
    }

    fn composite(items: &[&str]) -> CompositeCommand {
        let commands = items
            .iter()
            .map(|&s| {
                Box::new(AppendCommand {
                    to_append: s.to_string(),
                }) as Box<dyn Command>
            })
            .collect();
        CompositeCommand::new(commands, "Append Many".to_string())
    }

    #[test]
    fn execute_applies_all_commands_in_order() {
        let mut project = Project::default();
        let mut cmd = composite(&["a", "b", "c"]);
        cmd.execute(&mut project);
        assert_eq!(project.settings.video_library_paths, vec!["a", "b", "c"]);
    }

    #[test]
    fn undo_reverses_all_commands_in_reverse_order() {
        // Each sub-command's undo only knows how to pop the LAST entry — if
        // CompositeCommand::undo ran forward instead of reverse, undoing 'a'
        // first (pop) would remove 'c' (the actual last entry), corrupting
        // the list instead of cleanly unwinding to empty.
        let mut project = Project::default();
        let mut cmd = composite(&["a", "b", "c"]);
        cmd.execute(&mut project);
        assert_eq!(project.settings.video_library_paths, vec!["a", "b", "c"]);

        cmd.undo(&mut project);
        assert!(
            project.settings.video_library_paths.is_empty(),
            "undo must reverse in the opposite order execute applied them"
        );
    }

    #[test]
    fn redo_reapplies_the_whole_group_as_one_unit() {
        let mut project = Project::default();
        let mut cmd = composite(&["x", "y"]);
        cmd.execute(&mut project);
        cmd.undo(&mut project);
        cmd.execute(&mut project);
        assert_eq!(
            project.settings.video_library_paths,
            vec!["x", "y"],
            "redo re-applies every sub-command"
        );
    }

    #[test]
    fn empty_command_list_is_a_no_op() {
        let mut project = Project::default();
        let mut cmd = CompositeCommand::new(Vec::new(), "Nothing".to_string());
        cmd.execute(&mut project);
        assert!(project.settings.video_library_paths.is_empty());
        cmd.undo(&mut project);
        assert!(project.settings.video_library_paths.is_empty());
    }

    #[test]
    fn composite_forwards_graph_admission_targets() {
        let first = GraphTarget::Generator(manifold_core::LayerId::new("first"));
        let second = GraphTarget::Generator(manifold_core::LayerId::new("second"));
        let command = CompositeCommand::new(
            vec![
                Box::new(TargetCommand(first.clone())),
                Box::new(TargetCommand(second.clone())),
            ],
            "Targets".into(),
        );
        let mut targets = Vec::new();
        command.graph_admission_targets(&mut targets);
        assert_eq!(targets, vec![first, second]);
        // Neither helper writes; the audit holds them to that.
        let mut project = Project::default();
        let mut command = command;
        command.execute(&mut project);
        command.undo(&mut project);
        let mut reason = ReasonCommand("none");
        reason.execute(&mut project);
        reason.undo(&mut project);
    }

    #[test]
    fn composite_rejection_rolls_back_prior_writes_and_skips_later_children() {
        let mut project = Project::default();
        let mut command = CompositeCommand::new(
            vec![
                Box::new(AppendCommand {
                    to_append: "before".into(),
                }),
                Box::new(ReasonCommand("invalid source")),
                Box::new(AppendCommand {
                    to_append: "after".into(),
                }),
            ],
            "Atomic edit".into(),
        );
        command.execute(&mut project);
        assert!(project.settings.video_library_paths.is_empty());
        assert!(!command.was_applied());
        command.undo(&mut project);
        assert!(project.settings.video_library_paths.is_empty());
    }

    #[test]
    fn composite_reports_first_child_rejection_reason() {
        let mut command = CompositeCommand::new(
            vec![
                Box::new(ReasonCommand("first")),
                Box::new(ReasonCommand("second")),
            ],
            "Reasons".into(),
        );
        command.execute(&mut Project::default());
        assert_eq!(command.rejection_reason(), Some("first"));
    }

    /// The row key covers every card param, so a paste or multi-delete of
    /// hundreds of clips must walk it once, not twice per child.
    #[test]
    fn a_composite_walks_the_row_facts_once() {
        let mut project = Project::default();
        let items: Vec<String> = (0..64).map(|i| i.to_string()).collect();
        let mut cmd = composite(&items.iter().map(String::as_str).collect::<Vec<_>>());
        let walks = || ROW_WALKS.get();
        let before = walks();
        cmd.execute(&mut project);
        assert_eq!(walks() - before, 1, "execute");
        let before = walks();
        cmd.undo(&mut project);
        assert_eq!(walks() - before, 1, "undo");
        let before = walks();
        rows_deferred(&mut project, |project| cmd.execute(project));
        assert_eq!(
            walks() - before,
            0,
            "a gesture move walks nothing, nested or not"
        );
        let before = walks();
        cmd.undo(&mut project);
        assert_eq!(
            walks() - before,
            1,
            "the walk reopens after a deferred write"
        );
    }
}
