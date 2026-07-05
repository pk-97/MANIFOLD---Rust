//! The tree dump — the centerpiece of the harness. Walk a built `UITree` and
//! emit every node's real layout + style values as JSON (next to the PNG) and
//! a terse stdout summary, so UI work is reasoned on values, not eyeballed.
//! See `docs/HEADLESS_UI_HARNESS.md` §1.
//!
//! Extended (`UI_AUTOMATION_DESIGN.md` §3, additive): each node gains
//! `widget` (the durable `WidgetId`, interactive nodes only) and `name` (the
//! static component name, when registered — D8). A sibling top-level
//! `custom_surfaces` key carries the [`HitTargets`] enumeration (§5) for
//! surfaces `UITree::hit_test` can't see inside (graph canvas, timeline
//! clips, automation lanes) — none of those surfaces is owned by any single
//! `UITree` node in this headless harness (the graph canvas in particular is
//! addressed by a plain screen `Rect`, not a tree node — see
//! `app_render.rs::editor_canvas_viewport`), so their targets are carried
//! alongside `nodes` rather than nested under one, which keeps the field
//! genuinely additive: existing per-node fields are untouched either way.

use std::fmt::Write as _;

use manifold_ui::hit_targets::HitTargets;
use manifold_ui::{Color32, UITree};
use serde_json::{json, Value};

/// Full machine-readable dump of every node in paint order, plus the
/// enumeration of every `surfaces` entry under a sibling top-level
/// `custom_surfaces` array (empty when `surfaces` is empty) — additive: the
/// per-node fields are unaffected by whether any surfaces are passed.
pub fn dump_tree_ex(tree: &UITree, surfaces: &[&dyn HitTargets]) -> Value {
    let nodes: Vec<Value> = tree
        .nodes()
        .iter()
        .map(|n| {
            let s = &n.style;
            let widget = tree.widget_of(n.id);
            json!({
                "id": n.id.index(),
                "gen": n.id.generation(),
                "parent": n.parent_id.map(|p| p.index()),
                "type": format!("{:?}", n.node_type),
                "rect": [n.bounds.x, n.bounds.y, n.bounds.width, n.bounds.height],
                "text": n.text,
                "bg": hexa(s.bg_color),
                "text_color": hexa(s.text_color),
                "border": hexa(s.border_color),
                "radius": s.corner_radius,
                "border_width": s.border_width,
                "font_size": s.font_size,
                "font_weight": format!("{:?}", s.font_weight),
                "align": format!("{:?}", s.text_align),
                "flags": format!("{:?}", n.flags),
                "draw_order": n.draw_order,
                "widget": if n.flags.contains(manifold_ui::UIFlags::INTERACTIVE) {
                    Some(format!("{:016x}", widget.raw()))
                } else {
                    None
                },
                "name": tree.name_of(n.id),
            })
        })
        .collect();

    let custom_surfaces: Vec<Value> = surfaces
        .iter()
        .map(|surface| {
            let mut entries = Vec::new();
            surface.enumerate(&mut entries);
            let targets: Vec<Value> = entries
                .iter()
                .map(|e| {
                    json!({
                        "kind": e.kind,
                        "label": e.label,
                        "rect": [e.rect.x, e.rect.y, e.rect.width, e.rect.height],
                        "payload": e.payload,
                    })
                })
                .collect();
            json!({ "surface_id": surface.surface_id(), "targets": targets })
        })
        .collect();

    json!({ "node_count": nodes.len(), "nodes": nodes, "custom_surfaces": custom_surfaces })
}

/// One line per node — compact, for inline reading. Shows the fields that
/// actually distinguish a node: type, rect, bg, text, font size, radius, border.
pub fn terse(tree: &UITree) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} nodes:", tree.nodes().len());
    for n in tree.nodes() {
        let s = &n.style;
        let parent = n.parent_id.map_or_else(|| "-".to_string(), |p| p.index().to_string());
        let _ = write!(
            out,
            "#{:<3} {:<9} p={:<3} ({:>6.1},{:>6.1} {:>6.1}x{:>5.1}) bg={}",
            n.id.index(),
            format!("{:?}", n.node_type),
            parent,
            n.bounds.x,
            n.bounds.y,
            n.bounds.width,
            n.bounds.height,
            hexa(s.bg_color),
        );
        if s.border_width > 0.0 {
            let _ = write!(out, " bd={}@{:.1}", hexa(s.border_color), s.border_width);
        }
        if s.corner_radius > 0.0 {
            let _ = write!(out, " r={:.1}", s.corner_radius);
        }
        if let Some(t) = &n.text {
            let _ = write!(out, " fs={} txt={:?}", s.font_size, t);
        }
        let _ = writeln!(out);
    }
    out
}

fn hexa(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a)
}
