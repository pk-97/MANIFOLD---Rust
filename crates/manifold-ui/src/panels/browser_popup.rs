//! Grid-based browser popup for effect/generator selection.
//!
//! A floating modal with search bar, category chips, scrollable grid,
//! and optional paste button. Completely separate from DropdownPanel —
//! different layout, interaction, and rendering model.
//!
//! `OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` section 3/section 4 (P1+P2): per-open state is a
//! [`BrowserSession`] constructed whole by [`BrowserPopupPanel::open`] and
//! dropped whole by [`BrowserPopupPanel::close`] — no field-by-field reset
//! list to keep in sync as fields get added. Filtering, category-chip
//! bookkeeping, and keyboard nav are owned by the shared `PickerCore`
//! (`picker_core.rs`); this file keeps session lifecycle plus grid/chip
//! rendering and click routing (drawing stays per-surface — see picker_core's
//! module doc).
//!
//! PRESET_BROWSER_AUDITION P3 (D12/D14, F3/F7-F13): the popup sizes to
//! content and screen — width derives from the item count (clamped to the
//! screen, capped at [`MAX_COLUMNS`] columns of 16:9 cells), height is
//! content-sized under the screen as the ONLY cap, and the grid scrolls
//! internally beyond that. In a thumbnail grid every cell is the 16:9
//! thumbnail plus a caption block below it, word-wrapped to the cell width;
//! the block reserves as many lines as the longest name needs. Chips are
//! measured with the tree's font metrics and wrap instead of overflowing.
//! Keyboard nav moves in grid geometry with scroll reveal; the wheel only
//! scrolls the grid when it's over the grid.

use crate::{BrowserAction, ParamsAction, ProjectAction};
use super::InspectorTab;
use super::PanelAction;
use super::overlay::{Anchor, Modality, Overlay, OverlayPlacement, OverlayResponse};
use super::picker_core::{PickerCore, PickerItem, PickerNav, Source};
use super::popup_shell;
use crate::color;
use crate::input::{Key, UIEvent};
use crate::node::Color32;
use crate::node::*;
use crate::tree::UITree;
use manifold_foundation::LayerId;

// ── Layout constants ──
//
// P3 (PRESET_BROWSER_AUDITION_DESIGN D12) replaces the Unity-era fixed
// POPUP_WIDTH 600 / CELL 185×42.5 / POPUP_MAX_HEIGHT 550. Cells are true
// 16:9 — the committed thumbnails are 16:9 and the grid reads as a wall of
// small previews; do not resize cells off 16:9.

/// 16:9 cell size. THE ASPECT IS LOAD-BEARING (thumbnail cells).
const CELL_W: f32 = 170.0;
const CELL_H: f32 = 96.0;
const CELL_SPACING: f32 = 3.0;
/// Popup never renders wider than this many columns; 8 columns × 170px +
/// chrome ≈ 1400px, the D12 "6-8 columns at 1080p-class" target.
const MAX_COLUMNS: usize = 8;
/// Margin kept between the popup and the screen edges on both axes.
const SCREEN_MARGIN: f32 = 24.0;
const PADDING: f32 = 10.0;
const BORDER: f32 = 1.0;
const SEARCH_BAR_HEIGHT: f32 = 30.0;
const SEARCH_PAD_X: f32 = 10.0;
const CHIP_ROW_HEIGHT: f32 = 25.0;
const CHIP_ROW_GAP: f32 = 4.0;
const CHIP_SPACING: f32 = 5.0;
const CHIP_PAD_H: f32 = 10.0;
const SECTION_SPACING: f32 = 6.0;
const PASTE_BUTTON_HEIGHT: f32 = 28.0;
const CELL_RADIUS: f32 = 6.0;
const ACCENT_BAR_W: f32 = 3.0;
/// Cell label insets for flat (no-image) cells.
const CAPTION_PAD_X: f32 = 5.0;
/// Height of the "No presets match" row when the filter empties the grid (F9).
const EMPTY_STATE_H: f32 = 44.0;
const CELL_FONT: u16 = color::FONT_LABEL;
const SEARCH_FONT: u16 = color::FONT_LABEL;
/// Thumbnail-grid caption: below the 16:9 thumbnail, word-wrapped to the cell
/// width, as many lines as the longest name needs. Centering a single line on
/// the thumbnail spilled long names across neighbouring cells.
const CELL_LABEL_FONT: u16 = color::FONT_TITLE;
const CELL_LABEL_LINE_H: f32 = 22.0;
const CAPTION_GAP: f32 = 4.0;

// ── Colors ──

const SEARCH_BG: Color32 = Color32::new(31, 31, 32, 255);
const SEARCH_TEXT: Color32 = Color32::new(168, 168, 172, 255);
const CELL_NORMAL: Color32 = Color32::new(36, 36, 38, 255);
const CELL_HOVER: Color32 = Color32::new(51, 51, 56, 255);
const CELL_PRESSED: Color32 = Color32::new(46, 46, 48, 255);
/// Translucent hover/press tints for an image-filled cell (PRESET_LIBRARY_DESIGN
/// P6, D7) — `CELL_HOVER`/`CELL_PRESSED` are fully opaque and would blot the
/// thumbnail; these composite over it as a subtle lift instead.
const CELL_HOVER_OVER_IMAGE: Color32 = color::BROWSER_CELL_HOVER_OVER_IMAGE;
const CELL_PRESSED_OVER_IMAGE: Color32 = color::BROWSER_CELL_PRESSED_OVER_IMAGE;
/// Caption-strip fill for an image cell's label legibility band
const CHIP_INACTIVE: Color32 = Color32::new(41, 41, 43, 255);
const CHIP_HOVER: Color32 = Color32::new(56, 56, 58, 255);
const PASTE_BG: Color32 = Color32::new(40, 40, 42, 255);
const PASTE_HOVER: Color32 = Color32::new(55, 55, 59, 255);
const SEARCH_HOVER: Color32 = Color32::new(38, 38, 40, 255);
const TEXT_PRIMARY: Color32 = Color32::new(224, 224, 224, 255);
const TEXT_DIM: Color32 = Color32::new(120, 120, 124, 255);

// Category accent colors — the real buckets (F11): the four effect buckets
// (P1's recuration) plus the generator buckets; the stale table's dead arms
// died with the registry buckets they colored. Palette literals, not one-off
// paints: tokenizing into color.rs is the landing follow-up (lane ownership
// stops at this file).
const CAT_SPATIAL: Color32 = Color32::new(102, 191, 191, 255); // design-token-exempt: P3 category accent palette, tokenize into color.rs at landing
const CAT_COLOR: Color32 = Color32::new(219, 94, 124, 255); // design-token-exempt: P3 category accent palette, tokenize into color.rs at landing
const CAT_STYLIZE: Color32 = Color32::new(150, 130, 220, 255); // design-token-exempt: P3 category accent palette, tokenize into color.rs at landing
const CAT_FILMIC: Color32 = Color32::new(200, 180, 120, 255); // design-token-exempt: P3 category accent palette, tokenize into color.rs at landing
const CAT_GEOMETRY: Color32 = Color32::new(110, 155, 235, 255); // design-token-exempt: P3 category accent palette, tokenize into color.rs at landing
const CAT_PATTERN: Color32 = Color32::new(95, 190, 140, 255); // design-token-exempt: P3 category accent palette, tokenize into color.rs at landing
const CAT_SIM: Color32 = Color32::new(230, 145, 85, 255); // design-token-exempt: P3 category accent palette, tokenize into color.rs at landing
const CAT_TEXT_MEDIA: Color32 = Color32::new(150, 165, 190, 255); // design-token-exempt: P3 category accent palette, tokenize into color.rs at landing

/// Fixed source-chip order (PRESET_LIBRARY_DESIGN P5, D6): "All" is chip 0
/// (handled like the category row's "All"), then these three, always in this
/// order so a right-click's stored [`Source`] and the rendered chip agree.
const SOURCE_CHIPS: [(Source, &str); 3] = [
    (Source::Factory, "Factory"),
    (Source::MyLibrary, "My Library"),
    (Source::Project, "This Project"),
];

// ── Public types ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserPopupMode {
    Effect,
    Generator,
    /// Picking a parameter action (for example, an automation lane). Unlike
    /// preset modes this session returns the typed action captured at open.
    Actions,
    /// Picking a graph node to spawn in the node editor. Items carry node
    /// `type_id`s and selection returns `NodeSelected`.
    Node,
}

/// Result of an interaction.
#[derive(Debug, Clone)]
pub enum BrowserPopupAction {
    /// Selection carries the popup's context atomically — prevents temporal coupling
    /// where context could be read after close() clears it.
    Selected {
        /// The chosen preset's stable type id (effect or generator), resolved
        /// directly by the dispatch with no registry-index indirection — so
        /// presets outside the startup-static registry (project-embedded /
        /// forked) are selectable.
        type_id: String,
        mode: BrowserPopupMode,
        tab: InspectorTab,
        layer_id: Option<LayerId>,
    },
    Paste,
    Dismissed,
    /// A node `type_id` was chosen in Node mode, to spawn at `graph_pos` (the
    /// graph-space cursor position captured when the picker opened).
    NodeSelected {
        type_id: String,
        graph_pos: (f32, f32),
    },
    /// A typed action selected from an action picker. The action is captured
    /// with the item at open time, so filtering and navigation never require
    /// parsing a display string or re-resolving a target later.
    ActionSelected(PanelAction),
}

/// Everything the app needs to open the browser's right-click management menu
/// (PRESET_LIBRARY_DESIGN P5, D6) for one cell — returned by
/// [`BrowserPopupPanel::handle_right_click`]. `mode` is always `Effect` or
/// `Generator` (never `Node` — see that method's doc); it stands in for
/// `manifold_core::preset_def::PresetKind` here since this crate mirrors
/// core types rather than depending on `manifold-core` (see
/// `PickerItem`/`PresetTypeId`'s doc comments for the same pattern) — the app
/// layer converts at the boundary.
#[derive(Debug, Clone)]
pub struct BrowserCellContext {
    pub mode: BrowserPopupMode,
    pub type_id: String,
    pub source: Source,
}

/// Request to open the popup. Items travel as one `Vec<PickerItem>` (D5) —
/// replaces the 4-5 parallel per-field `Vec<String>`s (name / type id /
/// category / search-alias) a request used to carry.
pub struct BrowserPopupRequest {
    pub mode: BrowserPopupMode,
    pub tab: InspectorTab,
    /// For Generator mode: the layer whose generator type is being changed.
    pub layer_id: Option<LayerId>,
    pub items: Vec<PickerItem>,
    pub category_names: Vec<String>,
    /// Node mode: graph-space position to spawn the chosen node at.
    pub spawn_graph_pos: Option<(f32, f32)>,
    pub paste_count: usize,
    pub screen_anchor: Vec2,
}

/// How an Actions list reads: what an empty search says, whether each row
/// shows its label in the installed font it names (the font picker), and
/// which item is the current value (marked, and the list opens on it).
#[derive(Debug, Clone)]
pub struct ActionListOptions {
    pub empty_label: &'static str,
    pub label_in_own_font: bool,
    /// Index into the request's `items`.
    pub current: Option<usize>,
    /// Optional secondary action for each item. An entry with an action gets
    /// a compact Edit button at the right side of its row.
    pub secondary_actions: Vec<Option<PanelAction>>,
    /// Keep the Actions picker open after a primary selection.
    pub keep_open: bool,
}

impl Default for ActionListOptions {
    fn default() -> Self {
        Self {
            empty_label: "No matches",
            label_in_own_font: false,
            current: None,
            secondary_actions: Vec::new(),
            keep_open: false,
        }
    }
}

/// Compact right-side action button width in an Actions row.
const ACTION_EDIT_W: f32 = 52.0;
const ACTION_EDIT_GAP: f32 = 6.0;

/// Row font for `label_in_own_font` lists: large enough to judge a face.
const FONT_PREVIEW_SIZE: u16 = color::FONT_HEADING;

/// Per-cell metadata needed for click AND right-click routing. Selection only
/// needs `type_id`; the right-click management menu (PRESET_LIBRARY_DESIGN
/// P5) additionally needs the cell's classified source.
#[derive(Clone)]
struct CellMeta {
    item_index: usize,
    type_id: String,
    source: Option<Source>,
}

/// Rect/geometry output rebuilt every `build_at` — not meaningful state
/// to preserve across builds, so it's a plain rebuild-target, not part of the
/// session's semantic identity (kept as its own type only for readability).
/// All geometry derives from content + screen each build (D12); event
/// handlers read the LAST build's values.
struct BrowserLayout {
    columns: usize,
    popup_w: f32,
    popup_x: f32,
    popup_y: f32,
    total_height: f32,
    grid_viewport_height: f32,
    /// Full cell height (thumbnail plus caption block); the grid pitch is
    /// this plus [`CELL_SPACING`].
    cell_h: f32,
    /// Click point the popup opened at (drives the edge clamp every build).
    anchor: Vec2,

    backdrop_id: Option<NodeId>,
    search_bar_id: Option<NodeId>,
    chip_all_id: Option<NodeId>,
    chip_ids: Vec<NodeId>,
    /// Source-filter row (PRESET_LIBRARY_DESIGN P5, D6) — `None` for Node
    /// mode, which has no source concept and renders no row.
    source_all_id: Option<NodeId>,
    /// Parallel to [`SOURCE_CHIPS`] — `source_chip_ids[i]` is the chip for
    /// `SOURCE_CHIPS[i]`.
    source_chip_ids: Vec<NodeId>,
    cell_ids: Vec<(NodeId, CellMeta)>,
    /// Parallel to action rows that have a secondary action.
    secondary_ids: Vec<(NodeId, usize)>,
    paste_id: Option<NodeId>,
    first_node: usize,
    node_count: usize,
}

impl BrowserLayout {
    fn new() -> Self {
        Self {
            columns: 1,
            popup_w: 0.0,
            popup_x: 0.0,
            popup_y: 0.0,
            total_height: 0.0,
            grid_viewport_height: 0.0,
            cell_h: CELL_H,
            anchor: Vec2::ZERO,
            backdrop_id: None,
            search_bar_id: None,
            chip_all_id: None,
            chip_ids: Vec::new(),
            source_all_id: None,
            source_chip_ids: Vec::new(),
            cell_ids: Vec::new(),
            secondary_ids: Vec::new(),
            paste_id: None,
            first_node: 0,
            node_count: 0,
        }
    }
}

/// Per-open state (`OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` section 3, D1) —
/// constructed whole by `open()`, dropped whole by `close()`.
pub struct BrowserSession {
    pub mode: BrowserPopupMode,
    pub tab: InspectorTab,
    pub layer_id: Option<LayerId>,
    /// Items, filter, category, filtered indices, keyboard cursor, scroll.
    pub picker: PickerCore,
    pub pending_spawn_graph_pos: Option<(f32, f32)>,
    pub paste_count: usize,
    /// Typed actions parallel to `picker` items for an Actions session.
    actions: Option<Vec<PanelAction>>,
    list: ActionListOptions,
    /// Set at open when the cursor starts on `list.current`: the first build
    /// (the first with real geometry) scrolls it to the middle.
    center_cursor_on_build: bool,
    layout: BrowserLayout,
}

// ── Panel ──

pub struct BrowserPopupPanel {
    // Config — survives across opens.
    screen_w: f32,
    screen_h: f32,
    session: Option<BrowserSession>,
    /// Search-focus request raised at open (F5): the app pump drains it and
    /// takes the owned search session, same as the graph-editor Node picker
    /// does at open. `None` once drained or for Node mode.
    search_focus_dirty: bool,
}

impl Default for BrowserPopupPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserPopupPanel {
    pub fn new() -> Self {
        Self {
            screen_w: 1920.0,
            screen_h: 1080.0,
            session: None,
            search_focus_dirty: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.session.is_some()
    }

    pub fn set_screen_size(&mut self, w: f32, h: f32) {
        self.screen_w = w;
        self.screen_h = h;
    }

    /// The live search filter text (empty when closed).
    pub fn current_filter(&self) -> &str {
        self.session.as_ref().map_or("", |s| s.picker.filter())
    }

    /// The active category chip, if any (`None` = "All"). Read-side mirror
    /// of [`Self::set_category`] — lane-scoped opens (LED_STRIPS_DESIGN
    /// MVP-P3c) and tests assert through it.
    pub fn active_category(&self) -> Option<&str> {
        self.session.as_ref().and_then(|s| s.picker.active_category())
    }

    /// Read-only reach to the session's picker — the item list, the chip
    /// set, and the active chips. App-side tests (LED_STRIPS_DESIGN
    /// MVP-P3c) and overlay drivers introspect through this; `None` when
    /// the popup is closed.
    pub fn picker(&self) -> Option<&PickerCore> {
        self.session.as_ref().map(|s| &s.picker)
    }

    /// The owning layer of the currently open persistent Actions picker.
    pub fn persistent_actions_layer(&self) -> Option<&LayerId> {
        self.session
            .as_ref()
            .filter(|session| {
                session.mode == BrowserPopupMode::Actions && session.list.keep_open
            })
            .and_then(|session| session.layer_id.as_ref())
    }

    /// Every open item's thumbnail path (PRESET_LIBRARY_DESIGN P6, D7),
    /// regardless of the current filter/category/source — the app decodes +
    /// registers each one, once per distinct path, so the picture is ready
    /// the moment a cell scrolls into view. Empty when closed or in Node
    /// mode (no preset item ever carries a thumbnail there).
    pub fn thumbnail_paths(&self) -> impl Iterator<Item = &str> {
        self.session
            .iter()
            .flat_map(|s| s.picker.all_items())
            .filter_map(|it| it.thumbnail.as_deref())
    }

    pub fn open(&mut self, req: BrowserPopupRequest) {
        self.open_with_actions(req, None, ActionListOptions::default());
    }

    /// Open a searchable picker whose cells dispatch the supplied typed
    /// actions. `actions` is parallel to `req.items`, keyed by the picker
    /// item's stable index; filtering never changes that identity.
    pub fn open_actions(
        &mut self,
        req: BrowserPopupRequest,
        actions: Vec<PanelAction>,
        list: ActionListOptions,
    ) {
        debug_assert_eq!(req.mode, BrowserPopupMode::Actions);
        debug_assert_eq!(req.items.len(), actions.len());
        debug_assert!(list.secondary_actions.is_empty() || list.secondary_actions.len() == req.items.len());
        self.open_with_actions(req, Some(actions), list);
    }

    fn open_with_actions(
        &mut self,
        req: BrowserPopupRequest,
        actions: Option<Vec<PanelAction>>,
        list: ActionListOptions,
    ) {
        // The search-focus hook is effect/generator-only; Node mode never
        // dirties it.
        if req.mode != BrowserPopupMode::Node {
            self.search_focus_dirty = true;
        }
        let mut layout = BrowserLayout::new();
        layout.anchor = req.screen_anchor;
        let mut picker = PickerCore::new(req.items, req.category_names);
        let center_cursor_on_build = list.current.is_some_and(|i| picker.set_cursor_to_item(i));
        self.session = Some(BrowserSession {
            mode: req.mode,
            tab: req.tab,
            layer_id: req.layer_id,
            picker,
            pending_spawn_graph_pos: req.spawn_graph_pos,
            paste_count: req.paste_count,
            actions,
            list,
            center_cursor_on_build,
            layout,
        });
    }

    pub fn close(&mut self) {
        self.session = None;
    }

    /// Refresh the currently open Actions session without resetting its
    /// search, category, cursor, or scroll state. Returns `true` only when an
    /// Actions session was refreshed; the caller owns the repaint request.
    pub fn refresh_actions(
        &mut self,
        items: Vec<PickerItem>,
        category_names: Vec<String>,
        actions: Vec<PanelAction>,
        secondary_actions: Vec<Option<PanelAction>>,
    ) -> bool {
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        if session.mode != BrowserPopupMode::Actions {
            return false;
        }
        debug_assert_eq!(items.len(), actions.len());
        debug_assert!(secondary_actions.is_empty() || secondary_actions.len() == items.len());

        let current_type_id = session
            .list
            .current
            .and_then(|index| session.picker.item(index))
            .map(|item| item.type_id.clone());
        session.picker.replace_items(items, category_names);
        session.actions = Some(actions);
        session.list.secondary_actions = secondary_actions;
        session.list.current = current_type_id.and_then(|type_id| {
            session
                .picker
                .all_items()
                .position(|item| item.type_id == type_id)
        });
        true
    }

    /// Drain the open-time search-focus request (once per open, non-Node
    /// modes) — the app pump takes the owned search session with it, same
    /// as the Node picker does at open (F5). The session's anchor is the
    /// app's problem: the popup tree doesn't exist yet at open, so the app
    /// re-anchors over the real search bar every frame until close.
    pub fn take_search_focus(&mut self) -> bool {
        std::mem::take(&mut self.search_focus_dirty)
    }

    /// Called when the search filter changes (from TextInputManager commit
    /// or a live keystroke).
    pub fn set_filter(&mut self, filter: String) {
        if let Some(session) = self.session.as_mut() {
            session.picker.set_filter(filter);
        }
    }

    pub fn set_category(&mut self, category: Option<String>) {
        if let Some(session) = self.session.as_mut() {
            session.picker.set_category(category);
        }
    }

    /// Set the active source chip (`None` = "All" — PRESET_LIBRARY_DESIGN P5,
    /// D6). Mirrors [`Self::set_category`].
    pub fn set_source(&mut self, source: Option<Source>) {
        if let Some(session) = self.session.as_mut() {
            session.picker.set_source(source);
        }
    }

    // ── Build ──
    //
    // All geometry derives from content + screen EVERY build (D12): the
    // width from the filtered item count (screen-clamped, MAX_COLUMNS cap),
    // the height content-sized with the screen as the only cap. Nothing here
    // survives as meaningful state — event handlers read the last build.

    pub fn build(&mut self, tree: &mut UITree) {
        let screen_w = self.screen_w;
        let screen_h = self.screen_h;
        let Some(session) = self.session.as_mut() else {
            return;
        };

        session.layout.first_node = tree.count();
        session.layout.cell_ids.clear();
        session.layout.secondary_ids.clear();
        session.layout.chip_ids.clear();
        session.layout.source_chip_ids.clear();

        let count = session.picker.filtered_len();
        let mode = session.mode;
        // Caption block height comes from the FULL item set (same reason as
        // the width below: no row-height jitter while typing).
        let caption_lines = if mode == BrowserPopupMode::Actions {
            0
        } else {
            caption_line_count(tree.measurer(), session.picker.all_items())
        };
        let cell_h = grid_cell_h(mode, caption_lines);
        session.layout.cell_h = cell_h;
        let cell_w = if mode == BrowserPopupMode::Actions { 360.0_f32.min((screen_w - SCREEN_MARGIN * 2.0).max(1.0)) } else { CELL_W };

        // ── Width: content-sized, screen-clamped ──
        //
        // The WIDTH derives from the FULL item set, not the filtered count:
        // typing filters inside a stable-sized popup (no per-keystroke
        // resize jitter), and the empty state keeps a sensible surface. The
        // GRID's columns below follow the filtered count.
        let chrome_w = (PADDING + BORDER) * 2.0;
        let inner_max_w = (screen_w - SCREEN_MARGIN * 2.0 - chrome_w).max(cell_w);
        let cols_fit = ((inner_max_w + CELL_SPACING) / (cell_w + CELL_SPACING))
            .floor()
            .max(1.0) as usize;
        let full_count = session.picker.all_items().count();
        // Never more columns than items (content-sized) and never more than
        // MAX_COLUMNS — 6-8 columns at 1080p-class windows (D12).
        let width_columns = if mode == BrowserPopupMode::Actions { 1 } else { cols_fit.min(MAX_COLUMNS).min(full_count.max(1)) };
        let inner_w =
            width_columns as f32 * cell_w + width_columns.saturating_sub(1) as f32 * CELL_SPACING;
        let popup_w = (inner_w + chrome_w).min(screen_w);
        let content_w = popup_w - chrome_w;
        // Grid columns follow the FILTERED count — a narrow result set packs
        // into fewer columns inside the stable width.
        let columns = width_columns.min(count.max(1));

        // ── Chips: measured with the tree's real font metrics (F12), then
        // wrapped at the content edge — overflow wraps to another row, it
        // never silently runs past the popup. ──
        let has_source_row = matches!(mode, BrowserPopupMode::Effect | BrowserPopupMode::Generator);
        let has_chips = !session.picker.categories().is_empty();
        let active_source = session.picker.active_source();
        let active_category = session.picker.active_category().map(str::to_string);
        let category_names: Vec<String> = session.picker.categories().to_vec();

        let chip_width = |tree: &UITree, label: &str| {
            tree.text_width(label, CELL_FONT, FontWeight::Regular) + CHIP_PAD_H * 2.0
        };
        let source_labels: Vec<String> = if has_source_row {
            std::iter::once("All".to_string())
                .chain(SOURCE_CHIPS.iter().map(|(_, l)| (*l).to_string()))
                .collect()
        } else {
            Vec::new()
        };
        let source_widths: Vec<f32> = source_labels
            .iter()
            .map(|l| chip_width(tree, l))
            .collect();
        let chip_widths: Vec<f32> = if has_chips {
            std::iter::once(chip_width(tree, "All"))
                .chain(category_names.iter().map(|c| chip_width(tree, c)))
                .collect()
        } else {
            Vec::new()
        };
        // Row count for a measured chip list at the content width.
        let wrapped_rows = |widths: &[f32]| -> usize {
            let mut rows = 1usize;
            let mut x = 0.0f32;
            for w in widths {
                if x > 0.0 && x + *w > content_w {
                    rows += 1;
                    x = 0.0;
                }
                x += *w + CHIP_SPACING;
            }
            rows
        };
        let chip_block_h = |rows: usize| {
            rows as f32 * CHIP_ROW_HEIGHT + rows.saturating_sub(1) as f32 * CHIP_ROW_GAP
        };

        // ── Height: content-sized, the SCREEN is the only cap (D12) ──
        let pitch = cell_h + CELL_SPACING;
        let grid_content_h = if count == 0 {
            EMPTY_STATE_H
        } else {
            count.div_ceil(columns) as f32 * pitch - CELL_SPACING
        };
        let mut above = BORDER + PADDING + SEARCH_BAR_HEIGHT + SECTION_SPACING;
        if has_source_row {
            above += chip_block_h(wrapped_rows(&source_widths)) + SECTION_SPACING;
        }
        if has_chips {
            above += chip_block_h(wrapped_rows(&chip_widths)) + SECTION_SPACING;
        }
        let below = if mode != BrowserPopupMode::Actions && session.paste_count > 0 {
            SECTION_SPACING + PASTE_BUTTON_HEIGHT
        } else {
            0.0
        };
        let anchor = session.layout.anchor;
        let natural = above + grid_content_h + below + PADDING + BORDER;
        let max_total = screen_h - SCREEN_MARGIN * 2.0;
        let (popup_y, grid_vp_h, total_h) = if natural <= max_total {
            (
                anchor.y.clamp(0.0, (screen_h - natural).max(0.0)),
                grid_content_h,
                natural,
            )
        } else {
            // Screen cap: shrink ONLY the grid viewport — the grid scrolls
            // internally beyond it, exactly as before.
            let vp = (max_total - above - below - PADDING - BORDER).max(cell_h * 0.5);
            (SCREEN_MARGIN, vp, max_total)
        };
        let popup_x = anchor.x.clamp(0.0, (screen_w - popup_w).max(0.0));

        session.layout.columns = columns;
        session.layout.popup_w = popup_w;
        session.layout.popup_x = popup_x;
        session.layout.popup_y = popup_y;
        session.layout.total_height = total_h;
        session.layout.grid_viewport_height = grid_vp_h;

        // Scrim + modal container via the shared shell (section 17 lifts it with a
        // soft shadow). All content is parented to the container, which clips
        // children by construction — nothing can paint or take clicks outside it.
        let shell = popup_shell::build(
            tree,
            (screen_w, screen_h),
            Rect::new(popup_x, popup_y, popup_w, total_h),
            &popup_shell::PopupStyle::MODAL,
        );
        session.layout.backdrop_id = Some(shell.backdrop);
        let content_parent = Some(shell.container);

        let cx = popup_x + BORDER + PADDING;
        let mut cy = popup_y + BORDER + PADDING;

        // Search bar — real text inset at draw time, no space-padding.
        let filter_text = session.picker.filter().to_string();
        session.layout.search_bar_id = Some(tree.add_button(
            content_parent,
            cx,
            cy,
            content_w,
            SEARCH_BAR_HEIGHT,
            UIStyle {
                bg_color: SEARCH_BG,
                hover_bg_color: SEARCH_HOVER,
                corner_radius: color::BUTTON_RADIUS,
                font_size: SEARCH_FONT,
                text_color: SEARCH_TEXT,
                text_inset_x: SEARCH_PAD_X,
                ..UIStyle::default()
            },
            &if filter_text.is_empty() {
                "Search...".to_string()
            } else {
                filter_text
            },
        ));
        cy += SEARCH_BAR_HEIGHT + SECTION_SPACING;

        // Source filter row (PRESET_LIBRARY_DESIGN P5, D6): "All · Factory ·
        // My Library · This Project", above the category chips. Node mode
        // (the graph-editor's add-node picker) has no source concept, so it
        // renders no row.
        session.layout.source_all_id = None;
        if has_source_row {
            let active: Vec<bool> = std::iter::once(active_source.is_none())
                .chain(SOURCE_CHIPS.iter().map(|(src, _)| active_source == Some(*src)))
                .collect();
            let (all_id, ids) = build_chip_group(
                tree,
                content_parent,
                cx,
                cy,
                content_w,
                &source_labels,
                &active,
            );
            session.layout.source_all_id = all_id;
            session.layout.source_chip_ids = ids;
            cy += chip_block_h(wrapped_rows(&source_widths)) + SECTION_SPACING;
        }

        // Category chips — same measured/wrapped machinery as the source row.
        session.layout.chip_all_id = None;
        if has_chips {
            let active: Vec<bool> = std::iter::once(active_category.is_none())
                .chain(
                    category_names
                        .iter()
                        .map(|c| active_category.as_deref() == Some(c.as_str())),
                )
                .collect();
            let (all_id, ids) = build_chip_group(
                tree,
                content_parent,
                cx,
                cy,
                content_w,
                &category_chip_labels(&category_names),
                &active,
            );
            session.layout.chip_all_id = all_id;
            session.layout.chip_ids = ids;
            cy += chip_block_h(wrapped_rows(&chip_widths)) + SECTION_SPACING;
        }

        // Grid viewport — ClipRegion clips cells that extend beyond bounds.
        let vp_top = cy;
        let vp_h = grid_vp_h;

        let clip_id = session
            .picker
            .scroll
            .begin(tree, Rect::new(cx, vp_top, content_w, vp_h));
        // Content height now that the viewport is fresh — the clamp lands
        // against THIS build's geometry.
        session.picker.scroll.set_content_height(grid_content_h);
        if std::mem::take(&mut session.center_cursor_on_build)
            && let Some(cursor) = session.picker.cursor()
        {
            let row_y = (cursor / columns) as f32 * pitch;
            session
                .picker
                .scroll
                .set_scroll_offset(row_y + cell_h * 0.5 - vp_h * 0.5);
        }
        // The grid's own clip handles cell overflow against the viewport;
        // rooting it under the container also ties the grid to the popup's
        // structural containment, same as every other content node.
        tree.reparent_root_nodes(clip_id.index(), 1, shell.container);
        let clip_parent = Some(clip_id);

        // Empty state (F9): a filtered-out grid reads as a row, not a
        // collapsed blank popup.
        if count == 0 {
            tree.add_label(
                clip_parent,
                cx,
                vp_top,
                content_w,
                vp_h,
                if mode == BrowserPopupMode::Actions { session.list.empty_label } else { "No presets match" },
                UIStyle {
                    font_size: CELL_FONT,
                    text_color: TEXT_DIM,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
            );
        }

        let scroll_offset = session.picker.scroll.scroll_offset();
        let cursor = session.picker.cursor();

        for (fi, (item_index, item)) in session.picker.filtered().enumerate() {
            let col = fi % columns;
            let row = fi / columns;
            // Relative Y for culling check (viewport-local)
            let rel_y = row as f32 * pitch - scroll_offset;

            // Cull cells entirely outside viewport
            if rel_y + cell_h < 0.0 || rel_y > vp_h {
                continue;
            }

            let cell_x = cx + col as f32 * (cell_w + CELL_SPACING);
            let cell_y = vp_top + rel_y;

            // Category accent bar
            if let Some(cat) = item.category.as_deref()
                && !cat.is_empty()
            {
                tree.add_panel(
                    clip_parent,
                    cell_x,
                    cell_y,
                    ACCENT_BAR_W,
                    cell_h,
                    UIStyle {
                        bg_color: category_color(cat),
                        corner_radius: color::SMALL_RADIUS,
                        ..UIStyle::default()
                    },
                );
            }

            // Image cell: the save-time-rendered / factory-committed
            // thumbnail (STATIC_THUMBNAILS_DESIGN D1 — statics only, no live
            // preview); else a flat-color cell exactly as before (D7's
            // "clean fallback"). An image cell gets the caption strip with
            // the name inside it (F7/F8), with real named x-insets — the
            // space-padded prefix hack is gone (F10). All non-interactive
            // nodes paint BEFORE the button, so they never shadow its click
            // region and its hover/press tint composites on top.
            let has_image = mode != BrowserPopupMode::Actions && item.thumbnail.is_some();
            let has_caption = caption_lines > 0;
            if let Some(path) = item.thumbnail.as_deref() {
                let handle = crate::node::texture_handle_for_key(path);
                tree.add_image(clip_parent, cell_x, cell_y, cell_w, CELL_H, CELL_RADIUS, handle);
            }

            if has_caption {
                let rects = caption_line_rects(cell_x, cell_y, caption_lines);
                for (line, r) in caption_wrap(tree.measurer(), &item.label).iter().zip(rects) {
                    tree.add_label(
                        clip_parent,
                        r.x,
                        r.y,
                        r.width,
                        r.height,
                        line,
                        UIStyle {
                            font_size: CELL_LABEL_FONT,
                            text_color: Color32::WHITE,
                            text_align: TextAlign::Center,
                            ..UIStyle::default()
                        },
                    );
                }
            }

            // Cell button — full height, ClipRegion handles visual clipping.
            // The keyboard cursor (P2 arrow nav) reuses the existing hover
            // tint rather than a new design token — a highlighted cell reads
            // identically whether the mouse or the keyboard put it there.
            // Over an image the fill is transparent (the image already
            // fills the body) and the hover/press tints turn translucent so
            // interaction feedback still shows without blotting the picture.
            let is_cursor = cursor == Some(fi);
            // The current value reads like a dropdown's checked row.
            let is_current = session.list.current == Some(item_index);
            let own_font = session.list.label_in_own_font;
            let has_secondary = mode == BrowserPopupMode::Actions
                && session
                    .list
                    .secondary_actions
                    .get(item_index)
                    .and_then(Option::as_ref)
                    .is_some();
            let primary_w = if has_secondary {
                (cell_w - ACTION_EDIT_W - ACTION_EDIT_GAP).max(1.0)
            } else {
                cell_w
            };
            let id = tree.add_button(
                clip_parent,
                cell_x,
                cell_y,
                primary_w,
                cell_h,
                UIStyle {
                    bg_color: if has_image {
                        if is_cursor { CELL_HOVER_OVER_IMAGE } else { Color32::TRANSPARENT }
                    } else if is_cursor {
                        CELL_HOVER
                    } else if is_current {
                        color::DROPDOWN_ITEM_SELECTED
                    } else {
                        CELL_NORMAL
                    },
                    hover_bg_color: if has_image { CELL_HOVER_OVER_IMAGE } else { CELL_HOVER },
                    pressed_bg_color: if has_image { CELL_PRESSED_OVER_IMAGE } else { CELL_PRESSED },
                    corner_radius: CELL_RADIUS,
                    font_size: if own_font { FONT_PREVIEW_SIZE } else { CELL_FONT },
                    text_color: if is_current { color::DROPDOWN_CHECK_COLOR } else { TEXT_PRIMARY },
                    text_inset_x: CAPTION_PAD_X,
                    ..UIStyle::default()
                },
                if has_caption { "" } else { &item.label },
            );
            if own_font {
                tree.set_font_family(id, &item.label);
            }

            if has_secondary {
                let edit_id = tree.add_button(
                    clip_parent,
                    cell_x + cell_w - ACTION_EDIT_W,
                    cell_y,
                    ACTION_EDIT_W,
                    cell_h,
                    UIStyle {
                        bg_color: CELL_NORMAL,
                        hover_bg_color: CELL_HOVER,
                        pressed_bg_color: CELL_PRESSED,
                        corner_radius: CELL_RADIUS,
                        font_size: CELL_FONT,
                        text_color: TEXT_PRIMARY,
                        ..UIStyle::default()
                    },
                    "Edit",
                );
                session.layout.secondary_ids.push((edit_id, item_index));
            }

            session.layout.cell_ids.push((
                id,
                CellMeta {
                    item_index,
                    type_id: item.type_id.clone(),
                    source: item.source,
                },
            ));
        }

        cy += vp_h;

        // Paste button
        if mode != BrowserPopupMode::Actions && session.paste_count > 0 {
            cy += SECTION_SPACING;
            let paste_label = if session.paste_count == 1 {
                "Paste Effect".to_string()
            } else {
                format!("Paste {} Effects", session.paste_count)
            };
            session.layout.paste_id = Some(tree.add_button(
                content_parent,
                cx,
                cy,
                content_w,
                PASTE_BUTTON_HEIGHT,
                UIStyle {
                    bg_color: PASTE_BG,
                    hover_bg_color: PASTE_HOVER,
                    corner_radius: color::BUTTON_RADIUS,
                    font_size: CELL_FONT,
                    text_color: color::ACCENT_BLUE,
                    ..UIStyle::default()
                },
                &paste_label,
            ));
        } else {
            session.layout.paste_id = None;
        }

        session.layout.node_count = tree.count() - session.layout.first_node;
    }

    // ── Event handling ──

    pub fn handle_click(&mut self, node_id: NodeId) -> Option<BrowserPopupAction> {
        // Resolve the click against the last build's node ids with one
        // immutable borrow, then act — no Vec clones per click (F17).
        enum Hit {
            Backdrop,
            SearchBar,
            ChipAll,
            Chip(usize),
            SourceAll,
            Source(usize),
            Cell(usize),
            Secondary(usize),
            Paste,
        }
        let hit = {
            let session = self.session.as_ref()?;
            let layout = &session.layout;
            if layout.backdrop_id == Some(node_id) {
                Hit::Backdrop
            } else if layout.search_bar_id == Some(node_id) {
                Hit::SearchBar
            } else if layout.chip_all_id == Some(node_id) {
                Hit::ChipAll
            } else if let Some(i) = layout.chip_ids.iter().position(|&id| id == node_id) {
                Hit::Chip(i)
            } else if layout.source_all_id == Some(node_id) {
                Hit::SourceAll
            } else if let Some(i) = layout.source_chip_ids.iter().position(|&id| id == node_id)
            {
                Hit::Source(i)
            } else if let Some(i) = layout.cell_ids.iter().position(|(id, _)| *id == node_id) {
                Hit::Cell(i)
            } else if let Some((_, item_index)) = layout.secondary_ids.iter().find(|(id, _)| *id == node_id) {
                Hit::Secondary(*item_index)
            } else if layout.paste_id == Some(node_id) {
                Hit::Paste
            } else {
                return None;
            }
        };

        match hit {
            Hit::Backdrop => {
                self.close();
                Some(BrowserPopupAction::Dismissed)
            }
            // Search bar → signal to open text input
            Hit::SearchBar => None, // Caller checks is_search_bar()
            Hit::ChipAll => {
                self.set_category(None);
                None // Needs rebuild, no action
            }
            Hit::Chip(i) => {
                // chip_ids is built parallel to the picker's category list.
                let name = self
                    .session
                    .as_ref()
                    .and_then(|s| s.picker.categories().get(i).cloned());
                if let Some(name) = name {
                    self.set_category(Some(name));
                }
                None // Needs rebuild
            }
            Hit::SourceAll => {
                self.set_source(None);
                None // Needs rebuild
            }
            Hit::Source(i) => {
                if let Some((src, _)) = SOURCE_CHIPS.get(i) {
                    self.set_source(Some(*src));
                }
                None // Needs rebuild
            }
            Hit::Cell(i) => {
                let (action, keep_open) = {
                    let session = self.session.as_ref()?;
                    let (_, meta) = &session.layout.cell_ids[i];
                    if session.mode == BrowserPopupMode::Actions {
                        let action = session
                            .actions
                            .as_ref()
                            .and_then(|actions| actions.get(meta.item_index))
                            .cloned()
                            .map(BrowserPopupAction::ActionSelected)?;
                        (action, session.list.keep_open)
                    } else if session.mode == BrowserPopupMode::Node {
                        (BrowserPopupAction::NodeSelected {
                            type_id: meta.type_id.clone(),
                            graph_pos: session.pending_spawn_graph_pos.unwrap_or((0.0, 0.0)),
                        }, false)
                    } else {
                        (BrowserPopupAction::Selected {
                            type_id: meta.type_id.clone(),
                            mode: session.mode,
                            tab: session.tab,
                            layer_id: session.layer_id.clone(),
                        }, false)
                    }
                };
                if !keep_open {
                    self.close();
                }
                Some(action)
            }
            Hit::Secondary(item_index) => {
                let action = self
                    .session
                    .as_ref()?
                    .list
                    .secondary_actions
                    .get(item_index)
                    .and_then(Option::as_ref)
                    .cloned()
                    .map(BrowserPopupAction::ActionSelected)?;
                self.close();
                Some(action)
            }
            Hit::Paste => {
                self.close();
                Some(BrowserPopupAction::Paste)
            }
        }
    }

    /// Resolve a right-click on a grid cell to its management context.
    /// Returns `None` for: a miss, Node mode (no source concept — the
    /// graph-editor's add-node picker never gets this menu), or a Factory
    /// cell (read-only, D6: "NOT Factory"). Does NOT close the popup — the
    /// management menu (a `DropdownPanel` the caller opens) stacks on top of
    /// it, same as the card's right-click menu stacks on top of the
    /// inspector.
    pub fn handle_right_click(&self, node_id: NodeId) -> Option<BrowserCellContext> {
        let session = self.session.as_ref()?;
        if matches!(session.mode, BrowserPopupMode::Node | BrowserPopupMode::Actions) {
            return None;
        }
        let (_, meta) = session.layout.cell_ids.iter().find(|(id, _)| *id == node_id)?;
        match meta.source {
            Some(source @ (Source::MyLibrary | Source::Project)) => Some(BrowserCellContext {
                mode: session.mode,
                type_id: meta.type_id.clone(),
                source,
            }),
            _ => None,
        }
    }

    /// Returns true if the search bar was the clicked node.
    pub fn is_search_bar(&self, node_id: NodeId) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.layout.search_bar_id == Some(node_id))
    }

    /// Handle escape key.
    pub fn handle_escape(&mut self) -> Option<BrowserPopupAction> {
        if self.is_open() {
            self.close();
            Some(BrowserPopupAction::Dismissed)
        } else {
            None
        }
    }

    /// Arrow/Home/End/PageUp/PageDown/Enter/Escape keyboard nav (P2+P3, D14)
    /// — arrows move in grid geometry (Left/Right within the cursor's row,
    /// Up/Down a full row), Home/End jump to the ends, PageUp/PageDown move
    /// a screenful; a moved cursor is scrolled back into view
    /// (`scroll_to_reveal`). Enter picks (the type-and-enter fast path picks
    /// `filtered[0]` with no cursor and a non-empty filter), Escape
    /// dismisses. Mirrors `handle_click`'s action shape so callers dispatch
    /// identically regardless of whether the pick came from the mouse or the
    /// keyboard.
    pub fn handle_key_nav(&mut self, key: Key) -> Option<BrowserPopupAction> {
        let session = self.session.as_mut()?;
        let mode = session.mode;
        let keep_open = session.list.keep_open;
        let cell_h = session.layout.cell_h;
        let tab = session.tab;
        let layer_id = session.layer_id.clone();
        let spawn_pos = session.pending_spawn_graph_pos;
        let columns = session.layout.columns.max(1);
        let page = (session.layout.grid_viewport_height / (cell_h + CELL_SPACING))
            .floor()
            .max(1.0) as usize;

        let nav = session.picker.key_nav(key, columns, page);
        if matches!(nav, PickerNav::Moved)
            && let Some(cursor) = session.picker.cursor()
        {
            let row = cursor / columns;
            session
                .picker
                .scroll
                .scroll_to_reveal(row as f32 * (cell_h + CELL_SPACING), cell_h);
        }
        let picked_type_id = if let PickerNav::Picked(idx) = nav {
            session.picker.item(idx).map(|it| it.type_id.clone())
        } else {
            None
        };
        // `session`'s last use is above — safe to call `self.close()` below.

        match nav {
            PickerNav::Moved | PickerNav::Ignored => None,
            PickerNav::Dismissed => {
                self.close();
                Some(BrowserPopupAction::Dismissed)
            }
            PickerNav::Picked(_) => {
                if mode == BrowserPopupMode::Actions {
                    let idx = match nav {
                        PickerNav::Picked(idx) => idx,
                        _ => unreachable!(),
                    };
                    let action = session
                        .actions
                        .as_ref()
                        .and_then(|actions| actions.get(idx))
                        .cloned()
                        .map(BrowserPopupAction::ActionSelected);
                    if !keep_open {
                        self.close();
                    }
                    return action;
                }
                let type_id = picked_type_id.unwrap_or_default();
                let action = if mode == BrowserPopupMode::Node {
                    BrowserPopupAction::NodeSelected {
                        type_id,
                        graph_pos: spawn_pos.unwrap_or((0.0, 0.0)),
                    }
                } else {
                    BrowserPopupAction::Selected {
                        type_id,
                        mode,
                        tab,
                        layer_id,
                    }
                };
                self.close();
                Some(action)
            }
        }
    }

    /// Handle mouse wheel scroll within the popup. The caller hit-tests:
    /// only wheel events over the grid reach this (F13).
    pub fn handle_scroll(&mut self, delta: f32) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let columns = session.layout.columns.max(1);
        let cell_h = session.layout.cell_h;
        let rows = session.picker.filtered_len().div_ceil(columns);
        let content_h = rows as f32 * (cell_h + CELL_SPACING) - CELL_SPACING;
        session.picker.scroll.set_content_height(content_h);
        session.picker.scroll.apply_scroll_delta(delta);
    }

    /// Check if a node belongs to this popup.
    pub fn contains_node(&self, node_id: NodeId) -> bool {
        let Some(session) = self.session.as_ref() else {
            return false;
        };
        let id = node_id.index();
        id >= session.layout.first_node && id < session.layout.first_node + session.layout.node_count
    }

    /// Get search bar rect for text input anchoring.
    pub fn search_bar_rect(&self, tree: &UITree) -> Rect {
        if let Some(id) = self.session.as_ref().and_then(|s| s.layout.search_bar_id) {
            tree.get_bounds(id)
        } else {
            Rect::ZERO
        }
    }
}

// ── Helpers ──

/// Build one measured chip row group: chips are sized from the tree's font
/// metrics + [`CHIP_PAD_H`] padding, wrapped to a new row when the next chip
/// would pass the content edge (F12 — silent overflow is the bug class).
/// Returns the "All" chip (first) and the remaining chip ids in label order.
fn build_chip_group(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x0: f32,
    y0: f32,
    content_w: f32,
    labels: &[String],
    active: &[bool],
) -> (Option<NodeId>, Vec<NodeId>) {
    let mut x = x0;
    let mut y = y0;
    let mut ids = Vec::with_capacity(labels.len());
    for (i, label) in labels.iter().enumerate() {
        let w = tree.text_width(label, CELL_FONT, FontWeight::Regular) + CHIP_PAD_H * 2.0;
        if x > x0 && x + w > x0 + content_w {
            x = x0;
            y += CHIP_ROW_HEIGHT + CHIP_ROW_GAP;
        }
        let is_active = active.get(i).copied().unwrap_or(false);
        let id = tree.add_button(
            parent,
            x,
            y,
            w,
            CHIP_ROW_HEIGHT,
            UIStyle {
                bg_color: if is_active { color::ACCENT_BLUE } else { CHIP_INACTIVE },
                hover_bg_color: if is_active { color::ACCENT_BLUE } else { CHIP_HOVER },
                corner_radius: CHIP_ROW_HEIGHT * 0.5,
                font_size: CELL_FONT,
                text_color: if is_active { Color32::WHITE } else { TEXT_DIM },
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            label,
        );
        ids.push(id);
        x += w + CHIP_SPACING;
    }
    let mut ids = ids.into_iter();
    let all = ids.next();
    (all, ids.collect())
}

/// The category chip labels with the "All" chip prepended — one ordered
/// list for [`build_chip_group`] (its first returned id is the "All" chip).
fn category_chip_labels(categories: &[String]) -> Vec<String> {
    std::iter::once("All".to_string())
        .chain(categories.iter().cloned())
        .collect()
}

fn category_color(category: &str) -> Color32 {
    match category {
        "Spatial" => CAT_SPATIAL,
        "Color" => CAT_COLOR,
        "Stylize" => CAT_STYLIZE,
        "Filmic" => CAT_FILMIC,
        "Geometry" => CAT_GEOMETRY,
        "Pattern" => CAT_PATTERN,
        "Sim" => CAT_SIM,
        "Text & Media" => CAT_TEXT_MEDIA,
        // LED generator presets (LED_STRIPS_DESIGN MVP-P3c) — the same
        // green the rest of the UI accents LED state with.
        "LED" => color::LED_COLOR,
        _ => TEXT_DIM,
    }
}

impl Overlay for BrowserPopupPanel {
    fn is_open(&self) -> bool {
        self.is_open()
    }

    fn modality(&self) -> Modality {
        // The popup builds its own full-screen backdrop node, so the driver
        // must not add a second scrim.
        Modality::Modal {
            dim_background: false,
        }
    }

    fn anchor(&self) -> Anchor {
        // Click-anchored and content-sized; positions itself in build().
        Anchor::SelfManaged
    }

    fn desired_size(&self) -> Vec2 {
        Vec2::ZERO
    }

    fn build_at(&mut self, tree: &mut UITree, placement: OverlayPlacement) {
        self.set_screen_size(placement.screen.x, placement.screen.y);
        self.build(tree);
    }

    fn on_event(&mut self, event: &UIEvent, _tree: &mut UITree) -> OverlayResponse {
        if !self.is_open() {
            return OverlayResponse::Ignored;
        }
        match event {
            UIEvent::KeyDown {
                key: key @ (Key::Escape
                | Key::Up
                | Key::Down
                | Key::Left
                | Key::Right
                | Key::Home
                | Key::End
                | Key::PageUp
                | Key::PageDown
                | Key::Enter),
                ..
            } => match self.handle_key_nav(*key) {
                Some(BrowserPopupAction::ActionSelected(action)) => {
                    OverlayResponse::Consumed(vec![action])
                }
                Some(BrowserPopupAction::Selected {
                    type_id,
                    mode,
                    tab,
                    layer_id,
                }) => {
                    let action = match mode {
                        BrowserPopupMode::Effect => PanelAction::Params(ParamsAction::AddEffect {
                            tab,
                            // The session's layer_id — captured at open from
                            // the invoking button — rides the pick
                            // atomically (PRESET_BROWSER_AUDITION D2);
                            // dispatch builds EffectTarget from it instead
                            // of re-resolving the active layer.
                            layer_id,
                            preset: crate::types::PresetTypeId::from_string(type_id),
                        }),
                        BrowserPopupMode::Generator => PanelAction::Project(ProjectAction::SetGenType(
                            layer_id,
                            crate::types::PresetTypeId::from_string(type_id),
                        )),
                        BrowserPopupMode::Actions => return OverlayResponse::Consumed(Vec::new()),
                        // Node mode is editor-window only; never reached on
                        // the main-window overlay path.
                        BrowserPopupMode::Node => return OverlayResponse::Consumed(Vec::new()),
                    };
                    OverlayResponse::Consumed(vec![action])
                }
                // Dismissed / Moved / Ignored, or a Node-mode pick (never
                // reached here — see above): nothing to dispatch, but the
                // modal still swallows the key so it never leaks to panels
                // beneath.
                _ => OverlayResponse::Consumed(Vec::new()),
            },
            UIEvent::Click { node_id, .. } => {
                if self.is_search_bar(*node_id) {
                    return OverlayResponse::Consumed(vec![PanelAction::Params(ParamsAction::BrowserSearchClicked)]);
                }
                match self.handle_click(*node_id) {
                    Some(BrowserPopupAction::ActionSelected(action)) => {
                        OverlayResponse::Consumed(vec![action])
                    }
                    Some(BrowserPopupAction::Selected {
                        type_id,
                        mode,
                        tab,
                        layer_id,
                    }) => {
                        let action = match mode {
                            BrowserPopupMode::Effect => PanelAction::Params(ParamsAction::AddEffect {
                                tab,
                                // Same atomic layer_id as the keyboard arm
                                // above (PRESET_BROWSER_AUDITION D2).
                                layer_id,
                                preset: crate::types::PresetTypeId::from_string(type_id),
                            }),
                            BrowserPopupMode::Generator => PanelAction::Project(ProjectAction::SetGenType(
                                layer_id,
                                crate::types::PresetTypeId::from_string(type_id),
                            )),
                            BrowserPopupMode::Actions => {
                                return OverlayResponse::Consumed(Vec::new());
                            }
                            // Node mode is editor-window only; never reached on
                            // the main-window overlay path.
                            BrowserPopupMode::Node => {
                                return OverlayResponse::Consumed(Vec::new());
                            }
                        };
                        OverlayResponse::Consumed(vec![action])
                    }
                    Some(BrowserPopupAction::Paste) => {
                        OverlayResponse::Consumed(vec![PanelAction::Params(ParamsAction::PasteEffects)])
                    }
                    // Dismissed (incl. backdrop), or an internal chip/category
                    // click that needs a rebuild — consume so the modal swallows
                    // it and the driver re-runs build_at next tick.
                    _ => OverlayResponse::Consumed(Vec::new()),
                }
            }
            UIEvent::Scroll { pos, delta, .. } => {
                // The wheel scrolls the grid only when it's over the grid
                // (F13) — over the search bar or the chips it does nothing.
                // Consumed either way so it can't leak to panels beneath.
                let over_grid = self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.picker.scroll.viewport().contains(*pos));
                if over_grid {
                    self.handle_scroll(delta.y);
                }
                OverlayResponse::Consumed(Vec::new())
            }
            // Right-click management menu (PRESET_LIBRARY_DESIGN P5, D6).
            // Deliberately does NOT close the popup — the menu (a
            // `DropdownPanel` the app opens) stacks on top of it, same as
            // the card's right-click menu stacks on top of the inspector.
            // Consumed either way (a miss, Factory cell, or Node mode still
            // swallows the click so it can't leak to panels beneath the
            // modal), matching every other outcome in this match.
            UIEvent::RightClick {
                node_id: Some(node_id),
                ..
            } => {
                let action = self.handle_right_click(*node_id).map(|ctx| {
                    PanelAction::Browser(BrowserAction::BrowserCellRightClicked(ctx.mode, ctx.type_id, ctx.source))
                });
                OverlayResponse::Consumed(action.into_iter().collect())
            }
            _ => OverlayResponse::Ignored,
        }
    }
}

/// A thumbnail cell's caption, wrapped to the cell's inset width.
fn caption_wrap(measurer: &dyn crate::text::TextMeasure, label: &str) -> Vec<String> {
    crate::text::wrap_to_width(
        measurer,
        label,
        CELL_LABEL_FONT,
        FontWeight::Regular,
        CELL_W - CAPTION_PAD_X * 2.0,
    )
}

/// Caption lines every row reserves: the longest wrapped thumbnail-item
/// label. Zero when no item has a thumbnail (flat grid, label in the button).
fn caption_line_count<'a>(
    measurer: &dyn crate::text::TextMeasure,
    items: impl Iterator<Item = &'a PickerItem>,
) -> usize {
    let mut any_thumb = false;
    let mut lines = 0;
    for item in items {
        any_thumb |= item.thumbnail.is_some();
        lines = lines.max(caption_wrap(measurer, &item.label).len());
    }
    if any_thumb { lines.max(1) } else { 0 }
}

fn grid_cell_h(mode: BrowserPopupMode, caption_lines: usize) -> f32 {
    match mode {
        BrowserPopupMode::Actions => 32.0,
        _ if caption_lines == 0 => CELL_H,
        _ => CELL_H + CAPTION_GAP + caption_lines as f32 * CELL_LABEL_LINE_H,
    }
}

/// Caption line rects for a cell at (`cell_x`, `cell_y`): stacked below the
/// thumbnail, never over it.
fn caption_line_rects(cell_x: f32, cell_y: f32, lines: usize) -> impl Iterator<Item = Rect> {
    (0..lines).map(move |i| {
        Rect::new(
            cell_x + CAPTION_PAD_X,
            cell_y + CELL_H + CAPTION_GAP + i as f32 * CELL_LABEL_LINE_H,
            CELL_W - CAPTION_PAD_X * 2.0,
            CELL_LABEL_LINE_H,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The longest factory preset names (live-GPU water presets and the
    /// em-dash variants) were the ones that spilled into neighbouring cells.
    const LONG_NAMES: &[&str] = &[
        "Water — Floating Box (Live GPU)",
        "Water — Dam Break (GPU Surface)",
        "Ordered Recon — Clip Gesture",
        "Surface Peel - Clip Hit",
        "Blob Track V2 — Colour",
        "Chromatic Aberration",
    ];

    fn rects_overlap(a: Rect, b: Rect) -> bool {
        a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
    }

    #[test]
    fn thumbnail_caption_fits_below_thumbnail_and_inside_cell() {
        let tree = UITree::new();
        let m = tree.measurer();
        let items: Vec<PickerItem> = LONG_NAMES
            .iter()
            .map(|n| PickerItem {
                label: n.to_string(),
                type_id: n.to_string(),
                category: None,
                search_text: None,
                source: None,
                thumbnail: Some(format!("/thumbs/{n}.png")),
            })
            .collect();
        let lines = caption_line_count(m, items.iter());
        let cell_h = grid_cell_h(BrowserPopupMode::Generator, lines);
        let thumb = Rect::new(0.0, 0.0, CELL_W, CELL_H);
        for item in &items {
            let wrapped = caption_wrap(m, &item.label);
            assert!(wrapped.len() <= lines, "{}: {} lines > reserved {lines}", item.label, wrapped.len());
            assert_eq!(wrapped.join(" "), item.label, "wrap must keep the full name");
            for (text, r) in wrapped.iter().zip(caption_line_rects(0.0, 0.0, lines)) {
                assert!(!rects_overlap(r, thumb), "{}: caption over thumbnail", item.label);
                assert!(r.y + r.height <= cell_h, "{}: caption past cell bottom", item.label);
                let w = m.measure_text(text, CELL_LABEL_FONT, FontWeight::Regular).x;
                assert!(w <= r.width, "{}: line {text:?} {w}px > {}px", item.label, r.width);
                assert!(r.x >= 0.0 && r.x + r.width <= CELL_W);
            }
        }
    }

    #[test]
    fn flat_grid_keeps_plain_cell_height() {
        let tree = UITree::new();
        let item = PickerItem {
            label: "Blur".into(),
            type_id: "blur".into(),
            category: None,
            search_text: None,
            source: None,
            thumbnail: None,
        };
        let lines = caption_line_count(tree.measurer(), std::iter::once(&item));
        assert_eq!(lines, 0);
        assert_eq!(grid_cell_h(BrowserPopupMode::Node, lines), CELL_H);
    }

    #[test]
    fn actions_picker_returns_typed_action_after_filter_and_enter() {
        let action = PanelAction::Params(ParamsAction::ShowAutomation(
            crate::panels::GraphParamTarget::Generator,
            "density".to_string().into(),
        ));
        let mut popup = BrowserPopupPanel::new();
        popup.open_actions(
            BrowserPopupRequest {
                mode: BrowserPopupMode::Actions,
                tab: InspectorTab::Layer,
                layer_id: None,
                items: vec![PickerItem {
                    label: "Noise · Density".to_string(),
                    type_id: "generator:density".to_string(),
                    category: Some("Generator".to_string()),
                    search_text: Some("noise density".to_string()),
                    source: None,
                    thumbnail: None,
                }],
                category_names: vec!["Generator".to_string()],
                spawn_graph_pos: None,
                paste_count: 0,
                screen_anchor: Vec2::ZERO,
            },
            vec![action.clone()],
            ActionListOptions::default(),
        );
        popup.set_filter("density".to_string());
        let Some(BrowserPopupAction::ActionSelected(selected)) = popup.handle_key_nav(Key::Enter) else {
            panic!("filtered action picker should select its first action");
        };
        assert!(matches!(selected, PanelAction::Params(ParamsAction::ShowAutomation(
            crate::panels::GraphParamTarget::Generator,
            ref param,
        )) if param.as_ref() == "density"));
        assert!(!popup.is_open());
    }

    fn actions_request(labels: &[&str]) -> BrowserPopupRequest {
        BrowserPopupRequest {
            mode: BrowserPopupMode::Actions,
            tab: InspectorTab::Layer,
            layer_id: None,
            items: labels
                .iter()
                .map(|label| PickerItem {
                    label: (*label).to_string(),
                    type_id: (*label).to_string(),
                    category: Some("Checks".to_string()),
                    search_text: None,
                    source: None,
                    thumbnail: None,
                })
                .collect(),
            category_names: vec!["Checks".to_string()],
            spawn_graph_pos: None,
            paste_count: 0,
            screen_anchor: Vec2::ZERO,
        }
    }

    #[test]
    fn actions_primary_enter_can_keep_picker_open() {
        let mut popup = BrowserPopupPanel::new();
        popup.open_actions(
            actions_request(&["Check"]),
            vec![PanelAction::Params(ParamsAction::PasteEffects)],
            ActionListOptions {
                keep_open: true,
                ..ActionListOptions::default()
            },
        );

        popup.handle_key_nav(Key::Down);
        assert!(matches!(
            popup.handle_key_nav(Key::Enter),
            Some(BrowserPopupAction::ActionSelected(PanelAction::Params(
                ParamsAction::PasteEffects
            )))
        ));
        assert!(popup.is_open());
    }

    #[test]
    fn actions_edit_button_returns_secondary_action_and_closes() {
        let mut popup = BrowserPopupPanel::new();
        popup.open_actions(
            actions_request(&["Check"]),
            vec![PanelAction::Params(ParamsAction::PasteEffects)],
            ActionListOptions {
                secondary_actions: vec![Some(PanelAction::Params(ParamsAction::BrowserSearchClicked))],
                ..ActionListOptions::default()
            },
        );
        let mut tree = UITree::new();
        popup.build(&mut tree);
        let edit_id = popup.session.as_ref().unwrap().layout.secondary_ids[0].0;
        assert!(matches!(
            popup.handle_click(edit_id),
            Some(BrowserPopupAction::ActionSelected(PanelAction::Params(
                ParamsAction::BrowserSearchClicked
            )))
        ));
        assert!(!popup.is_open());
    }

    #[test]
    fn refresh_actions_preserves_search_category_cursor_scroll_and_current() {
        let mut popup = BrowserPopupPanel::new();
        popup.open_actions(
            actions_request(&["A", "B"]),
            vec![
                PanelAction::Params(ParamsAction::PasteEffects),
                PanelAction::Params(ParamsAction::BrowserSearchClicked),
            ],
            ActionListOptions {
                current: Some(0),
                ..ActionListOptions::default()
            },
        );
        popup.set_filter("".to_string());
        popup.set_category(Some("Checks".to_string()));
        popup.handle_key_nav(Key::Down);
        popup.session.as_mut().unwrap().picker.scroll.set_content_height(100.0);
        popup.session.as_mut().unwrap().picker.scroll.set_scroll_offset(12.0);

        assert!(popup.refresh_actions(
            actions_request(&["B", "A", "C"]).items,
            vec!["Checks".to_string()],
            vec![
                PanelAction::Params(ParamsAction::BrowserSearchClicked),
                PanelAction::Params(ParamsAction::PasteEffects),
                PanelAction::Params(ParamsAction::PasteEffects),
            ],
            vec![None, None, None],
        ));
        let session = popup.session.as_ref().unwrap();
        assert_eq!(session.picker.filter(), "");
        assert_eq!(session.picker.active_category(), Some("Checks"));
        assert_eq!(session.picker.cursor(), Some(1));
        assert_eq!(session.picker.scroll.scroll_offset(), 12.0);
        assert_eq!(session.list.current, Some(1));
    }

    fn font_list(count: usize, current: usize) -> BrowserPopupPanel {
        let labels: Vec<String> = (0..count).map(|i| format!("Font {i:03}")).collect();
        let items = labels
            .iter()
            .map(|l| PickerItem {
                label: l.clone(),
                type_id: l.clone(),
                category: None,
                search_text: None,
                source: None,
                thumbnail: None,
            })
            .collect();
        let actions = labels
            .iter()
            .map(|l| PanelAction::Params(ParamsAction::GenStringParamSelected(0, l.clone())))
            .collect();
        let mut popup = BrowserPopupPanel::new();
        popup.set_screen_size(1280.0, 800.0);
        popup.open_actions(
            BrowserPopupRequest {
                mode: BrowserPopupMode::Actions,
                tab: InspectorTab::Layer,
                layer_id: None,
                items,
                category_names: Vec::new(),
                spawn_graph_pos: None,
                paste_count: 0,
                screen_anchor: Vec2::new(100.0, 100.0),
            },
            actions,
            ActionListOptions {
                empty_label: "No fonts match",
                label_in_own_font: true,
                current: Some(current),
                ..ActionListOptions::default()
            },
        );
        popup
    }

    #[test]
    fn font_list_opens_on_the_current_font_drawn_in_its_own_face() {
        let mut tree = UITree::new();
        let mut popup = font_list(300, 150);
        popup.build(&mut tree);
        let session = popup.session.as_ref().unwrap();
        assert_eq!(session.picker.cursor(), Some(150));
        let current_row = session
            .layout
            .cell_ids
            .iter()
            .find(|(_, m)| m.item_index == 150)
            .map(|(id, _)| *id)
            .expect("the current font's row is built, i.e. scrolled into view");
        let node = tree.get_node(current_row).unwrap();
        assert_eq!(node.font_family.as_deref(), Some("Font 150"));

        // Down moves from the current font, Enter picks the next one.
        popup.handle_key_nav(Key::Down);
        let Some(BrowserPopupAction::ActionSelected(PanelAction::Params(
            ParamsAction::GenStringParamSelected(_, picked),
        ))) = popup.handle_key_nav(Key::Enter)
        else {
            panic!("Enter picks a font");
        };
        assert_eq!(picked, "Font 151");
    }

    #[test]
    fn font_list_search_narrows_and_says_so_when_empty() {
        let mut tree = UITree::new();
        let mut popup = font_list(20, 0);
        popup.set_filter("font 01".to_string());
        assert_eq!(popup.picker().unwrap().filtered_len(), 10);
        popup.set_filter("zzz".to_string());
        popup.build(&mut tree);
        let found = (0..tree.count()).any(|i| {
            tree.get_node(tree.id_at(i)).and_then(|n| n.text.as_deref()) == Some("No fonts match")
        });
        assert!(found);
    }
}
