use super::*;
use manifold_editing::commands::effect_groups::PasteEffectsCommand;
use manifold_editing::commands::effect_target::with_effects;
use manifold_ui::panels::actions::CardEditAction;

impl crate::app::Application {
    /// Context menus use exactly the same commands as keyboard editing.
    pub(crate) fn edit_inspector_cards(&mut self, action: CardEditAction) {
        let Some(content_tx) = self.content_tx.as_ref() else { return; };
        let mut host = AppInputHost {
            project: &mut self.local_project,
            content_tx,
            content_state: &self.content_state,
            ui_root: &mut self.ws.ui_root,
            selection: &mut self.selection,
            active_layer: &mut self.active_layer_id,
            needs_rebuild: &mut self.needs_rebuild,
            needs_structural_sync: &mut self.needs_structural_sync,
            scroll_dirty: &mut self.scroll_dirty,
            current_project_path: &self.current_project_path,
            has_output_window: self.window_registry.has_output_window(),
            pending_close_output: &mut self.pending_close_output,
            pending_export: &mut self.pending_export,
            project_io: &mut self.project_io,
            #[cfg(target_os = "macos")]
            internal_clipboard_change_count: &mut self.internal_clipboard_change_count,
        };
        match action {
            CardEditAction::Copy => host.handle_effect_copy(),
            CardEditAction::Cut => host.handle_effect_cut(),
            CardEditAction::Paste => host.handle_effect_paste(),
            CardEditAction::Duplicate => host.handle_effect_duplicate(),
            CardEditAction::Delete => host.handle_effect_delete(),
            CardEditAction::Group => host.handle_effect_group(),
            CardEditAction::Ungroup => host.handle_effect_ungroup(),
        };
    }
}

impl AppInputHost<'_> {
    pub(super) fn card_effect_target(&self) -> EffectTarget {
        let layer = self.ui_root.inspector.inspected_layer_id().cloned()
            .or_else(|| self.active_layer.clone());
        resolve_effect_target(self.ui_root.inspector.last_effect_tab(), &layer, self.selection)
    }

    pub(super) fn copy_effect_selection(&mut self) -> bool {
        let target = self.card_effect_target();
        let selected = self.ui_root.inspector.selected_effect_ids();
        with_effects(self.project, &target, |effects, groups| {
            let ids: Vec<_> = effects.iter().filter(|effect| selected.contains(&effect.id)).map(|effect| effect.id.clone()).collect();
            if ids.is_empty() { return false; }
            self.ui_root.effect_clipboard.copy_selection(effects, groups, &ids);
            self.ui_root.scene_modifier_clipboard = None;
            self.ui_root.object_modifier_clipboard = None;
            true
        }).unwrap_or(false)
    }

    pub(super) fn paste_effect_selection(&mut self) -> bool {
        if !self.ui_root.effect_clipboard.has_content() { return false; }
        let target = self.card_effect_target();
        let selected = self.ui_root.inspector.get_selected_effect_indices();
        let (pasted, groups) = self.ui_root.effect_clipboard.paste_payload();
        let ids: Vec<_> = pasted.iter().map(|effect| effect.id.clone()).collect();
        let Some((before, destination_group)) = with_effects(self.project, &target, |effects, _| {
            let mut insert_at = selected.last().map_or(effects.len(), |index| (index + 1).min(effects.len()));
            let selected_group = selected.last().and_then(|&index| effects.get(index)).and_then(|effect| effect.group_id.clone());
            // Complete copied groups are siblings of the current group. Plain
            // effects pasted after a member join that member's group.
            if !groups.is_empty() && let Some(group) = &selected_group {
                while insert_at < effects.len() && effects[insert_at].group_id.as_ref() == Some(group) { insert_at += 1; }
            }
            (effects.get(insert_at).map(|effect| effect.id.clone()), if groups.is_empty() { selected_group } else { None })
        }) else { return true; };
        ContentCommand::send(self.content_tx, ContentCommand::ExecuteSelecting(
            Box::new(PasteEffectsCommand::new(target.clone(), pasted, groups, before, destination_group)),
            crate::edit_selection::SelectAfterEdit::Effects { target, ids },
        ));
        *self.needs_structural_sync = true;
        *self.needs_rebuild = true;
        true
    }
}
