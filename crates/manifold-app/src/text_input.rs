//! Lightweight inline text input system.
//!
//! When active, keyboard events are intercepted for text editing.
//! The app layer renders a small text field overlay at the anchor position.
//! Enter commits, Escape cancels.
//!
//! Port of Unity BitmapTextInput -- a session-based coordinator between UI
//! callers and the text field renderer. Only ONE session active at a time;
//! `begin()` auto-cancels any existing session (matches Unity behavior).

/// What kind of field is being edited.
// FIXME(dead-code-audit): EffectParam/GroupRename/GenParam are matched on in app.rs
// but no path constructs them — begin() callers don't reach these branches.
//
// Phase 2 wire-format rule (see `docs/archive/BINDINGS_UNIFICATION_PLAN.md`): when
// these variants are revived, the per-param identifier must be `ParamId`,
// not positional `usize`. Today these arms still carry `usize` because
// they're dead; the handler in `app.rs` was left untouched. Converting
// in place will require dropping `Copy` from the enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TextInputField {
    Bpm,
    Fps,
    /// Layer rename. LayerId stored in `TextInputState::layer_id` (not `Copy`,
    /// so kept off this `Copy` enum — mirrors `MarkerName`/`AudioSendLabel`).
    /// Was index-based (BUG-031): a layer-list change between double-click and
    /// commit could rename the wrong row.
    LayerName,
    ClipBpm,
    MacroLabel(usize),
    /// Effect parameter: (effect_index, param_index). DEAD. On revival,
    /// switch to `(effect_index, ParamId)` per the Phase 2 contract or
    /// the user-tier slots will silently no-op on commit.
    EffectParam(usize, usize),
    /// Effect group rename: group index.
    GroupRename(usize),
    /// Generator parameter: param_index. DEAD. On revival, switch to
    /// `ParamId` per the Phase 2 contract.
    GenParam(usize),
    /// Generator string parameter: (string_param_index).
    GenStringParam(usize),
    /// Inspector param value type-in (effect / generator), opened by a
    /// double-click on the value cell. The target + id + clamp range ride on
    /// [`TextInputState::inspector_param`] (carries a non-`Copy` `ParamId`).
    /// Commit parses the f32, clamps, and dispatches `ParamChanged` + `ParamCommit`.
    InspectorParam,
    /// Driver (LFO) free-period type-in, opened by a click on the drawer's Free
    /// field. The target + id ride on [`TextInputState::driver_free_period`].
    /// Commit parses the beats f32 and dispatches `DriverConfig(SetFreePeriod)`.
    DriverFreePeriod,
    /// Browser popup search filter — commit updates filter, no undo command.
    SearchFilter,
    /// Timeline marker rename. MarkerId stored in TextInputState::marker_id.
    MarkerName,
    /// Audio send rename. AudioSendId stored in TextInputState::audio_send_id
    /// (Arc<str>, not Copy, so kept off this Copy enum).
    AudioSendLabel,
    /// Graph-editor group rename. Carries the group's runtime node id; the scope
    /// is read from the canvas at commit time. Routes to `RenameGroupCommand`.
    GraphGroupRename(u32),
    /// Graph-editor String node param (e.g. `render_text.text`). Carries the
    /// node's runtime id; the param name is in `TextInputState::graph_param_name`
    /// (String, not `Copy`). Routes to `SetGraphNodeParam(String)`.
    GraphStringParam(u32),
    /// Graph-editor `wgsl_compute` source. Carries the node's runtime id; the
    /// source edits multiline. Routes to `SetWgslSourceCommand`.
    GraphWgsl(u32),
    /// Graph-editor ranged-param numeric type-in, opened by a double-click on
    /// a node-face value box (UI_WIDGET_UNIFICATION P5d — the contract's
    /// `(ValueCell, DoubleClick) -> EditValue` row's last dead stop). Carries
    /// the node's runtime id; the rest (param name, clamp range,
    /// `outer_param_id`) rides on [`TextInputState::graph_numeric_param`]
    /// (not `Copy`). Commit parses, clamps, and dispatches
    /// `SetGraphNodeParam` — or `SetOuterParam`'s own write path for a
    /// group-face mirror row (D4/D6 parity).
    GraphNumericParam(u32),
    /// Graph-editor find-a-node search. Commit / live filter highlights matching
    /// nodes on the canvas; no undo command.
    GraphNodeSearch,
    /// Graph-editor `Table` param cell. The cell coordinate + full table ride on
    /// `TextInputState::graph_table_edit`; commit parses the new f32, rebuilds the
    /// one cell, and routes to `SetGraphNodeParam(Table)`.
    GraphTableCell,
    /// Save to Library / Save to Project name prompt (PRESET_LIBRARY_DESIGN
    /// D4, P3) — one field for both destinations (which one rides on
    /// `TextInputState::save_preset`, since `EffectGraphDef` isn't `Copy`).
    /// Opened from the card context menu AND the graph editor header; commit
    /// either calls `UserLibrary::save` directly (Library) or executes
    /// `SaveToProjectCommand` (Project).
    SavePresetName,
    /// Browser management-menu Rename prompt (PRESET_LIBRARY_DESIGN P5, D6) —
    /// one field for both sources (which one rides on
    /// `TextInputState::rename_preset`, since `PresetTypeId` isn't `Copy`).
    /// Opened from the browser's right-click menu; commit calls
    /// `UserLibrary::rename` (My Library) or executes
    /// `RenameEmbeddedPresetCommand` (Project).
    RenamePreset,
    /// Scene Setup panel Objects-row rename (SCENE_SETUP_PANEL_DESIGN.md P2).
    /// Carries the object's group node id; the target layer rides on
    /// `TextInputState::scene_object_layer_id` (`LayerId` not `Copy`). Commit
    /// routes to `RenameGroupCommand`, addressed at the layer directly (no
    /// graph editor needs to be open — the panel is a fourth surface).
    SceneObjectRename(u32),
    /// Scene Setup panel light-row rename (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md
    /// P5). Carries the light node's own doc id; the target layer rides on
    /// `TextInputState::scene_object_layer_id` (shared with
    /// `SceneObjectRename` — the two sessions are mutually exclusive, only
    /// one text edit is ever in flight at a time). Commit dispatches the
    /// plain `SetNodeHandleCommand` (no group sweep — a light is never
    /// wrapped in a group).
    SceneLightRename(u32),
    /// Scene Setup dock numeric value-cell type-in
    /// (`SCENE_OBJECT_AND_PANEL_V2_DESIGN.md` P4, D8's `EditValue` target).
    /// Carries the target node's doc id (mirrors `GraphNumericParam`'s
    /// shape); the rest (layer, scope_path, param_id, D10's `degrees` flag)
    /// rides on [`TextInputState::scene_numeric_param`] (not `Copy`). Commit
    /// parses f32 (degrees rows convert to radians), dispatches the SAME
    /// `PanelAction::SceneSetupParamChanged` write the dock's drag/steppers
    /// already use — ONE undo unit, no clamp (PARAM_RANGE_CONTRACT P1).
    SceneNumericParam(u32),
    /// Audio Setup dock gain-stepper value-cell type-in (P4's audio-dock
    /// sibling of `SceneNumericParam`, D8). Carries nothing on the `Copy`
    /// enum (`AudioSendId` isn't `Copy`); the send id rides on
    /// [`TextInputState::audio_send_gain_param`]. Commit parses f32,
    /// dispatches `SetAudioSendGainCommand` directly — no clamp (the
    /// `AudioSendGainDragChanged` action the live drag uses DOES clamp to
    /// the trim range, which type-in must not).
    AudioSendGainParam,
}

impl TextInputField {
    /// Whether this field is edited inside the graph-editor window (so the
    /// editor's key handler + overlay render own it, not the main window's).
    pub fn is_graph_field(self) -> bool {
        matches!(
            self,
            TextInputField::GraphGroupRename(_)
                | TextInputField::GraphStringParam(_)
                | TextInputField::GraphWgsl(_)
                | TextInputField::GraphNodeSearch
                | TextInputField::GraphTableCell
                | TextInputField::GraphNumericParam(_)
        )
    }
}

/// In-flight edit context for a `Table` cell — everything the commit needs to
/// rebuild the one edited cell back into a full `Table` value. Set when the
/// cell editor opens (`TextInputField::GraphTableCell`), consumed on commit.
#[derive(Debug, Clone)]
pub struct TableCellEdit {
    pub node_id: u32,
    pub param_name: String,
    pub row: usize,
    pub col: usize,
    /// The full table at edit-open time, row-major.
    pub rows: Vec<Vec<f32>>,
}

/// Context for an in-flight inspector param type-in — set when the box opens
/// ([`TextInputField::InspectorParam`]) and read on commit. Lives off the `Copy`
/// field enum because `ParamId` isn't `Copy`. No `min`/`max` here (removed
/// PARAM_RANGE_CONTRACT_DESIGN.md P1): the commit path no longer clamps a
/// typed value to the param's display hint, so nothing reads them.
#[derive(Debug, Clone)]
pub struct InspectorParamCtx {
    pub target: manifold_ui::panels::GraphParamTarget,
    pub param_id: manifold_core::effects::ParamId,
    /// Base value at open — the undo "from" value and the slider's set point.
    pub old_value: f32,
    pub whole_numbers: bool,
}

/// Context for an in-flight driver Free-period type-in — set when the box opens
/// ([`TextInputField::DriverFreePeriod`]) and read on commit. Lives off the
/// `Copy` field enum because `ParamId` isn't `Copy`.
#[derive(Debug, Clone)]
pub struct DriverFreePeriodCtx {
    pub target: manifold_ui::panels::GraphParamTarget,
    pub param_id: manifold_core::effects::ParamId,
}

/// Context for an in-flight graph-canvas numeric type-in — set when the box
/// opens ([`TextInputField::GraphNumericParam`]) and read on commit. Lives
/// off the `Copy` field enum because `param_name`/`outer_param_id` aren't
/// `Copy` (P5d).
#[derive(Debug, Clone)]
pub struct GraphNumericParamCtx {
    pub param_name: String,
    pub min: f32,
    pub max: f32,
    pub whole_numbers: bool,
    /// `Some(outer_param_id)` when this row is a group-face mirror (D6):
    /// commit must write through the outer card param's own path
    /// (`SetOuterParam`), never `SetGraphNodeParam` on the inner node.
    pub outer_param_id: Option<String>,
}

/// Context for an in-flight Scene Setup dock numeric type-in — set when the
/// box opens ([`TextInputField::SceneNumericParam`]) and read on commit
/// (`SCENE_OBJECT_AND_PANEL_V2_DESIGN.md` P4, D8). Mirrors `InspectorParamCtx`'s
/// shape; the write address is the dock's own `(scope_path, node_doc_id,
/// param_id)` (`PanelAction::SceneSetupParamChanged`'s tuple), not a
/// `ParamId` — the dock addresses graph nodes directly, not effect/generator
/// param slots. No `old_value` here (unlike `InspectorParamCtx`): commit
/// reads the live project value as the undo baseline directly — nothing else
/// can write this field while the single-session type-in box is open.
#[derive(Debug, Clone)]
pub struct SceneNumericParamCtx {
    pub layer_id: manifold_core::LayerId,
    pub scope_path: Vec<u32>,
    pub param_id: String,
    /// D10: true for the committed degrees-display row table
    /// (`transform_3d.rot_*`, `orbit_camera.orbit`/`tilt`, `free_camera`'s
    /// euler triplet, `fov_y` on all three camera atoms) — commit converts
    /// the typed degrees to radians before dispatch. Storage/model stay
    /// radians; this is the ONLY place that conversion happens.
    pub degrees: bool,
}

/// Context for an in-flight Audio Setup dock gain-stepper type-in
/// ([`TextInputField::AudioSendGainParam`]), read on commit.
#[derive(Debug, Clone)]
pub struct AudioSendGainParamCtx {
    pub send_id: manifold_core::AudioSendId,
}

/// Which library door a [`TextInputField::SavePresetName`] session is headed
/// for — set alongside [`SavePresetCtx`] when the session opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavePresetDestination {
    /// Write a new file under the user's library folder (`UserLibrary::save`).
    Library,
    /// Upsert a new `origin: Saved` project-embedded preset
    /// (`SaveToProjectCommand`), without retargeting any instance.
    Project,
}

/// Context for an in-flight Save to Library / Save to Project name prompt —
/// set when the box opens ([`TextInputField::SavePresetName`]) and read on
/// commit. Lives off the `Copy` field enum because `EffectGraphDef` isn't
/// `Copy`. The def is the instance's CURRENT effective definition, already
/// resolved (diverged `graph` if `Some`, else the catalog default) and
/// values-snapshotted at the point the prompt opened — the same
/// `preset_source_def` resolution Export/Make Unique use.
#[derive(Debug, Clone)]
pub struct SavePresetCtx {
    pub kind: manifold_core::preset_def::PresetKind,
    pub def: manifold_core::effect_graph_def::EffectGraphDef,
    pub destination: SavePresetDestination,
}

/// Context for an in-flight browser Rename prompt — set when the box opens
/// ([`TextInputField::RenamePreset`]) and read on commit. Lives off the
/// `Copy` field enum because `PresetTypeId` isn't `Copy`.
#[derive(Debug, Clone)]
pub struct RenamePresetCtx {
    pub kind: manifold_core::preset_def::PresetKind,
    pub id: manifold_core::PresetTypeId,
    pub source: manifold_ui::panels::picker_core::Source,
}

/// Which overlay (and in which window) an in-flight text session belongs to —
/// `OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` §3, D2. Set by `begin_owned`;
/// `None` means the field is panel-owned (BPM, layer name, etc.), not hosted
/// inside an overlay. The app's overlay pump drains `UIRoot::take_closed_overlays`
/// once per frame per window and calls `cancel_if_owned_by` for each closed id,
/// so a session can never outlive the overlay that hosts its field — closing
/// the class of bug where a popup's search text stuck around after the popup
/// closed (Peter, 2026-07-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextSessionOwner {
    /// An overlay on the main window's `UIRoot` (`self.ws.ui_root`).
    MainOverlay(crate::ui_root::OverlayId),
    /// An overlay on the graph-editor window's `UIRoot` (`ed.ui_root`).
    EditorOverlay(crate::ui_root::OverlayId),
}

/// Screen-space rectangle for anchoring the text input overlay.
#[derive(Debug, Clone, Copy)]
pub struct AnchorRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl AnchorRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            x,
            y,
            width: w,
            height: h,
        }
    }

    pub fn zero() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        }
    }
}

/// Active text input session.
pub struct TextInputState {
    pub active: bool,
    pub field: TextInputField,
    /// The editing model (UI_WIDGET_UNIFICATION P5b, D16) — caret, selection,
    /// and text mechanics, shared with `MappingPopover` (P5c) and the canvas
    /// numeric type-in (P5d). This struct keeps its own field/session/anchor
    /// bookkeeping; the model owns only the buffer.
    pub model: manifold_ui::text_edit::TextEditModel,
    /// The text at `begin()` — Cmd+Z in-session reverts to this (D16: single-
    /// level, never touches the app undo stack). Cleared on cancel/commit.
    seed: String,
    /// Anchor rect in logical pixels — the overlay renders here.
    pub anchor: AnchorRect,
    /// Font size for the overlay text (logical pixels).
    pub font_size: f32,
    /// When true, Shift+Enter inserts a newline instead of committing.
    pub multiline: bool,
    /// LayerId for the LayerName field (not Copy, so stored separately).
    pub layer_id: Option<manifold_core::LayerId>,
    /// MarkerId for MarkerName field (String not Copy, so stored separately).
    pub marker_id: Option<manifold_core::MarkerId>,
    /// AudioSendId for the AudioSendLabel field (Arc<str> not Copy).
    pub audio_send_id: Option<manifold_core::AudioSendId>,
    /// LayerId for the `SceneObjectRename` field (not `Copy`, so stored
    /// separately — mirrors `layer_id`/`audio_send_id`).
    pub scene_object_layer_id: Option<manifold_core::LayerId>,
    /// Param name for `GraphStringParam` (String not `Copy`, so stored here).
    pub graph_param_name: Option<String>,
    /// Cell context for `GraphTableCell` (carries the full table, so stored
    /// here rather than on the `Copy` field enum).
    pub graph_table_edit: Option<TableCellEdit>,
    /// Context for `InspectorParam` (target + id + clamp range; `ParamId` is
    /// not `Copy`). Set right after `begin()` by the app, read on commit.
    pub inspector_param: Option<InspectorParamCtx>,
    /// Context for `DriverFreePeriod` (target + id). Set right after `begin()`,
    /// read on commit.
    pub driver_free_period: Option<DriverFreePeriodCtx>,
    /// Context for `GraphNumericParam` (param name + clamp range +
    /// `outer_param_id`). Set right after `begin()`, read on commit (P5d).
    pub graph_numeric_param: Option<GraphNumericParamCtx>,
    /// Context for `SceneNumericParam` (layer + scope_path + param_id +
    /// D10's `degrees` flag). Set right after `begin()`, read on commit (P4).
    pub scene_numeric_param: Option<SceneNumericParamCtx>,
    /// Context for `AudioSendGainParam` (the send id). Set right after
    /// `begin()`, read on commit (P4).
    pub audio_send_gain_param: Option<AudioSendGainParamCtx>,
    /// Context for `SavePresetName` (kind + effective def + destination). Set
    /// right after `begin()`, read (and taken) on commit.
    pub save_preset: Option<SavePresetCtx>,
    /// Context for `RenamePreset` (kind + id + source). Set right after
    /// `begin()`, read (and taken) on commit.
    pub rename_preset: Option<RenamePresetCtx>,
    /// True between a mouse press inside the field and its release (P5b) —
    /// tells `on_pointer_move` to extend the selection via `drag_to` rather
    /// than ignore cursor motion. Cleared on release, cancel, and commit.
    pub(crate) dragging: bool,
    /// Time + position (logical px) of the previous press inside this field
    /// (P5b mouse double-click → select-word), compared against `color.rs`'s
    /// single-sourced double-click constants (I8). `None` after begin/cancel/
    /// commit — a double-click can't span two different sessions.
    pub(crate) last_press: Option<(f32, f32, f32)>,
    /// The overlay hosting this session, if any — `None` for panel-owned
    /// fields. Set by [`Self::begin_owned`], cleared by [`Self::begin`]
    /// (a raw `begin` is always panel-owned) and [`Self::cancel`]/[`Self::commit`].
    pub owner: Option<TextSessionOwner>,
}

impl TextInputState {
    pub fn new() -> Self {
        Self {
            active: false,
            field: TextInputField::Bpm,
            model: manifold_ui::text_edit::TextEditModel::new(""),
            seed: String::new(),
            anchor: AnchorRect::zero(),
            font_size: 12.0,
            multiline: false,
            layer_id: None,
            marker_id: None,
            audio_send_id: None,
            scene_object_layer_id: None,
            graph_param_name: None,
            graph_table_edit: None,
            inspector_param: None,
            driver_free_period: None,
            graph_numeric_param: None,
            scene_numeric_param: None,
            audio_send_gain_param: None,
            save_preset: None,
            rename_preset: None,
            dragging: false,
            last_press: None,
            owner: None,
        }
    }

    /// D4 (`EDITOR_WINDOW_UNIFICATION_DESIGN.md`): whether this session's
    /// TOOLTIP-depth overlay is owned by the graph-editor window's shared
    /// tree-overlay pass (`crate::tree_passes::render_tree_overlay_passes`).
    /// `active` gates whether there's a session to draw at all;
    /// `field.is_graph_field()` decides which window's pass draws it — the
    /// two predicates are mutually exclusive and jointly exhaustive over
    /// `active` sessions, so exactly one of `is_owned_by_editor`/
    /// `is_owned_by_main` is true whenever `active`.
    pub fn is_owned_by_editor(&self) -> bool {
        self.active && self.field.is_graph_field()
    }

    /// D4 sibling of [`Self::is_owned_by_editor`] — the main window's
    /// ownership test. Fixes the latent double-render this formalization
    /// replaces: pre-D4, the main window's overlay pass drew ANY active
    /// session (including graph fields it doesn't own) gated only on
    /// `active`, so a graph-editor text session rendered into the main
    /// window too whenever both windows happened to be composited the same
    /// frame.
    pub fn is_owned_by_main(&self) -> bool {
        self.active && !self.field.is_graph_field()
    }

    /// Begin editing a field with an initial value.
    /// Auto-cancels any existing session (Unity: only one active at a time).
    /// Always panel-owned (`owner` cleared) — use [`Self::begin_owned`] for a
    /// field hosted inside an overlay, so its session gets cancelled when the
    /// overlay closes.
    pub fn begin(
        &mut self,
        field: TextInputField,
        initial: &str,
        anchor: AnchorRect,
        font_size: f32,
    ) {
        self.active = true;
        self.field = field;
        self.model = manifold_ui::text_edit::TextEditModel::new(initial);
        self.model.select_all(); // first keystroke replaces everything (today's behavior, now a real selection)
        self.seed = initial.to_string();
        self.anchor = anchor;
        self.font_size = font_size;
        self.multiline = matches!(
            field,
            TextInputField::GenStringParam(_) | TextInputField::GraphWgsl(_)
        );
        // Stale param ctx from a prior session must not leak in; the caller sets
        // it again immediately for an `InspectorParam` / `DriverFreePeriod` /
        // `GraphNumericParam` / `SavePresetName` / `RenamePreset` field.
        self.inspector_param = None;
        self.driver_free_period = None;
        self.graph_numeric_param = None;
        self.scene_numeric_param = None;
        self.audio_send_gain_param = None;
        self.save_preset = None;
        self.rename_preset = None;
        self.dragging = false;
        self.last_press = None;
        self.owner = None;
    }

    /// `begin()` + tag the session with the overlay hosting it
    /// (`OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` §3, D2). The app's overlay
    /// pump cancels this session via [`Self::cancel_if_owned_by`] when
    /// `owner`'s overlay closes, so the field can't outlive its host.
    pub fn begin_owned(
        &mut self,
        owner: TextSessionOwner,
        field: TextInputField,
        initial: &str,
        anchor: AnchorRect,
        font_size: f32,
    ) {
        self.begin(field, initial, anchor, font_size);
        self.owner = Some(owner);
    }

    /// Cancel iff the active session is owned by `owner` — called by the
    /// app's overlay pump when `owner`'s overlay just closed. A no-op if the
    /// active session belongs to a different overlay (or is panel-owned), so
    /// draining closed overlays for one window never disturbs a session that
    /// belongs to the other.
    pub fn cancel_if_owned_by(&mut self, owner: TextSessionOwner) {
        if self.owner == Some(owner) {
            self.cancel();
        }
    }

    /// Cancel editing without committing.
    pub fn cancel(&mut self) {
        self.active = false;
        self.model = manifold_ui::text_edit::TextEditModel::new("");
        self.seed.clear();
        self.multiline = false;
        // Drop any per-session graph edit context so a cancelled cell/param
        // edit can't be mistaken for a later one.
        self.graph_param_name = None;
        self.graph_table_edit = None;
        self.inspector_param = None;
        self.driver_free_period = None;
        self.graph_numeric_param = None;
        self.scene_numeric_param = None;
        self.audio_send_gain_param = None;
        self.save_preset = None;
        self.rename_preset = None;
        self.dragging = false;
        self.last_press = None;
        self.owner = None;
    }

    /// The live text (delegates to the model — I7's single editing home).
    pub fn text(&self) -> &str {
        self.model.text()
    }

    /// Insert a character at the caret, replacing the selection if any
    /// (typing replaces the selection, D16).
    pub fn insert_char(&mut self, c: char) {
        self.model.insert_char(c);
    }

    /// Delete the selection, or (no selection) the char before the caret.
    pub fn backspace(&mut self) {
        self.model.backspace();
    }

    /// Delete the selection, or (no selection) the char after the caret.
    pub fn delete(&mut self) {
        self.model.delete();
    }

    /// Places the caret at `byte` (a click); `extend` (shift-click) grows
    /// the selection instead of collapsing it.
    pub fn caret_to(&mut self, byte: usize, extend: bool) {
        self.model.caret_to(byte, extend);
    }

    /// Moves the caret to `byte` during a mouse drag, anchor held at the
    /// press position.
    pub fn drag_to(&mut self, byte: usize) {
        self.model.drag_to(byte);
    }

    /// Selects the word touching `byte` (mouse double-click).
    pub fn select_word_at(&mut self, byte: usize) {
        self.model.select_word_at(byte);
    }

    /// Commit the current text. Returns the field and final text.
    pub fn commit(&mut self) -> (TextInputField, String) {
        self.active = false;
        self.dragging = false;
        self.last_press = None;
        self.owner = None;
        let field = self.field;
        let text = self.model.take_text();
        self.seed.clear();
        (field, text)
    }

    /// Move the caret left. `select` extends (Shift), `word` steps a whole
    /// word (Option) instead of one char — standard macOS bindings.
    pub fn move_left(&mut self, select: bool, word: bool) {
        self.model.move_left(select, word);
    }

    /// Move the caret right. `select` extends (Shift), `word` steps a whole
    /// word (Option) instead of one char.
    pub fn move_right(&mut self, select: bool, word: bool) {
        self.model.move_right(select, word);
    }

    /// Move the caret to the start of the text (Cmd+Left). `select` extends.
    pub fn move_home(&mut self, select: bool) {
        self.model.move_home(select);
    }

    /// Move the caret to the end of the text (Cmd+Right). `select` extends.
    pub fn move_end(&mut self, select: bool) {
        self.model.move_end(select);
    }

    /// Select all text (Cmd+A / Ctrl+A).
    pub fn select_all_text(&mut self) {
        self.model.select_all();
    }

    /// In-session Cmd+Z (D16): reverts the buffer to its seed text — the
    /// text at `begin()` — single-level, never touches the app undo stack.
    /// A no-op if there's no active session (nothing to revert).
    pub fn undo_to_seed(&mut self) {
        if !self.active {
            return;
        }
        self.model = manifold_ui::text_edit::TextEditModel::new(&self.seed);
    }

    /// Copies the current selection (empty string if none) — Cmd+C.
    pub fn copy_selection(&self) -> String {
        self.model.selected_text().to_string()
    }

    /// Cuts the current selection (empty string if none), removing it from
    /// the buffer — Cmd+X.
    pub fn cut_selection(&mut self) -> String {
        let s = self.model.selected_text().to_string();
        if !s.is_empty() {
            self.model.backspace(); // selection non-empty: this deletes exactly the selection
        }
        s
    }

    /// Pastes `s` at the caret, replacing the selection if any — Cmd+V.
    /// Single-line fields strip newlines (a paste can't smuggle in a
    /// multiline edit where the field has no way to render one).
    pub fn paste(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.multiline {
            self.model.insert_str(s);
        } else {
            let flat: String = s.chars().filter(|&c| c != '\n' && c != '\r').collect();
            self.model.insert_str(&flat);
        }
    }
}

// ── Overlay rendering constants ───────────────────────────────────
// From Unity UGUITextInputHost styling.

/// Background color: dark panel matching transport chrome. sRGB (was authored
/// as `[0.14, 0.14, 0.15, 1.0]`; the draw API now converts sRGB → linear once).
pub const TEXT_INPUT_BG: manifold_ui::Color32 = manifold_ui::Color32::new(36, 36, 38, 255);
/// Text color: light gray.
pub const TEXT_INPUT_FG: [u8; 4] = [224, 224, 224, 255];
/// Selection highlight (when select_all). sRGB (was `[0.3, 0.5, 0.8, 0.4]`).
pub const TEXT_INPUT_SELECT_BG: manifold_ui::Color32 = manifold_ui::Color32::new(77, 128, 204, 102);
/// Cursor color. sRGB (was `[0.88, 0.88, 0.88, 1.0]`).
pub const TEXT_INPUT_CURSOR: manifold_ui::Color32 = manifold_ui::Color32::new(224, 224, 224, 255);
/// Horizontal padding inside the text box.
pub const TEXT_INPUT_PAD_H: f32 = 4.0;
/// Vertical padding inside the text box.
pub const TEXT_INPUT_PAD_V: f32 = 2.0;
/// Cursor width in logical pixels.
pub const TEXT_INPUT_CURSOR_W: f32 = 1.0;
/// Cursor blink period (seconds per half-cycle).
pub const TEXT_INPUT_BLINK_PERIOD: f64 = 0.5;

/// Lenient numeric parse shared by every type-in commit path (`InspectorParam`,
/// `SceneNumericParam`, `AudioSendGainParam`): keep only the leading numeric
/// head, so a value typed with a trailing unit or stray character (e.g. an
/// angle "45°") still commits instead of silently no-op'ing. Pure — factored
/// out of the commit match arms so the parse itself is unit-testable without
/// constructing a whole `Application` (`SCENE_OBJECT_AND_PANEL_V2_DESIGN.md`
/// P4's BUG-198 note: type-in gates at L2 + unit, never a faked L3).
pub fn parse_lenient_numeric(text: &str) -> Option<f32> {
    let cleaned: String = text
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
        .collect();
    cleaned.parse::<f32>().ok()
}

/// D10's degrees→radians commit-side conversion — the panel boundary's other
/// half (the display side lives in `scene_setup_panel.rs`'s
/// `is_degrees_param` + triplet/camera-row formatters). Pure passthrough
/// when `degrees` is false, so every non-angle `SceneNumericParam` commit
/// takes the identical path it always did.
pub fn scene_numeric_commit_value(parsed: f32, degrees: bool) -> f32 {
    if degrees { parsed.to_radians() } else { parsed }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn lenient_parse_keeps_only_the_numeric_head() {
        assert_eq!(parse_lenient_numeric("3.5"), Some(3.5));
        assert_eq!(parse_lenient_numeric("45°"), Some(45.0));
        assert_eq!(parse_lenient_numeric("  -0.42  "), Some(-0.42));
        assert_eq!(parse_lenient_numeric("not a number"), None);
    }

    /// P4, D10: typing "45" into a degrees row must land at π/4 rad
    /// (0.7853981), tolerance-checked — the commit-side half of the
    /// degrees-display boundary (BUG-198's L2 + unit gate for type-in).
    #[test]
    fn degrees_row_commit_converts_to_radians() {
        let parsed = parse_lenient_numeric("45").expect("\"45\" parses");
        let radians = scene_numeric_commit_value(parsed, true);
        assert!(
            (radians - std::f32::consts::FRAC_PI_4).abs() < 1e-5,
            "got {radians}"
        );
    }

    /// A non-degrees row's commit value is untouched by the conversion.
    #[test]
    fn non_degrees_row_commit_passes_through() {
        let parsed = parse_lenient_numeric("0.42").expect("\"0.42\" parses");
        assert_eq!(scene_numeric_commit_value(parsed, false), 0.42);
    }
}
