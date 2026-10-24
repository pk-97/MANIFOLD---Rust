//! Addressed parameter navigation for the Scene tab.
//!
//! Navigation uses the same projected row identities as rendering and
//! dispatch. It only changes UI-local selection, fold, and drawer state.

use super::*;
use crate::panels::param_slider_shared::{active_mod_tabs, row_drawer_height};
use crate::view::UiGraphTarget;

impl ScenePanel {
    fn target_matches_scene(&self, target: &UiGraphTarget) -> bool {
        matches!(target, UiGraphTarget::Generator(layer) if self.live_layer_id() == Some(layer))
    }

    /// The item that owns param `id`: its id is `<item id>.<property>`, so the
    /// owner is read off the id, and a light shows as Lighting.
    fn owner_selection(vm: &SceneSetupVm, id: &str) -> Option<SceneSelection> {
        let (item, _) = split_param_id(id)?;
        let item = vm
            .items
            .iter()
            .find(|candidate| candidate.id.as_str() == item)?;
        Some(SceneSelection::Item(item.id.clone()).shown(vm))
    }

    fn has_param(&self, param: &manifold_foundation::ParamId) -> bool {
        self.full_params.is_some() && self.full_param_id_index.contains_key(param.as_ref())
    }

    /// The layer whose scene this tab shows, when it holds `param`.
    pub fn scene_layer_with(&self, param: &manifold_foundation::ParamId) -> Option<&LayerId> {
        self.live_layer_id().filter(|_| self.has_param(param))
    }

    /// Show `param` in the Scene tab: select the item that owns it, open an
    /// active response tab and scroll it into view on the next build.
    /// Ownership comes from the param's id (SCENE_SURFACES_DESIGN.md D10).
    /// Changes no authored state; `false` leaves the selection as it was.
    pub fn reveal_param(
        &mut self,
        target: &UiGraphTarget,
        param: &manifold_foundation::ParamId,
    ) -> bool {
        if !self.target_matches_scene(target) || !self.has_param(param) {
            return false;
        }
        let Some(layer_id) = self.live_layer_id().cloned() else {
            return false;
        };
        let Some(selection) = self
            .state
            .as_live()
            .and_then(|vm| Self::owner_selection(vm, param.as_ref()))
        else {
            return false;
        };
        self.set_selection(layer_id, selection);
        self.revealed_property = Some((target.clone(), param.clone()));
        self.pending_parameter_reveal = Some((target.clone(), param.clone()));
        true
    }

    /// A card section title's reveal (SCENE_SURFACES_DESIGN.md D10): the
    /// section's first row owned by Lighting or an object, else its first
    /// row. So the card's Lighting title opens Lighting.
    pub fn reveal_section(
        &mut self,
        target: &UiGraphTarget,
        params: &[manifold_foundation::ParamId],
    ) -> bool {
        let item_owned = |panel: &Self, param: &manifold_foundation::ParamId| {
            panel.state.as_live().is_some_and(|vm| {
                matches!(
                    Self::owner_selection(vm, param.as_ref()),
                    Some(SceneSelection::Lighting)
                ) || Self::owner_selection(vm, param.as_ref())
                    .and_then(|selection| selection.item().and_then(|id| vm.item(id)))
                    .is_some_and(|item| item.kind == ItemKind::Object)
            })
        };
        let pick = params
            .iter()
            .find(|param| item_owned(self, param))
            .or(params.first())
            .cloned();
        pick.is_some_and(|param| self.reveal_param(target, &param))
    }

    pub(super) fn property_explicitly_revealed(&self, row: &ParamRow) -> bool {
        self.revealed_property
            .as_ref()
            .is_some_and(|(target, param)| self.target_matches_scene(target) && row.id == *param)
    }

    /// Prepare an ordinary scene row for the pending reveal after the filtered
    /// card has been configured, before its section headers and rows are drawn.
    pub(super) fn prepare_pending_parameter_properties(&mut self) {
        let Some((target, param)) = self.pending_parameter_reveal.as_ref() else {
            return;
        };
        if !self.target_matches_scene(target) {
            return;
        }
        let Some(index) = self
            .properties_card
            .rows
            .iter()
            .position(|row| row.id == *param)
        else {
            return;
        };
        let section = self.material_section_name(&self.properties_card.rows[index]);
        if let Some(section) = section {
            self.section_folded.insert(section, false);
        }
        self.properties_card
            .row_host
            .open_colour_of(&self.properties_card.rows, param.as_ref());
        let active = active_mod_tabs(
            &self.properties_card.mod_state,
            &self.properties_card.rows[index],
            index,
        );
        if let Some(tab) = [ModTab::Envelope, ModTab::Audio]
            .into_iter()
            .find(|tab| active.contains(tab))
        {
            self.properties_card.mod_active_tab[index] = tab;
        }
    }

    /// Reveal the pending address when its row is visible in the freshly
    /// built tree. The address stays queued when its row is not built yet.
    pub(super) fn reveal_pending_parameter_response(&mut self, tree: &mut UITree) {
        let Some((target, param)) = self.pending_parameter_reveal.as_ref() else {
            return;
        };
        if !self.target_matches_scene(target) {
            return;
        }
        let Some(&index) = self.properties_card.row_id_index.get(param.as_ref()) else {
            return;
        };
        let Some(row) = self.properties_card.rows.get(index) else {
            return;
        };
        let Some(mut rect) = self.properties_card.row_host.param_row_rect(tree, index) else {
            return;
        };
        rect.height += row_drawer_height(
            false,
            &self.properties_card.mod_state,
            &self.properties_card.mod_active_tab,
            row,
            index,
        );
        self.scroll.reveal_rect(tree, rect);
        self.pending_parameter_reveal = None;
    }

    /// Drop navigation when its current projection no longer owns the address.
    pub(super) fn clear_stale_parameter_reveal(&mut self) {
        if self
            .revealed_property
            .as_ref()
            .is_some_and(|(target, param)| {
                !self.target_matches_scene(target) || !self.has_param(param)
            })
        {
            self.revealed_property = None;
        }
        let Some((target, param)) = self.pending_parameter_reveal.as_ref() else {
            return;
        };
        let valid = self.target_matches_scene(target)
            && self.has_param(param)
            && self
                .state
                .as_live()
                .is_some_and(|vm| Self::owner_selection(vm, param.as_ref()).is_some());
        if !valid {
            self.pending_parameter_reveal = None;
        }
    }
}

/// `<item id>.<property>` split at the first dot.
fn split_param_id(id: &str) -> Option<(&str, &str)> {
    id.split_once('.')
}
