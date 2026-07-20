//! Two-column Ableton macro picker popup.
//!
//! Opens on "Map to Ableton…" from a param right-click.
//! Left column: Ableton tracks that have rack devices (click to select).
//! Right column: Macros on the selected track (click to map and close).
//!
//! Follows the same open/close/build/handle_click contract as BrowserPopupPanel.
//! Does NOT depend on manifold_playback — callers pass `AbletonPickerSession`
//! which is constructed from the bridge session in manifold-app.

use crate::types::{AbletonDeviceIdentity, AbletonMacroAddress, is_default_macro_name};

use super::overlay::{Anchor, Modality, Overlay, OverlayPlacement, OverlayResponse};
use super::popup_shell;
use crate::color;
use crate::input::{Key, UIEvent};
use crate::node::*;
use crate::tree::UITree;

// ── Layout ────────────────────────────────────────────────────────

const POPUP_W: f32 = 510.0;
const PADDING: f32 = 10.0;
const BORDER: f32 = 1.0;
const LEFT_COL_W: f32 = 185.0;
const DIVIDER_W: f32 = 1.0;
/// Width of the right column content area.
const RIGHT_COL_W: f32 = POPUP_W - PADDING * 2.0 - BORDER * 2.0 - LEFT_COL_W - DIVIDER_W - 4.0;
const HEADER_H: f32 = 28.0;
const ITEM_H: f32 = 26.0;
const SECTION_H: f32 = 20.0; // device-name section header in right column
const MAX_POPUP_H: f32 = 480.0;
const MIN_POPUP_H: f32 = 120.0;

// ── Colors ────────────────────────────────────────────────────────

const HEADER_BG: Color32 = Color32::new(28, 28, 30, 255);
const TRACK_NORMAL: Color32 = Color32::new(36, 36, 38, 255);
const TRACK_HOVER: Color32 = Color32::new(51, 51, 56, 255);
const TRACK_SELECTED_BG: Color32 = Color32::new(38, 52, 80, 255);
const TRACK_SELECTED_HOVER: Color32 = Color32::new(46, 62, 95, 255);
const MACRO_NORMAL: Color32 = Color32::new(36, 36, 38, 255);
const MACRO_HOVER: Color32 = Color32::new(51, 51, 56, 255);
const MACRO_PRESSED: Color32 = Color32::new(46, 46, 48, 255);
const TEXT_HEADER: Color32 = Color32::new(100, 100, 105, 255);
const TEXT_TRACK: Color32 = Color32::new(200, 200, 202, 255);
const TEXT_MACRO: Color32 = Color32::new(220, 220, 222, 255);
const TEXT_SECTION: Color32 = Color32::new(100, 140, 200, 255);
const TEXT_DIM: Color32 = Color32::new(90, 90, 94, 255);
const DIVIDER_COLOR: Color32 = Color32::new(52, 52, 56, 255);
const SELECTED_ARROW: Color32 = Color32::new(100, 150, 220, 255);

// ── Input data (plain structs, no manifold_playback dependency) ───

/// A macro on a rack device.
#[derive(Clone)]
pub struct PickerMacro {
    pub param_id: i32,
    pub name: String,
}

/// A rack device on a track.
#[derive(Clone)]
pub struct PickerDevice {
    pub device_id: i32,
    pub device_name: String,
    pub device_class_name: String,
    pub macros: Vec<PickerMacro>,
}

/// A track that has at least one rack device.
#[derive(Clone)]
pub struct PickerTrack {
    pub track_id: i32,
    pub track_name: String,
    /// Only rack devices (those with macros).
    pub devices: Vec<PickerDevice>,
}

/// Flat session data passed to `open()`. Built by the app layer from AbletonSession.
pub struct AbletonPickerSession {
    pub rack_tracks: Vec<PickerTrack>,
}

// ── Public API ────────────────────────────────────────────────────

/// Result of a picker interaction.
#[derive(Debug, Clone)]
pub enum AbletonPickerAction {
    /// User selected a macro to map.
    Selected(AbletonMacroAddress),
    /// User dismissed without selecting.
    Dismissed,
}

/// Context stored by the caller so it knows which param to map when
/// the picker resolves.
///
/// Phase 2 of the bindings unification plan keys per-param contexts
/// by [`ParamId`], not positional `param_idx`. `fx_idx` (effect's
/// chain position) and `slot_idx` (macro bank slot) stay positional
/// because those identities ARE structural positions.
#[derive(Debug, Clone)]
pub enum AbletonPickerContext {
    /// A preset param (effect or generator), addressed by the unified
    /// [`GraphParamTarget`](super::GraphParamTarget). The mapping target +
    /// inspector tab are resolved at dispatch time — the SAME path the
    /// `UnmapParamAbleton` action uses — so the context carries only identity,
    /// not a pre-resolved tab/index. This is what keeps map and unmap on one
    /// code path instead of two parallel effect/generator arms.
    Param {
        gpt: super::GraphParamTarget,
        param_id: manifold_foundation::ParamId,
    },
    MacroSlot {
        slot_idx: usize,
    },
}

pub struct AbletonPickerPopup {
    is_open: bool,
    rack_tracks: Vec<PickerTrack>,
    selected_track_idx: Option<usize>,

    popup_x: f32,
    popup_y: f32,
    popup_h: f32,

    screen_w: f32,
    screen_h: f32,

    backdrop_id: Option<NodeId>,
    track_row_ids: Vec<NodeId>,
    /// (node_id, address) for each visible macro item.
    macro_item_ids: Vec<(NodeId, AbletonMacroAddress)>,
    first_node: usize,
    node_count: usize,
    /// Selection captured by `Overlay::on_event`, drained by the app-layer
    /// overlay driver and lowered against `UIRoot`'s picker context. The picker
    /// can't form the `MapParamToAbleton` action itself — the context (which
    /// param / macro slot) lives on `UIRoot`.
    pending_selection: Option<AbletonMacroAddress>,
}

impl Default for AbletonPickerPopup {
    fn default() -> Self {
        Self::new()
    }
}

impl AbletonPickerPopup {
    pub fn new() -> Self {
        Self {
            is_open: false,
            rack_tracks: Vec::new(),
            selected_track_idx: None,
            popup_x: 0.0,
            popup_y: 0.0,
            popup_h: 0.0,
            screen_w: 1920.0,
            screen_h: 1080.0,
            backdrop_id: None,
            track_row_ids: Vec::new(),
            macro_item_ids: Vec::new(),
            first_node: 0,
            node_count: 0,
            pending_selection: None,
        }
    }

    /// Popups open instantly at full size/opacity (no
    /// enter/exit motion). Kept as a no-op so callers can still call it
    /// unconditionally every frame without special-casing.
    pub fn update(&mut self, _tree: &mut UITree) {}

    /// Drain the macro address selected since the last call (set by
    /// `Overlay::on_event`). The app lowers it against its picker context.
    pub fn take_pending_selection(&mut self) -> Option<AbletonMacroAddress> {
        self.pending_selection.take()
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// Always `false` now — popups no longer have an entrance tween to
    /// settle. Kept so call sites polling it (to force a rebuild while
    /// animating) don't need special-casing.
    pub fn is_animating(&self) -> bool {
        false
    }

    pub fn first_node(&self) -> usize {
        self.first_node
    }

    pub fn set_screen_size(&mut self, w: f32, h: f32) {
        self.screen_w = w;
        self.screen_h = h;
    }

    /// Open the picker anchored near `anchor` (screen-space cursor position).
    pub fn open(&mut self, session: AbletonPickerSession, anchor: Vec2) {
        self.rack_tracks = session.rack_tracks;
        // Auto-select first track so right column is immediately populated.
        self.selected_track_idx = if self.rack_tracks.is_empty() {
            None
        } else {
            Some(0)
        };
        self.is_open = true;
        self.compute_layout(anchor);
    }

    /// Refresh picker data while it's already open (e.g. after re-discovery).
    /// Preserves the current track selection if the track still exists.
    pub fn update_session(&mut self, session: AbletonPickerSession) {
        if !self.is_open {
            return;
        }
        let prev_track_name = self
            .selected_track_idx
            .and_then(|i| self.rack_tracks.get(i))
            .map(|t| t.track_name.clone());
        self.rack_tracks = session.rack_tracks;
        // Try to preserve selection by matching track name.
        self.selected_track_idx = prev_track_name
            .and_then(|name| self.rack_tracks.iter().position(|t| t.track_name == name))
            .or(if self.rack_tracks.is_empty() {
                None
            } else {
                Some(0)
            });
    }

    pub fn close(&mut self) {
        self.is_open = false;
        self.rack_tracks.clear();
        self.selected_track_idx = None;
        self.track_row_ids.clear();
        self.macro_item_ids.clear();
    }

    /// Call once per frame (inside the tree-rebuild pass) when `is_open`.
    pub fn build(&mut self, tree: &mut UITree) {
        if !self.is_open {
            return;
        }

        self.first_node = tree.count();
        self.track_row_ids.clear();
        self.macro_item_ids.clear();

        // Popups appear instantly at full size/opacity (no
        // enter/exit motion). Every position below derives from these
        // four locals (never `self.popup_x`/`self.popup_h` directly), so
        // this is just the plain popup rect now.
        let px = self.popup_x;
        let py = self.popup_y;
        let pw = POPUP_W;
        let ph = self.popup_h;

        // Scrim + modal container via the shared shell (§17 lifts it with a
        // soft shadow; the header + columns are added on top as siblings).
        let shell = popup_shell::build(
            tree,
            (self.screen_w, self.screen_h),
            Rect::new(px, py, pw, ph),
            &popup_shell::PopupStyle::MODAL,
        );
        self.backdrop_id = Some(shell.backdrop);

        let content_x = px + BORDER + PADDING;
        let content_y = py + BORDER + PADDING;
        let content_h = ph - BORDER * 2.0 - PADDING * 2.0;

        // ── Header row ────────────────────────────────────────────

        tree.add_panel(
            None,
            px + BORDER,
            py + BORDER,
            pw - BORDER * 2.0,
            HEADER_H + PADDING,
            UIStyle {
                bg_color: HEADER_BG,
                corner_radius: color::POPUP_RADIUS,
                ..UIStyle::default()
            },
        );

        // "Ableton Tracks" label
        tree.add_label(
            None,
            content_x,
            content_y,
            LEFT_COL_W,
            HEADER_H,
            "Ableton Tracks",
            UIStyle {
                text_color: TEXT_HEADER,
                font_size: color::FONT_LABEL,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );

        // "Macros" label
        let right_x = content_x + LEFT_COL_W + DIVIDER_W + 4.0;
        tree.add_label(
            None,
            right_x,
            content_y,
            RIGHT_COL_W,
            HEADER_H,
            "Macros",
            UIStyle {
                text_color: TEXT_HEADER,
                font_size: color::FONT_LABEL,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );

        // Header separator line
        let sep_y = content_y + HEADER_H + 1.0;
        tree.add_panel(
            None,
            px + BORDER,
            sep_y,
            pw - BORDER * 2.0,
            1.0,
            UIStyle {
                bg_color: DIVIDER_COLOR,
                ..UIStyle::default()
            },
        );

        let body_y = sep_y + 2.0;
        let _body_h = content_h - HEADER_H - 3.0;

        // ── Vertical divider ──────────────────────────────────────

        let div_x = content_x + LEFT_COL_W;
        let divider_h = ph - BORDER * 2.0 - PADDING - (HEADER_H + 3.0) - PADDING;
        tree.add_panel(
            None,
            div_x,
            body_y,
            DIVIDER_W,
            divider_h,
            UIStyle {
                bg_color: DIVIDER_COLOR,
                ..UIStyle::default()
            },
        );

        // ── Left column: track rows ───────────────────────────────

        if self.rack_tracks.is_empty() {
            tree.add_label(
                None,
                content_x,
                body_y + 8.0,
                LEFT_COL_W,
                ITEM_H,
                "No racks found",
                UIStyle {
                    text_color: TEXT_DIM,
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
        } else {
            for (i, track) in self.rack_tracks.iter().enumerate() {
                let is_selected = self.selected_track_idx == Some(i);
                let row_y = body_y + i as f32 * ITEM_H;

                let (bg, hover_bg) = if is_selected {
                    (TRACK_SELECTED_BG, TRACK_SELECTED_HOVER)
                } else {
                    (TRACK_NORMAL, TRACK_HOVER)
                };

                let id = tree.add_button(
                    None,
                    content_x,
                    row_y,
                    LEFT_COL_W - 2.0,
                    ITEM_H,
                    UIStyle {
                        bg_color: bg,
                        hover_bg_color: hover_bg,
                        pressed_bg_color: hover_bg,
                        text_color: TEXT_TRACK,
                        font_size: color::FONT_LABEL,
                        text_align: TextAlign::Left,
                        corner_radius: color::BUTTON_RADIUS,
                        ..UIStyle::default()
                    },
                    &format!("  {}", track.track_name),
                );
                self.track_row_ids.push(id);

                // Selection arrow
                if is_selected {
                    tree.add_label(
                        None,
                        content_x + LEFT_COL_W - 14.0,
                        row_y,
                        12.0,
                        ITEM_H,
                        "▶",
                        UIStyle {
                            text_color: SELECTED_ARROW,
                            font_size: color::FONT_LABEL,
                            text_align: TextAlign::Left,
                            ..UIStyle::default()
                        },
                    );
                }
            }
        }

        // ── Right column: macros for selected track ───────────────

        let right_content_x = right_x;

        if let Some(sel_idx) = self.selected_track_idx {
            if let Some(track) = self.rack_tracks.get(sel_idx) {
                let mut ry = body_y;
                let track_name = track.track_name.clone();

                for (di, device) in track.devices.iter().enumerate() {
                    // Skip the entire device if no macros are renamed —
                    // a device of nothing-but-defaults has no mappable
                    // surface and shouldn't take up picker space.
                    if device.macros.iter().all(|m| is_default_macro_name(&m.name)) {
                        continue;
                    }
                    // Device name section header (non-interactive)
                    tree.add_label(
                        None,
                        right_content_x,
                        ry + 2.0,
                        RIGHT_COL_W,
                        SECTION_H,
                        &device.device_name,
                        UIStyle {
                            text_color: TEXT_SECTION,
                            font_size: color::FONT_LABEL,
                            text_align: TextAlign::Left,
                            ..UIStyle::default()
                        },
                    );
                    ry += SECTION_H + 2.0;

                    for mac in &device.macros {
                        // Skip unrenamed default macros ("Macro 1".."Macro 8").
                        // Mapping these is what corrupts projects: a previous
                        // resolver could silently rebind a stale "Macro N"
                        // mapping to a totally different rack at the same
                        // numeric coordinates, baking the wrong names into the
                        // file. By forbidding them here we make every stored
                        // mapping name a hand-typed user choice — which means
                        // the resolver's name-based lookups can never land on
                        // the wrong rack by accident. Rename the macro in
                        // Ableton (right-click → Rename) to make it mappable.
                        if is_default_macro_name(&mac.name) {
                            continue;
                        }
                        let addr = AbletonMacroAddress {
                            track_id: track.track_id,
                            device_id: device.device_id,
                            param_id: mac.param_id,
                            device_identity: AbletonDeviceIdentity {
                                device_class_name: device.device_class_name.clone(),
                            },
                            track_name: track_name.clone(),
                            device_name: device.device_name.clone(),
                            macro_name: mac.name.clone(),
                        };
                        let id = tree.add_button(
                            None,
                            right_content_x,
                            ry,
                            RIGHT_COL_W,
                            ITEM_H,
                            UIStyle {
                                bg_color: MACRO_NORMAL,
                                hover_bg_color: MACRO_HOVER,
                                pressed_bg_color: MACRO_PRESSED,
                                text_color: TEXT_MACRO,
                                font_size: color::FONT_LABEL,
                                text_align: TextAlign::Left,
                                corner_radius: color::BUTTON_RADIUS,
                                ..UIStyle::default()
                            },
                            &format!("  {}", mac.name),
                        );
                        self.macro_item_ids.push((id, addr));
                        ry += ITEM_H;
                    }

                    // Separator between devices (not after last)
                    if di + 1 < track.devices.len() {
                        tree.add_panel(
                            None,
                            right_content_x,
                            ry + 3.0,
                            RIGHT_COL_W,
                            1.0,
                            UIStyle {
                                bg_color: DIVIDER_COLOR,
                                ..UIStyle::default()
                            },
                        );
                        ry += 8.0;
                    }
                }
            }
        } else {
            let msg = if self.rack_tracks.is_empty() {
                "Ableton not connected"
            } else {
                "Select a track"
            };
            tree.add_label(
                None,
                right_content_x,
                body_y + 8.0,
                RIGHT_COL_W,
                ITEM_H,
                msg,
                UIStyle {
                    text_color: TEXT_DIM,
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
        }

        self.node_count = tree.count() - self.first_node;
    }

    /// Handle a click event. Returns an action if consumed.
    pub fn handle_click(&mut self, node_id: NodeId) -> Option<AbletonPickerAction> {
        if !self.is_open {
            return None;
        }

        if self.backdrop_id == Some(node_id) {
            self.close();
            return Some(AbletonPickerAction::Dismissed);
        }

        // Track row → select, update right column next build
        if let Some(idx) = self.track_row_ids.iter().position(|&tid| tid == node_id) {
            self.selected_track_idx = Some(idx);
            return None;
        }

        // Macro item → map and close
        for (item_id, addr) in &self.macro_item_ids {
            if node_id == *item_id {
                let addr = addr.clone();
                self.close();
                return Some(AbletonPickerAction::Selected(addr));
            }
        }

        // Internal non-interactive click — consume without closing
        if self.contains_node(node_id) {
            return None;
        }

        self.close();
        Some(AbletonPickerAction::Dismissed)
    }

    pub fn handle_escape(&mut self) -> Option<AbletonPickerAction> {
        if self.is_open {
            self.close();
            Some(AbletonPickerAction::Dismissed)
        } else {
            None
        }
    }

    pub fn contains_node(&self, node_id: NodeId) -> bool {
        let id = node_id.index();
        id >= self.first_node && id < self.first_node + self.node_count
    }

    // ── Layout ────────────────────────────────────────────────────

    fn compute_layout(&mut self, anchor: Vec2) {
        let left_h = (self.rack_tracks.len().max(1) as f32) * ITEM_H;

        let right_h = match self
            .selected_track_idx
            .and_then(|i| self.rack_tracks.get(i))
        {
            Some(track) => {
                let mut h = 0.0f32;
                for (di, device) in track.devices.iter().enumerate() {
                    h += SECTION_H + 2.0;
                    h += device.macros.len() as f32 * ITEM_H;
                    if di + 1 < track.devices.len() {
                        h += 8.0;
                    }
                }
                h
            }
            None => ITEM_H,
        };

        let body_h = left_h.max(right_h);
        let total_h = BORDER * 2.0 + PADDING * 2.0 + HEADER_H + 3.0 + body_h;
        self.popup_h = total_h.clamp(MIN_POPUP_H, MAX_POPUP_H);

        let mut x = anchor.x;
        let mut y = anchor.y;
        if x + POPUP_W > self.screen_w {
            x = (self.screen_w - POPUP_W).max(0.0);
        }
        if y + self.popup_h > self.screen_h {
            let above = anchor.y - self.popup_h;
            y = if above >= 0.0 {
                above
            } else {
                (self.screen_h - self.popup_h).max(0.0)
            };
        }
        self.popup_x = x;
        self.popup_y = y;
    }
}

impl Overlay for AbletonPickerPopup {
    fn is_open(&self) -> bool {
        self.is_open
    }

    fn modality(&self) -> Modality {
        // Builds its own backdrop, so the driver must not add a second scrim.
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
        if !self.is_open {
            return OverlayResponse::Ignored;
        }
        match event {
            UIEvent::KeyDown { key: Key::Escape, .. } => {
                self.handle_escape();
                OverlayResponse::Consumed(Vec::new())
            }
            UIEvent::Click { node_id, .. } => {
                if let Some(AbletonPickerAction::Selected(addr)) = self.handle_click(*node_id) {
                    // Stash; the app drains and lowers against its picker context.
                    self.pending_selection = Some(addr);
                }
                // Dismissed / track-select / internal clicks all resolve inside
                // handle_click — consume so the modal swallows them and the
                // driver re-runs build_at (track-select repaints the right col).
                OverlayResponse::Consumed(Vec::new())
            }
            _ => OverlayResponse::Ignored,
        }
    }
}
