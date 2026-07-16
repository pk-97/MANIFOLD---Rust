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
const KEY_ADD_OBJECT: u64 = 80_014;
const KEY_ADD_LIGHT: u64 = 80_015;

/// Per-object dynamic keys: `OBJ_KEY_BASE + index * OBJ_KEY_STRIDE + offset`.
/// Objects are a variable-length list (unlike the four fixed Environment/Fog
/// rows above), so — like `KEY_ROW_BASE`/`row_key` — every object gets a
/// private key range wide enough for its expand toggle, name, and up to 12
/// numeric controls (3 triplets + color + metallic + roughness).
const OBJ_KEY_BASE: u64 = 82_000;
const OBJ_KEY_STRIDE: u64 = 32;
const OBJ_OFF_EXPAND: u64 = 0;
const OBJ_OFF_NAME: u64 = 1;
// Triplet rows (`build_triplet_row`) take only the FIRST offset — Y/Z cells
// key off `base_offset + 1`/`+ 2` (the cell loop's `i`), so only the X/R
// anchor needs a named constant.
const OBJ_OFF_POS_X: u64 = 2;
const OBJ_OFF_ROT_X: u64 = 5;
const OBJ_OFF_SCALE_X: u64 = 8;
const OBJ_OFF_COLOR_R: u64 = 11;
// Numeric stepper rows (`build_object_numeric_row`) likewise take only the
// minus offset — value/plus key off `base_offset + 1`/`+ 2`.
const OBJ_OFF_METALLIC_MINUS: u64 = 14;
const OBJ_OFF_ROUGHNESS_MINUS: u64 = 17;

const fn obj_key(index: usize, offset: u64) -> u64 {
    OBJ_KEY_BASE + index as u64 * OBJ_KEY_STRIDE + offset
}

/// Stable automation name for one triplet cell (a `build_triplet_row` value
/// box), by control kind + axis — `nth` (per-object) still disambiguates
/// which object a flow means, mirroring the audio dock's `name` + `nth`
/// convention. Same fix as `fixed_row_automation_name` — a bare `text` +
/// `under_text` selector can't tell two "0.00" cells apart in this flat
/// (no per-section container) panel.
const fn triplet_cell_automation_name(base_offset: u64, axis: usize) -> Option<&'static str> {
    match (base_offset, axis) {
        (OBJ_OFF_POS_X, 0) => Some("scene_setup.object.pos_x"),
        (OBJ_OFF_POS_X, 1) => Some("scene_setup.object.pos_y"),
        (OBJ_OFF_POS_X, 2) => Some("scene_setup.object.pos_z"),
        (OBJ_OFF_ROT_X, 0) => Some("scene_setup.object.rot_x"),
        (OBJ_OFF_ROT_X, 1) => Some("scene_setup.object.rot_y"),
        (OBJ_OFF_ROT_X, 2) => Some("scene_setup.object.rot_z"),
        (OBJ_OFF_SCALE_X, 0) => Some("scene_setup.object.scale_x"),
        (OBJ_OFF_SCALE_X, 1) => Some("scene_setup.object.scale_y"),
        (OBJ_OFF_SCALE_X, 2) => Some("scene_setup.object.scale_z"),
        (OBJ_OFF_COLOR_R, 0) => Some("scene_setup.object.color_r"),
        (OBJ_OFF_COLOR_R, 1) => Some("scene_setup.object.color_g"),
        (OBJ_OFF_COLOR_R, 2) => Some("scene_setup.object.color_b"),
        _ => None,
    }
}

/// Stable automation name for an object-row numeric stepper's value cell
/// (metallic/roughness).
const fn object_numeric_row_automation_name(base_offset: u64) -> Option<&'static str> {
    match base_offset {
        OBJ_OFF_METALLIC_MINUS => Some("scene_setup.object.metallic_value"),
        OBJ_OFF_ROUGHNESS_MINUS => Some("scene_setup.object.roughness_value"),
        _ => None,
    }
}

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

/// Stable automation name for one of the four fixed rows' value cell —
/// `scripts/ui-flows/` selectors key on this instead of ambiguous
/// `text`/`under_text` queries (see `build_numeric_row`'s call site).
const fn fixed_row_automation_name(row_index: u64) -> Option<&'static str> {
    match row_index {
        ROW_ENV_INTENSITY => Some("scene_setup.environment.intensity_value"),
        ROW_ENV_FILL => Some("scene_setup.environment.fill_value"),
        ROW_FOG_DENSITY => Some("scene_setup.fog.density_value"),
        ROW_FOG_HEIGHT_FALLOFF => Some("scene_setup.fog.height_falloff_value"),
        _ => None,
    }
}

const PANEL_W_MIN: f32 = 320.0;
const TITLE_H: f32 = 26.0;
const ROW_H: f32 = 24.0;
const ROW_GAP: f32 = 4.0;
const PAD: f32 = 10.0;
const STEP_W: f32 = 22.0;
const LABEL_W: f32 = 130.0;
const VALUE_W: f32 = 70.0;

/// A single editable node-param address: the exact `(scope_path,
/// node_doc_id, param_id)` triple `SetGraphNodeParamCommand::with_scope`
/// takes. `scope_path` is empty for every P1 row (Environment/Fog, and
/// Objects' root-level transform_3d rows) and `[group_node_id]` for a P2
/// Objects material/modifier row living inside the object's own group.
#[derive(Clone, Debug, PartialEq)]
pub struct RowAddr {
    pub scope_path: Vec<u32>,
    pub node_doc_id: u32,
    pub param_id: String,
}

impl RowAddr {
    pub fn root(node_doc_id: u32, param_id: &str) -> Self {
        Self { scope_path: Vec::new(), node_doc_id, param_id: param_id.to_string() }
    }
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

/// One `node.transform_3d`'s "3 compact triplets" (D4): Position/Rotation/
/// Scale, each X/Y/Z a [`RowValue`].
#[derive(Clone, Debug, PartialEq)]
pub struct TransformRowVm {
    pub pos: (RowValue, RowValue, RowValue),
    pub rot: (RowValue, RowValue, RowValue),
    pub scale: (RowValue, RowValue, RowValue),
}

/// The Objects section's material quick-knob row (D3/D4): base color always,
/// metallic/roughness only for `pbr_material` (phong/unlit/cel don't have
/// that param — "the atom's own params otherwise").
#[derive(Clone, Debug, PartialEq)]
pub enum ObjectMaterialVm {
    Pbr { color: (RowValue, RowValue, RowValue), metallic: RowValue, roughness: RowValue },
    Other { color: (RowValue, RowValue, RowValue) },
    /// No material resolved on this object.
    None,
}

/// Payload for [`ObjectRowVm::Known`], boxed so the enum's footprint tracks
/// the small `Custom` variant instead of this one (clippy
/// `large_enum_variant` — same convention as `LightRow`/`OrbitCameraRow` in
/// `scene_vm.rs`).
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectKnownRow {
    pub index: usize,
    pub group_node_id: u32,
    pub name: String,
    pub transform: Option<Box<TransformRowVm>>,
    pub material: ObjectMaterialVm,
    /// Display names only in P2 (the interactive stack — add/remove/
    /// reorder — is P5); an empty list still renders "no modifiers".
    pub modifier_names: Vec<String>,
}

/// One Objects-section row (D3/D4).
#[derive(Clone, Debug, PartialEq)]
pub enum ObjectRowVm {
    /// Producer resolved to a named group.
    Known(Box<ObjectKnownRow>),
    /// Producer did NOT resolve to a group output — "Object k — custom
    /// (edit in graph)" per D3.
    Custom { index: usize, transform: Option<Box<TransformRowVm>> },
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
    /// "Add fog"/"+ Object"/"+ Light" composites wire into.
    pub scene_root_node_id: u32,
    pub environment: EnvironmentRowVm,
    pub atmosphere: AtmosphereRowVm,
    /// P2: the Objects section's rows, in `mesh_k` order.
    pub objects: Vec<ObjectRowVm>,
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
    add_object_id: Option<NodeId>,
    add_light_id: Option<NodeId>,
    /// P2 Objects section fold state — UI-local (like card sections, not
    /// serialized), keyed by the object's stable `index` (0..objects; never
    /// reassigned by rename — only append/remove would renumber, and remove
    /// isn't wired this phase). Missing entry = expanded (the default).
    object_expanded: std::collections::HashMap<usize, bool>,
    /// Every Objects-row drag-armable value cell built this frame: triplet
    /// axes (pos/rot/scale/color) + the metallic/roughness value boxes.
    /// Rebuilt fresh every `build_nodes` call — Objects is a variable-length
    /// list, so (unlike the fixed `row_ids` above) there's no fixed index
    /// table to key by; PointerDown/Drag look the control up directly here.
    object_value_cells: Vec<(NodeId, RowValue)>,
    /// Every Objects-row stepper (+/-) built this frame, with its fixed step
    /// delta (mirrors `stepper_hit` for the fixed rows above).
    object_steppers: Vec<(NodeId, RowValue, f32)>,
    /// `(group_node_id, name_label_node_id, current_name)` for every Known
    /// object row this frame — resolves a name-label click to its rename
    /// action, and backs `object_name_rect` (the app's text-input anchor
    /// lookup).
    object_name_ids: Vec<(u32, NodeId, String)>,
    /// `(index, expand_toggle_node_id)` for every object row this frame.
    object_expand_ids: Vec<(usize, NodeId)>,
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
            add_object_id: None,
            add_light_id: None,
            object_expanded: std::collections::HashMap::new(),
            object_value_cells: Vec::new(),
            object_steppers: Vec::new(),
            object_name_ids: Vec::new(),
            object_expand_ids: Vec::new(),
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
        self.add_object_id = None;
        self.add_light_id = None;
        self.object_value_cells.clear();
        self.object_steppers.clear();
        self.object_name_ids.clear();
        self.object_expand_ids.clear();

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

        // ── Objects ──
        cy = self.build_objects_section(tree, inner_x, inner_w, cy, vm);
        cy += ROW_GAP;

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
        // A stable automation name (P2 fix): `text`-based selectors like
        // `{"text": "0.00", "under_text": "Fog"}` are ambiguous the moment
        // ANY other row in the panel also shows "0.00" — `under_text`
        // matches on a SHARED ancestor, not literal nesting (this panel has
        // no per-section container, every row is a flat sibling under the
        // same scroll-clip parent), so it can't disambiguate two rows that
        // both read "0.00" (the P2 Objects section's default transform/
        // color cells collided with this exact row once Objects started
        // rendering above Environment/Fog — BUG found+fixed this phase).
        // `scripts/ui-flows/scene-setup-add-fog-drag.json` was updated to
        // select by name instead.
        if let Some(name) = fixed_row_automation_name(row_index) {
            tree.set_name(value, name);
        }
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

    /// The Objects section (P2, D4): per-object collapsible rows, then
    /// "+ Object"/"+ Light" (Import Model… is P4).
    fn build_objects_section(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        vm: &SceneSetupVm,
    ) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Objects", section_label_style());
        cy += ROW_H;
        for obj in &vm.objects {
            cy = self.build_object_row(tree, inner_x, inner_w, cy, obj);
        }
        cy += ROW_GAP;
        let half_w = (inner_w - ROW_GAP) / 2.0;
        self.add_object_id = Some(tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            half_w,
            ROW_H,
            btn_style(),
            "+ Object",
            KEY_ADD_OBJECT,
        ));
        self.add_light_id = Some(tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + half_w + ROW_GAP,
            cy,
            half_w,
            ROW_H,
            btn_style(),
            "+ Light",
            KEY_ADD_LIGHT,
        ));
        cy + ROW_H
    }

    fn is_expanded(&self, index: usize) -> bool {
        *self.object_expanded.get(&index).unwrap_or(&true)
    }

    /// One Objects-section row: expand toggle + editable name (or the D3
    /// "custom" label) — then, when expanded, transform triplets, material
    /// quick knobs, and the (display-only in P2) modifier list.
    fn build_object_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        obj: &ObjectRowVm,
    ) -> f32 {
        let (index, group_node_id, name, transform, material, modifier_names) = match obj {
            ObjectRowVm::Known(row) => (
                row.index,
                Some(row.group_node_id),
                row.name.clone(),
                row.transform.clone(),
                row.material.clone(),
                row.modifier_names.clone(),
            ),
            ObjectRowVm::Custom { index, transform } => (
                *index,
                None,
                format!("Object {index} — custom (edit in graph)"),
                transform.clone(),
                ObjectMaterialVm::None,
                Vec::new(),
            ),
        };
        let expanded = self.is_expanded(index);

        let expand_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            if expanded { "\u{25BE}" } else { "\u{25B8}" },
            obj_key(index, OBJ_OFF_EXPAND),
        );
        self.object_expand_ids.push((index, expand_id));

        let name_x = inner_x + STEP_W + 4.0;
        let name_w = inner_w - STEP_W - 4.0;
        if let Some(group_node_id) = group_node_id {
            let name_id = tree.add_button_keyed(
                Some(self.content_parent),
                name_x,
                cy,
                name_w,
                ROW_H,
                drag_value_style(),
                &name,
                obj_key(index, OBJ_OFF_NAME),
            );
            self.object_name_ids.push((group_node_id, name_id, name.clone()));
        } else {
            tree.add_label(Some(self.content_parent), name_x, cy, name_w, ROW_H, &name, label_style());
        }
        cy += ROW_H;

        if !expanded {
            return cy + ROW_GAP;
        }

        let body_x = inner_x + PAD;
        let body_w = inner_w - PAD;
        if let Some(t) = &transform {
            cy = self.build_triplet_row(tree, body_x, body_w, cy, "Position", &t.pos, index, OBJ_OFF_POS_X);
            cy = self.build_triplet_row(tree, body_x, body_w, cy, "Rotation", &t.rot, index, OBJ_OFF_ROT_X);
            cy = self.build_triplet_row(tree, body_x, body_w, cy, "Scale", &t.scale, index, OBJ_OFF_SCALE_X);
        }
        match &material {
            ObjectMaterialVm::Pbr { color, metallic, roughness } => {
                cy = self.build_triplet_row(tree, body_x, body_w, cy, "Color", color, index, OBJ_OFF_COLOR_R);
                cy = self.build_object_numeric_row(
                    tree, body_x, body_w, cy, "Metallic", metallic, index, OBJ_OFF_METALLIC_MINUS,
                );
                cy = self.build_object_numeric_row(
                    tree, body_x, body_w, cy, "Roughness", roughness, index, OBJ_OFF_ROUGHNESS_MINUS,
                );
            }
            ObjectMaterialVm::Other { color } => {
                cy = self.build_triplet_row(tree, body_x, body_w, cy, "Color", color, index, OBJ_OFF_COLOR_R);
            }
            ObjectMaterialVm::None => {
                tree.add_label(Some(self.content_parent), body_x, cy, body_w, ROW_H, "No material", label_style());
                cy += ROW_H;
            }
        }
        let modifiers_line =
            if modifier_names.is_empty() { "Modifiers: none".to_string() } else { format!("Modifiers: {}", modifier_names.join(", ")) };
        tree.add_label(Some(self.content_parent), body_x, cy, body_w, ROW_H, &modifiers_line, label_style());
        cy += ROW_H + ROW_GAP;
        cy
    }

    /// A "3 compact triplet" row (D4): label + X/Y/Z drag-value cells, no
    /// steppers (Position/Rotation/Scale/Color all use this shape). Driven
    /// axes render read-only with the same styling `build_numeric_row` uses.
    fn build_triplet_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        triplet: &(RowValue, RowValue, RowValue),
        index: usize,
        base_offset: u64,
    ) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
        let cell_x = inner_x + LABEL_W;
        let cell_w = ((inner_w - LABEL_W) / 3.0 - 2.0).max(20.0);
        for (i, row) in [&triplet.0, &triplet.1, &triplet.2].into_iter().enumerate() {
            let x = cell_x + i as f32 * (cell_w + 2.0);
            if row.driven {
                tree.add_label(
                    Some(self.content_parent),
                    x,
                    cy,
                    cell_w,
                    ROW_H,
                    &format!("{:.2}\u{2022}", row.value),
                    driven_label_style(),
                );
                continue;
            }
            let cell_id = tree.add_button_keyed(
                Some(self.content_parent),
                x,
                cy,
                cell_w,
                ROW_H,
                drag_value_style(),
                &format!("{:.2}", row.value),
                obj_key(index, base_offset + i as u64),
            );
            // Stable automation name (same fix as `build_numeric_row`'s
            // fixed rows) — `nth` picks which object's cell a flow means,
            // per the audio dock's own `name` + `nth` convention.
            if let Some(name) = triplet_cell_automation_name(base_offset, i) {
                tree.set_name(cell_id, name);
            }
            self.object_value_cells.push((cell_id, row.clone()));
        }
        cy + ROW_H
    }

    /// One object-row `[label] [−] value [＋]` numeric row (metallic/
    /// roughness) — same shape as [`Self::build_numeric_row`], generalized
    /// to a dynamic per-object key range instead of the fixed `row_ids`
    /// table (Objects is a variable-length list).
    fn build_object_numeric_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        row: &RowValue,
        index: usize,
        base_offset: u64,
    ) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
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
            return cy + ROW_H;
        }
        let step_x = inner_x + inner_w - VALUE_W - STEP_W * 2.0;
        let minus_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{2212}",
            obj_key(index, base_offset),
        );
        let value_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W,
            cy,
            VALUE_W,
            ROW_H,
            drag_value_style(),
            &format!("{:.2}", row.value),
            obj_key(index, base_offset + 1),
        );
        if let Some(name) = object_numeric_row_automation_name(base_offset) {
            tree.set_name(value_id, name);
        }
        let plus_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W + VALUE_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{002B}",
            obj_key(index, base_offset + 2),
        );
        self.object_steppers.push((minus_id, row.clone(), -0.05));
        self.object_value_cells.push((value_id, row.clone()));
        self.object_steppers.push((plus_id, row.clone(), 0.05));
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
                // Expand/collapse toggles fold state only — no command, no
                // layer needed, valid even before a `Live` state exists.
                if let Some((index, _)) = self.object_expand_ids.iter().find(|(_, id)| *id == *node_id) {
                    let cur = self.is_expanded(*index);
                    self.object_expanded.insert(*index, !cur);
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
                    } else if self.add_object_id == Some(*node_id) {
                        actions.push(PanelAction::SceneSetupAddObject(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                            vm.object_count as u32,
                        ));
                    } else if self.add_light_id == Some(*node_id) {
                        actions.push(PanelAction::SceneSetupAddLight(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                            vm.light_count as u32,
                        ));
                    } else if let Some((group_node_id, _, current_name)) =
                        self.object_name_ids.iter().find(|(_, id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::SceneSetupRenameObjectClicked(
                            vm.layer_id.clone(),
                            *group_node_id,
                            current_name.clone(),
                        ));
                    } else if let Some((row_value, delta)) = self.object_stepper_hit(*node_id) {
                        let new_value = (row_value.value + delta).clamp(row_value.min, row_value.max);
                        actions.push(PanelAction::SceneSetupParamChanged(
                            vm.layer_id.clone(),
                            row_value.addr.scope_path.clone(),
                            row_value.addr.node_doc_id,
                            row_value.addr.param_id.clone(),
                            new_value,
                        ));
                    } else if let Some((row, delta)) = self.stepper_hit(*node_id)
                        && let Some(row_value) = self.row_value_for(vm, row)
                    {
                        let new_value = (row_value.value + delta).clamp(row_value.min, row_value.max);
                        actions.push(PanelAction::SceneSetupParamChanged(
                            vm.layer_id.clone(),
                            row_value.addr.scope_path.clone(),
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
                if let SceneSetupState::Live(vm) = &self.state {
                    if let Some(row) = self.value_label_row_at(*node_id)
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
                    if let Some((_, row_value)) =
                        self.object_value_cells.iter().find(|(id, _)| *id == *node_id)
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
                            drag.addr.scope_path.clone(),
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

    /// Objects-row stepper hit test — mirrors `stepper_hit` for the fixed
    /// rows, but the `RowValue` (and its clamp delta) is captured directly at
    /// build time instead of looked up from a fixed index (Objects is a
    /// variable-length list).
    fn object_stepper_hit(&self, node_id: NodeId) -> Option<(RowValue, f32)> {
        self.object_steppers
            .iter()
            .find(|(id, _, _)| *id == node_id)
            .map(|(_, row, delta)| (row.clone(), *delta))
    }

    /// The name label's rect for `group_node_id`, if a row for it was built
    /// this frame — the app's text-input anchor lookup (mirrors
    /// `AudioSetupPanel::send_label_rect`).
    pub fn object_name_rect(&self, tree: &UITree, group_node_id: u32) -> Option<Rect> {
        let (_, node_id, _) = self.object_name_ids.iter().find(|(gid, _, _)| *gid == group_node_id)?;
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
    use crate::input::Modifiers;

    fn env_row(value: f32) -> RowValue {
        RowValue { addr: RowAddr::root(3, "intensity"), value, min: 0.0, max: 4.0, driven: false }
    }

    fn triplet(node_doc_id: u32, x: f32, y: f32, z: f32, min: f32, max: f32) -> (RowValue, RowValue, RowValue) {
        (
            RowValue { addr: RowAddr::root(node_doc_id, "x"), value: x, min, max, driven: false },
            RowValue { addr: RowAddr::root(node_doc_id, "y"), value: y, min, max, driven: false },
            RowValue { addr: RowAddr::root(node_doc_id, "z"), value: z, min, max, driven: false },
        )
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
            objects: Vec::new(),
        })));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.add_environment_id.is_some());
        assert!(panel.add_fog_id.is_some());
        assert!(panel.add_object_id.is_some());
        assert!(panel.add_light_id.is_some());
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
            objects: Vec::new(),
        })));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.row_ids[ROW_ENV_INTENSITY as usize].minus.is_none());
    }

    /// A synthetic multi-object def (P2 gate): one Known "Azalea" object with
    /// a full transform + pbr material + a Bend modifier, one Custom object,
    /// and header counts — proves the Objects section renders both shapes,
    /// the rename click resolves to the right group node id, and the
    /// "+ Object"/"+ Light" buttons carry the Vm's own counts as
    /// `next_index`.
    fn azalea_shaped_vm() -> SceneSetupVm {
        SceneSetupVm {
            layer_id: LayerId::new("layer-1"),
            scene_name: "Scene".to_string(),
            multiple_scenes: false,
            object_count: 2,
            light_count: 1,
            shadow_caster_count: 1,
            scene_root_node_id: 99,
            environment: EnvironmentRowVm::None,
            atmosphere: AtmosphereRowVm::None,
            objects: vec![
                ObjectRowVm::Known(Box::new(ObjectKnownRow {
                    index: 0,
                    group_node_id: 42,
                    name: "Azalea".to_string(),
                    transform: Some(Box::new(TransformRowVm {
                        pos: triplet(50, 1.0, 2.0, 3.0, -100.0, 100.0),
                        rot: triplet(50, 0.0, 0.0, 0.0, -6.28, 6.28),
                        scale: triplet(50, 1.0, 1.0, 1.0, 0.01, 10.0),
                    })),
                    material: ObjectMaterialVm::Pbr {
                        color: triplet(51, 0.8, 0.8, 0.82, 0.0, 1.0),
                        metallic: RowValue { addr: RowAddr::root(51, "metallic"), value: 0.0, min: 0.0, max: 1.0, driven: false },
                        roughness: RowValue { addr: RowAddr::root(51, "roughness"), value: 0.5, min: 0.01, max: 1.0, driven: false },
                    },
                    modifier_names: vec!["Bend".to_string()],
                })),
                ObjectRowVm::Custom { index: 1, transform: None },
            ],
        }
    }

    #[test]
    fn objects_section_renders_known_and_custom_rows_with_counts() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // Both rows built (one expand toggle each), only the Known row has a
        // clickable name (the Custom row is a label, per D3).
        assert_eq!(panel.object_expand_ids.len(), 2, "both objects get an expand toggle");
        assert_eq!(panel.object_name_ids.len(), 1, "only the Known object has a renamable name");
        assert_eq!(panel.object_name_ids[0].0, 42, "resolves to the object's own group node id");
        assert_eq!(panel.object_name_ids[0].2, "Azalea");
        // The full expanded body: 3 transform triplets (9 cells) + 1 color
        // triplet (3 cells) for the Known row = 12 drag cells; metallic/
        // roughness add 2 more value cells (steppers tested separately).
        assert_eq!(panel.object_value_cells.len(), 14, "9 transform + 3 color + metallic + roughness value cells");
        assert!(panel.add_object_id.is_some());
        assert!(panel.add_light_id.is_some());
    }

    #[test]
    fn add_object_and_add_light_buttons_carry_the_vms_own_counts_as_next_index() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let add_object_id = panel.add_object_id.unwrap();
        let add_light_id = panel.add_light_id.unwrap();

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: add_object_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        });
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupAddObject(l, 99, 2) if *l == LayerId::new("layer-1")
        ));

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: add_light_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        });
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupAddLight(l, 99, 1) if *l == LayerId::new("layer-1")
        ));
    }

    #[test]
    fn clicking_the_object_name_emits_rename_clicked_with_group_node_id() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let name_id = panel.object_name_ids[0].1;

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: name_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        });
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupRenameObjectClicked(l, 42, n)
                if *l == LayerId::new("layer-1") && n == "Azalea"
        ));
    }

    #[test]
    fn collapsing_an_object_hides_its_body_rows() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let expand_id = panel.object_expand_ids[0].1;

        let (consumed, _) = panel.handle_event(&UIEvent::Click {
            node_id: expand_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        });
        assert!(consumed);
        assert!(!panel.is_expanded(0), "the toggle click flipped this object's fold state");

        let mut tree2 = UITree::new();
        panel.build_docked(&mut tree2, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.object_value_cells.is_empty(), "collapsed row 0 builds no body controls");
    }
}
