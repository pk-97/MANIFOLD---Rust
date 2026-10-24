use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_editing::commands::graph::SetGraphNodeParamCommand;

/// BUG-282: an ordinary (unbound) node-face param/vec scrub session — the
/// mirror of [`super::BoundNodeParamDrag`] for the un-rerouted path. Opened at the
/// first `SetGraphNodeParam`/`SetOuterParam`-family move on a given
/// `(target, node_id, param_name, scope_path)`, live-written on every
/// subsequent move via `MutateProject` (Values or Shapes) (no undo entry), closed on the
/// matching `EndGraphNodeParamScrub` — ONE undo-worthy
/// `SetGraphNodeParamCommand` for the whole drag (`pre_drag_value` →
/// `current_value`), not one `Execute` per pointer-move tick.
#[derive(Debug, Clone)]
pub(crate) struct UnboundNodeParamDrag {
    pub(super) target: manifold_core::GraphTarget,
    pub(super) node_id: u32,
    pub(super) param_name: String,
    pub(super) scope_path: Vec<u32>,
    pub(super) catalog_default: EffectGraphDef,
    /// Value before the drag started. `None` means the key was absent —
    /// the same `Option<SerializedParamValue>` shape `with_previous` takes.
    pub(super) pre_drag_value: Option<SerializedParamValue>,
    /// Value as of the last move — the undo redo target.
    pub(super) current_value: SerializedParamValue,
}

impl UnboundNodeParamDrag {
    /// An unbound node param scrub on `target`, starting from
    /// `pre_drag_value`.
    pub(crate) fn new(
        target: manifold_core::GraphTarget,
        scope_path: Vec<u32>,
        node_id: u32,
        param_name: String,
        catalog_default: EffectGraphDef,
        pre_drag_value: Option<SerializedParamValue>,
        current_value: SerializedParamValue,
    ) -> Self {
        Self {
            target,
            node_id,
            param_name,
            scope_path,
            catalog_default,
            pre_drag_value,
            current_value,
        }
    }

    /// The live, non-undoable write of the current value.
    pub(crate) fn live_command(&self) -> SetGraphNodeParamCommand {
        SetGraphNodeParamCommand::new(
            self.target.clone(),
            self.node_id,
            self.param_name.clone(),
            self.current_value.clone(),
            self.catalog_default.clone(),
        )
        .with_scope(self.scope_path.clone())
    }

    pub(crate) fn set_current(&mut self, value: SerializedParamValue) {
        self.current_value = value;
    }

    /// The drag changed the value. An absent key is always a change.
    pub(crate) fn moved(&self) -> bool {
        self.pre_drag_value.as_ref() != Some(&self.current_value)
    }

    /// The one undoable write for the whole drag, seeded with the true
    /// pre-drag value so undo restores it.
    pub(crate) fn into_command(self) -> SetGraphNodeParamCommand {
        SetGraphNodeParamCommand::new(
            self.target,
            self.node_id,
            self.param_name,
            self.current_value,
            self.catalog_default,
        )
        .with_scope(self.scope_path)
        .with_previous(self.pre_drag_value)
    }
}

/// BUG-282 regression. An UNBOUND node-face param scrub used to push a
/// fresh undo-worthy `SetGraphNodeParamCommand::execute` on EVERY
/// pointer-move tick — an N-tick drag flooded the 200-cap undo stack
/// instead of coalescing to one entry. The fix mirrors the bound-row
/// pattern: N in-flight ticks land via `MutateProject` (Values or Shapes) — a direct
/// `Command::execute` call on the content-thread `Project` that never
/// touches an undo manager — and only the ONE release-time command, seeded
/// with `with_previous(pre_drag_value)` (the seam
/// `SetGraphNodeParamCommand::with_previous`'s doc comment describes for
/// exactly this drag-cadence-commit case), goes through
/// `UndoRedoManager::execute`. This test drives that same sequence against
/// a real `UndoRedoManager`: N direct `execute()` calls (the tick writes)
/// followed by one `UndoRedoManager::execute` (the release commit), then
/// asserts `undo_count() == 1` (not `N + 1`) and that `undo()` restores the
/// true pre-drag value rather than whatever `execute()`'s self-capture
/// would have seen post-drag.
#[cfg(test)]
mod unbound_node_param_drag_tests {
    use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
    use manifold_core::effects::PresetInstance;
    use manifold_core::project::Project;
    use manifold_core::{GraphTarget, NodeId, PresetTypeId};
    use manifold_editing::command::Command;
    use manifold_editing::commands::graph::SetGraphNodeParamCommand;
    use manifold_editing::undo::UndoRedoManager;
    use std::collections::BTreeMap;

    fn empty_def() -> EffectGraphDef {
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![],
            wires: vec![],
        }
    }

    /// One master effect carrying a per-instance graph override with a
    /// single node (id 1) holding one param ("amount") at `initial` — the
    /// unbound node-face scrub's write target.
    fn project_with_node_param(initial: f32) -> (Project, GraphTarget) {
        let mut project = Project::default();
        let mut fx = PresetInstance::new(PresetTypeId::new("Test"));
        let effect_id = fx.id.clone();
        let mut params = BTreeMap::new();
        params.insert(
            "amount".to_string(),
            SerializedParamValue::Float { value: initial },
        );
        let mut def = empty_def();
        def.nodes.push(EffectGraphNode {
            id: 1,
            node_id: NodeId::new("inner"),
            type_id: "node.test".to_string(),
            handle: None,
            params,
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        });
        fx.graph = Some(def);
        project.settings.master_effects.push(fx);
        (project, GraphTarget::Effect(effect_id))
    }

    fn read_amount(project: &Project) -> f32 {
        let def = project.settings.master_effects[0].graph.as_ref().unwrap();
        match def.nodes[0].params.get("amount") {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("expected a Float amount, got {other:?}"),
        }
    }

    #[test]
    fn n_tick_scrub_is_one_undo_entry_and_undo_restores_pre_drag_value() {
        let (mut project, target) = project_with_node_param(0.0);
        let pre_drag_value = Some(SerializedParamValue::Float { value: 0.0 });

        // N pointer-move ticks: each is a live write — the app's
        // `MutateProject` (Values or Shapes) path (`live_cmd.execute(p)` in the
        // `SetGraphNodeParam` arm) calling `Command::execute` directly on
        // the content-thread project, never touching an undo manager.
        let ticks = [0.1_f32, 0.3, 0.55, 0.72, 0.9];
        for &v in &ticks {
            let mut live = SetGraphNodeParamCommand::new(
                target.clone(),
                1,
                "amount".to_string(),
                SerializedParamValue::Float { value: v },
                empty_def(),
            );
            live.execute(&mut project);
        }
        let last_tick_value = *ticks.last().unwrap();
        assert_eq!(
            read_amount(&project),
            last_tick_value,
            "live writes DO land on the project every tick"
        );

        // Release: ONE command, seeded with the true pre-drag baseline via
        // `with_previous`, pushed through the real undo manager — mirrors
        // `EndGraphNodeParamScrub`'s unbound-drag close-out.
        let mut undo_mgr = UndoRedoManager::new();
        assert_eq!(undo_mgr.undo_count(), 0, "sanity: fresh manager");
        let commit = SetGraphNodeParamCommand::new(
            target,
            1,
            "amount".to_string(),
            SerializedParamValue::Float {
                value: last_tick_value,
            },
            empty_def(),
        )
        .with_previous(pre_drag_value);
        undo_mgr.execute(Box::new(commit), &mut project);

        assert_eq!(
            undo_mgr.undo_count(),
            1,
            "an N-tick drag must be EXACTLY one undo-worthy commit, not one per tick"
        );
        assert_eq!(read_amount(&project), last_tick_value);

        let _ = undo_mgr.undo(&mut project);
        assert_eq!(
            read_amount(&project),
            0.0,
            "undo must restore the true pre-drag value, not whatever execute()'s \
             self-capture would have seen post-drag"
        );
    }
}
