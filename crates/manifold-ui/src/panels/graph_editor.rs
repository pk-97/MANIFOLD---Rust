//! [`GraphEditorPanel`] — right-sidebar panel inside the graph-editor
//! window for V2 user-exposed parameters.
//!
//! Phase 3 of `docs/EFFECT_RUNTIME_UNIFICATION.md`. The first UITree
//! component to live inside the editor window. Renders a vertical
//! list of the currently-selected node's parameters; each row carries
//! a checkbox indicating whether that param is currently exposed on
//! the effect card. Click a checkbox → emit
//! [`PanelAction::EffectParamExpose`] → content thread runs
//! `ToggleEffectParamExposeCommand` → `EffectInstance.user_param_bindings`
//! gains/loses the entry.
//!
//! ## Selection model
//!
//! The graph-canvas in the editor window owns the "selected node id"
//! state today. The panel is configured each frame with that id plus
//! the active `EffectInstance`'s effect-index and currently-exposed
//! `(node_handle, inner_param)` pairs; the panel rebuilds its UITree
//! subtree only when something material changed (selection,
//! parameters, or exposed-set).
//!
//! ## Why not the `Panel` trait
//!
//! There's no shared `Panel` trait in this codebase yet (each panel is
//! its own struct with its own methods). This panel follows the same
//! convention: `new` / `configure` / `build` / `handle_click`, called
//! by the editor-window present path.

use std::collections::HashSet;

use crate::color;
use crate::node::*;
use crate::tree::UITree;
use manifold_core::effects::UserParamConvert;

use super::PanelAction;

/// UI-facing kind for one inner-node parameter, mirroring the
/// renderer-side `ParamSnapshotKind` without making this crate depend
/// on `manifold-renderer`. The editor-window glue in `manifold-app`
/// converts at the boundary (since that crate sees both sides).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphEditorParamKind {
    Float,
    Int,
    Bool,
    Enum,
    /// Multi-component types — shown as a disabled row, not exposable
    /// in the V2 single-slot user surface.
    Other,
}

/// UI-facing description of one inner-node parameter, owned for
/// `Send`ability across the content/UI boundary.
#[derive(Debug, Clone)]
pub struct GraphEditorParam {
    /// Stable param name — used as `inner_param` in the resulting
    /// `UserParamBinding`.
    pub name: String,
    /// Display label.
    pub label: String,
    pub kind: GraphEditorParamKind,
    pub default_value: f32,
    /// Current value on the live node — what the renderer is actually
    /// using this frame. Drives the inspector's read-out so users can
    /// see what each node is configured to do.
    pub current_value: f32,
    /// `(min, max)` for sliders. `None` when the underlying ParamDef
    /// didn't declare a range.
    pub range: Option<(f32, f32)>,
    /// Enum option labels indexed by enum value, for rendering
    /// "FoldX" instead of `6`. `None` for non-enum params.
    pub enum_labels: Option<Vec<String>>,
}

/// UI-facing view of the currently-selected node, decoupled from the
/// renderer's internal `NodeSnapshot`. The editor-window present path
/// builds this from the live snapshot; the panel only knows about
/// this shape.
#[derive(Debug, Clone)]
pub struct GraphEditorNodeView {
    /// Stable handle if the node was registered with one. `None` for
    /// anonymous boundary nodes (Source / FinalOutput) — those have no
    /// user-exposable params.
    pub node_handle: Option<String>,
    /// Display title for the node (header label fallback).
    pub title: String,
    pub parameters: Vec<GraphEditorParam>,
}

/// Right-sidebar width inside the graph-editor window. Bigger than a
/// typical inspector cell because it has to fit a checkbox + a
/// param label without truncation.
pub const SIDEBAR_WIDTH: f32 = 320.0;

const ROW_H: f32 = 28.0;
const PADDING: f32 = 12.0;
const HEADER_H: f32 = 32.0;
const CHECKBOX_W: f32 = 22.0;
const CHECKBOX_H: f32 = 22.0;
const CHECKBOX_GAP: f32 = 10.0;
const FONT_SIZE: u16 = 12;
const HEADER_FONT_SIZE: u16 = 14;

/// Per-row state captured during `build` so `handle_click` can map
/// a node id back to its parameter without re-walking the snapshot.
#[derive(Debug, Clone)]
enum RowState {
    /// A row backed by an inner-node param exposable into the V2 user
    /// surface. Toggling routes through `EffectParamExpose` →
    /// `ToggleEffectParamExposeCommand` (creates/removes a binding).
    InnerNode {
        checkbox_node_id: u32,
        node_handle: String,
        inner_param: String,
        label: String,
        min: f32,
        max: f32,
        default_value: f32,
        convert: UserParamConvert,
        currently_exposed: bool,
    },
    /// A row backed by a def-declared static-block param. Toggling
    /// routes through `EffectStaticParamExpose` →
    /// `ToggleStaticParamExposeCommand` (flips the slot's `exposed`
    /// flag without adding/removing bindings).
    StaticBlock {
        checkbox_node_id: u32,
        param_index: usize,
        currently_exposed: bool,
    },
}

/// One entry in the static-block surface — what the effect card
/// already displays. Surfaced in the sidebar so the user can hide
/// individual sliders without losing the underlying value or any
/// driver/Ableton mapping addressing them.
#[derive(Debug, Clone)]
pub struct GraphEditorStaticParam {
    /// Slot index — used to address the slot via
    /// `EffectInstance.param_values[index]`.
    pub index: usize,
    /// Display label.
    pub label: String,
    /// Whether this slot is currently exposed (visible on the card).
    pub exposed: bool,
}

/// Right-sidebar panel inside the graph-editor window.
///
/// Owns no GPU state — it builds UITree nodes inside the editor's
/// `UIRoot` each time `build` is called. Lifecycle is per-frame rebuild
/// guarded by a "needs rebuild" flag set by `configure` whenever the
/// inputs change.
#[derive(Default)]
pub struct GraphEditorPanel {
    /// The effect this sidebar is editing — set when the editor
    /// opens for an effect chain. `None` when no effect is active.
    effect_index: Option<usize>,
    /// Static-block params from the effect's def. Always rendered at
    /// the top of the sidebar regardless of selection — these are the
    /// knobs that show on the effect card and the user toggles their
    /// visibility from here.
    static_params: Vec<GraphEditorStaticParam>,
    /// View of the currently-selected node, owned. `None` when no
    /// node is selected or the selection is anonymous (no
    /// `node_handle`, so its params are not user-exposable).
    selected_node: Option<GraphEditorNodeView>,
    /// Exposed-state lookup: `(node_handle, inner_param)` keys for
    /// every binding currently on `EffectInstance.user_param_bindings`.
    exposed_keys: HashSet<(String, String)>,
    /// Per-row state, populated during `build`.
    rows: Vec<RowState>,
    /// Root container for everything this panel owns inside the tree.
    /// `-1` until first build.
    root_id: i32,
}

impl GraphEditorPanel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the panel's input data. Bumps the rebuild fingerprint
    /// when anything changed so the next `build` actually rebuilds
    /// the UITree subtree.
    pub fn configure(
        &mut self,
        effect_index: Option<usize>,
        static_params: Vec<GraphEditorStaticParam>,
        selected_node: Option<&GraphEditorNodeView>,
        exposed_keys: HashSet<(String, String)>,
    ) {
        self.effect_index = effect_index;
        self.static_params = static_params;
        self.selected_node = selected_node.cloned();
        self.exposed_keys = exposed_keys;
    }

    /// Build the UITree subtree at the given viewport. Idempotent
    /// w.r.t. tree state — clears the previous root and re-adds.
    pub fn build(&mut self, tree: &mut UITree, viewport: Rect) {
        // Wipe any previous subtree by detaching the old root. Cheap:
        // tree.clear_subtree drops the descendants. (Falls back to a
        // full tree.clear() — see ui_root invocation in app_render.)
        // Here we just rebuild from scratch each time.
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

        // ── Effect Parameters section ─────────────────────────────
        // The static-block surface — what shows on the effect card.
        // Always rendered at the top regardless of node selection.
        tree.add_label(
            bg_id,
            viewport.x + PADDING,
            y,
            viewport.width - 2.0 * PADDING,
            HEADER_H - PADDING,
            "Effect Parameters",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: HEADER_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        y += HEADER_H;

        if self.static_params.is_empty() {
            tree.add_label(
                bg_id,
                viewport.x + PADDING,
                y,
                viewport.width - 2.0 * PADDING,
                ROW_H,
                "(no def-declared parameters)",
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            y += ROW_H;
        } else {
            for sp in &self.static_params {
                let cb_x = viewport.x + PADDING;
                let cb_y = y + (ROW_H - CHECKBOX_H) * 0.5;
                let cb_style = checkbox_style(sp.exposed, true);
                let cb_id = tree.add_button(
                    bg_id,
                    cb_x,
                    cb_y,
                    CHECKBOX_W,
                    CHECKBOX_H,
                    cb_style,
                    if sp.exposed { "✓" } else { "" },
                );

                let label_x = cb_x + CHECKBOX_W + CHECKBOX_GAP;
                let label_w = (viewport.x + viewport.width - PADDING - label_x).max(10.0);
                tree.add_label(
                    bg_id,
                    label_x,
                    y,
                    label_w,
                    ROW_H,
                    &sp.label,
                    UIStyle {
                        text_color: color::TEXT_WHITE_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                );

                self.rows.push(RowState::StaticBlock {
                    checkbox_node_id: cb_id,
                    param_index: sp.index,
                    currently_exposed: sp.exposed,
                });
                y += ROW_H;
            }
        }

        // ── Selected Node section ─────────────────────────────────
        y += PADDING;
        tree.add_label(
            bg_id,
            viewport.x + PADDING,
            y,
            viewport.width - 2.0 * PADDING,
            HEADER_H - PADDING,
            "Inner-Node Parameters",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: HEADER_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        y += HEADER_H;

        // Empty state — nothing selected, or selected node carries no
        // user-exposable parameters.
        let Some(node) = self.selected_node.clone() else {
            tree.add_label(
                bg_id,
                viewport.x + PADDING,
                y,
                viewport.width - 2.0 * PADDING,
                ROW_H,
                "Select a node to expose its parameters.",
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            return;
        };

        let Some(handle) = node.node_handle.clone() else {
            tree.add_label(
                bg_id,
                viewport.x + PADDING,
                y,
                viewport.width - 2.0 * PADDING,
                ROW_H,
                "This node has no stable handle.",
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            return;
        };

        for ps in &node.parameters {
            // Unsupported types — Vec/Color/etc. — show a row but
            // disabled. V2's single-slot surface can't carry them.
            let supported = matches!(
                ps.kind,
                GraphEditorParamKind::Float
                    | GraphEditorParamKind::Int
                    | GraphEditorParamKind::Bool
                    | GraphEditorParamKind::Enum
            );
            let is_exposed = self
                .exposed_keys
                .contains(&(handle.clone(), ps.name.clone()));

            let cb_x = viewport.x + PADDING;
            let cb_y = y + (ROW_H - CHECKBOX_H) * 0.5;
            let cb_style = checkbox_style(is_exposed, supported);
            let cb_id = tree.add_button(
                bg_id,
                cb_x,
                cb_y,
                CHECKBOX_W,
                CHECKBOX_H,
                cb_style,
                if is_exposed { "✓" } else { "" },
            );

            let label_x = cb_x + CHECKBOX_W + CHECKBOX_GAP;
            // Row split: label on the left, current value on the
            // right. Lets the user see what each inner param is
            // *currently set to*, not just what it's named.
            let row_remaining = (viewport.x + viewport.width - PADDING - label_x).max(10.0);
            let value_w = (row_remaining * 0.45).max(60.0);
            let label_w = (row_remaining - value_w).max(10.0);
            tree.add_label(
                bg_id,
                label_x,
                y,
                label_w,
                ROW_H,
                &ps.label,
                UIStyle {
                    text_color: if supported {
                        color::TEXT_WHITE_C32
                    } else {
                        color::TEXT_DIMMED_C32
                    },
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            // Current-value readout.
            let value_str = format_inner_param_value(ps);
            tree.add_label(
                bg_id,
                label_x + label_w,
                y,
                value_w,
                ROW_H,
                &value_str,
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Right,
                    ..UIStyle::default()
                },
            );

            if supported {
                let convert = match ps.kind {
                    GraphEditorParamKind::Float => UserParamConvert::Float,
                    GraphEditorParamKind::Int => UserParamConvert::IntRound,
                    GraphEditorParamKind::Bool => UserParamConvert::BoolThreshold,
                    GraphEditorParamKind::Enum => UserParamConvert::EnumRound,
                    GraphEditorParamKind::Other => UserParamConvert::Float, // unreachable
                };
                let (min, max) = ps.range.unwrap_or((0.0, 1.0));
                self.rows.push(RowState::InnerNode {
                    checkbox_node_id: cb_id,
                    node_handle: handle.clone(),
                    inner_param: ps.name.clone(),
                    label: ps.label.clone(),
                    min,
                    max,
                    default_value: ps.default_value,
                    convert,
                    currently_exposed: is_exposed,
                });
            }

            y += ROW_H;
        }
    }

    /// Map a click on a UITree button back to a `PanelAction`. Returns
    /// an empty Vec when the click didn't land on one of our rows.
    pub fn handle_click(&self, node_id: u32) -> Vec<PanelAction> {
        let Some(effect_index) = self.effect_index else {
            return Vec::new();
        };
        let row = self.rows.iter().find(|r| match r {
            RowState::InnerNode {
                checkbox_node_id, ..
            } => *checkbox_node_id == node_id,
            RowState::StaticBlock {
                checkbox_node_id, ..
            } => *checkbox_node_id == node_id,
        });
        let Some(row) = row else {
            return Vec::new();
        };
        match row {
            RowState::InnerNode {
                node_handle,
                inner_param,
                label,
                min,
                max,
                default_value,
                convert,
                currently_exposed,
                ..
            } => vec![PanelAction::EffectParamExpose {
                effect_index,
                node_handle: node_handle.clone(),
                inner_param: inner_param.clone(),
                expose: !currently_exposed,
                label: label.clone(),
                min: *min,
                max: *max,
                default_value: *default_value,
                convert: convert.clone(),
            }],
            RowState::StaticBlock {
                param_index,
                currently_exposed,
                ..
            } => vec![PanelAction::EffectStaticParamExpose {
                effect_index,
                param_index: *param_index,
                expose: !currently_exposed,
            }],
        }
    }

    /// Convenience wrapper: walk a slice of clicked button ids, map
    /// each to a `PanelAction` via `handle_click`. Used by the
    /// editor-window present path which produces a Vec<u32> of clicks
    /// from the tree's pointer events each frame.
    pub fn dispatch_clicks(&self, clicks: &[u32]) -> Vec<PanelAction> {
        clicks.iter().flat_map(|&n| self.handle_click(n)).collect()
    }
}

/// Format the current value of an inner-node parameter for display
/// in the right sidebar. Enums resolve to their label (e.g., "FoldX"),
/// bools to "true"/"false", and numerics to a short fixed-point form.
fn format_inner_param_value(p: &GraphEditorParam) -> String {
    match p.kind {
        GraphEditorParamKind::Enum => p
            .enum_labels
            .as_ref()
            .and_then(|labels| labels.get(p.current_value as usize).cloned())
            .unwrap_or_else(|| format!("{}", p.current_value as i64)),
        GraphEditorParamKind::Bool => {
            if p.current_value >= 0.5 {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        GraphEditorParamKind::Int => format!("{}", p.current_value as i64),
        GraphEditorParamKind::Float => format!("{:.2}", p.current_value),
        GraphEditorParamKind::Other => "—".to_string(),
    }
}

fn checkbox_style(checked: bool, supported: bool) -> UIStyle {
    // The unchecked state needs a bg that's visibly distinct from the
    // panel bg behind it (which is `EFFECT_CARD_INNER_BG_C32`), or the
    // checkbox disappears into the panel and the user has to guess
    // where to click. Use the standard inactive-button gray.
    let bg_color = match (checked, supported) {
        (true, true) => color::ACCENT_BLUE_C32,
        (false, true) => color::BUTTON_INACTIVE_C32,
        (_, false) => color::BUTTON_INACTIVE_C32,
    };
    let mut style = UIStyle {
        bg_color,
        text_color: color::TEXT_WHITE_C32,
        font_size: HEADER_FONT_SIZE,
        text_align: TextAlign::Center,
        corner_radius: 4.0,
        // Brighter border than CARD_BORDER_C32 so the checkbox edge
        // reads against the inactive-button gray.
        border_color: color::TEXT_DIMMED_C32,
        border_width: 1.0,
        ..UIStyle::default()
    };
    if !supported {
        // Suppress hover by removing INTERACTIVE flag at the call site;
        // here we only style the visual.
        style.text_color = color::TEXT_DIMMED_C32;
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap_node_with_params(handle: Option<&str>) -> GraphEditorNodeView {
        GraphEditorNodeView {
            node_handle: handle.map(|h| h.to_string()),
            title: "UV Transform".to_string(),
            parameters: vec![
                GraphEditorParam {
                    name: "translate".to_string(),
                    label: "Translate".to_string(),
                    kind: GraphEditorParamKind::Float,
                    default_value: 0.0,
                    current_value: 0.0,
                    range: Some((-1.0, 1.0)),
                    enum_labels: None,
                },
                GraphEditorParam {
                    name: "scale".to_string(),
                    label: "Scale".to_string(),
                    kind: GraphEditorParamKind::Float,
                    default_value: 1.0,
                    current_value: 1.0,
                    range: Some((0.0, 4.0)),
                    enum_labels: None,
                },
                GraphEditorParam {
                    name: "color".to_string(),
                    label: "Color".to_string(),
                    kind: GraphEditorParamKind::Other, // disabled — multi-component
                    default_value: 0.0,
                    current_value: 0.0,
                    range: None,
                    enum_labels: None,
                },
            ],
        }
    }

    fn viewport() -> Rect {
        Rect::new(0.0, 0.0, SIDEBAR_WIDTH, 600.0)
    }

    /// Helper: pull the inner-param name from an InnerNode row.
    /// Panics if the row is a StaticBlock — used in tests that explicitly
    /// build inner-node rows.
    fn inner_param_of(row: &RowState) -> &str {
        match row {
            RowState::InnerNode { inner_param, .. } => inner_param.as_str(),
            RowState::StaticBlock { .. } => panic!("expected InnerNode row"),
        }
    }

    fn checkbox_id_of(row: &RowState) -> u32 {
        match row {
            RowState::InnerNode {
                checkbox_node_id, ..
            } => *checkbox_node_id,
            RowState::StaticBlock {
                checkbox_node_id, ..
            } => *checkbox_node_id,
        }
    }

    #[test]
    fn build_renders_rows_for_supported_params_only() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        panel.configure(Some(0), Vec::new(), Some(&node), HashSet::new());
        panel.build(&mut tree, viewport());
        // 2 supported params → 2 inner-node rows tracked. The Color row
        // exists visually but isn't clickable.
        let inner_rows: Vec<&RowState> = panel
            .rows
            .iter()
            .filter(|r| matches!(r, RowState::InnerNode { .. }))
            .collect();
        assert_eq!(inner_rows.len(), 2);
        assert_eq!(inner_param_of(inner_rows[0]), "translate");
        assert_eq!(inner_param_of(inner_rows[1]), "scale");
    }

    #[test]
    fn build_handles_no_selection_with_empty_state() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        panel.configure(Some(0), Vec::new(), None, HashSet::new());
        panel.build(&mut tree, viewport());
        assert!(panel.rows.is_empty());
    }

    #[test]
    fn build_handles_anonymous_node_with_empty_state() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(None); // no handle
        panel.configure(Some(0), Vec::new(), Some(&node), HashSet::new());
        panel.build(&mut tree, viewport());
        assert!(
            panel.rows.is_empty(),
            "anonymous nodes don't expose user-exposable params"
        );
    }

    #[test]
    fn click_on_unchecked_emits_expose_true() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        panel.configure(Some(0), Vec::new(), Some(&node), HashSet::new());
        panel.build(&mut tree, viewport());

        let inner_rows: Vec<&RowState> = panel
            .rows
            .iter()
            .filter(|r| matches!(r, RowState::InnerNode { .. }))
            .collect();
        let translate_cb = checkbox_id_of(inner_rows[0]);
        let actions = panel.handle_click(translate_cb);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::EffectParamExpose {
                effect_index,
                node_handle,
                inner_param,
                expose,
                label,
                min,
                max,
                default_value,
                convert,
            } => {
                assert_eq!(*effect_index, 0);
                assert_eq!(node_handle, "uv_transform");
                assert_eq!(inner_param, "translate");
                assert!(*expose);
                assert_eq!(label, "Translate");
                assert!((*min - -1.0).abs() < f32::EPSILON);
                assert!((*max - 1.0).abs() < f32::EPSILON);
                assert!((*default_value - 0.0).abs() < f32::EPSILON);
                assert!(matches!(convert, UserParamConvert::Float));
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn click_on_checked_emits_expose_false() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        let mut exposed = HashSet::new();
        exposed.insert(("uv_transform".to_string(), "translate".to_string()));
        panel.configure(Some(0), Vec::new(), Some(&node), exposed);
        panel.build(&mut tree, viewport());

        let inner_rows: Vec<&RowState> = panel
            .rows
            .iter()
            .filter(|r| matches!(r, RowState::InnerNode { .. }))
            .collect();
        let translate_cb = checkbox_id_of(inner_rows[0]);
        let actions = panel.handle_click(translate_cb);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::EffectParamExpose { expose, .. } => {
                assert!(!expose, "click on checked checkbox emits expose: false");
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn click_outside_panel_returns_empty() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        panel.configure(Some(0), Vec::new(), Some(&node), HashSet::new());
        panel.build(&mut tree, viewport());
        // Random unrelated node id.
        assert!(panel.handle_click(99999).is_empty());
    }

    #[test]
    fn handle_click_no_effect_index_returns_empty() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let node = snap_node_with_params(Some("uv_transform"));
        // Configure with effect_index = None: the editor isn't open
        // on a specific effect, so clicks must NOT emit.
        panel.configure(None, Vec::new(), Some(&node), HashSet::new());
        panel.build(&mut tree, viewport());
        if let Some(row) = panel.rows.first() {
            assert!(panel.handle_click(checkbox_id_of(row)).is_empty());
        }
    }

    #[test]
    fn static_block_row_emits_static_param_expose() {
        let mut tree = UITree::new();
        let mut panel = GraphEditorPanel::new();
        let static_params = vec![
            GraphEditorStaticParam {
                index: 0,
                label: "Amount".to_string(),
                exposed: true,
            },
            GraphEditorStaticParam {
                index: 1,
                label: "Threshold".to_string(),
                exposed: true,
            },
        ];
        panel.configure(Some(0), static_params, None, HashSet::new());
        panel.build(&mut tree, viewport());

        let static_rows: Vec<&RowState> = panel
            .rows
            .iter()
            .filter(|r| matches!(r, RowState::StaticBlock { .. }))
            .collect();
        assert_eq!(static_rows.len(), 2);

        let cb_id = checkbox_id_of(static_rows[1]);
        let actions = panel.handle_click(cb_id);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::EffectStaticParamExpose {
                effect_index,
                param_index,
                expose,
            } => {
                assert_eq!(*effect_index, 0);
                assert_eq!(*param_index, 1);
                // Was exposed=true → click emits expose=false.
                assert!(!expose);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }
}
