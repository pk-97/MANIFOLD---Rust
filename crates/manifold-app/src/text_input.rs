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
// Phase 2 wire-format rule (see `docs/BINDINGS_UNIFICATION_PLAN.md`): when
// these variants are revived, the per-param identifier must be `ParamId`,
// not positional `usize`. Today these arms still carry `usize` because
// they're dead; the handler in `app.rs` was left untouched. Converting
// in place will require dropping `Copy` from the enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum TextInputField {
    Bpm,
    Fps,
    LayerName(usize),
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
    /// Browser popup search filter — commit updates filter, no undo command.
    SearchFilter,
    /// Timeline marker rename. MarkerId stored in TextInputState::marker_id.
    MarkerName,
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
    /// Graph-editor find-a-node search. Commit / live filter highlights matching
    /// nodes on the canvas; no undo command.
    GraphNodeSearch,
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
        )
    }
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
    pub text: String,
    pub cursor: usize,
    /// Anchor rect in logical pixels — the overlay renders here.
    pub anchor: AnchorRect,
    /// Font size for the overlay text (logical pixels).
    pub font_size: f32,
    /// When true, the entire text is selected. First keystroke replaces all.
    /// Set on `begin()`, cleared on any edit action.
    pub select_all: bool,
    /// When true, Shift+Enter inserts a newline instead of committing.
    pub multiline: bool,
    /// MarkerId for MarkerName field (String not Copy, so stored separately).
    pub marker_id: Option<manifold_core::MarkerId>,
    /// Param name for `GraphStringParam` (String not `Copy`, so stored here).
    pub graph_param_name: Option<String>,
}

impl TextInputState {
    pub fn new() -> Self {
        Self {
            active: false,
            field: TextInputField::Bpm,
            text: String::new(),
            cursor: 0,
            anchor: AnchorRect::zero(),
            font_size: 12.0,
            select_all: false,
            multiline: false,
            marker_id: None,
            graph_param_name: None,
        }
    }

    /// Begin editing a field with an initial value.
    /// Auto-cancels any existing session (Unity: only one active at a time).
    pub fn begin(
        &mut self,
        field: TextInputField,
        initial: &str,
        anchor: AnchorRect,
        font_size: f32,
    ) {
        self.active = true;
        self.field = field;
        self.text = initial.to_string();
        self.cursor = self.text.len();
        self.anchor = anchor;
        self.font_size = font_size;
        self.select_all = true;
        self.multiline = matches!(
            field,
            TextInputField::GenStringParam(_) | TextInputField::GraphWgsl(_)
        );
    }

    /// Cancel editing without committing.
    pub fn cancel(&mut self) {
        self.active = false;
        self.text.clear();
        self.select_all = false;
        self.multiline = false;
    }

    /// Insert a character at the cursor position.
    /// If select_all is active, replaces all text first.
    pub fn insert_char(&mut self, c: char) {
        if self.select_all {
            self.text.clear();
            self.cursor = 0;
            self.select_all = false;
        }
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the character before the cursor (backspace).
    /// If select_all is active, clears all text.
    pub fn backspace(&mut self) {
        if self.select_all {
            self.text.clear();
            self.cursor = 0;
            self.select_all = false;
            return;
        }
        if self.cursor > 0 {
            let prev = self.text[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.text.remove(prev);
            self.cursor = prev;
        }
    }

    /// Delete the character after the cursor.
    /// If select_all is active, clears all text.
    pub fn delete(&mut self) {
        if self.select_all {
            self.text.clear();
            self.cursor = 0;
            self.select_all = false;
            return;
        }
        if self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }

    /// Commit the current text. Returns the field and final text.
    pub fn commit(&mut self) -> (TextInputField, String) {
        self.active = false;
        self.select_all = false;
        let field = self.field;
        let text = std::mem::take(&mut self.text);
        (field, text)
    }

    /// Move cursor left. Clears select_all.
    pub fn move_left(&mut self) {
        if self.select_all {
            self.cursor = 0;
            self.select_all = false;
            return;
        }
        if self.cursor > 0 {
            self.cursor = self.text[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    /// Move cursor right. Clears select_all.
    pub fn move_right(&mut self) {
        if self.select_all {
            self.cursor = self.text.len();
            self.select_all = false;
            return;
        }
        if self.cursor < self.text.len() {
            self.cursor += self.text[self.cursor..]
                .chars()
                .next()
                .map_or(0, |c| c.len_utf8());
        }
    }

    /// Select all text (Cmd+A / Ctrl+A).
    pub fn select_all_text(&mut self) {
        self.select_all = true;
        self.cursor = self.text.len();
    }
}

// ── Overlay rendering constants ───────────────────────────────────
// From Unity UGUITextInputHost styling.

/// Background color: dark panel matching transport chrome.
pub const TEXT_INPUT_BG: [f32; 4] = [0.14, 0.14, 0.15, 1.0];
/// Text color: light gray.
pub const TEXT_INPUT_FG: [u8; 4] = [224, 224, 224, 255];
/// Selection highlight (when select_all).
pub const TEXT_INPUT_SELECT_BG: [f32; 4] = [0.3, 0.5, 0.8, 0.4];
/// Cursor color.
pub const TEXT_INPUT_CURSOR: [f32; 4] = [0.88, 0.88, 0.88, 1.0];
/// Horizontal padding inside the text box.
pub const TEXT_INPUT_PAD_H: f32 = 4.0;
/// Vertical padding inside the text box.
pub const TEXT_INPUT_PAD_V: f32 = 2.0;
/// Cursor width in logical pixels.
pub const TEXT_INPUT_CURSOR_W: f32 = 1.0;
/// Cursor blink period (seconds per half-cycle).
pub const TEXT_INPUT_BLINK_PERIOD: f64 = 0.5;
