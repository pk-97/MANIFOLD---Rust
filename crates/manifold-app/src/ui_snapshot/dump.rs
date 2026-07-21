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
                // BUG-071: the LIVE reparented parent (`UITree::parent_of`,
                // backed by `parent_index`), not `n.parent_id` — the mint-time
                // struct field `reparent_root_nodes` never updates. Serializing
                // the stale field made a correctly reparented (and correctly
                // clipped/rendered) tree look unclipped in the dump.
                "parent": tree.parent_of(n.id).map(|p| p.index()),
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

/// D9 widget catalog as JSON — the enumeration view over the surface layer
/// (`docs/UI_FUNNEL_DECOMPOSITION_DESIGN.md` D9). Reuses the tree dump's own
/// field encodings verbatim: `widget` is the SAME `{:016x}` durable `WidgetId`
/// hex [`dump_tree_ex`] emits, `name` the SAME `name_of` string. This is not a
/// new protocol — it only regroups those durable facts per panel → per card →
/// per row affordance, adding the `RowRole` (from the card's `RowIndex`). A
/// `name` of `null` is a nameless sanctioned affordance surfaced (the BUG-239
/// shape), never invented. Empty `surfaces` when no card is live in the scene.
pub fn catalog_json(panel: &str, surfaces: &[manifold_ui::param_surface::CatalogSurface]) -> Value {
    let surfaces_json: Vec<Value> = surfaces
        .iter()
        .map(|s| {
            let affordances: Vec<Value> = s
                .affordances
                .iter()
                .map(|a| {
                    json!({
                        "row_id": a.row_id,
                        "role": format!("{:?}", a.role),
                        // Same durable WidgetId encoding as the node dump's `widget`.
                        "widget": format!("{:016x}", a.widget),
                        "name": a.name,
                    })
                })
                .collect();
            json!({
                "kind": format!("{:?}", s.kind),
                "title": s.title,
                "affordance_count": affordances.len(),
                "affordances": affordances,
            })
        })
        .collect();
    json!({
        "panel": panel,
        "surface_count": surfaces_json.len(),
        "surfaces": surfaces_json,
    })
}

/// Terse stdout catalog — one line per card, then one per affordance, marking
/// a nameless affordance with `name=<none>` so a BUG-239 gap is obvious inline.
pub fn terse_catalog(panel: &str, surfaces: &[manifold_ui::param_surface::CatalogSurface]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "catalog[{panel}]: {} surface(s)", surfaces.len());
    for s in surfaces {
        let _ = writeln!(out, "  [{:?}] {} — {} affordance(s)", s.kind, s.title, s.affordances.len());
        for a in &s.affordances {
            let name = a.name.as_deref().unwrap_or("<none>");
            let _ = writeln!(out, "    {} {:?} name={} widget={:016x}", a.row_id, a.role, name, a.widget);
        }
    }
    out
}

/// One line per node — compact, for inline reading. Shows the fields that
/// actually distinguish a node: type, rect, bg, text, font size, radius, border.
pub fn terse(tree: &UITree) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} nodes:", tree.nodes().len());
    for n in tree.nodes() {
        let s = &n.style;
        // BUG-071: live reparented parent, not the mint-time `n.parent_id` —
        // see the matching comment in `dump_tree_ex` above.
        let parent = tree.parent_of(n.id).map_or_else(|| "-".to_string(), |p| p.index().to_string());
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
