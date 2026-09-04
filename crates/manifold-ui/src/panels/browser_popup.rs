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
//! internally beyond that. Cell captions live in a real strip: label
//! bottom-left, badge bottom-right, one baseline, named insets. Chips are
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
// 16:9 — the audition atlas cells are 256×144 and the UV mapping assumes
// the aspect; do not resize cells off 16:9.

/// 16:9 cell size. THE ASPECT IS LOAD-BEARING (audition atlas UVs).
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
/// Caption strip + insets (F7/F8/F10): the strip backs the label and badge,
/// both sit INSIDE it on one baseline, and the x-insets are named here —
/// never space-padded prefixes.
const CAPTION_STRIP_H: f32 = 14.0;
const CAPTION_PAD_X: f32 = 5.0;
/// Height of the "No presets match" row when the filter empties the grid (F9).
const EMPTY_STATE_H: f32 = 44.0;
const CELL_FONT: u16 = color::FONT_LABEL;
const SEARCH_FONT: u16 = color::FONT_LABEL;

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
/// (PRESET_LIBRARY_DESIGN P6, D7) — dark enough that light label text reads
/// over any thumbnail content.
const CAPTION_STRIP_BG: Color32 = color::BROWSER_CELL_CAPTION_BG;
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

/// Per-cell metadata needed for click AND right-click routing. Selection only
/// needs `type_id`; the right-click management menu (PRESET_LIBRARY_DESIGN
/// P5) additionally needs the cell's classified source, and whether it's a
/// "missing from library" Snapshot entry (which gets no menu at all — an
/// auto-captured cache isn't user-manageable the way a `Saved` entry is).
#[derive(Clone)]
struct CellMeta {
    type_id: String,
    source: Option<Source>,
    missing_from_library: bool,
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
            anchor: Vec2::ZERO,
            backdrop_id: None,
            search_bar_id: None,
            chip_all_id: None,
            chip_ids: Vec::new(),
            source_all_id: None,
            source_chip_ids: Vec::new(),
            cell_ids: Vec::new(),
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
    layout: BrowserLayout,
}

// ── Panel ──

pub struct BrowserPopupPanel {
    // Config — survives across opens.
    screen_w: f32,
    screen_h: f32,
    session: Option<BrowserSession>,
    /// Live audition cell source (PRESET_BROWSER_AUDITION_DESIGN D1/D3):
    /// the shared audition-atlas texture handle + per-item UV rects, pushed
    /// per frame by the app while the browser is open. `None` = flat cells
    /// (Node mode, transport not up yet, or browser closed) — the cell
    /// renders exactly as before.
    audition_src: Option<(crate::node::TextureHandle, ahash::AHashMap<String, [f32; 4]>)>,
    /// Open/close/render-list transitions for the app pump to drain and
    /// forward over `ContentCommand` (the panel is pure UI — it never sends
    /// commands itself).
    audition_open_dirty: Option<AuditionOpenInfo>,
    audition_close_dirty: bool,
    last_render_list: Option<Vec<String>>,
    /// Search-focus request raised at open (F5): the app pump drains it and
    /// takes the owned search session, same as the graph-editor Node picker
    /// does at open. `None` once drained or for Node mode.
    search_focus_dirty: bool,
}

/// What the app needs to start an audition session on the content thread:
/// every item's `(type id, mode)` for `ensure_cells` (the app maps mode →
/// `PresetKind`; the UI crate doesn't see core types), plus the invocation
/// context for the tap (D2 — master vs layer).
#[derive(Debug, Clone)]
pub struct AuditionOpenInfo {
    pub items: Vec<(String, BrowserPopupMode)>,
    pub tab: InspectorTab,
    pub layer_id: Option<LayerId>,
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
            audition_src: None,
            audition_open_dirty: None,
            audition_close_dirty: false,
            last_render_list: None,
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
        // Audition is effect/generator-only (D1); Node mode renders flat
        // cells exactly as before and never dirties the session hooks.
        if req.mode != BrowserPopupMode::Node {
            self.audition_open_dirty = Some(AuditionOpenInfo {
                items: req
                    .items
                    .iter()
                    .map(|it| (it.type_id.clone(), req.mode))
                    .collect(),
                tab: req.tab,
                layer_id: req.layer_id.clone(),
            });
            self.last_render_list = None;
            self.search_focus_dirty = true;
        }
        let mut layout = BrowserLayout::new();
        layout.anchor = req.screen_anchor;
        self.session = Some(BrowserSession {
            mode: req.mode,
            tab: req.tab,
            layer_id: req.layer_id,
            picker: PickerCore::new(req.items, req.category_names),
            pending_spawn_graph_pos: req.spawn_graph_pos,
            paste_count: req.paste_count,
            layout,
        });
    }

    pub fn close(&mut self) {
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.mode != BrowserPopupMode::Node)
        {
            self.audition_close_dirty = true;
        }
        self.session = None;
    }

    /// Drain the open transition (once per open) — the app forwards it as
    /// the content-thread `ensure_cells` + tap selection.
    pub fn take_audition_open(&mut self) -> Option<AuditionOpenInfo> {
        self.audition_open_dirty.take()
    }

    /// Drain the close transition (once per close) — the app sends an empty
    /// render list, making a closed browser cost literally zero (D6/§6.8).
    pub fn take_audition_close(&mut self) -> bool {
        std::mem::take(&mut self.audition_close_dirty)
    }

    /// Drain the open-time search-focus request (once per open, non-Node
    /// modes) — the app pump takes the owned search session with it, same
    /// as the Node picker does at open (F5). The session's anchor is the
    /// app's problem: the popup tree doesn't exist yet at open, so the app
    /// re-anchors over the real search bar every frame until close.
    pub fn take_search_focus(&mut self) -> bool {
        std::mem::take(&mut self.search_focus_dirty)
    }

    /// The current filtered render list, `Some` only when it changed since
    /// the last drain (search typing / chip picks) — so a stable browse
    /// sends no per-frame commands at all.
    pub fn take_audition_render_list(&mut self) -> Option<Vec<String>> {
        let session = self.session.as_ref()?;
        if session.mode == BrowserPopupMode::Node {
            return None;
        }
        let current: Vec<String> = session
            .picker
            .filtered()
            .map(|(_, item)| item.type_id.clone())
            .collect();
        if self.last_render_list.as_ref() == Some(&current) {
            return None;
        }
        self.last_render_list = Some(current.clone());
        Some(current)
    }

    /// Install the live audition cell source for this frame (`None` clears —
    /// cells fall back to the static thumbnail / flat text). The app
    /// computes the atlas handle + per-item UVs from the content state.
    pub fn set_audition_src(
        &mut self,
        src: Option<(crate::node::TextureHandle, ahash::AHashMap<String, [f32; 4]>)>,
    ) {
        self.audition_src = src;
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
        session.layout.chip_ids.clear();
        session.layout.source_chip_ids.clear();

        let count = session.picker.filtered_len();
        let mode = session.mode;

        // ── Width: content-sized, screen-clamped ──
        //
        // The WIDTH derives from the FULL item set, not the filtered count:
        // typing filters inside a stable-sized popup (no per-keystroke
        // resize jitter), and the empty state keeps a sensible surface. The
        // GRID's columns below follow the filtered count.
        let chrome_w = (PADDING + BORDER) * 2.0;
        let inner_max_w = (screen_w - SCREEN_MARGIN * 2.0 - chrome_w).max(CELL_W);
        let cols_fit = ((inner_max_w + CELL_SPACING) / (CELL_W + CELL_SPACING))
            .floor()
            .max(1.0) as usize;
        let full_count = session.picker.all_items().count();
        // Never more columns than items (content-sized) and never more than
        // MAX_COLUMNS — 6-8 columns at 1080p-class windows (D12).
        let width_columns = cols_fit.min(MAX_COLUMNS).min(full_count.max(1));
        let inner_w =
            width_columns as f32 * CELL_W + width_columns.saturating_sub(1) as f32 * CELL_SPACING;
        let popup_w = (inner_w + chrome_w).min(screen_w);
        let content_w = popup_w - chrome_w;
        // Grid columns follow the FILTERED count — a narrow result set packs
        // into fewer columns inside the stable width.
        let columns = width_columns.min(count.max(1));

        // ── Chips: measured with the tree's real font metrics (F12), then
        // wrapped at the content edge — overflow wraps to another row, it
        // never silently runs past the popup. ──
        let has_source_row = mode != BrowserPopupMode::Node;
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
        let pitch = CELL_H + CELL_SPACING;
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
        let below = if session.paste_count > 0 {
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
            let vp = (max_total - above - below - PADDING - BORDER).max(CELL_H * 0.5);
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
                "No presets match",
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

        for (fi, (_, item)) in session.picker.filtered().enumerate() {
            let col = fi % columns;
            let row = fi / columns;
            // Relative Y for culling check (viewport-local)
            let rel_y = row as f32 * pitch - scroll_offset;

            // Cull cells entirely outside viewport
            if rel_y + CELL_H < 0.0 || rel_y > vp_h {
                continue;
            }

            let cell_x = cx + col as f32 * (CELL_W + CELL_SPACING);
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
                    CELL_H,
                    UIStyle {
                        bg_color: category_color(cat),
                        corner_radius: color::SMALL_RADIUS,
                        ..UIStyle::default()
                    },
                );
            }

            // Image cell: the live audition atlas at this item's UV takes
            // precedence (D1); else the save-time-rendered thumbnail; else a
            // flat-color cell exactly as before (D7's "clean fallback").
            // Both image paths get the caption strip; the label row and the
            // badge live INSIDE it on one baseline (F7/F8), with real named
            // x-insets — the space-padded prefix hack is gone (F10). All
            // non-interactive nodes paint BEFORE the button, so they never
            // shadow its click region and its hover/press tint composites
            // on top.
            let audition = self
                .audition_src
                .as_ref()
                .and_then(|(handle, map)| map.get(&item.type_id).map(|uv| (*handle, *uv)));
            let has_image = item.thumbnail.is_some() || audition.is_some();
            if let Some((handle, uv)) = audition {
                tree.add_image_uv(
                    clip_parent,
                    cell_x,
                    cell_y,
                    CELL_W,
                    CELL_H,
                    CELL_RADIUS,
                    handle,
                    uv,
                );
            } else if let Some(path) = item.thumbnail.as_deref() {
                let handle = crate::node::texture_handle_for_key(path);
                tree.add_image(clip_parent, cell_x, cell_y, CELL_W, CELL_H, CELL_RADIUS, handle);
            }

            if has_image {
                let strip_y = cell_y + CELL_H - CAPTION_STRIP_H;
                tree.add_panel(
                    clip_parent,
                    cell_x,
                    strip_y,
                    CELL_W,
                    CAPTION_STRIP_H,
                    UIStyle {
                        bg_color: CAPTION_STRIP_BG,
                        ..UIStyle::default()
                    },
                );
                tree.add_label(
                    clip_parent,
                    cell_x + CAPTION_PAD_X,
                    strip_y,
                    CELL_W - CAPTION_PAD_X * 2.0,
                    CAPTION_STRIP_H,
                    &item.label,
                    UIStyle {
                        font_size: CELL_FONT,
                        text_color: TEXT_PRIMARY,
                        ..UIStyle::default()
                    },
                );
                if let Some(badge) = item.badge.as_deref() {
                    tree.add_label(
                        clip_parent,
                        cell_x + CAPTION_PAD_X,
                        strip_y,
                        CELL_W - CAPTION_PAD_X * 2.0,
                        CAPTION_STRIP_H,
                        badge,
                        UIStyle {
                            font_size: CELL_FONT,
                            text_color: color::BROWSER_CELL_BADGE_TEXT,
                            text_align: TextAlign::Right,
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
            let id = tree.add_button(
                clip_parent,
                cell_x,
                cell_y,
                CELL_W,
                CELL_H,
                UIStyle {
                    bg_color: if has_image {
                        if is_cursor { CELL_HOVER_OVER_IMAGE } else { Color32::TRANSPARENT }
                    } else if is_cursor {
                        CELL_HOVER
                    } else {
                        CELL_NORMAL
                    },
                    hover_bg_color: if has_image { CELL_HOVER_OVER_IMAGE } else { CELL_HOVER },
                    pressed_bg_color: if has_image { CELL_PRESSED_OVER_IMAGE } else { CELL_PRESSED },
                    corner_radius: CELL_RADIUS,
                    font_size: CELL_FONT,
                    text_color: TEXT_PRIMARY,
                    text_inset_x: CAPTION_PAD_X,
                    ..UIStyle::default()
                },
                if has_image { "" } else { &item.label },
            );

            session.layout.cell_ids.push((
                id,
                CellMeta {
                    type_id: item.type_id.clone(),
                    source: item.source,
                    missing_from_library: item.missing_from_library,
                },
            ));
        }

        cy += vp_h;

        // Paste button
        if session.paste_count > 0 {
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
                let action = {
                    let session = self.session.as_ref()?;
                    let (_, meta) = &session.layout.cell_ids[i];
                    if session.mode == BrowserPopupMode::Node {
                        BrowserPopupAction::NodeSelected {
                            type_id: meta.type_id.clone(),
                            graph_pos: session.pending_spawn_graph_pos.unwrap_or((0.0, 0.0)),
                        }
                    } else {
                        BrowserPopupAction::Selected {
                            type_id: meta.type_id.clone(),
                            mode: session.mode,
                            tab: session.tab,
                            layer_id: session.layer_id.clone(),
                        }
                    }
                };
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
    /// graph-editor's add-node picker never gets this menu), a Factory cell
    /// (read-only, D6: "NOT Factory"), or a "missing from library" Snapshot
    /// entry (an auto-captured cache, not user-manageable the way a `Saved`
    /// entry is). Does NOT close the popup — the management menu (a
    /// `DropdownPanel` the caller opens) stacks on top of it, same as the
    /// card's right-click menu stacks on top of the inspector.
    pub fn handle_right_click(&self, node_id: NodeId) -> Option<BrowserCellContext> {
        let session = self.session.as_ref()?;
        if session.mode == BrowserPopupMode::Node {
            return None;
        }
        let (_, meta) = session.layout.cell_ids.iter().find(|(id, _)| *id == node_id)?;
        if meta.missing_from_library {
            return None;
        }
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
        let tab = session.tab;
        let layer_id = session.layer_id.clone();
        let spawn_pos = session.pending_spawn_graph_pos;
        let columns = session.layout.columns.max(1);
        let page = (session.layout.grid_viewport_height / (CELL_H + CELL_SPACING))
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
                .scroll_to_reveal(row as f32 * (CELL_H + CELL_SPACING), CELL_H);
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
        let rows = session.picker.filtered_len().div_ceil(columns);
        let content_h = rows as f32 * (CELL_H + CELL_SPACING) - CELL_SPACING;
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
