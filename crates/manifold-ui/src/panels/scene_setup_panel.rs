//! Scene Setup panel — the "add effects, for 3D" dock
//! (`docs/SCENE_SETUP_PANEL_DESIGN.md`).
//!
//! A `ScreenLayout::scene_setup()` docked column, cloned from
//! [`super::audio_setup_panel::AudioSetupPanel`] (D2): same fold-out /
//! resize / snap-back / Escape-close mechanics, mutually exclusive with the
//! Audio Setup dock. Self-contained like that panel — it builds `UITree`
//! nodes from data handed in via [`ScenePanel::configure`] and maps clicks/
//! drags to [`super::PanelAction`]. P1 scope: Header + Environment + Fog
//! sections live, plus the D7 empty states. Objects/Lights/Camera land in
//! later phases (P2/P3) — this panel never builds a generic param-tree
//! renderer (D3's named wrong turn); every row here is a curated, named
//! control.
//!
//! Every value this panel writes goes through [`super::PanelAction::SceneSetupParamChanged`]
//! — the SAME `SetGraphNodeParamCommand` the graph editor's ordinary
//! (unbound) node-face numeric row already dispatches per drag tick
//! (`manifold-app/src/app_render.rs`'s `GraphEditCommand::SetGraphNodeParam`
//! handling) — never a new mutation path. No direct project mutation and no
//! shared-lock wrapper types appear anywhere in this file (§4 negative gate).

use crate::chrome::{ChromeHost, Pad, Sizing, View};
use crate::color;
use crate::drag::DragController;
use crate::input::UIEvent;
use crate::node::*;
use crate::scroll_container::{SCROLLBAR_W, ScrollContainer, ScrollbarStyle};
use crate::tree::UITree;
use manifold_foundation::LayerId;

use super::PanelAction;

// ── Stable keys ──
const KEY_BG: u64 = 80_001;
const KEY_CLOSE: u64 = 80_002;
const KEY_ADD_ENVIRONMENT: u64 = 80_010;
const KEY_ADD_FOG: u64 = 80_011;
const KEY_NEW_SCENE: u64 = 80_012;
const KEY_OPEN_GRAPH_EDITOR: u64 = 80_013;

/// Per-row stepper/drag control keys — stride leaves headroom for a handful
/// of controls per row (value drag zone + minus + plus).
const KEY_ROW_BASE: u64 = 81_000;
const KEY_ROW_STRIDE: u64 = 8;
const ROW_OFF_MINUS: u64 = 0;
const ROW_OFF_VALUE: u64 = 1;
const ROW_OFF_PLUS: u64 = 2;

const fn row_key(row: u64, offset: u64) -> u64 {
    KEY_ROW_BASE + row * KEY_ROW_STRIDE + offset
}
// Row indices for the curated P1 sliders (stable across rebuilds regardless
// of which optional rows are present, so a widget's identity never shifts
// under the user's cursor mid-drag).
const ROW_ENV_INTENSITY: u64 = 0;
const ROW_ENV_FILL: u64 = 1;
const ROW_FOG_DENSITY: u64 = 2;
const ROW_FOG_HEIGHT_FALLOFF: u64 = 3;

const PANEL_W_MIN: f32 = 320.0;
const TITLE_H: f32 = 26.0;
const ROW_H: f32 = 24.0;
const ROW_GAP: f32 = 4.0;
const PAD: f32 = 10.0;
const STEP_W: f32 = 22.0;
const LABEL_W: f32 = 130.0;
const VALUE_W: f32 = 70.0;

/// A single editable node-param address: the exact `(node_doc_id, param_id)`
/// pair `SetGraphNodeParamCommand` takes (scope_path is empty for every P1
/// row — Environment/Fog live at the generator graph's root; nested-group
/// scoping arrives with Objects in P2).
#[derive(Clone, Debug, PartialEq)]
pub struct RowAddr {
    pub node_doc_id: u32,
    pub param_id: String,
}

/// One numeric row: its write address, current value, range, and whether a
/// wire currently drives it (driven rows render read-only — D4).
#[derive(Clone, Debug, PartialEq)]
pub struct RowValue {
    pub addr: RowAddr,
    pub value: f32,
    pub min: f32,
    pub max: f32,
    pub driven: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EnvironmentRowVm {
    /// Importer shape (switch_texture selecting Softbox/HDRI) — Mode is
    /// shown as a static chip in P1 (toggling it is a P2+ affordance; the
    /// value is legible, just not yet a control here).
    Importer { mode_is_hdri: bool, intensity: RowValue, fill: RowValue, hdri_file: String },
    Bare { intensity: RowValue, fill: RowValue },
    /// Some other producer wired into `envmap` — honest custom row, no
    /// controls (D3).
    Custom,
    /// Unwired — the "Add environment" empty row.
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AtmosphereRowVm {
    Wired { density: RowValue, height_falloff: RowValue },
    /// Unwired — the "Add fog" empty row.
    None,
}

/// Full live-panel view model for one selected generator layer's scene —
/// translated 1:1 from `manifold_renderer::node_graph::scene_vm::SceneVm`'s
/// Header/Environment/Atmosphere sections by `state_sync` (this crate can't
/// depend on `manifold-renderer`/`manifold-core`, so the translation is the
/// UI-facing DTO boundary, same convention as `AudioSendRow`).
#[derive(Clone, Debug, PartialEq)]
pub struct SceneSetupVm {
    pub layer_id: LayerId,
    pub scene_name: String,
    pub multiple_scenes: bool,
    pub object_count: usize,
    pub light_count: usize,
    pub shadow_caster_count: usize,
    /// `render_scene`'s own doc id — the target the "Add environment"/
    /// "Add fog" composites wire into.
    pub scene_root_node_id: u32,
    pub environment: EnvironmentRowVm,
    pub atmosphere: AtmosphereRowVm,
}

/// D7's four empty/live states for the selected layer.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneSetupState {
    /// Nothing selected, or the selection isn't a generator layer — one
    /// sentence naming what to select.
    NoSelection(String),
    /// A generator layer with no generator assigned (or an empty slot).
    NoGenerator { layer_id: LayerId },
    /// A generator layer whose graph has no `render_scene`.
    NoScene { layer_id: LayerId },
    /// The full panel.
    Live(Box<SceneSetupVm>),
}

impl Default for SceneSetupState {
    fn default() -> Self {
        SceneSetupState::NoSelection("Select a layer to set up its scene.".to_string())
    }
}

/// A value-label drag session (D7 gesture: "ride Fog density with the
/// mouse") — same pointer-down-arms/drag-computes/release-clears shape as
/// `AudioSetupPanel`'s gain-stepper calibration drag.
#[derive(Clone, Debug)]
struct ValueDrag {
    addr: RowAddr,
    start_x: f32,
    start_value: f32,
    min: f32,
    max: f32,
}

/// One numeric row's interactive node ids, set by `build_numeric_row` when
/// the row is live (driven rows leave all three `None` — no steppers).
/// Imperative `tree.add_*` calls (unlike the declarative `ChromeHost`/`View`
/// chrome) don't register a key→NodeId lookup of their own, so — same
/// convention `AudioSetupPanel::SendRowIds` uses — the panel stores each
/// dynamic control's id directly instead of re-deriving it from a key.
#[derive(Clone, Copy, Default)]
struct RowIds {
    minus: Option<NodeId>,
    value: Option<NodeId>,
    plus: Option<NodeId>,
}

pub struct ScenePanel {
    open: bool,
    state: SceneSetupState,
    panel_w: f32,
    host: ChromeHost,
    scroll: ScrollContainer,
    content_parent: NodeId,
    bg_id: NodeId,
    close_id: NodeId,
    add_environment_id: Option<NodeId>,
    add_fog_id: Option<NodeId>,
    new_scene_id: Option<NodeId>,
    open_graph_editor_id: Option<NodeId>,
    /// Indexed by the `ROW_*` constants.
    row_ids: [RowIds; 4],
    panel_rect: Rect,
    drag: DragController<ValueDrag>,
    /// The layer_id a drag targets — captured at PointerDown so `on_event`
    /// doesn't need to re-read `self.state` (which may rebuild mid-drag on
    /// an unrelated `configure`, per D1 "no staleness": the drag itself
    /// still targets the layer it started on).
    drag_layer_id: Option<LayerId>,
}

impl Default for ScenePanel {
    fn default() -> Self {
        Self {
            open: false,
            state: SceneSetupState::default(),
            panel_w: PANEL_W_MIN,
            host: ChromeHost::new(),
            scroll: ScrollContainer::new(),
            content_parent: NodeId::PLACEHOLDER,
            bg_id: NodeId::PLACEHOLDER,
            close_id: NodeId::PLACEHOLDER,
            add_environment_id: None,
            add_fog_id: None,
            new_scene_id: None,
            open_graph_editor_id: None,
            row_ids: [RowIds::default(); 4],
            panel_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            drag: DragController::new(),
            drag_layer_id: None,
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
    /// structural sync while the panel is open (or about to become open) —
    /// rebuilt fresh from the snapshot every time (D1: "no rotting, no
    /// staleness").
    pub fn configure(&mut self, state: SceneSetupState) {
        self.state = state;
    }

    /// Build the panel as a docked column into `rect`
    /// (`ScreenLayout::scene_setup()`). No-op when closed.
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
            .pad(Pad::all(PAD))
            .child(
                View::row(0.0)
                    .fill_w()
                    .h(Sizing::Fixed(TITLE_H))
                    .child(
                        View::label("Scene Setup")
                            .fill_w()
                            .fill_h()
                            .font(color::FONT_BODY)
                            .text_color(Color32::new(224, 224, 228, 255))
                            .align_text(TextAlign::Left),
                    )
                    .child(
                        View::button("\u{00D7}")
                            .w(Sizing::Fixed(STEP_W))
                            .fill_h()
                            .style(btn_style())
                            .inert()
                            .key(KEY_CLOSE),
                    ),
            )
    }

    fn build_nodes(&mut self, tree: &mut UITree, x: f32, y: f32, panel_h: f32) {
        let chrome = self.chrome_view();
        self.host.build(tree, &chrome, Rect::new(x, y, self.panel_w, panel_h));
        self.bg_id = self.host.node_id_for_key(KEY_BG).unwrap_or(NodeId::PLACEHOLDER);
        self.close_id = self.host.node_id_for_key(KEY_CLOSE).unwrap_or(NodeId::PLACEHOLDER);
        self.panel_rect = Rect::new(x, y, self.panel_w, panel_h);
        // Reset every dynamic control id — repopulated by whichever
        // `build_*` branch below actually builds this frame (state_sync
        // rebuilds fresh every pass, D1 "no staleness").
        self.add_environment_id = None;
        self.add_fog_id = None;
        self.new_scene_id = None;
        self.open_graph_editor_id = None;
        self.row_ids = [RowIds::default(); 4];

        let inner_x = x + PAD;
        let inner_w = self.panel_w - PAD * 2.0;
        let content_top = y + PAD + TITLE_H;
        let body_viewport = Rect::new(x, content_top, self.panel_w, (y + panel_h - PAD - content_top).max(0.0));
        let clip_id = self.scroll.begin(tree, body_viewport);
        self.content_parent = clip_id;
        let content_start = tree.count();
        let mut cy = content_top;

        cy = match self.state.clone() {
            SceneSetupState::NoSelection(sentence) => {
                self.build_sentence(tree, inner_x, inner_w, cy, &sentence)
            }
            SceneSetupState::NoGenerator { .. } => self.build_no_generator(tree, inner_x, inner_w, cy),
            SceneSetupState::NoScene { .. } => self.build_no_scene(tree, inner_x, inner_w, cy),
            SceneSetupState::Live(vm) => self.build_live(tree, inner_x, inner_w, cy, &vm),
        };

        let content_height = (cy - content_top + PAD).max(0.0);
        self.scroll.set_content_height(content_height);
        self.scroll.reparent_content(tree, content_start);
        let offset = self.scroll.scroll_offset();
        if offset != 0.0 {
            self.scroll.offset_content(tree, -offset);
        }
        let sb_x = x + self.panel_w - SCROLLBAR_W - 2.0;
        self.scroll.build_scrollbar(tree, sb_x, &scrollbar_style());
    }

    fn build_sentence(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, cy: f32, sentence: &str) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H * 2.0, sentence, wrapped_label_style());
        cy + ROW_H * 2.0 + ROW_GAP
    }

    fn build_no_generator(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32) -> f32 {
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "This layer has no 3D scene yet.",
            label_style(),
        );
        cy += ROW_H + ROW_GAP * 2.0;
        self.new_scene_id = Some(tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            btn_style(),
            "New 3D Scene",
            KEY_NEW_SCENE,
        ));
        cy + ROW_H + ROW_GAP
    }

    fn build_no_scene(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32) -> f32 {
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "This generator has no 3D scene.",
            label_style(),
        );
        cy += ROW_H + ROW_GAP * 2.0;
        self.open_graph_editor_id = Some(tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            btn_style(),
            "Open Graph Editor",
            KEY_OPEN_GRAPH_EDITOR,
        ));
        cy + ROW_H + ROW_GAP
    }

    fn build_live(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32, vm: &SceneSetupVm) -> f32 {
        // ── Header ──
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, &vm.scene_name, header_label_style());
        cy += ROW_H;
        if vm.multiple_scenes {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                "2+ scenes in this graph — showing the first",
                label_style(),
            );
            cy += ROW_H;
        }
        let counts = format!(
            "{} object{} · {} light{} · {} shadow caster{}",
            vm.object_count,
            if vm.object_count == 1 { "" } else { "s" },
            vm.light_count,
            if vm.light_count == 1 { "" } else { "s" },
            vm.shadow_caster_count,
            if vm.shadow_caster_count == 1 { "" } else { "s" },
        );
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, &counts, label_style());
        cy += ROW_H + ROW_GAP * 2.0;

        // ── Environment ──
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Environment", section_label_style());
        cy += ROW_H;
        match &vm.environment {
            EnvironmentRowVm::Importer { mode_is_hdri, intensity, fill, hdri_file } => {
                tree.add_label(
                    Some(self.content_parent),
                    inner_x,
                    cy,
                    inner_w,
                    ROW_H,
                    if *mode_is_hdri { "Mode: HDRI" } else { "Mode: Softbox" },
                    label_style(),
                );
                cy += ROW_H;
                cy = self.build_numeric_row(tree, inner_x, inner_w, cy, "Intensity", intensity, ROW_ENV_INTENSITY);
                cy = self.build_numeric_row(tree, inner_x, inner_w, cy, "Fill", fill, ROW_ENV_FILL);
                if !hdri_file.is_empty() {
                    tree.add_label(
                        Some(self.content_parent),
                        inner_x,
                        cy,
                        inner_w,
                        ROW_H,
                        &format!("HDRI: {hdri_file}"),
                        label_style(),
                    );
                    cy += ROW_H;
                }
            }
            EnvironmentRowVm::Bare { intensity, fill } => {
                cy = self.build_numeric_row(tree, inner_x, inner_w, cy, "Intensity", intensity, ROW_ENV_INTENSITY);
                cy = self.build_numeric_row(tree, inner_x, inner_w, cy, "Fill", fill, ROW_ENV_FILL);
            }
            EnvironmentRowVm::Custom => {
                tree.add_label(
                    Some(self.content_parent),
                    inner_x,
                    cy,
                    inner_w,
                    ROW_H,
                    "Custom (edit in graph)",
                    label_style(),
                );
                cy += ROW_H;
            }
            EnvironmentRowVm::None => {
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "None", label_style());
                cy += ROW_H;
                self.add_environment_id = Some(tree.add_button_keyed(
                    Some(self.content_parent),
                    inner_x,
                    cy,
                    inner_w,
                    ROW_H,
                    btn_style(),
                    "+ Add Environment",
                    KEY_ADD_ENVIRONMENT,
                ));
                cy += ROW_H;
            }
        }
        cy += ROW_GAP * 2.0;

        // ── Fog ──
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Fog", section_label_style());
        cy += ROW_H;
        match &vm.atmosphere {
            AtmosphereRowVm::Wired { density, height_falloff } => {
                cy = self.build_numeric_row(tree, inner_x, inner_w, cy, "Density", density, ROW_FOG_DENSITY);
                cy = self.build_numeric_row(
                    tree,
                    inner_x,
                    inner_w,
                    cy,
                    "Height Falloff",
                    height_falloff,
                    ROW_FOG_HEIGHT_FALLOFF,
                );
            }
            AtmosphereRowVm::None => {
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "None", label_style());
                cy += ROW_H;
                self.add_fog_id = Some(tree.add_button_keyed(
                    Some(self.content_parent),
                    inner_x,
                    cy,
                    inner_w,
                    ROW_H,
                    btn_style(),
                    "+ Add Fog",
                    KEY_ADD_FOG,
                ));
                cy += ROW_H;
            }
        }
        cy + ROW_GAP
    }

    /// One `[label]  [−] value [＋]` numeric row. Driven rows (D4) render
    /// with no interactive steppers and a dimmed "driven" badge — the panel
    /// never fights the graph.
    fn build_numeric_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        row: &RowValue,
        row_index: u64,
    ) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
        let slot = &mut self.row_ids[row_index as usize];
        if row.driven {
            tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                inner_w - LABEL_W,
                ROW_H,
                &format!("{:.2} (driven)", row.value),
                driven_label_style(),
            );
            *slot = RowIds::default();
            return cy + ROW_H;
        }
        let step_x = inner_x + inner_w - VALUE_W - STEP_W * 2.0;
        let minus = tree.add_button_keyed(
            Some(self.content_parent),
            step_x,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{2212}",
            row_key(row_index, ROW_OFF_MINUS),
        );
        // A real interactive widget, not a plain label: `UITree::hit_test`
        // only ever returns `INTERACTIVE`-flagged nodes (it skips a bare
        // label and falls through to whatever's behind it — the panel
        // background, in this dock), so the value cell must carry the flag
        // itself to be a legitimate pointer-down/drag target. Styled to read
        // as a drag zone, not a push-button (no press-style feedback beyond
        // the hover fill already in `drag_value_style`).
        let value = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W,
            cy,
            VALUE_W,
            ROW_H,
            drag_value_style(),
            &format!("{:.2}", row.value),
            row_key(row_index, ROW_OFF_VALUE),
        );
        let plus = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W + VALUE_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{002B}",
            row_key(row_index, ROW_OFF_PLUS),
        );
        *slot = RowIds { minus: Some(minus), value: Some(value), plus: Some(plus) };
        cy + ROW_H
    }

    /// Mouse-wheel scroll for the docked body.
    pub fn handle_scroll(&mut self, delta: f32) -> bool {
        self.scroll.apply_scroll_delta(-delta)
    }

    /// Whether a point lands inside the panel's own rect — for the app's
    /// drag-ownership dispatch (mirrors `AudioSetupPanel::point_in_panel`).
    pub fn point_in_panel(&self, pos: crate::node::Vec2) -> bool {
        self.open && self.panel_rect.contains(pos)
    }

    /// Handle one input event. Returns `(consumed, actions)`.
    pub fn handle_event(&mut self, event: &UIEvent) -> (bool, Vec<PanelAction>) {
        if !self.open {
            return (false, Vec::new());
        }
        match event {
            UIEvent::Click { node_id, .. } => {
                if *node_id == self.close_id {
                    self.close();
                    return (true, Vec::new());
                }
                let mut actions = Vec::new();
                if let SceneSetupState::Live(vm) = &self.state {
                    if self.add_environment_id == Some(*node_id) {
                        actions.push(PanelAction::SceneSetupAddEnvironment(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                        ));
                    } else if self.add_fog_id == Some(*node_id) {
                        actions.push(PanelAction::SceneSetupAddFog(vm.layer_id.clone(), vm.scene_root_node_id));
                    } else if let Some((row, delta)) = self.stepper_hit(*node_id)
                        && let Some(row_value) = self.row_value_for(vm, row)
                    {
                        let new_value = (row_value.value + delta).clamp(row_value.min, row_value.max);
                        actions.push(PanelAction::SceneSetupParamChanged(
                            vm.layer_id.clone(),
                            row_value.addr.node_doc_id,
                            row_value.addr.param_id.clone(),
                            new_value,
                        ));
                    }
                }
                match &self.state {
                    SceneSetupState::NoGenerator { layer_id } if self.new_scene_id == Some(*node_id) => {
                        actions.push(PanelAction::SceneSetupNewScene(layer_id.clone()));
                    }
                    SceneSetupState::NoScene { layer_id } if self.open_graph_editor_id == Some(*node_id) => {
                        actions.push(PanelAction::SceneSetupOpenGraphEditor(layer_id.clone()));
                    }
                    _ => {}
                }
                (!actions.is_empty() || *node_id == self.close_id, actions)
            }
            UIEvent::PointerDown { node_id, pos, .. } => {
                if let SceneSetupState::Live(vm) = &self.state
                    && let Some(row) = self.value_label_row_at(*node_id)
                    && let Some(row_value) = self.row_value_for(vm, row)
                    && !row_value.driven
                {
                    self.drag_layer_id = Some(vm.layer_id.clone());
                    self.drag.start(
                        ValueDrag {
                            addr: row_value.addr.clone(),
                            start_x: pos.x,
                            start_value: row_value.value,
                            min: row_value.min,
                            max: row_value.max,
                        },
                        *pos,
                    );
                    return (true, Vec::new());
                }
                (self.owns_node(*node_id) || self.point_in_panel(*pos), Vec::new())
            }
            UIEvent::DragBegin { .. } => (self.drag.is_active(), Vec::new()),
            UIEvent::Drag { pos, .. } => match (self.drag.payload().cloned(), self.drag_layer_id.clone()) {
                (Some(drag), Some(layer_id)) => {
                    // 1 px = 0.01 units — the same order-of-magnitude scrub
                    // rate the audio dock's gain drag uses (0.1 dB/px),
                    // scaled for these params' typical [0, ~2] ranges.
                    let new_value = (drag.start_value + (pos.x - drag.start_x) * 0.01).clamp(drag.min, drag.max);
                    (
                        true,
                        vec![PanelAction::SceneSetupParamChanged(
                            layer_id,
                            drag.addr.node_doc_id,
                            drag.addr.param_id.clone(),
                            new_value,
                        )],
                    )
                }
                _ => (false, Vec::new()),
            },
            UIEvent::DragEnd { .. } | UIEvent::PointerUp { .. } => {
                self.drag.release();
                self.drag_layer_id = None;
                (false, Vec::new())
            }
            _ => (false, Vec::new()),
        }
    }

    fn owns_node(&self, node_id: NodeId) -> bool {
        node_id == self.bg_id
    }

    fn stepper_hit(&self, node_id: NodeId) -> Option<(u64, f32)> {
        for (row, ids) in self.row_ids.iter().enumerate() {
            if ids.minus == Some(node_id) {
                return Some((row as u64, -0.05));
            }
            if ids.plus == Some(node_id) {
                return Some((row as u64, 0.05));
            }
        }
        None
    }

    fn value_label_row_at(&self, node_id: NodeId) -> Option<u64> {
        self.row_ids
            .iter()
            .position(|ids| ids.value == Some(node_id))
            .map(|row| row as u64)
    }

    fn row_value_for(&self, vm: &SceneSetupVm, row: u64) -> Option<RowValue> {
        match row {
            ROW_ENV_INTENSITY => match &vm.environment {
                EnvironmentRowVm::Importer { intensity, .. } | EnvironmentRowVm::Bare { intensity, .. } => {
                    Some(intensity.clone())
                }
                _ => None,
            },
            ROW_ENV_FILL => match &vm.environment {
                EnvironmentRowVm::Importer { fill, .. } | EnvironmentRowVm::Bare { fill, .. } => Some(fill.clone()),
                _ => None,
            },
            ROW_FOG_DENSITY => match &vm.atmosphere {
                AtmosphereRowVm::Wired { density, .. } => Some(density.clone()),
                AtmosphereRowVm::None => None,
            },
            ROW_FOG_HEIGHT_FALLOFF => match &vm.atmosphere {
                AtmosphereRowVm::Wired { height_falloff, .. } => Some(height_falloff.clone()),
                AtmosphereRowVm::None => None,
            },
            _ => None,
        }
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

fn btn_style() -> UIStyle {
    UIStyle { font_size: color::FONT_LABEL, ..crate::chrome::components::segment_style(false) }
}

fn label_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(150, 150, 160, 255),
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

fn wrapped_label_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(150, 150, 160, 255),
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

fn header_label_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(224, 224, 228, 255),
        font_size: color::FONT_BODY,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

fn section_label_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(190, 190, 198, 255),
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

/// A drag-armable value label — visually distinct (subtle hover fill) from a
/// bare `label_style()` text row so it reads as draggable, not static prose
/// (the affordance-legibility rule: DESIGN_DOC_STANDARD §5).
fn drag_value_style() -> UIStyle {
    UIStyle {
        bg_color: Color32::new(30, 30, 34, 200),
        hover_bg_color: Color32::new(44, 44, 50, 255),
        text_color: Color32::new(214, 214, 220, 255),
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Center,
        corner_radius: color::SMALL_RADIUS,
        ..UIStyle::default()
    }
}

fn driven_label_style() -> UIStyle {
    UIStyle {
        text_color: color::TEXT_DIMMED,
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Right,
        ..UIStyle::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_row(value: f32) -> RowValue {
        RowValue { addr: RowAddr { node_doc_id: 3, param_id: "intensity".to_string() }, value, min: 0.0, max: 4.0, driven: false }
    }

    #[test]
    fn closed_panel_builds_nothing() {
        let mut panel = ScenePanel::new();
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(tree.count(), 0, "a closed panel must not build any node");
    }

    #[test]
    fn no_selection_state_renders_a_sentence_without_panicking() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::NoSelection("Select a layer.".to_string()));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(tree.count() > 0);
    }

    #[test]
    fn live_state_with_unwired_env_and_fog_shows_add_buttons() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(SceneSetupVm {
            layer_id: LayerId::new("layer-1"),
            scene_name: "Scene".to_string(),
            multiple_scenes: false,
            object_count: 0,
            light_count: 0,
            shadow_caster_count: 0,
            scene_root_node_id: 0,
            environment: EnvironmentRowVm::None,
            atmosphere: AtmosphereRowVm::None,
        })));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.add_environment_id.is_some());
        assert!(panel.add_fog_id.is_some());
    }

    #[test]
    fn driven_row_has_no_steppers() {
        let mut panel = ScenePanel::new();
        panel.open();
        let mut intensity = env_row(1.0);
        intensity.driven = true;
        panel.configure(SceneSetupState::Live(Box::new(SceneSetupVm {
            layer_id: LayerId::new("layer-1"),
            scene_name: "Scene".to_string(),
            multiple_scenes: false,
            object_count: 0,
            light_count: 0,
            shadow_caster_count: 0,
            scene_root_node_id: 0,
            environment: EnvironmentRowVm::Bare { intensity, fill: env_row(0.0) },
            atmosphere: AtmosphereRowVm::None,
        })));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.row_ids[ROW_ENV_INTENSITY as usize].minus.is_none());
    }
}
