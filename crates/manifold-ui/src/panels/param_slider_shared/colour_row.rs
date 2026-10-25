//! The colour row ([`RowKind::Colour`]): one RGB colour stored as three
//! scalar params, drawn as its name, a swatch, a hex field and a fold that
//! holds the three channel sliders. The host builds this header, then each
//! channel through its own slider-row path, so every channel keeps its
//! driver, envelope and audio lane. The swatch a non-channel row carries (a
//! light's fader on the scene card) is built here too.

use super::*;
use crate::param_surface::{ColourGroup, RowKind, RowRole};

/// The row's colour swatch. Role nibble 0 is free; sub-tag 1 keeps the
/// key the material swatch had before it moved here.
pub(crate) const ROW_ROLE_SWATCH: u64 = 1;
/// A colour row's hex field.
pub(crate) const ROW_ROLE_COLOUR_HEX: u64 = 2;
/// A colour row's name and fold triangle.
pub(crate) const ROW_ROLE_COLOUR_FOLD: u64 = 3;

/// Side of the square colour swatch.
pub(crate) const SWATCH_SIZE: f32 = 14.0;
pub(crate) const SWATCH_GAP: f32 = 4.0;
const HEX_FIELD_W: f32 = 76.0;
const FOLD_TRIANGLE_W: f32 = 16.0;

/// The swatch's fill for an `[r, g, b]` in 0–1.
pub(crate) fn swatch_colour(rgb: [f32; 3]) -> Color32 {
    let [r, g, b] = rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    Color32::new(r, g, b, 255)
}

/// `#RRGGBB` for a 0–1 colour, written into `buf`, so a per-frame repaint
/// formats without allocating.
pub fn colour_hex_into(rgb: [f32; 3], buf: &mut [u8; 7]) -> &str {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    buf[0] = b'#';
    for (channel, value) in rgb.into_iter().enumerate() {
        let byte = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        buf[1 + channel * 2] = DIGITS[usize::from(byte >> 4)];
        buf[2 + channel * 2] = DIGITS[usize::from(byte & 0xF)];
    }
    std::str::from_utf8(buf).unwrap_or("#000000")
}

/// `#RRGGBB` for a 0–1 colour: the one hex format the row, the node face
/// and the hex type-in share.
pub fn colour_hex(rgb: [f32; 3]) -> String {
    colour_hex_into(rgb, &mut [0; 7]).to_string()
}

/// The colour's name: its R channel's name without the channel suffix
/// ("Base Colour R" reads "Base Colour").
pub(crate) fn colour_name(name: &str) -> &str {
    name.strip_suffix(" R")
        .or_else(|| name.strip_suffix(" Red"))
        .unwrap_or(name)
}

/// A swatch showing `rgb` at `x`, clickable when one RGB write owns its
/// `rgb_members` (a click opens the colour menu, `RowRole::ColourSwatch`).
pub(crate) fn build_colour_swatch(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    cy: f32,
    info: &ParamRow,
    rgb: [f32; 3],
    row_key_base: Option<u64>,
) -> Option<(NodeId, ColourGroup)> {
    let members = info.rgb_members.as_ref()?;
    let rect = Rect::new(
        x,
        cy + (ROW_HEIGHT - SWATCH_SIZE) * 0.5,
        SWATCH_SIZE,
        SWATCH_SIZE,
    );
    let style = UIStyle {
        bg_color: swatch_colour(rgb),
        border_color: color::BORDER,
        border_width: 1.0,
        corner_radius: color::SMALL_RADIUS,
        ..UIStyle::default()
    };
    let flags = if members.editable {
        UIFlags::VISIBLE | UIFlags::INTERACTIVE
    } else {
        UIFlags::VISIBLE
    };
    let id = match row_key_base {
        Some(base) => tree.add_node_keyed(
            parent,
            rect,
            UINodeType::Button,
            style,
            None,
            flags,
            base | ROW_ROLE_SWATCH,
        ),
        None => tree.add_node(parent, rect, UINodeType::Button, style, None, flags),
    };
    tree.set_name(id, format!("param_row.{}.swatch", info.id));
    Some((id, members.group))
}

/// The two colour-row nodes besides its swatch.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ColourRowNodes {
    pub fold: NodeId,
    pub hex: NodeId,
}

pub(crate) struct ColourRowIds {
    pub swatch: Option<(NodeId, ColourGroup)>,
    pub nodes: ColourRowNodes,
    pub new_cy: f32,
}

/// The colour row's header: fold triangle and name across the label lane,
/// then the swatch and hex field where the track starts. `rgb` is the live
/// colour, `open` whether the channel sliders follow. `None` for a row that
/// is not a colour row.
pub(crate) fn build_colour_row(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    cy: f32,
    slider_w: f32,
    label_width: f32,
    info: &ParamRow,
    rgb: [f32; 3],
    open: bool,
    row_key_base: Option<u64>,
) -> Option<ColourRowIds> {
    let members = info.rgb_members.as_ref()?;
    let label_w = label_width.min(slider_w);
    let fold_style = UIStyle {
        bg_color: Color32::TRANSPARENT,
        hover_bg_color: color::HOVER_OVERLAY,
        pressed_bg_color: color::PRESS_OVERLAY,
        corner_radius: color::SMALL_RADIUS,
        ..UIStyle::default()
    };
    let fold = match row_key_base {
        Some(base) => tree.add_button_keyed(
            parent,
            x,
            cy,
            label_w,
            ROW_HEIGHT,
            fold_style,
            "",
            base | ROW_ROLE_COLOUR_FOLD,
        ),
        None => tree.add_button(parent, x, cy, label_w, ROW_HEIGHT, fold_style, ""),
    };
    tree.set_name(fold, format!("param_row.{}.colour", info.id));
    let dimmed = UIStyle {
        text_color: color::TEXT_DIMMED_C32,
        font_size: FONT_SIZE,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    };
    tree.add_label(
        Some(fold),
        x,
        cy,
        FOLD_TRIANGLE_W,
        ROW_HEIGHT,
        if open { "\u{25BE}" } else { "\u{25B8}" },
        dimmed,
    );
    let name_style = UIStyle {
        text_color: color::SLIDER_TEXT_C32,
        font_size: FONT_SIZE,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    };
    tree.add_label(
        Some(fold),
        x + FOLD_TRIANGLE_W,
        cy,
        (label_w - FOLD_TRIANGLE_W - GAP).max(0.0),
        ROW_HEIGHT,
        colour_name(&info.spec.name),
        name_style,
    );

    let swatch_x = x + label_w;
    let swatch = build_colour_swatch(tree, parent, swatch_x, cy, info, rgb, row_key_base);
    let hex_x = swatch_x + SWATCH_SIZE + SWATCH_GAP;
    let hex_w = (x + slider_w - hex_x).clamp(0.0, HEX_FIELD_W);
    let hex_style = UIStyle {
        bg_color: color::INSPECTOR_BG,
        hover_bg_color: color::HOVER_OVERLAY,
        pressed_bg_color: color::PRESS_OVERLAY,
        text_color: if members.editable {
            color::TEXT_WHITE_C32
        } else {
            color::TEXT_DIMMED_C32
        },
        font_size: FONT_SIZE,
        text_align: TextAlign::Left,
        corner_radius: color::SMALL_RADIUS,
        ..UIStyle::default()
    };
    let rect = Rect::new(
        hex_x,
        cy + (ROW_HEIGHT - TOGGLE_BTN_H) * 0.5,
        hex_w,
        TOGGLE_BTN_H,
    );
    let flags = if members.editable {
        UIFlags::VISIBLE | UIFlags::INTERACTIVE
    } else {
        UIFlags::VISIBLE
    };
    let text = colour_hex_into(rgb, &mut [0; 7]).to_string();
    let hex = match row_key_base {
        Some(base) => tree.add_node_keyed(
            parent,
            rect,
            UINodeType::Button,
            hex_style,
            Some(&text),
            flags,
            base | ROW_ROLE_COLOUR_HEX,
        ),
        None => tree.add_node(
            parent,
            rect,
            UINodeType::Button,
            hex_style,
            Some(&text),
            flags,
        ),
    };
    tree.set_name(hex, format!("param_row.{}.hex", info.id));
    Some(ColourRowIds {
        swatch,
        nodes: ColourRowNodes { fold, hex },
        new_cy: cy + ROW_HEIGHT + ROW_SPACING,
    })
}

impl RowHost {
    /// Register a built colour row's nodes under row `row`. Call after the
    /// row's own channel slider is installed, which clears its swatch slot.
    pub(crate) fn install_colour_row(&mut self, tree: &UITree, row: usize, ids: &ColourRowIds) {
        self.swatch_ids[row] = ids.swatch;
        self.colour_ids[row] = Some(ids.nodes);
        if let Some((swatch, group)) = ids.swatch {
            self.row_index
                .insert(tree.widget_of(swatch), row, RowRole::ColourSwatch(group));
        }
        self.row_index
            .insert(tree.widget_of(ids.nodes.fold), row, RowRole::ColourFold);
        self.row_index
            .insert(tree.widget_of(ids.nodes.hex), row, RowRole::ColourHex);
    }

    /// Whether a colour row shows its channel sliders: the user's choice
    /// this session, else open exactly when one RGB write cannot own the
    /// three, so a modulated or wired channel's slider and lane show.
    pub(crate) fn colour_open(&self, info: &ParamRow) -> bool {
        self.colour_open
            .get(info.id.as_ref())
            .copied()
            .unwrap_or_else(|| {
                info.rgb_members
                    .as_ref()
                    .is_some_and(|members| !members.editable)
            })
    }

    /// Flip a colour row's fold.
    pub(crate) fn toggle_colour(&mut self, info: &ParamRow) {
        let open = !self.colour_open(info);
        self.colour_open.insert(info.id.to_string(), open);
    }

    /// Open the colour whose channel `id` is, so a reveal lands on a built
    /// slider.
    pub(crate) fn open_colour_of(&mut self, rows: &[ParamRow], id: &str) {
        let head = rows.iter().find(|row| {
            row.kind(true) == RowKind::Colour
                && row
                    .rgb_members
                    .as_ref()
                    .is_some_and(|members| members.ids.iter().any(|member| member == id))
        });
        if let Some(head) = head {
            self.colour_open.insert(head.id.to_string(), true);
        }
    }

    /// Row `i` is a G or B channel that its colour row on this surface draws
    /// inside its fold, so the host's own walk skips it.
    pub(crate) fn is_colour_tail(&self, rows: &[ParamRow], i: usize, scene: bool) -> bool {
        rows[i].kind(scene) != RowKind::Colour
            && rows.iter().any(|head| {
                head.kind(scene) == RowKind::Colour
                    && head
                        .rgb_members
                        .as_ref()
                        .is_some_and(|members| members.ids[1..].contains(&rows[i].id))
            })
    }

    /// The rows of colour row `head`'s three channels on this surface, in
    /// R, G, B order; `None` for a channel the surface does not show.
    pub(crate) fn colour_channels(rows: &[ParamRow], head: usize) -> [Option<usize>; 3] {
        let Some(members) = rows[head].rgb_members.as_ref() else {
            return [None; 3];
        };
        members
            .ids
            .each_ref()
            .map(|id| rows.iter().position(|row| row.id == *id))
    }

    /// The live colour a swatch row shows.
    pub(crate) fn swatch_rgb_of(&self, row: usize) -> Option<[f32; 3]> {
        self.swatch_rgb.get(row).copied()
    }

    /// Index the swatch channels of `rows` by id and seed each swatch with
    /// its projected colour. Call whenever the rows are reconfigured.
    pub(crate) fn index_swatches(&mut self, rows: &[ParamRow]) {
        self.swatch_members.clear();
        self.swatch_rgb.resize(rows.len(), [0.0; 3]);
        for (row, info) in rows.iter().enumerate() {
            let Some(members) = &info.rgb_members else {
                continue;
            };
            self.swatch_rgb[row] = members.values;
            for (channel, id) in members.ids.iter().enumerate() {
                self.swatch_members.insert(id.to_string(), (row, channel));
            }
        }
    }

    /// Take one live value from the value sync; ignored unless `id` is a
    /// swatch channel.
    pub(crate) fn feed_swatch_channel(&mut self, id: &str, value: f32) {
        if let Some(&(row, channel)) = self.swatch_members.get(id)
            && let Some(rgb) = self.swatch_rgb.get_mut(row)
        {
            rgb[channel] = value;
        }
    }

    /// Set a swatch row's live colour directly (hosts whose channels are
    /// rows of their own).
    pub(crate) fn set_swatch_rgb(&mut self, row: usize, rgb: [f32; 3]) {
        if let Some(slot) = self.swatch_rgb.get_mut(row) {
            *slot = rgb;
        }
    }

    /// Repaint every built swatch, and its colour row's hex, whose shown
    /// colour differs from its live channels. Only changed nodes are touched.
    pub(crate) fn paint_swatches(&self, tree: &mut UITree) {
        for (row, (swatch, rgb)) in self.swatch_ids.iter().zip(&self.swatch_rgb).enumerate() {
            let Some((node, _)) = swatch else { continue };
            let colour = swatch_colour(*rgb);
            if tree
                .get_node(*node)
                .is_some_and(|n| n.style.bg_color != colour)
            {
                let mut style = tree.get_node(*node).map(|n| n.style).unwrap_or_default();
                style.bg_color = colour;
                tree.set_style(*node, style);
                if let Some(nodes) = self.colour_ids.get(row).copied().flatten() {
                    tree.set_text(nodes.hex, colour_hex_into(*rgb, &mut [0; 7]));
                }
            }
        }
    }
}
