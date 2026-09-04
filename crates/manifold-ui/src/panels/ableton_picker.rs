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
use crate::scroll_container::ScrollContainer;
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
    /// Body scroll (both columns move together). The clip node minted by
    /// `begin()` is reparented under the shell container, so scrolled
    /// content is bound by both the body viewport and the popup surface.
    scroll: ScrollContainer,

    popup_x: f32,
    popup_y: f32,
    popup_h: f32,

    screen_w: f32,
    screen_h: f32,

    backdrop_id: Option<NodeId>,
    /// (node_id, track index) for each minted row — culling means the
    /// minted rows are not a contiguous prefix, so the track index rides
    /// along instead of being implied by position.
    track_row_ids: Vec<(NodeId, usize)>,
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
            scroll: ScrollContainer::new(),
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
        self.scroll.reset();
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
        self.scroll.reset();
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

        // Scrim + modal container via the shared shell (section 17 lifts it with a
        // soft shadow). All content is parented to the container, which clips
        // children by construction — a list taller than the popup can neither
        // paint nor take clicks outside it.
        let shell = popup_shell::build(
            tree,
            (self.screen_w, self.screen_h),
            Rect::new(px, py, pw, ph),
            &popup_shell::PopupStyle::MODAL,
        );
        self.backdrop_id = Some(shell.backdrop);
        let content_parent = Some(shell.container);

        let content_x = px + BORDER + PADDING;
        let content_y = py + BORDER + PADDING;

        // ── Header row ────────────────────────────────────────────

        tree.add_panel(
            content_parent,
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
            content_parent,
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
            content_parent,
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
            content_parent,
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
        let body_bottom = py + ph - BORDER - PADDING;

        // ── Vertical divider (fixed — the columns scroll behind it) ──

        let div_x = content_x + LEFT_COL_W;
        tree.add_panel(
            content_parent,
            div_x,
            body_y,
            DIVIDER_W,
            body_bottom - body_y,
            UIStyle {
                bg_color: DIVIDER_COLOR,
                ..UIStyle::default()
            },
        );

        // ── Scrollable body (both columns) ────────────────────────
        // The clip minted by begin() is reparented under the shell container,
        // so scrolled content is bound by the body viewport AND the popup
        // surface. Rows outside the viewport are culled, never minted.

        let body_vp = Rect::new(px + BORDER, body_y, pw - BORDER * 2.0, body_bottom - body_y);
        let clip_id = self.scroll.begin(tree, body_vp);
        tree.reparent_root_nodes(clip_id.index(), 1, shell.container);
        let body_parent = Some(clip_id);

        // ── Left column: track rows ───────────────────────────────

        if self.rack_tracks.is_empty() {
            tree.add_label(
                body_parent,
                content_x,
                self.scroll.content_y(8.0),
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
                let local_y = i as f32 * ITEM_H;
                if !self.scroll.is_visible(local_y, ITEM_H) {
                    continue;
                }
                let is_selected = self.selected_track_idx == Some(i);
                let row_y = self.scroll.content_y(local_y);

                let (bg, hover_bg) = if is_selected {
                    (TRACK_SELECTED_BG, TRACK_SELECTED_HOVER)
                } else {
                    (TRACK_NORMAL, TRACK_HOVER)
                };

                let id = tree.add_button(
                    body_parent,
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
                self.track_row_ids.push((id, i));

                // Selection arrow
                if is_selected {
                    tree.add_label(
                        body_parent,
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
                let mut local_ry = 0.0f32;
                let track_name = track.track_name.clone();

                // Only devices with at least one renamed macro render (a
                // device of nothing-but-defaults has no mappable surface) —
                // separators go between VISIBLE devices, never dangling.
                let visible_devices: Vec<&PickerDevice> = Self::visible_devices(track);
                let last_visible = visible_devices.len().saturating_sub(1);

                for (vi, device) in visible_devices.iter().enumerate() {
                    // Device name section header (non-interactive)
                    let header_local = local_ry + 2.0;
                    if self.scroll.is_visible(header_local, SECTION_H) {
                        tree.add_label(
                            body_parent,
                            right_content_x,
                            self.scroll.content_y(header_local),
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
                    }
                    local_ry += SECTION_H + 2.0;

                    for mac in device
                        .macros
                        .iter()
                        .filter(|m| !is_default_macro_name(&m.name))
                    {
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
                        if !self.scroll.is_visible(local_ry, ITEM_H) {
                            local_ry += ITEM_H;
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
                            body_parent,
                            right_content_x,
                            self.scroll.content_y(local_ry),
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
                        local_ry += ITEM_H;
                    }

                    // Separator between visible devices (not after last)
                    if vi < last_visible {
                        let sep_local = local_ry + 3.0;
                        if self.scroll.is_visible(sep_local, 1.0) {
                            tree.add_panel(
                                body_parent,
                                right_content_x,
                                self.scroll.content_y(sep_local),
                                RIGHT_COL_W,
                                1.0,
                                UIStyle {
                                    bg_color: DIVIDER_COLOR,
                                    ..UIStyle::default()
                                },
                            );
                        }
                        local_ry += 8.0;
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
                body_parent,
                right_content_x,
                self.scroll.content_y(8.0),
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

        self.scroll.set_content_height(self.body_content_height());
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
        if let Some((_, track_idx)) = self
            .track_row_ids
            .iter()
            .find(|(tid, _)| *tid == node_id)
        {
            self.selected_track_idx = Some(*track_idx);
            // Right column content changed — start it from the top.
            self.scroll.reset();
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

    /// Devices the build actually renders: those with at least one renamed
    /// macro. A device of nothing-but-defaults has no mappable surface and
    /// takes no picker space.
    fn visible_devices(track: &PickerTrack) -> Vec<&PickerDevice> {
        track
            .devices
            .iter()
            .filter(|d| d.macros.iter().any(|m| !is_default_macro_name(&m.name)))
            .collect()
    }

    /// Height of one device block as built: section header + its renamed
    /// macros. The layout math and the build loop must agree on this.
    fn device_block_h(device: &PickerDevice) -> f32 {
        let renamed = device
            .macros
            .iter()
            .filter(|m| !is_default_macro_name(&m.name))
            .count();
        SECTION_H + 2.0 + renamed as f32 * ITEM_H
    }

    /// Right-column content height for a track, separators between visible
    /// devices included.
    fn right_column_h(track: &PickerTrack) -> f32 {
        let devices: Vec<&PickerDevice> = Self::visible_devices(track);
        if devices.is_empty() {
            return 0.0;
        }
        let blocks: f32 = devices.iter().map(|d| Self::device_block_h(d)).sum();
        blocks + (devices.len() - 1) as f32 * 8.0
    }

    /// Scrollable body content height: the taller of the two columns.
    /// Single source for `compute_layout`'s popup sizing and the
    /// `set_content_height` scroll clamp.
    fn body_content_height(&self) -> f32 {
        let left_h = (self.rack_tracks.len().max(1) as f32) * ITEM_H;
        let right_h = self
            .selected_track_idx
            .and_then(|i| self.rack_tracks.get(i))
            .map(|t| Self::right_column_h(t).max(ITEM_H))
            .unwrap_or(ITEM_H);
        left_h.max(right_h)
    }

    fn compute_layout(&mut self, anchor: Vec2) {
        let body_h = self.body_content_height();
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
            UIEvent::Scroll { delta, .. } => {
                self.scroll.apply_scroll_delta(delta.y);
                // Consumed so the wheel doesn't scroll the viewport behind
                // the modal; the driver re-runs build_at with the new offset.
                OverlayResponse::Consumed(Vec::new())
            }
            _ => OverlayResponse::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::ZTier;

    fn macro_named(name: &str) -> PickerMacro {
        PickerMacro {
            param_id: 1,
            name: name.to_string(),
        }
    }

    fn track(name: &str, devices: Vec<PickerDevice>) -> PickerTrack {
        PickerTrack {
            track_id: 1,
            track_name: name.to_string(),
            devices,
        }
    }

    fn device(name: &str, macros: Vec<PickerMacro>) -> PickerDevice {
        PickerDevice {
            device_id: 1,
            device_name: name.to_string(),
            device_class_name: "TestDevice".to_string(),
            macros,
        }
    }

    fn session(tracks: Vec<PickerTrack>) -> AbletonPickerSession {
        AbletonPickerSession { rack_tracks: tracks }
    }

    /// Open + build through the same path the overlay driver uses, on a
    /// fresh tree each call so stale nodes from an earlier build never
    /// pollute assertions.
    fn open_and_build(
        tracks: Vec<PickerTrack>,
    ) -> (AbletonPickerPopup, UITree) {
        let mut dd = AbletonPickerPopup::new();
        dd.open(session(tracks), Vec2::new(100.0, 100.0));
        let mut tree = UITree::new();
        rebuild(&mut dd, &mut tree);
        (dd, tree)
    }

    fn rebuild(dd: &mut AbletonPickerPopup, tree: &mut UITree) {
        let region = tree.begin_region(
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            ZTier::Overlay,
            "overlay",
            UIFlags::empty(),
        );
        let start = tree.count();
        Overlay::build_at(
            dd,
            tree,
            OverlayPlacement {
                rect: Rect::ZERO,
                screen: Vec2::new(1920.0, 1080.0),
            },
        );
        tree.end_region(region, start);
    }

    fn container_rect(dd: &AbletonPickerPopup) -> Rect {
        Rect::new(dd.popup_x, dd.popup_y, POPUP_W, dd.popup_h)
    }

    fn scroll_by(dd: &mut AbletonPickerPopup, tree: &mut UITree, delta_y: f32) {
        let event = UIEvent::Scroll {
            pos: Vec2::new(150.0, 150.0),
            delta: Vec2::new(0.0, delta_y),
            modifiers: crate::input::Modifiers::default(),
        };
        assert!(
            matches!(dd.on_event(&event, tree), OverlayResponse::Consumed(_)),
            "the open modal consumes wheel events"
        );
    }

    #[test]
    fn tall_track_list_scrolls_and_stays_contained() {
        // 30 tracks × 26px = 780px of left column against a 480px popup cap:
        // rows past the body viewport must be culled, scrollable to, and
        // hittable only inside the container.
        let tracks: Vec<PickerTrack> = (0..30)
            .map(|i| track(&format!("Track {i}"), vec![]))
            .collect();
        let (mut dd, mut tree) = open_and_build(tracks);

        let container = container_rect(&dd);
        assert!(
            (dd.popup_h - MAX_POPUP_H).abs() < 0.01,
            "30 tracks overflow the popup cap, popup_h={}",
            dd.popup_h
        );
        assert!(
            dd.track_row_ids.len() < 30,
            "rows past the viewport are culled, minted {}",
            dd.track_row_ids.len()
        );

        // Every minted row is interactive with its top inside the container.
        // A row straddling the viewport edge overhangs the container by up to
        // a row height — the shell container's CLIPS_CHILDREN cuts it on
        // paint and hit-test, exactly like the dropdown's edge swatches.
        for (id, _) in &dd.track_row_ids {
            let node = tree.get_node(*id).unwrap();
            assert!(node.flags.contains(UIFlags::INTERACTIVE));
            assert!(
                node.bounds.y >= container.y - 0.01 && node.bounds.y < container.y_max() + 0.01,
                "row top inside the container: {:?} vs {:?}",
                node.bounds,
                container
            );
        }

        // Nothing is hittable below the container (the scrim takes the click).
        let below = Vec2::new(container.x + 40.0, container.y_max() + 10.0);
        let hit = tree.hit_test(below);
        assert!(
            !dd.track_row_ids.iter().any(|(id, _)| Some(*id) == hit),
            "no row is hittable below the container"
        );

        // Wheel to the bottom: the last track becomes reachable.
        scroll_by(&mut dd, &mut tree, -10_000.0);
        assert!(
            (dd.scroll.scroll_offset() - dd.scroll.max_scroll()).abs() < 0.01,
            "scroll reaches the bottom"
        );
        let mut tree = UITree::new();
        rebuild(&mut dd, &mut tree);
        let last = dd
            .track_row_ids
            .iter()
            .find(|(_, i)| *i == 29)
            .map(|(id, _)| *id)
            .expect("last track minted after scrolling");
        let node = tree.get_node(last).unwrap();
        let container = container_rect(&dd);
        assert!(
            node.bounds.y >= container.y - 0.01 && node.bounds.y < container.y_max() + 0.01,
            "last row top visible inside the container after scrolling"
        );

        // Clicking it selects track 29 — culling must not shift indices.
        let action = dd.handle_click(last);
        assert!(action.is_none(), "track select consumes without closing");
        assert_eq!(dd.selected_track_idx, Some(29));
        // Track change resets the scroll for the new right column.
        assert_eq!(dd.scroll.scroll_offset(), 0.0);
    }

    #[test]
    fn right_column_height_counts_only_shown_macros() {
        // One device with 8 default (unmappable, hidden) macros and 1 renamed:
        // the old layout counted all 9, inflating the popup ~200px; the new
        // one sizes to the single visible row.
        let renamed = vec![
            macro_named("Macro 1"),
            macro_named("Macro 2"),
            macro_named("Macro 3"),
            macro_named("Macro 4"),
            macro_named("Macro 5"),
            macro_named("Macro 6"),
            macro_named("Macro 7"),
            macro_named("Macro 8"),
            macro_named("Filter Cut"),
        ];
        let (dd, tree) = open_and_build(vec![track("Drums", vec![device("Eq Eight", renamed)])]);

        let expected_body = ITEM_H.max(SECTION_H + 2.0 + ITEM_H);
        let expected_h = (BORDER * 2.0 + PADDING * 2.0 + HEADER_H + 3.0 + expected_body)
            .clamp(MIN_POPUP_H, MAX_POPUP_H);
        assert!(
            (dd.popup_h - expected_h).abs() < 0.01,
            "popup sized to visible content only: got {}, expected {expected_h}",
            dd.popup_h
        );
        assert_eq!(
            dd.macro_item_ids.len(),
            1,
            "only the renamed macro is minted"
        );
        let _ = tree;
    }

    #[test]
    fn all_default_device_takes_no_space_and_no_separator_dangles() {
        // Two devices: the first all-default (hidden), the second with one
        // renamed macro. The hidden device contributes no block and no
        // separator before the visible one.
        let hidden_device = device(
            "Hidden Rack",
            vec![macro_named("Macro 1"), macro_named("Macro 2")],
        );
        let visible_device = device("Visible Rack", vec![macro_named("Resonance")]);
        let (dd, _tree) = open_and_build(vec![track(
            "Bass",
            vec![hidden_device, visible_device],
        )]);

        let expected_body = ITEM_H.max(SECTION_H + 2.0 + ITEM_H);
        let expected_h = (BORDER * 2.0 + PADDING * 2.0 + HEADER_H + 3.0 + expected_body)
            .clamp(MIN_POPUP_H, MAX_POPUP_H);
        assert!(
            (dd.popup_h - expected_h).abs() < 0.01,
            "hidden device contributes no height: got {}, expected {expected_h}",
            dd.popup_h
        );
        assert_eq!(dd.macro_item_ids.len(), 1);
    }
}
