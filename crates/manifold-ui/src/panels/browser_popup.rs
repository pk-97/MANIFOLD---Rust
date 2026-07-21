//! Grid-based browser popup for effect/generator selection.
//! Port of Unity BrowserPopupPanel.cs (632 lines).
//!
//! A floating modal with search bar, category chips, scrollable grid,
//! and optional paste button. Completely separate from DropdownPanel —
//! different layout, interaction, and rendering model.
//!
//! `OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` §3/§4 (P1+P2): per-open state is a
//! [`BrowserSession`] constructed whole by [`BrowserPopupPanel::open`] and
//! dropped whole by [`BrowserPopupPanel::close`] — no field-by-field reset
//! list to keep in sync as fields get added. Filtering, category-chip
//! bookkeeping, and keyboard nav are owned by the shared `PickerCore`
//! (`picker_core.rs`); this file keeps session lifecycle plus grid/chip
//! rendering and click routing (drawing stays per-surface — see picker_core's
//! module doc).

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

// ── Layout constants (from Unity BrowserPopupPanel.cs + BrowserPopupLayout.cs) ──

const POPUP_WIDTH: f32 = 600.0;
const POPUP_MAX_HEIGHT: f32 = 550.0;
const PADDING: f32 = 12.5;
const BORDER: f32 = 1.0;
const CELL_WIDTH: f32 = 185.0;
const CELL_HEIGHT: f32 = 42.5;
const CELL_SPACING: f32 = 3.75;
const SEARCH_BAR_HEIGHT: f32 = 35.0;
const CHIP_ROW_HEIGHT: f32 = 25.0;
/// Source-filter row (PRESET_LIBRARY_DESIGN P5, D6) — same chip height as the
/// category row, rendered above it.
const SOURCE_ROW_HEIGHT: f32 = CHIP_ROW_HEIGHT;
const SECTION_SPACING: f32 = CELL_SPACING;
const PASTE_BUTTON_HEIGHT: f32 = 28.0;
const CELL_RADIUS: f32 = 6.0;
const CHIP_PAD_H: f32 = 10.0;
const CHIP_SPACING: f32 = 5.0;
const CHIP_FONT: f32 = 12.5;
const CELL_FONT: u16 = color::FONT_LABEL;
const SEARCH_FONT: u16 = color::FONT_LABEL;
const ACCENT_BAR_W: f32 = 3.0;

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
/// Caption-strip height + fill for an image cell's label legibility band
/// (PRESET_LIBRARY_DESIGN P6, D7) — dark enough that light label text reads
/// over any thumbnail content.
const CAPTION_STRIP_H: f32 = 14.0;
const CAPTION_STRIP_BG: Color32 = color::BROWSER_CELL_CAPTION_BG;
const CHIP_INACTIVE: Color32 = Color32::new(41, 41, 43, 255);
const CHIP_HOVER: Color32 = Color32::new(56, 56, 58, 255);
const PASTE_BG: Color32 = Color32::new(40, 40, 42, 255);
const PASTE_HOVER: Color32 = Color32::new(55, 55, 59, 255);
const SEARCH_HOVER: Color32 = Color32::new(38, 38, 40, 255);
const TEXT_PRIMARY: Color32 = Color32::new(224, 224, 224, 255);
const TEXT_DIM: Color32 = Color32::new(120, 120, 124, 255);

const CAT_SPATIAL: Color32 = Color32::new(102, 191, 191, 255);
const CAT_POST_PROCESS: Color32 = Color32::new(140, 160, 220, 255);
const CAT_FILMIC: Color32 = Color32::new(200, 180, 120, 255);
const CAT_SURVEILLANCE: Color32 = Color32::new(180, 100, 100, 255);


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

/// Node-range / rect output rebuilt every `build_at` — not meaningful state
/// to preserve across builds, so it's a plain rebuild-target, not part of the
/// session's semantic identity (kept as its own type only for readability).
struct BrowserLayout {
    columns: usize,
    grid_viewport_height: f32,
    total_height: f32,
    popup_x: f32,
    popup_y: f32,

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
            columns: 3,
            grid_viewport_height: 200.0,
            total_height: 300.0,
            popup_x: 100.0,
            popup_y: 100.0,
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

/// Per-open state (`OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` §3, D1) —
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
        }
    }

    /// Popups open instantly at full size/opacity (no
    /// enter/exit motion). Kept as a no-op so callers can still poll/call it
    /// unconditionally every frame without special-casing.
    pub fn update(&mut self, _tree: &mut UITree) {}

    pub fn is_open(&self) -> bool {
        self.session.is_some()
    }

    /// Always `false` now — popups no longer have an entrance tween to
    /// settle. Kept so call sites polling it (to force a rebuild while
    /// animating) don't need special-casing.
    pub fn is_animating(&self) -> bool {
        false
    }

    pub fn set_screen_size(&mut self, w: f32, h: f32) {
        self.screen_w = w;
        self.screen_h = h;
    }

    /// The live search filter text (empty when closed).
    pub fn current_filter(&self) -> &str {
        self.session.as_ref().map_or("", |s| s.picker.filter())
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
        self.session = Some(BrowserSession {
            mode: req.mode,
            tab: req.tab,
            layer_id: req.layer_id,
            picker: PickerCore::new(req.items, req.category_names),
            pending_spawn_graph_pos: req.spawn_graph_pos,
            paste_count: req.paste_count,
            layout: BrowserLayout::new(),
        });
        self.compute_layout(req.screen_anchor);
    }

    pub fn close(&mut self) {
        self.session = None;
    }

    /// Called when the search filter changes (from TextInputManager commit
    /// or a live keystroke).
    pub fn set_filter(&mut self, filter: String) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.picker.set_filter(filter);
        Self::recompute_height(session);
    }

    pub fn set_category(&mut self, category: Option<String>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.picker.set_category(category);
        Self::recompute_height(session);
    }

    /// Set the active source chip (`None` = "All" — PRESET_LIBRARY_DESIGN P5,
    /// D6). Mirrors [`Self::set_category`].
    pub fn set_source(&mut self, source: Option<Source>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.picker.set_source(source);
        Self::recompute_height(session);
    }

    // ── Layout ──

    fn compute_layout(&mut self, anchor: Vec2) {
        let screen_w = self.screen_w;
        let screen_h = self.screen_h;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let inner_w = POPUP_WIDTH - PADDING * 2.0 - BORDER * 2.0;
        session.layout.columns = ((inner_w + CELL_SPACING) / (CELL_WIDTH + CELL_SPACING))
            .floor()
            .max(1.0) as usize;
        Self::recompute_height(session);

        // Position: anchor the popup at the click position, edge-clamp
        session.layout.popup_x = anchor.x;
        session.layout.popup_y = anchor.y;

        if session.layout.popup_x + POPUP_WIDTH > screen_w {
            session.layout.popup_x = screen_w - POPUP_WIDTH;
        }
        if session.layout.popup_x < 0.0 {
            session.layout.popup_x = 0.0;
        }
        if session.layout.popup_y + session.layout.total_height > screen_h {
            session.layout.popup_y = screen_h - session.layout.total_height;
        }
        if session.layout.popup_y < 0.0 {
            session.layout.popup_y = 0.0;
        }
    }

    fn recompute_height(session: &mut BrowserSession) {
        let has_chips = !session.picker.categories().is_empty();
        // Source row (PRESET_LIBRARY_DESIGN P5, D6): Effect/Generator modes
        // only — Node mode (the graph-editor's add-node picker) has no
        // source concept, so it renders no row and gets no extra height.
        let has_source_row = session.mode != BrowserPopupMode::Node;
        let has_paste = session.paste_count > 0;
        let columns = session.layout.columns.max(1);
        let rows = session.picker.filtered_len().div_ceil(columns);
        let grid_content_h = rows as f32 * (CELL_HEIGHT + CELL_SPACING) - CELL_SPACING;

        let mut h = BORDER + PADDING;
        h += SEARCH_BAR_HEIGHT + SECTION_SPACING;
        if has_source_row {
            h += SOURCE_ROW_HEIGHT + SECTION_SPACING;
        }
        if has_chips {
            h += CHIP_ROW_HEIGHT + SECTION_SPACING;
        }

        let available = POPUP_MAX_HEIGHT
            - h
            - PADDING
            - BORDER
            - if has_paste {
                PASTE_BUTTON_HEIGHT + SECTION_SPACING
            } else {
                0.0
            };
        session.layout.grid_viewport_height = grid_content_h.min(available).max(CELL_HEIGHT);

        h += session.layout.grid_viewport_height;
        if has_paste {
            h += SECTION_SPACING + PASTE_BUTTON_HEIGHT;
        }
        h += PADDING + BORDER;
        session.layout.total_height = h;
    }

    // ── Build ──

    pub fn build(&mut self, tree: &mut UITree) {
        let screen_w = self.screen_w;
        let screen_h = self.screen_h;
        let Some(session) = self.session.as_mut() else {
            return;
        };

        session.layout.first_node = tree.count();
        session.layout.cell_ids.clear();
        session.layout.chip_ids.clear();

        // Popups appear instantly at full size/opacity (no
        // enter/exit motion). Every position below derives from these
        // four locals (never `session.layout.popup_x`/`total_height`
        // directly), so this is just the plain popup rect now.
        let px = session.layout.popup_x;
        let py = session.layout.popup_y;
        let pw = POPUP_WIDTH;
        let ph = session.layout.total_height;

        // Scrim + modal container via the shared shell (§17 lifts it with a
        // soft shadow; search bar / chips / grid are added on top as siblings).
        let shell = popup_shell::build(
            tree,
            (screen_w, screen_h),
            Rect::new(px, py, pw, ph),
            &popup_shell::PopupStyle::MODAL,
        );
        session.layout.backdrop_id = Some(shell.backdrop);

        let cx = px + BORDER + PADDING;
        let content_w = pw - BORDER * 2.0 - PADDING * 2.0;
        let mut cy = py + BORDER + PADDING;

        // Search bar
        let filter_text = session.picker.filter().to_string();
        session.layout.search_bar_id = Some(tree.add_button(
            None,
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
                ..UIStyle::default()
            },
            &if filter_text.is_empty() {
                "  Search...".to_string()
            } else {
                format!("  {filter_text}")
            },
        ));
        cy += SEARCH_BAR_HEIGHT + SECTION_SPACING;

        // Source filter row (PRESET_LIBRARY_DESIGN P5, D6): "All · Factory ·
        // My Library · This Project", above the category chips. Node mode
        // (the graph-editor's add-node picker) has no source concept, so it
        // renders no row — mirrors `recompute_height`'s `has_source_row` gate.
        session.layout.source_all_id = None;
        session.layout.source_chip_ids.clear();
        if session.mode != BrowserPopupMode::Node {
            let active_source = session.picker.active_source();
            let mut chip_x = cx;
            let chip_h = SOURCE_ROW_HEIGHT;

            let all_active = active_source.is_none();
            let all_w = estimate_chip_width("All");
            session.layout.source_all_id = Some(tree.add_button(
                None,
                chip_x,
                cy,
                all_w,
                chip_h,
                UIStyle {
                    bg_color: if all_active { color::ACCENT_BLUE } else { CHIP_INACTIVE },
                    hover_bg_color: if all_active { color::ACCENT_BLUE } else { CHIP_HOVER },
                    corner_radius: chip_h * 0.5,
                    font_size: CELL_FONT,
                    text_color: if all_active { Color32::WHITE } else { TEXT_DIM },
                    ..UIStyle::default()
                },
                "All",
            ));
            chip_x += all_w + CHIP_SPACING;

            for (src, label) in SOURCE_CHIPS {
                let is_active = active_source == Some(src);
                let w = estimate_chip_width(label);
                let id = tree.add_button(
                    None,
                    chip_x,
                    cy,
                    w,
                    chip_h,
                    UIStyle {
                        bg_color: if is_active { color::ACCENT_BLUE } else { CHIP_INACTIVE },
                        hover_bg_color: if is_active { color::ACCENT_BLUE } else { CHIP_HOVER },
                        corner_radius: chip_h * 0.5,
                        font_size: CELL_FONT,
                        text_color: if is_active { Color32::WHITE } else { TEXT_DIM },
                        ..UIStyle::default()
                    },
                    &format!(" {label} "),
                );
                session.layout.source_chip_ids.push(id);
                chip_x += w + CHIP_SPACING;
            }
            cy += SOURCE_ROW_HEIGHT + SECTION_SPACING;
        }

        // Category chips — cloned out so the loop below can hold `session`
        // mutably (chip_ids.push) without also borrowing `picker.categories()`.
        let category_names: Vec<String> = session.picker.categories().to_vec();
        let active_category = session.picker.active_category().map(str::to_string);
        if !category_names.is_empty() {
            let mut chip_x = cx;
            let chip_h = CHIP_ROW_HEIGHT;

            // "All" chip
            let all_active = active_category.is_none();
            let all_w = estimate_chip_width("All");
            session.layout.chip_all_id = Some(tree.add_button(
                None,
                chip_x,
                cy,
                all_w,
                chip_h,
                UIStyle {
                    bg_color: if all_active {
                        color::ACCENT_BLUE
                    } else {
                        CHIP_INACTIVE
                    },
                    hover_bg_color: if all_active {
                        color::ACCENT_BLUE
                    } else {
                        CHIP_HOVER
                    },
                    corner_radius: chip_h * 0.5,
                    font_size: CELL_FONT,
                    text_color: if all_active { Color32::WHITE } else { TEXT_DIM },
                    ..UIStyle::default()
                },
                "All",
            ));
            chip_x += all_w + CHIP_SPACING;

            for cat in &category_names {
                if cat == "Generators" {
                    continue;
                } // Don't show "Generators" in effect browser
                let is_active = active_category.as_deref() == Some(cat.as_str());
                let w = estimate_chip_width(cat);
                let id = tree.add_button(
                    None,
                    chip_x,
                    cy,
                    w,
                    chip_h,
                    UIStyle {
                        bg_color: if is_active {
                            color::ACCENT_BLUE
                        } else {
                            CHIP_INACTIVE
                        },
                        hover_bg_color: if is_active {
                            color::ACCENT_BLUE
                        } else {
                            CHIP_HOVER
                        },
                        corner_radius: chip_h * 0.5,
                        font_size: CELL_FONT,
                        text_color: if is_active { Color32::WHITE } else { TEXT_DIM },
                        ..UIStyle::default()
                    },
                    &format!(" {cat} "),
                );
                session.layout.chip_ids.push(id);
                chip_x += w + CHIP_SPACING;
            }
            cy += CHIP_ROW_HEIGHT + SECTION_SPACING;
        }

        // Grid viewport — ClipRegion clips cells that extend beyond bounds.
        let vp_top = cy;
        let vp_h = session.layout.grid_viewport_height;

        let clip_parent = Some(
            session
                .picker
                .scroll
                .begin(tree, Rect::new(cx, vp_top, content_w, vp_h)),
        );

        let columns = session.layout.columns;
        let scroll_offset = session.picker.scroll.scroll_offset();
        let cursor = session.picker.cursor();
        let has_categories = !category_names.is_empty();

        for (fi, (_, item)) in session.picker.filtered().enumerate() {
            let col = fi % columns;
            let row = fi / columns;
            // Relative Y for culling check (viewport-local)
            let rel_y = row as f32 * (CELL_HEIGHT + CELL_SPACING) - scroll_offset;

            // Cull cells entirely outside viewport
            if rel_y + CELL_HEIGHT < 0.0 || rel_y > vp_h {
                continue;
            }

            let cell_x = cx + col as f32 * (CELL_WIDTH + CELL_SPACING);
            let cell_y = vp_top + rel_y;

            // Category accent bar
            if let Some(cat) = item.category.as_deref()
                && !cat.is_empty()
            {
                let accent_color = category_color(cat);
                tree.add_panel(
                    clip_parent,
                    cell_x,
                    cell_y,
                    ACCENT_BAR_W,
                    CELL_HEIGHT,
                    UIStyle {
                        bg_color: accent_color,
                        corner_radius: color::SMALL_RADIUS,
                        ..UIStyle::default()
                    },
                );
            }

            // Image cell (PRESET_LIBRARY_DESIGN P6, D7): a save-time-rendered
            // thumbnail fills the body, with a dark caption strip behind the
            // label for legibility over arbitrary thumbnail content. Both are
            // non-interactive and painted BEFORE the button below, so they
            // never shadow its click region and the button's own (in this
            // case transparent) fill + hover/press tint composite on top.
            // No thumbnail → skip entirely, today's flat-color cell exactly
            // (D7's "clean fallback").
            let has_thumbnail = item.thumbnail.is_some();
            if let Some(path) = item.thumbnail.as_deref() {
                let handle = crate::node::texture_handle_for_key(path);
                tree.add_image(clip_parent, cell_x, cell_y, CELL_WIDTH, CELL_HEIGHT, CELL_RADIUS, handle);
                tree.add_panel(
                    clip_parent,
                    cell_x,
                    cell_y + CELL_HEIGHT - CAPTION_STRIP_H,
                    CELL_WIDTH,
                    CAPTION_STRIP_H,
                    UIStyle {
                        bg_color: CAPTION_STRIP_BG,
                        ..UIStyle::default()
                    },
                );
            }

            // Cell button — full height, ClipRegion handles visual clipping.
            // The keyboard cursor (P2 arrow nav) reuses the existing hover
            // tint rather than a new design token — a highlighted cell reads
            // identically whether the mouse or the keyboard put it there.
            // Over a thumbnail the fill is transparent (the image already
            // fills the body) and the hover/press tints turn translucent so
            // interaction feedback still shows without blotting the picture.
            let prefix = if has_categories { "     " } else { "  " };
            let label = format!("{prefix}{}", item.label);
            let is_cursor = cursor == Some(fi);
            let id = tree.add_button(
                clip_parent,
                cell_x,
                cell_y,
                CELL_WIDTH,
                CELL_HEIGHT,
                UIStyle {
                    bg_color: if has_thumbnail {
                        if is_cursor { CELL_HOVER_OVER_IMAGE } else { Color32::TRANSPARENT }
                    } else if is_cursor {
                        CELL_HOVER
                    } else {
                        CELL_NORMAL
                    },
                    hover_bg_color: if has_thumbnail { CELL_HOVER_OVER_IMAGE } else { CELL_HOVER },
                    pressed_bg_color: if has_thumbnail { CELL_PRESSED_OVER_IMAGE } else { CELL_PRESSED },
                    corner_radius: CELL_RADIUS,
                    font_size: CELL_FONT,
                    text_color: TEXT_PRIMARY,
                    ..UIStyle::default()
                },
                &label,
            );

            // Origin badge (PRESET_LIBRARY_DESIGN P5, D6) — a non-interactive
            // label so it never shadows the cell button's click region.
            // Bottom-right corner, tiny caption font: metadata about the
            // cell, not a call to action.
            if let Some(badge) = item.badge.as_deref() {
                tree.add_label(
                    clip_parent,
                    cell_x,
                    cell_y + CELL_HEIGHT - 13.0,
                    CELL_WIDTH - 8.0,
                    12.0,
                    badge,
                    UIStyle {
                        font_size: color::FONT_CAPTION,
                        text_color: color::BROWSER_CELL_BADGE_TEXT,
                        text_align: TextAlign::Right,
                        ..UIStyle::default()
                    },
                );
            }

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
                None,
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
        let session = self.session.as_ref()?;

        // Copy out everything needed before any `&mut self` call below (close/
        // set_category) — avoids holding a `session` borrow across those calls.
        let backdrop_id = session.layout.backdrop_id;
        let search_bar_id = session.layout.search_bar_id;
        let chip_all_id = session.layout.chip_all_id;
        let chip_ids = session.layout.chip_ids.clone();
        let source_all_id = session.layout.source_all_id;
        let source_chip_ids = session.layout.source_chip_ids.clone();
        let cell_ids = session.layout.cell_ids.clone();
        let paste_id = session.layout.paste_id;
        let cat_names: Vec<String> = session
            .picker
            .categories()
            .iter()
            .filter(|c| c.as_str() != "Generators")
            .cloned()
            .collect();
        let mode = session.mode;
        let tab = session.tab;
        let layer_id = session.layer_id.clone();
        let spawn_pos = session.pending_spawn_graph_pos;

        if backdrop_id == Some(node_id) {
            self.close();
            return Some(BrowserPopupAction::Dismissed);
        }

        // Search bar → signal to open text input
        if search_bar_id == Some(node_id) {
            return None; // Caller checks is_search_bar()
        }

        // "All" chip
        if chip_all_id == Some(node_id) {
            self.set_category(None);
            return None; // Needs rebuild, no action
        }

        // Category chips
        for (i, chip_id) in chip_ids.iter().enumerate() {
            if node_id == *chip_id && i < cat_names.len() {
                self.set_category(Some(cat_names[i].clone()));
                return None; // Needs rebuild
            }
        }

        // Source "All" chip (PRESET_LIBRARY_DESIGN P5, D6)
        if source_all_id == Some(node_id) {
            self.set_source(None);
            return None; // Needs rebuild
        }

        // Source chips
        for (i, chip_id) in source_chip_ids.iter().enumerate() {
            if node_id == *chip_id && i < SOURCE_CHIPS.len() {
                self.set_source(Some(SOURCE_CHIPS[i].0));
                return None; // Needs rebuild
            }
        }

        // Grid cells
        for (cell_id, meta) in &cell_ids {
            if node_id == *cell_id {
                let action = if mode == BrowserPopupMode::Node {
                    BrowserPopupAction::NodeSelected {
                        type_id: meta.type_id.clone(),
                        graph_pos: spawn_pos.unwrap_or((0.0, 0.0)),
                    }
                } else {
                    BrowserPopupAction::Selected {
                        type_id: meta.type_id.clone(),
                        mode,
                        tab,
                        layer_id: layer_id.clone(),
                    }
                };
                self.close();
                return Some(action);
            }
        }

        // Paste button
        if paste_id == Some(node_id) {
            self.close();
            return Some(BrowserPopupAction::Paste);
        }

        None
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

    /// Up/Down/Enter/Escape keyboard nav (P2) — arrows move the grid cursor
    /// with wrap, Enter picks (the type-and-enter fast path picks
    /// `filtered[0]` with no cursor and a non-empty filter), Escape dismisses.
    /// Mirrors `handle_click`'s action shape so callers dispatch identically
    /// regardless of whether the pick came from the mouse or the keyboard.
    pub fn handle_key_nav(&mut self, key: Key) -> Option<BrowserPopupAction> {
        let session = self.session.as_mut()?;
        let mode = session.mode;
        let tab = session.tab;
        let layer_id = session.layer_id.clone();
        let spawn_pos = session.pending_spawn_graph_pos;

        let nav = session.picker.key_nav(key);
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

    /// Handle mouse wheel scroll within the popup.
    pub fn handle_scroll(&mut self, delta: f32) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let columns = session.layout.columns.max(1);
        let rows = session.picker.filtered_len().div_ceil(columns);
        let content_h = rows as f32 * (CELL_HEIGHT + CELL_SPACING) - CELL_SPACING;
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

fn estimate_chip_width(label: &str) -> f32 {
    label.len() as f32 * CHIP_FONT * 0.6 + CHIP_PAD_H * 2.0
}

fn category_color(category: &str) -> Color32 {
    match category {
        "Spatial" => CAT_SPATIAL,
        "Post-Process" => CAT_POST_PROCESS,
        "Filmic" => CAT_FILMIC,
        "Surveillance" => CAT_SURVEILLANCE,
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
                key: key @ (Key::Escape | Key::Up | Key::Down | Key::Enter),
                ..
            } => match self.handle_key_nav(*key) {
                Some(BrowserPopupAction::Selected {
                    type_id,
                    mode,
                    tab,
                    layer_id,
                }) => {
                    let action = match mode {
                        BrowserPopupMode::Effect => PanelAction::Params(ParamsAction::AddEffect(
                            tab,
                            crate::types::PresetTypeId::from_string(type_id),
                        )),
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
                            BrowserPopupMode::Effect => PanelAction::Params(ParamsAction::AddEffect(
                                tab,
                                crate::types::PresetTypeId::from_string(type_id),
                            )),
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
            UIEvent::Scroll { delta, .. } => {
                self.handle_scroll(delta.y);
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
