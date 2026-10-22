//! The Scene tab of the inspector (SCENE_DOCUMENT_DESIGN.md section 8 (Cards
//! by kind), SCENE_SURFACES_DESIGN.md D8 (Scene tab tree)).
//!
//! The tree of scene items sits in its own pane on top (`outliner.rs`), and
//! the selected item's header (`item_header.rs`) and controls, grouped by
//! control group, scroll in a second pane below a draggable split
//! (`panes.rs`). The panel builds `UITree` nodes from data handed in via
//! [`ScenePanel::configure`] and maps clicks and drags to
//! [`super::PanelAction`]; it never writes the project itself. Param rows
//! and structural field rows are the shared card rows, dispatched through the
//! same `RowHost` every card uses.

#[path = "scene_setup_panel/outliner.rs"]
mod outliner;
#[path = "scene_setup_panel/item_header.rs"]
mod item_header;
#[path = "scene_setup_panel/keyboard.rs"]
mod keyboard;
pub use keyboard::SceneItemAddress;
#[path = "scene_setup_panel/material_block.rs"]
mod material_block;
#[path = "scene_setup_panel/parameter_navigation.rs"]
mod parameter_navigation;
#[path = "scene_setup_panel/properties.rs"]
mod properties;
#[path = "scene_setup_panel/scale_link.rs"]
mod scale_link;
#[path = "scene_setup_panel/value_sync.rs"]
mod value_sync;
mod panes;
mod view_model;
pub use view_model::*;
#[cfg(test)]
#[path = "scene_setup_panel/tests.rs"]
mod tests;

use crate::chrome::{ChromeHost, View};
use crate::color;
use crate::input::UIEvent;
use crate::node::*;
use crate::scroll_container::{SCROLLBAR_W, ScrollContainer, ScrollbarStyle};
use crate::tree::UITree;
use crate::{ProjectAction, RootAction};
use manifold_foundation::{ItemId, LayerId};

use super::copy_to_clipboard_label::CopyToClipboardLabelState;
use super::param_card::{RowGeometry, RowMod};
use super::param_slider_shared::{
    AudioRowState, ModTab, ParamModState, RowHost, RowInteraction, build_button_row, build_colour_row,
    build_param_row, param_row_key_base, toggle_btn_style,
};
use super::{GraphParamTarget, PanelAction, ParamsAction};
use crate::param_surface::{ParamRow, ParamSurface, RowKind, RowMapping, RowRole, RowSpec};
use crate::slider::GAP;
use manifold_foundation::scene_surface::{ControlGroup, Feature, Gate, GizmoDrawMode, ItemKind, SurfaceRole};

// ── Stable keys ──
const KEY_BG: u64 = 80_001;
const KEY_NEW_SCENE: u64 = 80_012;
const KEY_SHOW_GIZMOS: u64 = 80_026;
const KEY_GIZMO_MODE: u64 = 80_028;
const KEY_RENDER_MODE: u64 = 80_029;
const KEY_ADD: u64 = 80_030;

const PANEL_W_MIN: f32 = 320.0;
pub(crate) const ROW_H: f32 = 24.0;
pub(crate) const ROW_GAP: f32 = 4.0;
const PAD: f32 = 10.0;
const STEP_W: f32 = 22.0;

/// The panel's card-shaped row state: the rows, their modulation
/// bookkeeping (the same vectors `ParamCardPanel` keeps) and the shared
/// [`RowHost`] that owns widget-id bundles and click routing.
struct SceneCardState {
    rows: Vec<ParamRow>,
    mod_state: ParamModState,
    /// Per-row CURRENT value cache for the properties card's rows: seeded
    /// from `ParamRow.value.base` at `configure_from_filtered`, kept fresh
    /// every frame by `ScenePanel::sync_properties_values`. Multi-writer: the
    /// drag and type-in paths also write it mid-gesture, so it is NOT a safe
    /// dirty-check key; `last_pushed_values` is that.
    current_values: Vec<f32>,
    /// Dirty-check key for `sync_properties_values`: the last value the sync
    /// pushed to the tree. Single-writer. The tree is minted fresh every
    /// frame and `build_properties_row` DRAWS this value at build time, so a
    /// skipped push must find it already drawn. New slots seed NaN;
    /// `configure_from_filtered` carries values over by row id.
    last_pushed_values: Vec<f32>,
    /// BUG-313: param id → local row index, the join key for the per-frame
    /// value sync. A manifest param the current selection isn't showing
    /// misses the map and is skipped.
    row_id_index: ahash::AHashMap<String, usize>,
    /// Per-frame coverage scratch for the id-join miss invariant (INV-6).
    row_value_synced: Vec<bool>,
    /// Always `None`: no OSC address surface on scene rows.
    osc_addresses: Vec<Option<String>>,
    mod_active_tab: Vec<ModTab>,
    row_host: RowHost,
}

impl SceneCardState {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            mod_state: ParamModState::allocate(0),
            current_values: Vec::new(),
            last_pushed_values: Vec::new(),
            row_id_index: ahash::AHashMap::new(),
            row_value_synced: Vec::new(),
            osc_addresses: Vec::new(),
            mod_active_tab: Vec::new(),
            row_host: RowHost::new(),
        }
    }

    /// Resize every per-row vector to `n`. `mod_state` is rebuilt (the build
    /// pass re-syncs its model facts every frame; its session-only flags
    /// survive inside `ParamModState::rebuild`).
    fn resize(&mut self, n: usize) {
        self.rows.resize(n, placeholder_param_info());
        self.mod_state.rebuild(n);
        self.current_values.resize(n, 0.0);
        self.last_pushed_values.resize_with(n, || f32::NAN);
        self.row_value_synced.resize(n, false);
        self.osc_addresses.resize(n, None);
        while self.mod_active_tab.len() < n {
            self.mod_active_tab.push(ModTab::Driver);
        }
        self.row_host.resize(n);
        self.row_host.row_index.clear();
        self.row_id_index.retain(|_, row| *row < n);
    }

    /// Forget the node ids from the last build. Value caches and the id join
    /// stay, so the next build of the same rows carries its values over.
    fn clear_nodes(&mut self) {
        self.row_host.clear_nodes();
    }

    /// Populate this card from a FILTERED slice of the layer's generator
    /// [`ParamSurface`] — `retained` is a retained-index list into
    /// `config.rows`, applied uniformly so index-alignment survives the
    /// filter. `rows[i].id` IS the real exposed param id (or a field row's
    /// synthetic id), so writes dispatch through the same path every other
    /// card uses.
    fn configure_from_filtered(&mut self, config: &ParamSurface, retained: &[usize]) {
        let n = retained.len();
        let prev_index = std::mem::take(&mut self.row_id_index);
        let prev_pushed = std::mem::take(&mut self.last_pushed_values);
        self.resize(n);
        self.rows = retained.iter().map(|&i| config.rows[i].clone()).collect();
        self.row_host.index_swatches(&self.rows);
        self.current_values = retained.iter().map(|&i| config.rows[i].value.base).collect();
        self.last_pushed_values = self
            .rows
            .iter()
            .map(|row| prev_index.get(row.id.as_ref()).map(|&j| prev_pushed[j]).unwrap_or(f32::NAN))
            .collect();

        self.row_id_index.clear();
        self.row_id_index.reserve(n);
        for (i, row) in self.rows.iter().enumerate() {
            let prev = self.row_id_index.insert(row.id.to_string(), i);
            debug_assert!(prev.is_none(), "BUG-313: duplicate param id {:?} in scene properties rows", row.id);
        }

        let mods: Vec<RowMod> = retained.iter().map(|&i| config.rows[i].modulation.clone()).collect();
        self.mod_state.sync_from_config(n, &mods);
        self.mod_state.sync_audio(self.rows.iter().map(|row| row.audio.clone()), &config.audio_sends);
    }

    fn restore_live(&mut self, target: &GraphParamTarget) {
        let ctx = RowInteraction {
            target,
            rows: &mut self.rows,
            modulation: &mut self.mod_state,
            values: &mut self.current_values,
            row_indices: &self.row_id_index,
        };
        self.row_host.restore_live(ctx);
    }

    fn handle_pointer_down(&mut self, node: NodeId, pos: Vec2, tree: &mut UITree, target: &GraphParamTarget) -> Vec<PanelAction> {
        let ctx = RowInteraction {
            target,
            rows: &mut self.rows,
            modulation: &mut self.mod_state,
            values: &mut self.current_values,
            row_indices: &self.row_id_index,
        };
        self.row_host.handle_pointer_down(node, pos, tree, ctx)
    }

    fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree, fine: bool, target: &GraphParamTarget) -> Vec<PanelAction> {
        let ctx = RowInteraction {
            target,
            rows: &mut self.rows,
            modulation: &mut self.mod_state,
            values: &mut self.current_values,
            row_indices: &self.row_id_index,
        };
        self.row_host.handle_drag(pos, tree, fine, ctx)
    }
}

/// Placeholder `ParamRow` used only to size `SceneCardState::resize`'s grow
/// step before the real rows are written by the build pass.
fn placeholder_param_info() -> ParamRow {
    ParamRow {
        id: manifold_foundation::ParamId::from(""),
        spec: RowSpec {
            tooltip: None,
            name: String::new(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            whole_numbers: false,
            is_angle: false,
            is_toggle: false,
            is_trigger: false,
            is_trigger_gate: false,
            value_labels: None,
            unit: None,
            section: None,
            disabled: None,
            material_role: None,
            inactive_reason: None,
            setup_warning: None,
            scene: None,
            limit: Default::default(),
        },
        value: crate::param_surface::RowValue { base: 0.0, effective: 0.0, exposed: false, driven: false },
        audio: AudioRowState::default(),
        clip_trigger: None,
        modulation: RowMod::default(),
        mapping: RowMapping { osc_address: None, ableton_display: None, ableton_range: None, mappable: false },
        rgb_members: None,
        modulator_stored: false,
        affordance: None,
    }
}

/// What a selection note compares: the layer, its active item and its
/// selected set (`None` when it follows the active item).
type NotedSelection = (LayerId, Option<SceneSelection>, Option<Vec<SceneSelection>>);

pub struct ScenePanel {
    open: bool,
    state: SceneSetupState,
    panel_w: f32,
    host: ChromeHost,
    /// The settings pane's scroll column; the tree pane's lives in `panes`.
    scroll: ScrollContainer,
    panes: panes::ScenePanes,
    reveal_properties: bool,
    pub(crate) content_parent: NodeId,
    bg_id: NodeId,
    new_scene_id: Option<NodeId>,
    show_gizmos_id: Option<NodeId>,
    gizmo_mode_id: Option<NodeId>,
    render_mode_id: Option<NodeId>,
    /// The tree's "+ Add" button.
    add_id: Option<NodeId>,
    /// How gizmos meet scene depth, for the preview, the viewport and an
    /// export. UI-only workspace state, like `show_gizmos`.
    gizmo_draw_mode: GizmoDrawMode,
    /// The global "Show gizmos" switch. UI-only workspace state, never saved.
    show_gizmos: bool,
    /// The ONE properties card: the selected item's rows, filtered straight
    /// off `full_params` by item, rendered and dispatched through the same
    /// `build_param_row`/`RowIndex`/`row_action` core every card row uses.
    properties_card: SceneCardState,
    /// A clip response reveal queued for the next structural build.
    pending_parameter_reveal: Option<(crate::view::UiGraphTarget, manifold_foundation::ParamId)>,
    /// Keep an explicitly opened property visible until selection changes.
    revealed_property: Option<(crate::view::UiGraphTarget, manifold_foundation::ParamId)>,
    /// The scene panel's bound layer's FULL generator `ParamSurface` (every
    /// exposed param), built by `state_sync` the same way the main
    /// inspector's generator card is, for THIS panel's layer (never
    /// `active_layer`). The properties body filters it to the selected
    /// item's rows at build time.
    full_params: Option<ParamSurface>,
    full_param_id_index: ahash::AHashMap<String, usize>,
    header_buttons: item_header::HeaderButtons,
    /// The outliner selection, per layer: UI-local workspace state, never
    /// serialized. A missing entry resolves to the first item.
    selection: std::collections::HashMap<LayerId, SceneSelection>,
    /// The selected items per layer, as Blender keeps a selection beside its
    /// active object: `selection` is the active item whose card shows, this
    /// the set the outliner highlights and the gizmos handle. A layer with no
    /// entry has its active item alone selected; an empty entry has nothing
    /// selected, its card still showing.
    selected: std::collections::HashMap<LayerId, Vec<SceneSelection>>,
    /// UI_INVALIDATION D14 for the Scene panel: moves when a noted selection
    /// differs from the last noted one, or the selection write cleared a
    /// reveal. Noted at the gesture's commit, never on press.
    selection_version: u64,
    noted_selection: Option<NotedSelection>,
    /// Moves on a body scroll or a view-only change (fold, gizmo mode).
    view_version: u64,
    focus_cleared: bool,
    /// Every outliner row's click target this frame.
    outliner_row_ids: Vec<(NodeId, SceneSelection)>,
    /// Every row's eye toggle this frame: `(node, visible param id, value)`.
    outliner_eye_ids: Vec<(NodeId, String, f32)>,
    /// Every group's chevron this frame.
    group_toggle_ids: Vec<(NodeId, ItemId)>,
    /// Groups the user folded, per layer. Groups start open.
    collapsed_groups: std::collections::HashMap<LayerId, ahash::AHashSet<ItemId>>,
    /// Items whose Scale link is off. The link starts on (section 7.17);
    /// card view state, never saved or mapped.
    scale_unlinked: ahash::AHashSet<ItemId>,
    /// Fold state for properties sections, keyed by section NAME globally
    /// within the panel. UI-local, never serialized. Missing = expanded.
    section_folded: ahash::AHashMap<String, bool>,
    material_block: material_block::MaterialButtons,
    /// Each modifier section's header buttons built this frame: (button,
    /// object, modifier key, where it moves to; `None` removes it).
    modifier_buttons: Vec<(NodeId, ItemId, String, Option<usize>)>,
    panel_rect: Rect,
}

impl Default for ScenePanel {
    fn default() -> Self {
        Self {
            open: false,
            state: SceneSetupState::default(),
            panel_w: PANEL_W_MIN,
            host: ChromeHost::new(),
            scroll: ScrollContainer::new(),
            panes: Default::default(),
            reveal_properties: false,
            content_parent: NodeId::PLACEHOLDER,
            bg_id: NodeId::PLACEHOLDER,
            new_scene_id: None,
            show_gizmos_id: None,
            gizmo_mode_id: None,
            render_mode_id: None,
            add_id: None,
            gizmo_draw_mode: GizmoDrawMode::default(),
            show_gizmos: false,
            properties_card: SceneCardState::new(),
            pending_parameter_reveal: None,
            revealed_property: None,
            full_params: None,
            full_param_id_index: ahash::AHashMap::new(),
            header_buttons: Default::default(),
            selection: std::collections::HashMap::new(),
            selected: std::collections::HashMap::new(),
            selection_version: 0,
            noted_selection: None,
            view_version: 0,
            focus_cleared: false,
            outliner_row_ids: Vec::new(),
            outliner_eye_ids: Vec::new(),
            group_toggle_ids: Vec::new(),
            collapsed_groups: std::collections::HashMap::new(),
            scale_unlinked: ahash::AHashSet::new(),
            section_folded: ahash::AHashMap::new(),
            material_block: Default::default(),
            modifier_buttons: Vec::new(),
            panel_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
        }
    }
}

impl ScenePanel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Node-intent dispatch for this panel's right-click gestures: the
    /// properties card's slider resets (main rows + armed drawers).
    pub fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        let (true, SceneSetupState::Live(vm)) = (self.open, &self.state) else {
            return;
        };
        self.properties_card.row_host.register_intents(intents);
        // The one param menu, addressed at this panel's own layer (BUG-292).
        let target = GraphParamTarget::GeneratorOf(vm.layer_id.clone());
        self.properties_card.row_host.register_param_menus(&self.properties_card.rows, &target, intents);
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Update the data the panel renders. Called from `state_sync` on a
    /// structural sync while the panel is open — rebuilt fresh from the
    /// snapshot every time (D1: "no rotting, no staleness").
    pub fn configure(&mut self, state: SceneSetupState) {
        self.state = state;
        self.clear_stale_parameter_reveal();
    }

    /// Hand the panel the layer's FULL generator `ParamSurface` (state_sync
    /// builds it via the SAME `gen_params_to_surface` the main inspector's
    /// generator card uses, for THIS panel's bound layer — `live_layer_id()`,
    /// never `active_layer`: the panel edits the scene of its OWN docked
    /// layer, which can legitimately differ from the app's active layer
    /// (BUG-292)). The properties body filters it to the selected item's
    /// controls at build time.
    pub fn configure_params(&mut self, config: Option<ParamSurface>) {
        self.full_param_id_index.clear();
        if let Some(surface) = config.as_ref() {
            self.full_param_id_index.reserve(surface.rows.len());
            for (index, row) in surface.rows.iter().enumerate() {
                self.full_param_id_index.insert(row.id.to_string(), index);
            }
        }
        self.full_params = config;
        self.clear_stale_parameter_reveal();
    }

    /// The layer this panel is live on, if it's showing the full panel. The
    /// app's per-frame value sync uses this to resolve the layer's generator
    /// graph without re-deriving the selection.
    pub fn live_layer_id(&self) -> Option<&LayerId> {
        match &self.state {
            SceneSetupState::Live(vm) => Some(&vm.layer_id),
            _ => None,
        }
    }

    /// The live scene's view model.
    pub fn live_vm(&self) -> Option<&SceneSetupVm> {
        self.state.as_live()
    }

    /// How the panel's gizmos meet scene depth; export reads it too.
    pub fn gizmo_draw_mode(&self) -> GizmoDrawMode {
        self.gizmo_draw_mode
    }

    /// The scene's Render Mode row, drawn in the header beside Show gizmos
    /// instead of under World.
    pub(super) fn render_mode_row(&self) -> Option<&ParamRow> {
        self.full_params
            .as_ref()?
            .rows
            .iter()
            .find(|row| row.spec.scene.is_some_and(|scene| scene.group == ControlGroup::RenderMode))
    }

    /// The header button's text for the Render Mode row's current value.
    pub(super) fn render_mode_label(&self) -> Option<String> {
        let row = self.render_mode_row()?;
        let labels = row.spec.value_labels.as_deref().unwrap_or(&[]);
        let index = (row.value.base - row.spec.min).round().max(0.0) as usize;
        Some(format!("Render: {}", labels.get(index).map_or("Rendered", String::as_str)))
    }

    /// The scene layer whose gizmos draw over the output preview: the one
    /// this panel shows, while "Show gizmos" is on.
    pub fn gizmo_layer(&self) -> Option<&LayerId> {
        self.live_layer_id().filter(|_| self.show_gizmos)
    }

    /// Turn "Show gizmos" off: perform mode forces it off (D6), so a show
    /// never starts with editor chrome armed over the preview.
    pub fn hide_gizmos(&mut self) {
        if self.show_gizmos {
            self.show_gizmos = false;
            self.view_version += 1;
        }
    }

    /// The active item of the scene this panel shows: the one whose card
    /// shows.
    pub fn live_selection(&self) -> Option<&SceneSelection> {
        self.selection.get(self.live_layer_id()?)
    }

    /// Every selected item of the scene this panel shows, in selection
    /// order, the active one last when it is selected.
    pub fn live_selected(&self) -> &[SceneSelection] {
        self.live_layer_id().map_or(&[], |layer| self.selected_of(layer))
    }

    fn selected_of(&self, layer: &LayerId) -> &[SceneSelection] {
        match self.selected.get(layer) {
            Some(set) => set,
            None => self.selection.get(layer).map_or(&[], std::slice::from_ref),
        }
    }

    /// Build into the inspector's Scene tab content rect
    /// (`InspectorCompositePanel::scene_content_rect`). No-op when closed.
    pub fn build_docked(&mut self, tree: &mut UITree, rect: Rect) {
        if !self.open {
            return;
        }
        self.panel_w = rect.width.max(PANEL_W_MIN);
        self.build_nodes(tree, rect.x, rect.y, rect.height);
    }

    fn chrome_view(&self) -> View {
        View::panel()
            .fill()
            .style(UIStyle {
                bg_color: Color32::new(19, 19, 22, 250),
                border_color: Color32::new(48, 48, 52, 255),
                border_width: 1.0,
                corner_radius: color::POPUP_RADIUS,
                ..UIStyle::default()
            })
            .interactive()
            .inert()
            .key(KEY_BG)
    }

    fn build_nodes(&mut self, tree: &mut UITree, x: f32, y: f32, panel_h: f32) {
        let chrome = self.chrome_view();
        self.host.build(tree, &chrome, Rect::new(x, y, self.panel_w, panel_h));
        self.bg_id = self.host.node_id_for_key(KEY_BG).unwrap_or(NodeId::PLACEHOLDER);
        self.panel_rect = Rect::new(x, y, self.panel_w, panel_h);
        // Reset every dynamic control id — repopulated by whichever `build_*`
        // branch below actually builds this frame.
        self.new_scene_id = None;
        self.show_gizmos_id = None;
        self.gizmo_mode_id = None;
        self.render_mode_id = None;
        self.add_id = None;
        self.header_buttons.clear();
        self.outliner_row_ids.clear();
        self.group_toggle_ids.clear();
        self.outliner_eye_ids.clear();
        self.material_block.clear();
        self.modifier_buttons.clear();
        // The properties card's old ids would otherwise register intents on
        // whatever now sits in those slots.
        self.properties_card.clear_nodes();
        let body = Rect::new(x, y + PAD, self.panel_w, (panel_h - PAD * 2.0).max(0.0));
        self.build_body(tree, body);
        self.reveal_pending_parameter_response(tree);
    }

    fn build_sentence(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, cy: f32, sentence: &str) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H * 2.0, sentence, wrapped_label_style());
        cy + ROW_H * 2.0 + ROW_GAP
    }

    fn build_no_scene(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "This layer has no 3D scene yet.", label_style());
        cy += ROW_H + ROW_GAP * 2.0;
        self.new_scene_id =
            Some(tree.add_button_keyed(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, btn_style(), "New 3D Scene", KEY_NEW_SCENE));
        cy + ROW_H + ROW_GAP
    }

    /// External selection write (REALTIME_3D_DESIGN.md P6): a viewport
    /// object-pick sets the SAME UI-local `self.selection` map an outliner
    /// row click does — one selection store for the whole app. A change moves
    /// the selection version, which rebuilds the Properties section.
    pub fn set_selection(&mut self, layer_id: LayerId, sel: SceneSelection) {
        self.clear_item_focus();
        let sel = self.shown(&layer_id, sel);
        self.selection.insert(layer_id.clone(), sel);
        self.selected.remove(&layer_id);
        self.reveal_properties = true;
        self.note_selection(&layer_id);
    }

    /// External selection write of a whole set (a Shift-click on a gizmo):
    /// `items` selected, the last one active. An empty set clears the
    /// selection and keeps the active item's card showing, as Blender does.
    pub fn select_items(&mut self, layer_id: LayerId, items: Vec<SceneSelection>) {
        let items: Vec<SceneSelection> = items.into_iter().map(|sel| self.shown(&layer_id, sel)).collect();
        match items.last().cloned() {
            None => {
                self.selected.insert(layer_id.clone(), items);
                self.note_selection(&layer_id);
            }
            Some(active) => {
                if self.selection.get(&layer_id) != Some(&active) {
                    self.clear_item_focus();
                    self.reveal_properties = true;
                }
                self.selection.insert(layer_id.clone(), active);
                if items.len() == 1 {
                    self.selected.remove(&layer_id);
                } else {
                    self.selected.insert(layer_id.clone(), items);
                }
                self.note_selection(&layer_id);
            }
        }
    }

    /// What the tab shows for `sel` on `layer`: a light shows as Lighting.
    fn shown(&self, layer: &LayerId, sel: SceneSelection) -> SceneSelection {
        match self.state.as_live() {
            Some(vm) if &vm.layer_id == layer => sel.shown(vm),
            _ => sel,
        }
    }

    /// Nothing selected on `layer_id` (a click on empty space); the active
    /// item's card keeps showing.
    pub fn clear_selected(&mut self, layer_id: LayerId) {
        self.select_items(layer_id, Vec::new());
    }

    /// A Shift-click on an outliner row: `sel` joins the selection as the
    /// active item, or leaves it when it was selected.
    fn toggle_selected(&mut self, layer_id: &LayerId, sel: SceneSelection) {
        let sel = self.shown(layer_id, sel);
        let mut items = self.selected_of(layer_id).to_vec();
        let before = items.len();
        items.retain(|item| *item != sel);
        // When the active item leaves, the last one still selected shows.
        if items.len() == before {
            items.push(sel);
        }
        self.select_items(layer_id.clone(), items);
    }

    /// Unfold the group `parent`, so a selected item under it shows.
    pub fn unfold(&mut self, layer_id: LayerId, parent: ItemId) {
        if let Some(collapsed) = self.collapsed_groups.get_mut(&layer_id) {
            collapsed.remove(&parent);
        }
    }

    /// The Scene panel's selection version (UI_INVALIDATION D14): the
    /// planner re-runs the inspector projection when it moves.
    pub fn selection_version(&self) -> u64 {
        self.selection_version
    }

    /// A selection write drops any reveal focus; the next note counts that as
    /// a change when something was focused.
    fn clear_item_focus(&mut self) {
        if self.pending_parameter_reveal.is_some() || self.revealed_property.is_some() {
            self.focus_cleared = true;
        }
        self.pending_parameter_reveal = None;
        self.revealed_property = None;
    }

    /// Compare-before-bump: the version moves only when `layer_id`'s
    /// selection differs from the last noted one, or a focus was cleared
    /// since the last note.
    pub(crate) fn note_selection(&mut self, layer_id: &LayerId) {
        let current = (layer_id.clone(), self.selection.get(layer_id).cloned(), self.selected.get(layer_id).cloned());
        let cleared = std::mem::take(&mut self.focus_cleared);
        if cleared || self.noted_selection.as_ref() != Some(&current) {
            self.noted_selection = Some(current);
            self.selection_version += 1;
        }
    }

    /// Capture the destination before a keyboard shortcut can overtake the
    /// queued click.
    pub fn select_outliner_node(&mut self, node_id: NodeId) -> bool {
        let Some((_, selection)) = self.outliner_row_ids.iter().find(|(id, _)| *id == node_id) else {
            return false;
        };
        let SceneSetupState::Live(vm) = &self.state else { return false };
        let (layer_id, selection) = (vm.layer_id.clone(), selection.clone());
        self.clear_item_focus();
        self.selected.remove(&layer_id);
        self.selection.insert(layer_id, selection);
        self.view_version += 1;
        true
    }

    /// The current selection for `vm.layer_id`, resolving a dangling id (an
    /// item a command removed) or no entry yet to the first item, and
    /// persisting the resolved value back so a later lookup this same frame
    /// sees the same answer the build used.
    fn resolve_selection(&mut self, vm: &SceneSetupVm) -> SceneSelection {
        let current = self.selection.get(&vm.layer_id).cloned().map(|sel| sel.shown(vm));
        let resolved = match current {
            Some(sel) if Self::selection_exists(vm, &sel) => sel,
            _ => Self::default_selection(vm),
        };
        if let Some(set) = self.selected.get_mut(&vm.layer_id) {
            set.retain(|sel| Self::selection_exists(vm, sel));
        }
        // The build shows the fallback, so it is what the next note compares
        // against: reselecting the vanished item after a redo is a change.
        if let Some((layer, noted, noted_set)) = self.noted_selection.as_mut()
            && *layer == vm.layer_id
        {
            *noted = Some(resolved.clone());
            *noted_set = self.selected.get(&vm.layer_id).cloned();
        }
        self.selection.insert(vm.layer_id.clone(), resolved.clone());
        resolved
    }

    fn selection_exists(vm: &SceneSetupVm, sel: &SceneSelection) -> bool {
        match sel {
            SceneSelection::Lighting => vm.items.iter().any(|item| item.kind == ItemKind::Light),
            SceneSelection::Item(id) => vm.item(id).is_some(),
        }
    }

    /// The first object, else the first item.
    fn default_selection(vm: &SceneSetupVm) -> SceneSelection {
        vm.items
            .iter()
            .find(|item| item.kind == ItemKind::Object)
            .or_else(|| vm.items.first())
            .map_or(SceneSelection::Lighting, |item| SceneSelection::Item(item.id.clone()))
    }

    /// The docked body's view version: scroll and fold frames.
    pub fn view_version(&self) -> u64 {
        self.view_version
    }

    /// Whether a point lands inside the panel's own rect — for the app's
    /// drag-ownership dispatch.
    pub fn point_in_panel(&self, pos: crate::node::Vec2) -> bool {
        self.open && self.panel_rect.contains(pos)
    }

    /// The Add menu for the live scene: every entry, a separator between
    /// menu sections. An entry adds into the selected group, else at the top.
    fn add_menu_items(&self) -> Vec<super::dropdown::DropdownItem> {
        let Some(vm) = self.state.as_live() else { return Vec::new() };
        let parent = match self.selection.get(&vm.layer_id) {
            Some(SceneSelection::Item(id)) => vm.item(id).filter(|item| item.kind == ItemKind::Group).map(|item| item.id.clone()),
            _ => None,
        };
        let mut items: Vec<super::dropdown::DropdownItem> = Vec::with_capacity(vm.add_entries.len());
        for (index, entry) in vm.add_entries.iter().enumerate() {
            let mut item = super::dropdown::DropdownItem::new(&format!("{} \u{2014} {}", entry.group, entry.label));
            if entry.group == entry.label {
                item.label = entry.label.clone();
            }
            item.action = Some(PanelAction::Project(ProjectAction::SceneAdd {
                layer_id: vm.layer_id.clone(),
                entry: entry.key.clone(),
                parent: parent.clone(),
            }));
            item.separator_after = vm.add_entries.get(index + 1).is_some_and(|next| next.group != entry.group);
            items.push(item);
        }
        items
    }

    /// Handle one input event. Returns `(consumed, actions)`.
    pub fn handle_event(&mut self, event: &UIEvent, tree: &mut UITree) -> (bool, Vec<PanelAction>) {
        if !self.open {
            return (false, Vec::new());
        }
        if let Some(handled) = self.handle_pane_event(event) {
            return handled;
        }
        match event {
            UIEvent::RightClick { node_id: Some(node_id), .. } if self.select_outliner_node(*node_id) => {
                if let Some(layer_id) = self.state.as_live().map(|vm| vm.layer_id.clone()) {
                    self.note_selection(&layer_id);
                }
                (true, vec![PanelAction::Root(RootAction::SceneItemRightClicked)])
            }
            UIEvent::Click { node_id, modifiers, .. } => {
                if self.show_gizmos_id == Some(*node_id) {
                    self.show_gizmos = !self.show_gizmos;
                    self.view_version += 1;
                    return (true, Vec::new());
                }
                if self.gizmo_mode_id == Some(*node_id) {
                    self.gizmo_draw_mode = self.gizmo_draw_mode.next();
                    self.view_version += 1;
                    return (true, Vec::new());
                }
                if self.render_mode_id == Some(*node_id) {
                    return (true, self.render_mode_pick_list(*node_id).into_iter().collect());
                }
                if let Some((_, group)) = self.group_toggle_ids.iter().find(|(id, _)| *id == *node_id).cloned() {
                    let Some(vm) = self.state.as_live() else {
                        return (true, Vec::new());
                    };
                    let collapsed = self.collapsed_groups.entry(vm.layer_id.clone()).or_default();
                    if !collapsed.insert(group.clone()) {
                        collapsed.remove(&group);
                    }
                    return (true, vec![PanelAction::Params(ParamsAction::SectionFoldToggled)]);
                }
                // An outliner row click sets the UI-local selection — no
                // command, no undo unit. The noted selection version
                // re-projects Properties THIS frame.
                if let Some((_, sel)) = self.outliner_row_ids.iter().find(|(id, _)| *id == *node_id) {
                    if let SceneSetupState::Live(vm) = &self.state {
                        let (layer_id, sel) = (vm.layer_id.clone(), sel.clone());
                        if modifiers.shift {
                            self.toggle_selected(&layer_id, sel);
                        } else {
                            self.clear_item_focus();
                            self.selection.insert(layer_id.clone(), sel);
                            self.selected.remove(&layer_id);
                            self.note_selection(&layer_id);
                        }
                        return (true, vec![PanelAction::Root(RootAction::SceneSetupSelectionChanged(layer_id))]);
                    }
                    return (true, Vec::new());
                }
                if let Some((row, _)) = self.properties_card.row_host.row_index.get(tree.widget_of(*node_id))
                    && self.scale_link_click(row)
                {
                    return (true, vec![PanelAction::Params(ParamsAction::SectionFoldToggled)]);
                }
                let mut actions = Vec::new();
                if let SceneSetupState::Live(vm) = &self.state {
                    if let Some(param) = self.outliner_eye_ids.iter().find(|(id, _, _)| *id == *node_id).map(|(_, param, _)| param.clone()) {
                        // The eye is the item's Visible switch: the same
                        // toggle every switch row sends.
                        let target = GraphParamTarget::GeneratorOf(vm.layer_id.clone());
                        actions.push(PanelAction::Params(ParamsAction::ParamToggle(target, manifold_foundation::ParamId::from(param))));
                    } else if self.add_id == Some(*node_id) {
                        actions.push(PanelAction::Root(RootAction::SceneSetupActionMenuClicked {
                            button_node_id: *node_id,
                            items: self.add_menu_items(),
                        }));
                    } else if let Some(act) = self.item_header_click(*node_id, vm).or_else(|| self.material_click(*node_id, vm)).or_else(|| self.modifier_click(*node_id, vm)) {
                        actions.push(act);
                    } else if let Some(row_role) = self.properties_card.row_host.row_index.get(tree.widget_of(*node_id)) {
                        let (row, role) = row_role;
                        let target = GraphParamTarget::GeneratorOf(vm.layer_id.clone());
                        for mut action in self.properties_row_action(tree, row, role, *node_id, target) {
                            if let PanelAction::Root(RootAction::BeginDriverPeriodTextInput { anchor, value, .. }) = &mut action {
                                *anchor = tree.get_bounds(*node_id);
                                *value = self.properties_card.mod_state.driver_effective_period(row);
                            }
                            actions.push(action);
                        }
                    }
                }
                if let SceneSetupState::NoScene { layer_id } = &self.state
                    && self.new_scene_id == Some(*node_id)
                {
                    actions.push(PanelAction::Project(ProjectAction::SceneNew(layer_id.clone())));
                }
                (!actions.is_empty(), actions)
            }
            // Double-click on a drag-armable value cell opens its type-in
            // box, through the SAME shared `RowHost::value_cell_typein`
            // `ParamCardPanel` uses; target is the panel's own bound layer
            // (BUG-292).
            UIEvent::DoubleClick { node_id, .. } => {
                if let SceneSetupState::Live(vm) = &self.state {
                    let card = &self.properties_card;
                    let target = GraphParamTarget::GeneratorOf(vm.layer_id.clone());
                    if let Some(action) = card.row_host.value_cell_typein(*node_id, tree, &card.rows, &card.current_values, target) {
                        return (true, vec![action]);
                    }
                }
                (false, Vec::new())
            }
            UIEvent::PointerDown { node_id, pos, .. } => {
                if let SceneSetupState::Live(vm) = &self.state {
                    let target = GraphParamTarget::GeneratorOf(vm.layer_id.clone());
                    let was_dragging = self.properties_card.row_host.is_dragging();
                    let actions = self.properties_card.handle_pointer_down(*node_id, *pos, tree, &target);
                    let actions = self.rewrite_drag_actions(actions);
                    if was_dragging || self.properties_card.row_host.is_dragging() || !actions.is_empty() {
                        return (true, actions);
                    }
                }
                (self.owns_node(*node_id) || self.point_in_panel(*pos), Vec::new())
            }
            UIEvent::DragBegin { .. } => (self.properties_card.row_host.is_dragging(), Vec::new()),
            UIEvent::Drag { pos, modifiers, .. } => {
                if let SceneSetupState::Live(vm) = &self.state {
                    let target = GraphParamTarget::GeneratorOf(vm.layer_id.clone());
                    let was_dragging = self.properties_card.row_host.is_dragging();
                    let actions = self.properties_card.handle_drag(*pos, tree, modifiers.shift, &target);
                    let actions = self.rewrite_drag_actions(actions);
                    if was_dragging || !actions.is_empty() {
                        return (true, actions);
                    }
                }
                (false, Vec::new())
            }
            UIEvent::DragEnd { .. } | UIEvent::PointerUp { .. } => {
                let was_dragging = self.properties_card.row_host.is_dragging();
                let actions = self.properties_card.row_host.handle_drag_end();
                let actions = self.rewrite_drag_actions(actions);
                (was_dragging || !actions.is_empty(), actions)
            }
            _ => (false, Vec::new()),
        }
    }

    /// A drag's actions after the colour swatch's RGB rewrite.
    fn rewrite_drag_actions(&mut self, actions: Vec<PanelAction>) -> Vec<PanelAction> {
        self.rewrite_material_rgb_actions(actions)
    }

    fn owns_node(&self, node_id: NodeId) -> bool {
        node_id == self.bg_id
    }

    /// The name label's rect for `item`, if its header was built this frame
    /// — the app's text-input anchor lookup.
    pub fn item_name_rect(&self, tree: &UITree, item: &ItemId) -> Option<Rect> {
        let (_, node_id, _) = self.header_buttons.names.iter().find(|(row, _, _)| row == item)?;
        Some(tree.get_bounds(*node_id))
    }
}

fn scrollbar_style() -> ScrollbarStyle {
    ScrollbarStyle {
        track_color: color::SCROLLBAR_TRACK_C32,
        thumb_color: color::SCROLLBAR_THUMB_C32,
        thumb_hover_color: color::SCROLLBAR_THUMB_HOVER_C32,
        corner_radius: color::SMALL_RADIUS,
    }
}

pub(crate) fn btn_style() -> UIStyle {
    UIStyle { font_size: color::FONT_LABEL, ..crate::chrome::components::segment_style(false) }
}

pub(crate) fn label_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(150, 150, 160, 255),
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

fn wrapped_label_style() -> UIStyle {
    label_style()
}

fn header_label_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(224, 224, 228, 255),
        font_size: color::FONT_BODY,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

/// A drag-armable value label — visually distinct (subtle hover fill) from a
/// bare `label_style()` text row so it reads as clickable, not static prose.
fn drag_value_style() -> UIStyle {
    UIStyle {
        bg_color: Color32::new(30, 30, 34, 200),
        hover_bg_color: Color32::new(44, 44, 50, 255),
        text_color: color::SLIDER_TEXT_C32,
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Center,
        corner_radius: color::SMALL_RADIUS,
        ..UIStyle::default()
    }
}

