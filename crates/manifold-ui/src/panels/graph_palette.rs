//! [`GraphPalette`] — left-sidebar panel inside the graph-editor
//! window listing the atoms the user can drop into the watched
//! graph.
//!
//! Mirrors [`super::graph_editor::GraphEditorPanel`]'s shape: owns no
//! GPU state, just configures a UITree subtree each frame from the
//! atom list the app supplies. Clicking an entry emits a
//! [`PanelAction::AddGraphNode`] which the app turns into an
//! `AddGraphNodeCommand` using the watched effect's id and the
//! catalog default for its type.

use crate::color;
use crate::node::*;
use crate::tree::UITree;

use super::PanelAction;

/// One palette entry the user can click to add a node. Owned strings
/// so the list can be cloned across the content/UI boundary without
/// borrowing back into the renderer's `&'static` symbol table.
#[derive(Debug, Clone)]
pub struct GraphPaletteAtom {
    /// Display label, e.g. "Blur", "Mix".
    pub label: String,
    /// Stable `type_id` for the node — passed verbatim to
    /// `AddGraphNodeCommand`.
    pub type_id: String,
    /// Stable section identifier. UI groups entries with the same
    /// `category` under one header. Matches `PaletteCategory::label()`
    /// on the renderer side. Plain string here so this crate stays
    /// independent of `manifold-renderer`.
    pub category: String,
}

/// Left-sidebar width inside the graph-editor window. Mirrors the
/// right sidebar's `SIDEBAR_WIDTH` so the canvas is centered.
pub const PALETTE_WIDTH: f32 = 200.0;

const ROW_H: f32 = 28.0;
const PADDING: f32 = 12.0;
const HEADER_H: f32 = 32.0;
const FONT_SIZE: u16 = 12;
const HEADER_FONT_SIZE: u16 = 14;

/// Left-sidebar panel inside the graph-editor window.
#[derive(Default)]
pub struct GraphPalette {
    /// Active when an editor target is set. Mirrors the right-sidebar
    /// gating — clicks are inert until `configure` runs with an
    /// `effect_index` to operate on.
    active: bool,
    atoms: Vec<GraphPaletteAtom>,
    /// Per-row state populated during `build` so `handle_click` can
    /// map a clicked button back to the atom's `type_id`.
    rows: Vec<RowState>,
    /// Root container id inside the editor's UITree.
    root_id: i32,
}

#[derive(Debug, Clone)]
struct RowState {
    button_id: u32,
    type_id: String,
}

impl GraphPalette {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the panel's input data. Always cheap — the atom list is
    /// expected to be a small (≈13 entry) `Vec` cloned from a static
    /// catalog at app startup.
    pub fn configure(&mut self, active: bool, atoms: Vec<GraphPaletteAtom>) {
        self.active = active;
        self.atoms = atoms;
    }

    /// Build the UITree subtree at the given viewport.
    pub fn build(&mut self, tree: &mut UITree, viewport: Rect) {
        self.rows.clear();

        let bg_id = tree.add_panel(
            -1,
            viewport.x,
            viewport.y,
            viewport.width,
            viewport.height,
            UIStyle {
                bg_color: color::EFFECT_CARD_INNER_BG_C32,
                ..UIStyle::default()
            },
        ) as i32;
        self.root_id = bg_id;

        let mut y = viewport.y + PADDING;

        if !self.active {
            tree.add_label(
                bg_id,
                viewport.x + PADDING,
                y,
                viewport.width - 2.0 * PADDING,
                HEADER_H - PADDING,
                "Atoms",
                UIStyle {
                    text_color: color::TEXT_WHITE_C32,
                    font_size: HEADER_FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            y += HEADER_H;
            tree.add_label(
                bg_id,
                viewport.x + PADDING,
                y,
                viewport.width - 2.0 * PADDING,
                ROW_H,
                "(open an effect to add nodes)",
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            return;
        }

        // Render each contiguous run of same-category atoms under one
        // header. The atom list arrives pre-grouped from the renderer
        // (palette.rs sorts by category then label) so we just watch
        // for category changes as we walk it.
        let mut current_category: Option<&str> = None;
        for atom in &self.atoms {
            if Some(atom.category.as_str()) != current_category {
                tree.add_label(
                    bg_id,
                    viewport.x + PADDING,
                    y,
                    viewport.width - 2.0 * PADDING,
                    HEADER_H - PADDING,
                    &atom.category,
                    UIStyle {
                        text_color: color::TEXT_WHITE_C32,
                        font_size: HEADER_FONT_SIZE,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                );
                y += HEADER_H;
                current_category = Some(atom.category.as_str());
            }
            let btn_id = tree.add_button(
                bg_id,
                viewport.x + PADDING,
                y,
                viewport.width - 2.0 * PADDING,
                ROW_H - 4.0,
                UIStyle {
                    bg_color: color::BUTTON_INACTIVE_C32,
                    hover_bg_color: color::ACCENT_BLUE_C32,
                    text_color: color::TEXT_WHITE_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    corner_radius: 4.0,
                    ..UIStyle::default()
                },
                &format!("  + {}", atom.label),
            );
            self.rows.push(RowState {
                button_id: btn_id,
                type_id: atom.type_id.clone(),
            });
            y += ROW_H;
        }
    }

    /// Map a click on a UITree button back to a `PanelAction`. Returns
    /// an empty Vec when the click didn't land on one of our rows or
    /// when the palette is inactive.
    pub fn handle_click(&self, node_id: u32) -> Vec<PanelAction> {
        if !self.active {
            return Vec::new();
        }
        match self.rows.iter().find(|r| r.button_id == node_id) {
            Some(row) => vec![PanelAction::AddGraphNode {
                type_id: row.type_id.clone(),
            }],
            None => Vec::new(),
        }
    }

    /// Bulk variant for the editor window's input loop.
    pub fn dispatch_clicks(&self, clicks: &[u32]) -> Vec<PanelAction> {
        clicks.iter().flat_map(|&n| self.handle_click(n)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_atoms() -> Vec<GraphPaletteAtom> {
        vec![
            GraphPaletteAtom {
                label: "Blur".to_string(),
                type_id: "node.blur".to_string(),
                category: "Atoms".to_string(),
            },
            GraphPaletteAtom {
                label: "Mix".to_string(),
                type_id: "node.mix".to_string(),
                category: "Atoms".to_string(),
            },
        ]
    }

    fn viewport() -> Rect {
        Rect::new(0.0, 0.0, PALETTE_WIDTH, 600.0)
    }

    #[test]
    fn inactive_palette_renders_hint_and_swallows_clicks() {
        let mut tree = UITree::new();
        let mut palette = GraphPalette::new();
        palette.configure(false, sample_atoms());
        palette.build(&mut tree, viewport());
        assert!(palette.rows.is_empty());
        assert!(palette.handle_click(123).is_empty());
    }

    #[test]
    fn active_palette_emits_add_graph_node_on_click() {
        let mut tree = UITree::new();
        let mut palette = GraphPalette::new();
        palette.configure(true, sample_atoms());
        palette.build(&mut tree, viewport());
        assert_eq!(palette.rows.len(), 2);

        let blur_id = palette.rows[0].button_id;
        let actions = palette.handle_click(blur_id);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::AddGraphNode { type_id } => {
                assert_eq!(type_id, "node.blur");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn click_outside_palette_returns_empty() {
        let mut tree = UITree::new();
        let mut palette = GraphPalette::new();
        palette.configure(true, sample_atoms());
        palette.build(&mut tree, viewport());
        assert!(palette.handle_click(99999).is_empty());
    }
}
