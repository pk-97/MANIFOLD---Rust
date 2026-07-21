//! Pure-logic tests for the group-aware canvas. Everything that isn't
//! pixels is exercised here so a misbehaving canvas points to rendering
//! (eyes only), not logic. Per the handoff doc's debug-friendly mandate.
use crate::{ParamsAction};
use super::*;
// Items used only by tests are imported directly from their module rather than
// re-exported crate-wide from `mod.rs` (which would read as unused in a
// non-test build).
use super::hit::{ports_compatible, rects_overlap};
use super::layout::LayeredLayout;
use crate::graph_view::{
    GraphSnapshot, GroupSnapshot, NodeSnapshot, PortKindSnapshot, PortSnapshot, WireSnapshot,
};
use crate::panels::GraphParamTarget;

fn port(name: &str) -> PortSnapshot {
    PortSnapshot {
        name: name.to_string(),
        kind: PortKindSnapshot::Texture2D,
    }
}

/// Build a plain (non-group) node snapshot with one `in` / one `out`.
fn node(id: u32, type_id: &str, handle: Option<&str>) -> NodeSnapshot {
    NodeSnapshot {
        id,
        node_id: handle.map(manifold_foundation::NodeId::new).unwrap_or_default(),
        node_handle: handle.map(|h| h.to_string()),
        type_id: type_id.to_string(),
        title: handle.unwrap_or(type_id).to_string(),
        inputs: vec![port("in")],
        outputs: vec![port("out")],
        parameters: Vec::new(),
        editor_pos: None,
        breaks_dependency_cycle: false,
        group: None,
        wgsl_source: None,
        category: crate::graph_view::Category::Uncategorized,
        tooltip: None,
    }
}

fn wire(fln: u32, fp: &str, tn: u32, tp: &str) -> WireSnapshot {
    WireSnapshot {
        from_node: fln,
        from_port: fp.to_string(),
        to_node: tn,
        to_port: tp.to_string(),
    }
}

/// Root: source(0) → group(10) → final(2). The group body is
/// group_input(0) → inner(1) → group_output(2).
fn grouped_snapshot() -> GraphSnapshot {
    let body = GroupSnapshot {
        nodes: vec![
            node(0, "system.group_input", None),
            node(1, "node.blur", Some("inner")),
            node(2, "system.group_output", None),
        ],
        wires: vec![wire(0, "src", 1, "in"), wire(1, "out", 2, "out")],
        tint: None,
    };
    let mut group = node(10, GROUP_TYPE_ID, Some("tweak"));
    group.inputs = vec![port("src")];
    group.outputs = vec![port("out")];
    group.group = Some(Box::new(body));
    GraphSnapshot {
        nodes: vec![
            node(0, "system.source", Some("source")),
            group,
            node(2, "system.final_output", Some("final")),
        ],
        wires: vec![wire(0, "out", 10, "src"), wire(10, "out", 2, "in")],
        outer_routings: Vec::new(),
    }
}

#[test]
fn resolve_level_root_then_descend_then_invalid() {
    let snap = grouped_snapshot();

    // Empty scope → document root (3 nodes incl. the group).
    let (rn, rw) = resolve_level(&snap, &[]).expect("root resolves");
    assert_eq!(rn.len(), 3);
    assert_eq!(rw.len(), 2);
    assert!(rn.iter().any(|n| n.type_id == GROUP_TYPE_ID));

    // Into the group → its body (group_input, inner, group_output).
    let (bn, bw) = resolve_level(&snap, &[10]).expect("group body resolves");
    assert_eq!(bn.len(), 3);
    assert_eq!(bw.len(), 2);
    assert!(bn.iter().any(|n| n.node_handle.as_deref() == Some("inner")));

    // A non-group id (source) or a missing id → None.
    assert!(resolve_level(&snap, &[0]).is_none());
    assert!(resolve_level(&snap, &[999]).is_none());
}

#[test]
fn set_snapshot_marks_groups_and_navigation_swaps_level() {
    let snap = grouped_snapshot();
    let mut canvas = GraphCanvas::new();

    // Root level: the group node is flagged and the inner node is hidden.
    canvas.set_snapshot(&snap);
    assert_eq!(canvas.nodes.len(), 3);
    let group = canvas.nodes.iter().find(|n| n.is_group).expect("group view");
    assert_eq!(group.id, 10);
    assert!(canvas.nodes.iter().all(|n| n.title != "inner"));

    // Descend → the canvas now shows the group body.
    canvas.enter_group(10);
    canvas.set_snapshot(&snap);
    assert_eq!(canvas.scope_path(), &[10]);
    assert!(canvas.nodes.iter().any(|n| n.title == "inner"));
    assert!(canvas.nodes.iter().all(|n| !n.is_group));

    // Exit → back to root.
    assert!(canvas.exit_group());
    canvas.set_snapshot(&snap);
    assert!(canvas.scope_path().is_empty());
    assert!(canvas.nodes.iter().any(|n| n.is_group));
}

#[test]
fn visible_node_thumbnails_preview_image_nodes_and_groups_via_producer() {
    let snap = grouped_snapshot();
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);
    // A viewport huge enough that nothing culls.
    let vp = Rect::new(-10_000.0, -10_000.0, 20_000.0, 20_000.0);
    let thumbs = canvas.visible_node_thumbnails(vp);
    // All three root nodes output an image (the fixture's ports are
    // Texture2D), so each gets a preview strip: source, final, and the
    // group — but the group is keyed to its OUTPUT PRODUCER ("inner"), not
    // its own id, since that inner node is the cell already in the atlas.
    assert_eq!(thumbs.len(), 3, "two image nodes + the group via its producer");
    assert!(
        thumbs.iter().any(|(id, ..)| id.as_str() == "inner"),
        "the group previews its inner output producer"
    );
    assert!(
        thumbs.iter().all(|(id, ..)| id.as_str() != "tweak"),
        "a group is never keyed to its own id — always its producer's"
    );
    // Each preview-strip rect is the image area and has positive size.
    assert!(thumbs.iter().all(|(_, _, _, w, h)| *w > 0.0 && *h > 0.0));
}

#[test]
fn preview_screen_size_follows_project_aspect() {
    // Landscape (16:9) is width-bound: full inner width, short.
    let (w, h) = preview_screen_size(16.0 / 9.0);
    assert!((w - PREVIEW_IMG_W).abs() < 0.01, "16:9 fills the node width");
    assert!((w / h - 16.0 / 9.0).abs() < 0.01, "16:9 aspect preserved");
    assert!(h <= PREVIEW_MAX_H, "16:9 height sits under the cap");

    // Portrait (9:16) is height-bound: capped height, narrower, aspect kept.
    let (pw, ph) = preview_screen_size(9.0 / 16.0);
    assert!((ph - PREVIEW_MAX_H).abs() < 0.01, "portrait clamps to the height cap");
    assert!((pw / ph - 9.0 / 16.0).abs() < 0.01, "portrait aspect preserved");
    assert!(pw < PREVIEW_IMG_W, "portrait screen is narrower than the band");

    // A non-finite / non-positive aspect falls back to 16:9 rather than NaN.
    let (fw, fh) = preview_screen_size(0.0);
    assert!(fw > 0.0 && fh > 0.0 && (fw / fh - 16.0 / 9.0).abs() < 0.01);
}

#[test]
fn set_preview_aspect_resizes_screens_and_node_heights() {
    let snap = grouped_snapshot();
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);
    let landscape_h = canvas
        .nodes
        .iter()
        .find(|n| n.preview_screen.is_some())
        .map(|n| n.height())
        .expect("an image node exists");

    // Switching to a portrait project grows previewable node heights (taller
    // screen) and resizes the screens in place; positions are untouched.
    canvas.set_preview_aspect(9.0 / 16.0);
    let node = canvas
        .nodes
        .iter()
        .find(|n| n.preview_screen.is_some())
        .unwrap();
    let (sw, sh) = node.preview_screen.unwrap();
    assert!((sw / sh - 9.0 / 16.0).abs() < 0.01, "screen took the portrait aspect");
    assert!(node.height() > landscape_h, "portrait preview makes the node taller");
}

#[test]
fn stale_scope_falls_back_to_root() {
    let snap = grouped_snapshot();
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);
    canvas.enter_group(10);
    canvas.set_snapshot(&snap);
    assert_eq!(canvas.scope_path(), &[10]);

    // The group vanishes (e.g. an undo dissolved it). Next push of a
    // snapshot without node 10 must drop the canvas back to root rather
    // than render an empty level.
    let mut flat = grouped_snapshot();
    flat.nodes.retain(|n| n.id != 10);
    flat.wires.clear();
    canvas.set_snapshot(&flat);
    assert!(canvas.scope_path().is_empty());
}

#[test]
fn breadcrumb_segments_track_scope_titles() {
    let snap = grouped_snapshot();
    let mut canvas = GraphCanvas::new();
    let vp = Rect::new(0.0, 0.0, 1200.0, 800.0);

    // Root → no breadcrumb.
    canvas.set_snapshot(&snap);
    assert!(canvas.breadcrumb_segments(vp).is_empty());

    // Inside the group → [Root, tweak], with "tweak" current.
    canvas.enter_group(10);
    canvas.set_snapshot(&snap);
    let segs = canvas.breadcrumb_segments(vp);
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0].2, "Root");
    assert!(!segs[0].3, "root crumb is an ancestor, not current");
    assert_eq!(segs[1].2, "tweak");
    assert!(segs[1].3, "deepest crumb is current");

    // Breadcrumb jump back to root.
    canvas.set_scope_depth(0);
    assert!(canvas.scope_path().is_empty());
}

#[test]
fn rects_overlap_is_strict_and_symmetric() {
    let a = (0.0, 0.0, 10.0, 10.0);
    // Overlapping.
    assert!(rects_overlap(a, (5.0, 5.0, 10.0, 10.0)));
    assert!(rects_overlap((5.0, 5.0, 10.0, 10.0), a));
    // Fully containing.
    assert!(rects_overlap(a, (2.0, 2.0, 1.0, 1.0)));
    // Touching edge only — not an overlap (strict).
    assert!(!rects_overlap(a, (10.0, 0.0, 5.0, 5.0)));
    // Disjoint.
    assert!(!rects_overlap(a, (20.0, 20.0, 5.0, 5.0)));
}

#[test]
fn double_click_window_requires_same_target() {
    let mut canvas = GraphCanvas::new();
    // First press on node 7.
    canvas.note_click(100.0, 100.0, 1.0, Some(7));
    // Second press just after, same spot, same node → double.
    assert!(canvas.is_double_click(100.5, 100.0, 1.1, Some(7)));
    // Same timing but a different node → not a double.
    assert!(!canvas.is_double_click(100.5, 100.0, 1.1, Some(8)));
    // Same node but too far → not a double.
    assert!(!canvas.is_double_click(140.0, 100.0, 1.1, Some(7)));
    // Same node but too slow → not a double.
    assert!(!canvas.is_double_click(100.5, 100.0, 1.0 + 5.0, Some(7)));
}

#[test]
fn wrap_text_breaks_on_spaces_within_limit() {
    let lines = wrap_text("the quick brown fox jumps", 11);
    // Every line is within the limit and nothing is dropped.
    assert!(lines.iter().all(|l| l.chars().count() <= 11));
    assert_eq!(lines.join(" "), "the quick brown fox jumps");
    assert!(lines.len() > 1);
}

#[test]
fn wrap_text_keeps_an_overlong_word_whole() {
    // A single word past the limit isn't chopped mid-word; it gets its
    // own line and overflows the box slightly rather than corrupting.
    let lines = wrap_text("supercalifragilistic ok", 8);
    assert_eq!(lines[0], "supercalifragilistic");
    assert_eq!(lines[1], "ok");
}

#[test]
fn wrap_text_empty_input_is_empty() {
    assert!(wrap_text("", 20).is_empty());
    assert!(wrap_text("   ", 20).is_empty());
}

// ── Layered auto-layout ─────────────────────────────────────────

#[test]
fn layout_uncrosses_a_simple_swap() {
    // Two columns, edges 0→3 and 1→2 — one crossing as ordered.
    let mut l = LayeredLayout {
        num_cols: 2,
        column: vec![0, 0, 1, 1],
        height: vec![40.0; 4],
        order: vec![vec![0, 1], vec![2, 3]],
        up_edges: vec![vec![], vec![], vec![(1, 20.0, 20.0)], vec![(0, 20.0, 20.0)]],
        down_edges: vec![vec![(3, 20.0, 20.0)], vec![(2, 20.0, 20.0)], vec![], vec![]],
    };
    assert_eq!(l.count_crossings(), 1);
    l.minimise_crossings();
    assert_eq!(l.count_crossings(), 0);
}

#[test]
fn layout_straightens_a_chain() {
    // 0 → 1 → 2 across three columns: equal heights and port offsets,
    // so coordinate assignment should give all three the same top.
    let off = 25.0;
    let l = LayeredLayout {
        num_cols: 3,
        column: vec![0, 1, 2],
        height: vec![50.0; 3],
        order: vec![vec![0], vec![1], vec![2]],
        up_edges: vec![vec![], vec![(0, off, off)], vec![(1, off, off)]],
        down_edges: vec![vec![(1, off, off)], vec![(2, off, off)], vec![]],
    };
    let y = l.assign_y();
    assert!((y[0] - y[1]).abs() < 0.01, "y0 {} y1 {}", y[0], y[1]);
    assert!((y[1] - y[2]).abs() < 0.01, "y1 {} y2 {}", y[1], y[2]);
}

#[test]
fn layout_threads_long_edge_straight_through_waypoint() {
    // node0 (col0) → node1 (col2), routed through waypoint lvid 2 in
    // col1. The two ports and the waypoint centre must end up colinear.
    let off = 30.0;
    let mid = LAYOUT_DUMMY_H * 0.5;
    let l = LayeredLayout {
        num_cols: 3,
        column: vec![0, 2, 1],
        height: vec![50.0, 50.0, LAYOUT_DUMMY_H],
        order: vec![vec![0], vec![2], vec![1]],
        up_edges: vec![vec![], vec![(2, mid, off)], vec![(0, off, mid)]],
        down_edges: vec![vec![(2, off, mid)], vec![], vec![(1, mid, off)]],
    };
    let y = l.assign_y();
    let p_out = y[0] + off; // node0 output port
    let p_mid = y[2] + mid; // waypoint centre
    let p_in = y[1] + off; // node1 input port
    assert!((p_out - p_mid).abs() < 0.01, "out {p_out} mid {p_mid}");
    assert!((p_in - p_mid).abs() < 0.01, "in {p_in} mid {p_mid}");
}

// ── Live values overlay ─────────────────────────────────────────

/// A Float param snapshot over `[0, 1]`, current value `current`.
fn float_param(name: &str, current: f32) -> crate::graph_view::ParamSnapshot {
    crate::graph_view::ParamSnapshot {
        name: name.to_string(),
        label: name.to_string(),
        kind: crate::graph_view::ParamSnapshotKind::Float,
        default_value: 0.0,
        current_value: current,
        range: Some((0.0, 1.0)),
        enum_labels: None,
        exposed: false,
        summary: None,
        vec_value: None,
        string_value: None,
        table_value: None,
        tooltip: None,
    }
}

/// One plain node (`id == 1`, given handle so its `node_id` is set) carrying
/// `params`, wrapped in a root snapshot.
fn snapshot_with_param_node(
    handle: &str,
    params: Vec<crate::graph_view::ParamSnapshot>,
) -> GraphSnapshot {
    let mut n = node(1, "node.exposure", Some(handle));
    n.parameters = params;
    GraphSnapshot {
        nodes: vec![n],
        wires: Vec::new(),
        outer_routings: Vec::new(),
    }
}

#[test]
fn apply_live_values_refreshes_on_face_value_and_fill() {
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.25)]));
    // The frozen snapshot value formats to 0.25.
    assert_eq!(canvas.find_node(1).unwrap().params[0].value, "0.25");

    // A live value of 0.80 overlays the frozen value, refreshing the string,
    // the fill bar, and the scrub anchor — matched by stable node_id.
    let live = vec![(manifold_foundation::NodeId::new("gain"), vec![("amount", 0.8_f32)])];
    canvas.apply_live_values(&live);
    let pv = &canvas.find_node(1).unwrap().params[0];
    assert_eq!(pv.value, "0.80");
    assert!((pv.fill.unwrap() - 0.8).abs() < 1e-3);
    assert!((pv.scrub.unwrap().current_value - 0.8).abs() < 1e-6);
}

#[test]
fn apply_live_values_skips_the_actively_scrubbed_param() {
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.25)]));
    // Mid-scrub on this exact (node, param): the drag stays the source of
    // truth, so the live feed must not overwrite it.
    canvas.drag.start(
        CanvasDrag::ParamScrub {
            node_id: 1,
            param_name: "amount".to_string(),
            range: (0.0, 1.0),
            start_value: 0.25,
            is_int: false,
            outer_param_id: None,
        },
        crate::node::Vec2::ZERO,
    );
    let live = vec![(manifold_foundation::NodeId::new("gain"), vec![("amount", 0.8_f32)])];
    canvas.apply_live_values(&live);
    assert_eq!(canvas.find_node(1).unwrap().params[0].value, "0.25");
}

#[test]
fn apply_live_values_empty_feed_is_a_noop() {
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.25)]));
    canvas.apply_live_values(&Vec::new());
    assert_eq!(canvas.find_node(1).unwrap().params[0].value, "0.25");
}

#[test]
fn apply_live_values_ignores_unmatched_node_ids() {
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.25)]));
    // A live entry for a different node leaves this one untouched.
    let live = vec![(manifold_foundation::NodeId::new("other"), vec![("amount", 0.8_f32)])];
    canvas.apply_live_values(&live);
    assert_eq!(canvas.find_node(1).unwrap().params[0].value, "0.25");
}

/// A grouped snapshot whose sole inner node ("inner") carries a Float param
/// exposed on the group's own face via `outer_routings` — the D6 group-face
/// mirror row fixture for [`GraphCanvas::apply_live_values`]'s inner-source
/// matching (`ParamView::live_source`).
fn grouped_snapshot_with_group_param(current: f32) -> GraphSnapshot {
    let mut inner = node(1, "node.blur", Some("inner"));
    inner.parameters = vec![float_param("amount", current)];
    let body = GroupSnapshot {
        nodes: vec![node(0, "system.group_input", None), inner, node(2, "system.group_output", None)],
        wires: vec![wire(0, "src", 1, "in"), wire(1, "out", 2, "out")],
        tint: None,
    };
    let mut group = node(10, GROUP_TYPE_ID, Some("tweak"));
    group.inputs = vec![port("src")];
    group.outputs = vec![port("out")];
    group.group = Some(Box::new(body));
    GraphSnapshot {
        nodes: vec![
            node(0, "system.source", Some("source")),
            group,
            node(2, "system.final_output", Some("final")),
        ],
        wires: vec![wire(0, "out", 10, "src"), wire(10, "out", 2, "in")],
        outer_routings: vec![crate::graph_view::OuterParamRouting {
            outer_label: "Amount".to_string(),
            outer_param_id: "og_amount".to_string(),
            node_handle: "inner".to_string(),
            inner_param: "amount".to_string(),
            source: crate::graph_view::OuterParamSource::User,
        }],
    }
}

#[test]
fn apply_live_values_updates_group_face_mirror_row_via_inner_source() {
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&grouped_snapshot_with_group_param(0.25));
    let group = canvas.find_node(10).unwrap();
    assert_eq!(group.params.len(), 1);
    assert_eq!(group.params[0].name, "og_amount", "row is renamed to the outer id (D6)");
    assert_eq!(group.params[0].value, "0.25");

    // The live entry is keyed by the INNER node's id/param — not the
    // group's own (empty) node_id, and not the row's renamed
    // `outer_param_id` — and must still reach the group-face row via
    // `live_source`.
    let live = vec![(manifold_foundation::NodeId::new("inner"), vec![("amount", 0.8_f32)])];
    canvas.apply_live_values(&live);
    let pv = &canvas.find_node(10).unwrap().params[0];
    assert_eq!(pv.value, "0.80");
    assert!((pv.fill.unwrap() - 0.8).abs() < 1e-3);
}

#[test]
fn apply_live_values_group_face_row_keeps_snapshot_value_when_unmatched() {
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&grouped_snapshot_with_group_param(0.25));
    // A live feed with no entry for the inner node's id leaves the mirror
    // row at its frozen snapshot value — no panic, no stale-key match.
    let live = vec![(manifold_foundation::NodeId::new("unrelated"), vec![("amount", 0.9_f32)])];
    canvas.apply_live_values(&live);
    let pv = &canvas.find_node(10).unwrap().params[0];
    assert_eq!(pv.value, "0.25");
}

#[test]
fn apply_live_values_feeds_sparkline_history() {
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.2)]));
    for v in [0.2_f32, 0.5, 0.8] {
        canvas.apply_live_values(&vec![(
            manifold_foundation::NodeId::new("gain"),
            vec![("amount", v)],
        )]);
    }
    let hist = canvas
        .spark_history
        .get(&manifold_foundation::NodeId::new("gain"))
        .expect("history recorded for the primary param");
    assert_eq!(hist.len(), 3);
    // Range is 0..1, so the stored normalized (fill) value equals the input.
    assert!((hist[0] - 0.2).abs() < 1e-4);
    assert!((hist[2] - 0.8).abs() < 1e-4);
}

#[test]
fn sparkline_history_is_capped_at_capacity() {
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.2)]));
    for i in 0..(SPARK_CAPACITY + 10) {
        let v = (i % 10) as f32 / 10.0;
        canvas.apply_live_values(&vec![(
            manifold_foundation::NodeId::new("gain"),
            vec![("amount", v)],
        )]);
    }
    let hist = canvas
        .spark_history
        .get(&manifold_foundation::NodeId::new("gain"))
        .unwrap();
    assert_eq!(hist.len(), SPARK_CAPACITY, "ring buffer holds the cap, no more");
}

#[test]
fn topology_rebuild_prunes_stale_sparkline_history() {
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snapshot_with_param_node("gain", vec![float_param("amount", 0.2)]));
    canvas.apply_live_values(&vec![(
        manifold_foundation::NodeId::new("gain"),
        vec![("amount", 0.5_f32)],
    )]);
    assert!(canvas.spark_history.contains_key(&manifold_foundation::NodeId::new("gain")));

    // A real topology change (different runtime id, so `hash_level` differs
    // and `set_snapshot` takes the full-rebuild path) evicts the old node's
    // trace so the history map can't accrete across a session.
    let mut n = node(2, "node.exposure", Some("other"));
    n.parameters = vec![float_param("amount", 0.2)];
    let snap2 = GraphSnapshot {
        nodes: vec![n],
        wires: Vec::new(),
        outer_routings: Vec::new(),
    };
    canvas.set_snapshot(&snap2);
    assert!(!canvas.spark_history.contains_key(&manifold_foundation::NodeId::new("gain")));
}

// ── Jump-to-node ────────────────────────────────────────────────

#[test]
fn find_node_scope_locates_root_and_nested_nodes() {
    let snap = grouped_snapshot();
    // Root-level node: empty scope.
    let (path, _, rid) = find_node_scope(&snap, &manifold_foundation::NodeId::new("source")).unwrap();
    assert!(path.is_empty());
    assert_eq!(rid, 0);
    // Node inside the group: scope = [group runtime id], title carried.
    let (path, titles, rid) =
        find_node_scope(&snap, &manifold_foundation::NodeId::new("inner")).unwrap();
    assert_eq!(path, vec![10]);
    assert_eq!(titles, vec!["tweak".to_string()]);
    assert_eq!(rid, 1);
    // Unknown id.
    assert!(find_node_scope(&snap, &manifold_foundation::NodeId::new("nope")).is_none());
}

#[test]
fn focus_node_descends_into_the_group_and_selects() {
    let snap = grouped_snapshot();
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);
    assert!(canvas.focus_node(&snap, &manifold_foundation::NodeId::new("inner")));
    assert_eq!(canvas.scope_path(), &[10]);
    assert_eq!(canvas.selected_ids(), vec![1]);
    assert_eq!(canvas.pending_focus, Some(1));
}

#[test]
fn focus_node_unknown_id_is_a_noop() {
    let snap = grouped_snapshot();
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);
    assert!(!canvas.focus_node(&snap, &manifold_foundation::NodeId::new("nope")));
    assert!(canvas.scope_path().is_empty());
    assert_eq!(canvas.pending_focus, None);
}

#[test]
fn resolve_card_param_node_id_via_outer_routing() {
    let mut snap = grouped_snapshot();
    snap.outer_routings = vec![crate::graph_view::OuterParamRouting {
        outer_label: "Amount".into(),
        outer_param_id: "user.inner.amount.0".into(),
        node_handle: "inner".into(),
        inner_param: "amount".into(),
        source: crate::graph_view::OuterParamSource::Static,
    }];
    let nid = resolve_card_param_node_id(&snap, "user.inner.amount.0").unwrap();
    assert_eq!(nid.as_str(), "inner");
    // Unrouted param id resolves to nothing.
    assert!(resolve_card_param_node_id(&snap, "user.nope.0").is_none());
}

// ── Group tint ──────────────────────────────────────────────────

#[test]
fn cycle_group_tint_emits_first_palette_colour_for_untinted_group() {
    let snap = grouped_snapshot();
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);
    canvas.select_single(10); // the group node
    canvas.request_cycle_group_tint();
    let emitted = canvas.drain_edits().into_iter().find_map(|a| match a {
        GraphEditCommand::SetGroupTint {
            group_id: 10, tint, ..
        } => Some(tint),
        _ => None,
    });
    // The command carries the def's plain-sRGB float array; the palette is
    // sRGB `Color32`. Compare through the same boundary conversion.
    assert_eq!(emitted, Some(Some(GROUP_TINT_PALETTE[0].to_srgb_f32())));
}

#[test]
fn cycle_group_tint_noop_without_a_selected_group() {
    let snap = grouped_snapshot();
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);
    canvas.select_single(0); // a plain node, not a group
    canvas.request_cycle_group_tint();
    assert!(
        canvas
            .drain_edits()
            .iter()
            .all(|a| !matches!(a, GraphEditCommand::SetGroupTint { .. })),
        "no tint action without a selected group"
    );
}

// ── On-node expose glyph (Blender-style) ────────────────────────

/// An Enum param over labels `["A","B","C"]`, no declared range, unexposed.
fn enum_param(name: &str, current: f32) -> crate::graph_view::ParamSnapshot {
    crate::graph_view::ParamSnapshot {
        name: name.to_string(),
        label: name.to_string(),
        kind: crate::graph_view::ParamSnapshotKind::Enum,
        default_value: 0.0,
        current_value: current,
        range: None,
        enum_labels: Some(vec!["A".into(), "B".into(), "C".into()]),
        exposed: false,
        summary: None,
        vec_value: None,
        string_value: None,
        table_value: None,
        tooltip: None,
    }
}

fn bool_param(name: &str, current: f32) -> crate::graph_view::ParamSnapshot {
    crate::graph_view::ParamSnapshot {
        name: name.to_string(),
        label: name.to_string(),
        kind: crate::graph_view::ParamSnapshotKind::Bool,
        default_value: 0.0,
        current_value: current,
        range: None,
        enum_labels: None,
        exposed: false,
        summary: None,
        vec_value: None,
        string_value: None,
        table_value: None,
        tooltip: None,
    }
}

fn trigger_param(name: &str, current: f32) -> crate::graph_view::ParamSnapshot {
    crate::graph_view::ParamSnapshot {
        name: name.to_string(),
        label: name.to_string(),
        kind: crate::graph_view::ParamSnapshotKind::Trigger,
        default_value: 0.0,
        current_value: current,
        range: None,
        enum_labels: None,
        exposed: false,
        summary: None,
        vec_value: None,
        string_value: None,
        table_value: None,
        tooltip: None,
    }
}

/// Expand node 1 and return a viewport that culls nothing.
fn expanded_canvas(param: crate::graph_view::ParamSnapshot) -> (GraphCanvas, Rect) {
    let mut canvas = GraphCanvas::new();
    // Expand before the (full-rebuild) snapshot so the param rows lay out.
    canvas.collapsed.insert(1, false);
    canvas.set_snapshot(&snapshot_with_param_node("gain", vec![param]));
    (canvas, Rect::new(0.0, 0.0, 1200.0, 800.0))
}

/// Screen-space centre of node 1's row-`pi` expose glyph.
fn glyph_centre(canvas: &GraphCanvas, vp: Rect, pi: usize) -> (f32, f32) {
    let row = canvas.param_row_rect(vp, 1, pi).expect("row rect");
    let (gx, gy, gd) = expose_glyph_bounds(row.x, row.y, PARAM_ROW_H * canvas.zoom, canvas.zoom);
    (gx + gd * 0.5, gy + gd * 0.5)
}

#[test]
fn clicking_expose_glyph_emits_toggle_and_selects() {
    let (mut canvas, vp) = expanded_canvas(float_param("amount", 0.25));
    let (cx, cy) = glyph_centre(&canvas, vp, 0);
    canvas.on_left_button_down(vp, cx, cy, 0.0, false);

    // Selecting the node is part of the gesture (so the rest of the UI follows).
    assert_eq!(canvas.selected_ids(), vec![1]);

    let cmd = canvas
        .drain_edits()
        .into_iter()
        .find_map(|a| match a {
            GraphEditCommand::ToggleNodeParamExpose {
                node_handle,
                inner_param,
                expose,
                convert,
                min,
                max,
                default_value,
                ..
            } => Some((node_handle, inner_param, expose, convert, min, max, default_value)),
            _ => None,
        })
        .expect("expose toggle emitted");
    assert_eq!(cmd.0, "gain", "node handle");
    assert_eq!(cmd.1, "amount", "inner param");
    assert!(cmd.2, "was unexposed → expose=true");
    assert!(matches!(cmd.3, crate::types::ParamConvert::Float));
    assert_eq!((cmd.4, cmd.5), (0.0, 1.0), "range carried");
    assert_eq!(cmd.6, 0.0, "default carried");
}

#[test]
fn clicking_expose_glyph_when_exposed_unexposes() {
    let mut p = float_param("amount", 0.25);
    p.exposed = true;
    let (mut canvas, vp) = expanded_canvas(p);
    let (cx, cy) = glyph_centre(&canvas, vp, 0);
    canvas.on_left_button_down(vp, cx, cy, 0.0, false);
    let expose = canvas.drain_edits().into_iter().find_map(|a| match a {
        GraphEditCommand::ToggleNodeParamExpose { expose, .. } => Some(expose),
        _ => None,
    });
    assert_eq!(expose, Some(false), "was exposed → expose=false");
}

#[test]
fn enum_expose_carries_enum_convert_and_labels() {
    let (mut canvas, vp) = expanded_canvas(enum_param("mode", 1.0));
    let (cx, cy) = glyph_centre(&canvas, vp, 0);
    canvas.on_left_button_down(vp, cx, cy, 0.0, false);
    let (convert, labels) = canvas
        .drain_edits()
        .into_iter()
        .find_map(|a| match a {
            GraphEditCommand::ToggleNodeParamExpose {
                convert,
                value_labels,
                ..
            } => Some((convert, value_labels)),
            _ => None,
        })
        .expect("expose toggle emitted");
    assert!(matches!(convert, crate::types::ParamConvert::EnumRound));
    assert_eq!(labels, vec!["A", "B", "C"]);
}

#[test]
fn pressing_row_body_scrubs_and_emits_no_expose() {
    let (mut canvas, vp) = expanded_canvas(float_param("amount", 0.25));
    // Press on the value side of the row, well past the left-edge glyph.
    let row = canvas.param_row_rect(vp, 1, 0).expect("row rect");
    canvas.on_left_button_down(vp, row.x + row.w * 0.7, row.y + row.h * 0.5, 0.0, false);
    // A numeric row starts a scrub, not an expose.
    assert!(matches!(canvas.drag.payload(), Some(CanvasDrag::ParamScrub { .. })));
    assert!(
        canvas
            .drain_edits()
            .iter()
            .all(|a| !matches!(a, GraphEditCommand::ToggleNodeParamExpose { .. })),
        "row-body press must not toggle exposure"
    );
}

/// right-clicking a numeric node-face slider's TRACK zone (right of
/// the label cell) resets it to its declared default — the same gesture
/// every card/panel slider already honors via `chrome/diff.rs`'s
/// `Gesture::RightClick -> SliderReset`, which this immediate-mode canvas
/// never reached before. `float_param`'s `default_value` is `0.0`; the
/// current value is `0.8` so a reset is observable.
#[test]
fn right_click_track_zone_resets_numeric_param_to_default() {
    let (mut canvas, vp) = expanded_canvas(float_param("amount", 0.8));
    let row = canvas.param_row_rect(vp, 1, 0).expect("row rect");
    // Same track-zone x `press_row_value`/`pressing_row_body_scrubs_...` use
    // for a left-click scrub — well past the label cell.
    let hit = canvas.on_right_button_down(vp, row.x + row.w * 0.7, row.y + row.h * 0.5);
    assert!(
        hit.is_none(),
        "a track-zone reset must not also report a row hit for the mapping popover"
    );
    let (nid, name, val) = drained_set_param(&mut canvas)
        .expect("track-zone right-click on a numeric row must emit SetGraphNodeParam");
    assert_eq!((nid, name.as_str()), (1, "amount"));
    match val {
        crate::SerializedParamValue::Float { value } => {
            assert!((value - 0.0).abs() < 1e-6, "reset must write the declared default (0.0), got {value}");
        }
        other => panic!("expected a Float reset value, got {other:?}"),
    }
}

/// the LABEL zone of the same row is untouched — a
/// right-click there still reports the row hit so the app's mapping-popover
/// path (checked separately against whether the inner param is exposed as a
/// card binding) keeps working exactly as before.
#[test]
fn right_click_label_zone_still_reports_row_hit_for_mapping_popover() {
    let (mut canvas, vp) = expanded_canvas(float_param("amount", 0.8));
    let row = canvas.param_row_rect(vp, 1, 0).expect("row rect");
    let hit = canvas.on_right_button_down(vp, row.x + 4.0, row.y + row.h * 0.5);
    assert_eq!(hit, Some((1, 0)), "label-zone right-click must still resolve to the row");
    assert!(
        canvas
            .drain_edits()
            .iter()
            .all(|a| !matches!(a, GraphEditCommand::SetGraphNodeParam { .. })),
        "label-zone right-click must not itself reset the param"
    );
}

// ─── Phase 2: discrete on-face editing (bool / trigger / enum) ───────────────

/// Press the value side of node 1's row `pi` (past the left-edge expose glyph).
fn press_row_value(canvas: &mut GraphCanvas, vp: Rect, pi: usize) {
    let row = canvas.param_row_rect(vp, 1, pi).expect("row rect");
    canvas.on_left_button_down(vp, row.x + row.w * 0.7, row.y + row.h * 0.5, 0.0, false);
}

fn drained_set_param(canvas: &mut GraphCanvas) -> Option<(u32, String, crate::SerializedParamValue)> {
    canvas.drain_edits().into_iter().find_map(|a| match a {
        GraphEditCommand::SetGraphNodeParam { node_id, param_name, new_value } => {
            Some((node_id, param_name, new_value))
        }
        _ => None,
    })
}

#[test]
fn clicking_bool_value_toggles_via_set_graph_node_param() {
    // false → true
    let (mut canvas, vp) = expanded_canvas(bool_param("on", 0.0));
    press_row_value(&mut canvas, vp, 0);
    let (nid, name, val) = drained_set_param(&mut canvas).expect("bool click emits SetGraphNodeParam");
    assert_eq!((nid, name.as_str()), (1, "on"));
    assert!(matches!(val, crate::SerializedParamValue::Bool { value: true }));

    // true → false
    let (mut canvas, vp) = expanded_canvas(bool_param("on", 1.0));
    press_row_value(&mut canvas, vp, 0);
    let (_, _, val) = drained_set_param(&mut canvas).expect("emits");
    assert!(matches!(val, crate::SerializedParamValue::Bool { value: false }));
}

#[test]
fn clicking_trigger_value_fires_via_set_graph_node_param() {
    let (mut canvas, vp) = expanded_canvas(trigger_param("fire", 3.0));
    press_row_value(&mut canvas, vp, 0);
    let (nid, name, val) = drained_set_param(&mut canvas).expect("trigger click emits");
    assert_eq!((nid, name.as_str()), (1, "fire"));
    match val {
        crate::SerializedParamValue::Float { value } => assert!((value - 4.0).abs() < 1e-6),
        other => panic!("expected Float(+1), got {other:?}"),
    }
}

#[test]
fn clicking_enum_value_opens_dropdown_without_a_command() {
    let (mut canvas, vp) = expanded_canvas(enum_param("mode", 1.0));
    press_row_value(&mut canvas, vp, 0);
    // No value command yet — the dropdown is now open, seeded from the param.
    assert!(drained_set_param(&mut canvas).is_none(), "opening the list emits nothing");
    let dd = canvas.enum_dropdown.as_ref().expect("dropdown open");
    assert_eq!(dd.node_id, 1);
    assert_eq!(dd.param_name, "mode");
    assert_eq!(dd.options, vec!["A", "B", "C"]);
    assert_eq!(dd.current, 1, "current index seeded from current_value");
}

#[test]
fn picking_enum_option_emits_set_and_closes() {
    let (mut canvas, vp) = expanded_canvas(enum_param("mode", 0.0));
    press_row_value(&mut canvas, vp, 0);
    // Click option index 2 in the open list.
    let opt = canvas.enum_dropdown.as_ref().unwrap().option_rect(2);
    canvas.on_left_button_down(vp, opt.x + opt.w * 0.5, opt.y + opt.h * 0.5, 0.0, false);
    let (nid, name, val) = drained_set_param(&mut canvas).expect("pick emits SetGraphNodeParam");
    assert_eq!((nid, name.as_str()), (1, "mode"));
    assert!(matches!(val, crate::SerializedParamValue::Enum { value: 2 }));
    assert!(canvas.enum_dropdown.is_none(), "picking closes the list");
}

#[test]
fn picking_current_enum_option_emits_nothing_and_closes() {
    let (mut canvas, vp) = expanded_canvas(enum_param("mode", 1.0));
    press_row_value(&mut canvas, vp, 0);
    let opt = canvas.enum_dropdown.as_ref().unwrap().option_rect(1); // == current
    canvas.on_left_button_down(vp, opt.x + opt.w * 0.5, opt.y + opt.h * 0.5, 0.0, false);
    assert!(drained_set_param(&mut canvas).is_none(), "re-picking current is a no-op");
    assert!(canvas.enum_dropdown.is_none(), "still closes");
}

#[test]
fn pressing_outside_open_enum_dropdown_dismisses_it() {
    let (mut canvas, vp) = expanded_canvas(enum_param("mode", 0.0));
    press_row_value(&mut canvas, vp, 0);
    assert!(canvas.enum_dropdown.is_some());
    // Press far from both the row and the list — empty canvas.
    canvas.on_left_button_down(vp, 1000.0, 700.0, 0.0, false);
    assert!(canvas.enum_dropdown.is_none(), "outside press dismisses");
    assert!(
        drained_set_param(&mut canvas).is_none(),
        "dismissal emits no value command"
    );
}

// ─── Phase 3: Color / Vec channel editing on the node face ───────────────────

fn color_param(name: &str, rgba: [f32; 4]) -> crate::graph_view::ParamSnapshot {
    crate::graph_view::ParamSnapshot {
        name: name.to_string(),
        label: name.to_string(),
        kind: crate::graph_view::ParamSnapshotKind::Color,
        default_value: 0.0,
        current_value: 0.0,
        range: None,
        enum_labels: None,
        exposed: false,
        summary: None,
        vec_value: Some(rgba),
        string_value: None,
        table_value: None,
        tooltip: None,
    }
}

fn vec2_param(name: &str, xy: [f32; 2]) -> crate::graph_view::ParamSnapshot {
    crate::graph_view::ParamSnapshot {
        name: name.to_string(),
        label: name.to_string(),
        kind: crate::graph_view::ParamSnapshotKind::Vec2,
        default_value: 0.0,
        current_value: 0.0,
        range: Some((-1.0, 1.0)),
        enum_labels: None,
        exposed: false,
        summary: None,
        vec_value: Some([xy[0], xy[1], 0.0, 0.0]),
        string_value: None,
        table_value: None,
        tooltip: None,
    }
}

#[test]
fn clicking_color_value_opens_editor_without_a_command() {
    let (mut canvas, vp) = expanded_canvas(color_param("tint", [0.2, 0.4, 0.6, 1.0]));
    press_row_value(&mut canvas, vp, 0);
    assert!(
        drained_set_param(&mut canvas).is_none(),
        "opening the editor emits nothing"
    );
    let ed = canvas.vec_editor.as_ref().expect("editor open");
    assert_eq!(ed.node_id, 1);
    assert_eq!(ed.param_name, "tint");
    assert!(ed.is_color, "colour editor");
    assert_eq!(ed.components, 4, "RGBA");
    assert!(ed.swatch_rect().is_some(), "colour gets a swatch header");
}

#[test]
fn pressing_a_color_channel_starts_a_vec_scrub() {
    let (mut canvas, vp) = expanded_canvas(color_param("tint", [0.2, 0.4, 0.6, 1.0]));
    press_row_value(&mut canvas, vp, 0);
    let g = canvas.vec_editor.as_ref().unwrap().channel_rect(1); // G
    canvas.on_left_button_down(vp, g.x + g.w * 0.3, g.y + g.h * 0.5, 0.0, false);
    assert!(
        matches!(canvas.drag.payload(), Some(CanvasDrag::VecScrub { channel: 1, .. })),
        "channel press starts a scrub on that channel"
    );
    // Starting the scrub is not itself a value edit.
    assert!(drained_set_param(&mut canvas).is_none());
    // The panel stays open so another channel can be grabbed.
    assert!(canvas.vec_editor.is_some(), "editor stays open during a scrub");
}

#[test]
fn dragging_a_color_channel_emits_full_color_with_others_held() {
    let (mut canvas, vp) = expanded_canvas(color_param("tint", [0.2, 0.4, 0.6, 1.0]));
    press_row_value(&mut canvas, vp, 0);
    let g = canvas.vec_editor.as_ref().unwrap().channel_rect(1); // G
    let px = g.x + g.w * 0.3;
    canvas.on_left_button_down(vp, px, g.y + g.h * 0.5, 0.0, false);
    // Drag right +120px → +0.5 over the 240px full-range (colour span 1.0).
    canvas.on_pointer_move(vp, px + 120.0, g.y + g.h * 0.5);
    let (nid, name, val) = drained_set_param(&mut canvas).expect("scrub emits");
    assert_eq!((nid, name.as_str()), (1, "tint"));
    match val {
        crate::SerializedParamValue::Color { value } => {
            assert!((value[0] - 0.2).abs() < 1e-4, "R held");
            assert!((value[1] - 0.9).abs() < 1e-3, "G scrubbed 0.4 → 0.9");
            assert!((value[2] - 0.6).abs() < 1e-4, "B held");
            assert!((value[3] - 1.0).abs() < 1e-4, "A held");
        }
        other => panic!("expected Color, got {other:?}"),
    }
}

#[test]
fn color_channel_scrub_clamps_to_zero_one() {
    let (mut canvas, vp) = expanded_canvas(color_param("tint", [0.2, 0.9, 0.6, 1.0]));
    press_row_value(&mut canvas, vp, 0);
    let g = canvas.vec_editor.as_ref().unwrap().channel_rect(1); // G at 0.9
    let px = g.x + g.w * 0.3;
    canvas.on_left_button_down(vp, px, g.y + g.h * 0.5, 0.0, false);
    // Drag hard right past the top of the range → clamps at 1.0, never above.
    canvas.on_pointer_move(vp, px + 600.0, g.y + g.h * 0.5);
    let (_, _, val) = drained_set_param(&mut canvas).expect("scrub emits");
    match val {
        crate::SerializedParamValue::Color { value } => {
            assert!((value[1] - 1.0).abs() < 1e-6, "G clamps at 1.0");
        }
        other => panic!("expected Color, got {other:?}"),
    }
}

#[test]
fn vec2_editor_has_two_channels_and_no_swatch() {
    let (mut canvas, vp) = expanded_canvas(vec2_param("offset", [0.5, -0.5]));
    press_row_value(&mut canvas, vp, 0);
    let ed = canvas.vec_editor.as_ref().expect("editor open");
    assert!(!ed.is_color, "vector, not colour");
    assert_eq!(ed.components, 2, "XY");
    assert!(ed.swatch_rect().is_none(), "no swatch header on a vector");
    // Panel is exactly two channel rows tall (no header row).
    let panel = ed.panel_rect();
    assert!((panel.h - 2.0 * ed.anchor.h).abs() < 1e-3);
}

#[test]
fn dragging_a_vec2_channel_emits_vec2_over_declared_range() {
    let (mut canvas, vp) = expanded_canvas(vec2_param("offset", [0.0, 0.0]));
    press_row_value(&mut canvas, vp, 0);
    let x = canvas.vec_editor.as_ref().unwrap().channel_rect(0); // X
    let px = x.x + x.w * 0.3;
    canvas.on_left_button_down(vp, px, x.y + x.h * 0.5, 0.0, false);
    // Vec range (-1,1) → span 2.0 → +120px = +1.0.
    canvas.on_pointer_move(vp, px + 120.0, x.y + x.h * 0.5);
    let (_, name, val) = drained_set_param(&mut canvas).expect("scrub emits");
    assert_eq!(name, "offset");
    match val {
        crate::SerializedParamValue::Vec2 { value } => {
            assert!((value[0] - 1.0).abs() < 1e-3, "X scrubbed 0 → +1.0");
            assert!((value[1] - 0.0).abs() < 1e-4, "Y held");
        }
        other => panic!("expected Vec2, got {other:?}"),
    }
}

#[test]
fn pressing_outside_open_vec_editor_dismisses_it() {
    let (mut canvas, vp) = expanded_canvas(color_param("tint", [0.1, 0.2, 0.3, 1.0]));
    press_row_value(&mut canvas, vp, 0);
    assert!(canvas.vec_editor.is_some());
    // Press far from both the row and the panel — empty canvas.
    canvas.on_left_button_down(vp, 1000.0, 700.0, 0.0, false);
    assert!(canvas.vec_editor.is_none(), "outside press dismisses");
    assert!(
        drained_set_param(&mut canvas).is_none(),
        "dismissal emits no value command"
    );
}

#[test]
fn pressing_inside_vec_editor_header_swallows_and_stays_open() {
    let (mut canvas, vp) = expanded_canvas(color_param("tint", [0.1, 0.2, 0.3, 1.0]));
    press_row_value(&mut canvas, vp, 0);
    // The swatch header row is inside the panel but not a channel row.
    let sw = canvas.vec_editor.as_ref().unwrap().swatch_rect().unwrap();
    canvas.on_left_button_down(vp, sw.x + sw.w * 0.5, sw.y + sw.h * 0.5, 0.0, false);
    assert!(canvas.vec_editor.is_some(), "header press keeps it open");
    assert!(
        !matches!(canvas.drag.payload(), Some(CanvasDrag::VecScrub { .. })),
        "header press starts no scrub"
    );
}

#[test]
fn color_param_carries_vec_value_onto_the_face() {
    let (canvas, _vp) = expanded_canvas(color_param("tint", [0.25, 0.5, 0.75, 1.0]));
    let pv = &canvas.find_node(1).unwrap().params[0];
    assert_eq!(pv.vec_value, [0.25, 0.5, 0.75, 1.0], "vec value on the face");
    assert_eq!(pv.value, "#4080BF", "hex string on the face");
}

// ─── Phase 4: string / path + table + WGSL editing on the face ───────────────

fn string_param(name: &str, value: &str) -> crate::graph_view::ParamSnapshot {
    crate::graph_view::ParamSnapshot {
        name: name.to_string(),
        label: name.to_string(),
        kind: crate::graph_view::ParamSnapshotKind::String,
        default_value: 0.0,
        current_value: 0.0,
        range: None,
        enum_labels: None,
        exposed: false,
        summary: Some(value.to_string()),
        vec_value: None,
        string_value: Some(value.to_string()),
        table_value: None,
        tooltip: None,
    }
}

fn table_param(name: &str, rows: Vec<Vec<f32>>) -> crate::graph_view::ParamSnapshot {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    crate::graph_view::ParamSnapshot {
        name: name.to_string(),
        label: name.to_string(),
        // A Table param carries `kind: Other` + a `table_value`, exactly the
        // renderer's translation — the on-node dispatch branches on the value,
        // not the kind.
        kind: crate::graph_view::ParamSnapshotKind::Other,
        default_value: 0.0,
        current_value: 0.0,
        range: None,
        enum_labels: None,
        exposed: false,
        summary: Some(format!("{}×{}", rows.len(), cols)),
        vec_value: None,
        string_value: None,
        table_value: Some(rows),
        tooltip: None,
    }
}

/// Snapshot with node 1 (`node.wgsl`) carrying a custom kernel source.
fn wgsl_snapshot(src: &str) -> GraphSnapshot {
    let mut n = node(1, "node.wgsl_compute", Some("kernel"));
    n.wgsl_source = Some(src.to_string());
    GraphSnapshot {
        nodes: vec![n],
        wires: Vec::new(),
        outer_routings: Vec::new(),
    }
}

#[test]
fn clicking_string_value_opens_inline_text_editor() {
    let (mut canvas, vp) = expanded_canvas(string_param("label", "hello world"));
    press_row_value(&mut canvas, vp, 0);
    let cmd = canvas
        .drain_edits()
        .into_iter()
        .find_map(|a| match a {
            GraphEditCommand::EditGraphNodeStringParam {
                node_id,
                param_name,
                current,
                anchor,
            } => Some((node_id, param_name, current, anchor)),
            _ => None,
        })
        .expect("string click opens the text editor");
    assert_eq!(cmd.0, 1, "node id");
    assert_eq!(cmd.1, "label", "param name");
    assert_eq!(cmd.2, "hello world", "raw current value, untruncated");
    // Anchor is the row rect (parity with the sidebar's whole-row editor).
    let row = canvas.param_row_rect(vp, 1, 0).unwrap();
    assert!((cmd.3.0 - row.x).abs() < 1e-3 && (cmd.3.1 - row.y).abs() < 1e-3);
}

#[test]
fn clicking_path_value_opens_native_browse() {
    // A path-like name ("folder") routes to the native picker, not the editor.
    let (mut canvas, vp) = expanded_canvas(string_param("folder", "/tmp/clips"));
    press_row_value(&mut canvas, vp, 0);
    let cmd = canvas
        .drain_edits()
        .into_iter()
        .find_map(|a| match a {
            GraphEditCommand::BrowseGraphNodePath { node_id, param_name } => {
                Some((node_id, param_name))
            }
            _ => None,
        })
        .expect("path click opens the folder picker");
    assert_eq!(cmd, (1, "folder".to_string()));
}

#[test]
fn path_param_is_flagged_on_the_face() {
    let (canvas, _vp) = expanded_canvas(string_param("output_path", "/x"));
    assert!(canvas.find_node(1).unwrap().params[0].is_path, "path param");
    let (canvas2, _vp) = expanded_canvas(string_param("caption", "hi"));
    assert!(!canvas2.find_node(1).unwrap().params[0].is_path, "free text");
}

#[test]
fn clicking_table_value_opens_grid_editor_without_a_command() {
    let (mut canvas, vp) =
        expanded_canvas(table_param("stops", vec![vec![0.0, 1.0], vec![0.5, 0.25]]));
    press_row_value(&mut canvas, vp, 0);
    assert!(
        canvas.drain_edits().is_empty(),
        "opening the grid editor emits nothing"
    );
    let ed = canvas.table_editor.as_ref().expect("grid editor open");
    assert_eq!(ed.node_id, 1);
    assert_eq!(ed.param_name, "stops");
    assert_eq!((ed.rows, ed.cols), (2, 2), "grid dimensions captured");
}

#[test]
fn clicking_a_table_cell_emits_edit_table_cell() {
    let rows = vec![vec![0.0, 1.0, 2.0], vec![10.0, 11.0, 12.0]];
    let (mut canvas, vp) = expanded_canvas(table_param("seq", rows.clone()));
    press_row_value(&mut canvas, vp, 0); // open the grid
    // Press the centre of cell (row 1, col 2) → value 12.0.
    let cell = canvas.table_editor.as_ref().unwrap().cell_rect(1, 2);
    canvas.on_left_button_down(vp, cell.x + cell.w * 0.5, cell.y + cell.h * 0.5, 0.0, false);
    let cmd = canvas
        .drain_edits()
        .into_iter()
        .find_map(|a| match a {
            GraphEditCommand::EditGraphNodeTableCell {
                node_id,
                param_name,
                row,
                col,
                current,
                rows,
                ..
            } => Some((node_id, param_name, row, col, current, rows)),
            _ => None,
        })
        .expect("cell click opens the numeric editor");
    assert_eq!(cmd.0, 1, "node id");
    assert_eq!(cmd.1, "seq", "param name");
    assert_eq!((cmd.2, cmd.3), (1, 2), "cell coords");
    assert_eq!(cmd.4, 12.0, "current cell value");
    assert_eq!(cmd.5, rows, "whole table stashed for the rebuild");
    // The grid stays open so more cells can be edited.
    assert!(canvas.table_editor.is_some(), "grid stays open across a cell edit");
}

#[test]
fn pressing_outside_open_table_editor_dismisses_it() {
    let (mut canvas, vp) = expanded_canvas(table_param("t", vec![vec![1.0, 2.0]]));
    press_row_value(&mut canvas, vp, 0);
    assert!(canvas.table_editor.is_some());
    // Press far below the panel — dismiss.
    let p = canvas.table_editor.as_ref().unwrap().panel_rect();
    canvas.on_left_button_down(vp, p.x + p.w * 0.5, p.y + p.h + 200.0, 0.0, false);
    assert!(canvas.table_editor.is_none(), "outside press dismisses");
}

#[test]
fn pressing_inside_table_editor_header_swallows_and_stays_open() {
    let (mut canvas, vp) = expanded_canvas(table_param("t", vec![vec![1.0, 2.0]]));
    press_row_value(&mut canvas, vp, 0);
    // The header line sits between the anchor row and the grid — inside the
    // panel but not a cell.
    let anchor = canvas.param_row_rect(vp, 1, 0).unwrap();
    let hy = anchor.y + anchor.h + anchor.h * 0.5; // header row centre
    canvas.on_left_button_down(vp, anchor.x + anchor.w * 0.5, hy, 0.0, false);
    assert!(canvas.table_editor.is_some(), "header press keeps it open");
    assert!(canvas.drain_edits().is_empty(), "header press emits nothing");
}

#[test]
fn wgsl_node_has_edit_code_footer_and_click_opens_editor() {
    let mut canvas = GraphCanvas::new();
    canvas.collapsed.insert(1, false);
    canvas.set_snapshot(&wgsl_snapshot("fn main() {}"));
    let vp = Rect::new(0.0, 0.0, 1200.0, 800.0);
    // The footer exists exactly for a wgsl node with a kernel.
    let r = canvas.wgsl_edit_rect(vp, 1).expect("wgsl footer rect");
    canvas.on_left_button_down(vp, r.x + r.w * 0.5, r.y + r.h * 0.5, 0.0, false);
    let cmd = canvas
        .drain_edits()
        .into_iter()
        .find_map(|a| match a {
            GraphEditCommand::EditGraphNodeWgsl { node_id, current, .. } => {
                Some((node_id, current))
            }
            _ => None,
        })
        .expect("footer click opens the WGSL editor");
    assert_eq!(cmd.0, 1, "node id");
    assert_eq!(cmd.1, "fn main() {}", "current kernel source");
    assert_eq!(canvas.selected_ids(), vec![1], "footer click selects the node");
}

#[test]
fn non_wgsl_node_has_no_edit_code_footer() {
    let (canvas, vp) = expanded_canvas(float_param("amount", 0.5));
    assert!(
        canvas.wgsl_edit_rect(vp, 1).is_none(),
        "a plain node has no kernel footer"
    );
    assert!(
        canvas.find_node(1).unwrap().wgsl_footer_offset().is_none(),
        "no footer offset without a kernel"
    );
}

#[test]
fn wgsl_footer_hidden_when_collapsed() {
    let mut canvas = GraphCanvas::new();
    canvas.collapsed.insert(1, true); // collapsed
    canvas.set_snapshot(&wgsl_snapshot("fn k() {}"));
    let vp = Rect::new(0.0, 0.0, 1200.0, 800.0);
    assert!(
        canvas.wgsl_edit_rect(vp, 1).is_none(),
        "collapsed node hides the kernel footer (expand to edit)"
    );
}

#[test]
fn expanded_rows_merge_shadowing_ports_onto_param_rows() {
    use crate::graph_canvas::NodeRow;
    // Math-like node: inputs a,b shadow params a,b (port-shadows-param); param
    // `op` has no input; a texture input `tex` has no param.
    let mut n = node(1, "node.math", Some("m"));
    n.inputs = vec![port("a"), port("b"), port("tex")];
    n.outputs = vec![port("out")];
    n.parameters = vec![
        float_param("a", 5.0),
        float_param("b", 2.0),
        enum_param("op", 2.0),
    ];
    let snap = GraphSnapshot {
        nodes: vec![n],
        wires: Vec::new(),
        outer_routings: Vec::new(),
    };
    let mut canvas = GraphCanvas::new();
    canvas.collapsed.insert(1, false);
    canvas.set_snapshot(&snap);
    let node = canvas.find_node(1).unwrap();

    // Blender order: output, then param rows (a/b carry their input socket, op
    // has none), then the leftover texture input as its own row.
    assert_eq!(
        node.rows,
        vec![
            NodeRow::Output { port: 0 },
            NodeRow::Param {
                param: 0,
                input_port: Some(0)
            },
            NodeRow::Param {
                param: 1,
                input_port: Some(1)
            },
            NodeRow::Param {
                param: 2,
                input_port: None
            },
            NodeRow::Input { port: 2 },
        ]
    );

    // The `a` input socket sits on param-a's row (row 1), not a separate band.
    let off_a = node.input_port_pos_graph(0).1 - node.pos_graph.1;
    assert!((off_a - node.expanded_row_center(1)).abs() < 1e-3);
    // The leftover texture input sits on the last row (row 4).
    let off_tex = node.input_port_pos_graph(2).1 - node.pos_graph.1;
    assert!((off_tex - node.expanded_row_center(4)).abs() < 1e-3);
    // The output sits on row 0.
    let off_out = node.output_port_pos_graph(0).1 - node.pos_graph.1;
    assert!((off_out - node.expanded_row_center(0)).abs() < 1e-3);
}

// ── Connection type feedback ────────────────────────────────────

#[test]
fn ports_compatible_is_colour_category_equality() {
    // Same category → compatible (the ghost wire reads green).
    assert!(ports_compatible(PORT_TEXTURE2D_COLOR, PORT_TEXTURE2D_COLOR));
    // Cross-category → incompatible (red), so a mis-wire is caught pre-drop.
    assert!(!ports_compatible(PORT_TEXTURE2D_COLOR, PORT_SCALAR_COLOR));
    assert!(!ports_compatible(PORT_SCALAR_COLOR, PORT_ARRAY_COLOR));
}

// ─── Hide-unused sockets + reveal chip ───────────────────────────────────────

/// A distributor node (id 1, outputs a/b/c, no inputs) feeding a sink (id 2)
/// from output `a` only. `wired` gates whether the wire exists.
fn distributor_snapshot(wired: bool) -> GraphSnapshot {
    let mut dist = node(1, "system.generator_input", Some("dist"));
    dist.inputs = vec![];
    dist.outputs = vec![port("a"), port("b"), port("c")];
    dist.parameters = vec![];
    let mut sink = node(2, "node.blur", Some("sink"));
    sink.inputs = vec![port("in")];
    sink.outputs = vec![port("out")];
    GraphSnapshot {
        nodes: vec![dist, sink],
        wires: if wired { vec![wire(1, "a", 2, "in")] } else { Vec::new() },
        outer_routings: Vec::new(),
    }
}

fn expanded_dist(wired: bool) -> (GraphCanvas, Rect) {
    let mut canvas = GraphCanvas::new();
    canvas.collapsed.insert(1, false);
    canvas.collapsed.insert(2, false);
    canvas.set_snapshot(&distributor_snapshot(wired));
    (canvas, Rect::new(0.0, 0.0, 1200.0, 800.0))
}

#[test]
fn unused_outputs_hide_once_a_sibling_is_wired() {
    use crate::graph_canvas::NodeRow;
    let (canvas, _vp) = expanded_dist(true);
    let dist = canvas.find_node(1).unwrap();
    // Only the wired output `a` (index 0) keeps a row; b/c drop.
    assert_eq!(dist.rows, vec![NodeRow::Output { port: 0 }]);
    assert_eq!(dist.hideable_ports, 2, "b + c are hideable");
    assert!(!dist.revealed);
}

#[test]
fn fresh_node_shows_all_sockets() {
    use crate::graph_canvas::NodeRow;
    let (canvas, _vp) = expanded_dist(false);
    let dist = canvas.find_node(1).unwrap();
    // Nothing wired → every output shown, nothing hideable.
    assert_eq!(
        dist.rows,
        vec![
            NodeRow::Output { port: 0 },
            NodeRow::Output { port: 1 },
            NodeRow::Output { port: 2 },
        ]
    );
    assert_eq!(dist.hideable_ports, 0);
}

#[test]
fn revealing_shows_all_sockets_but_keeps_hideable_count() {
    use crate::graph_canvas::NodeRow;
    let (mut canvas, _vp) = expanded_dist(true);
    canvas.revealed_ports.insert(1, true);
    canvas.rebuild_rows();
    let dist = canvas.find_node(1).unwrap();
    assert_eq!(
        dist.rows,
        vec![
            NodeRow::Output { port: 0 },
            NodeRow::Output { port: 1 },
            NodeRow::Output { port: 2 },
        ],
        "revealed → all outputs shown"
    );
    assert!(dist.revealed);
    assert_eq!(dist.hideable_ports, 2, "chip still knows 2 can be re-hidden");
}

#[test]
fn clicking_reveal_chip_toggles_and_rebuilds() {
    let (mut canvas, vp) = expanded_dist(true);
    let chip = canvas.reveal_chip_rect(vp, 1).expect("chip shows when hideable > 0");
    canvas.on_left_button_down(vp, chip.x + chip.w * 0.5, chip.y + chip.h * 0.5, 0.0, false);
    assert_eq!(canvas.revealed_ports.get(&1).copied(), Some(true), "chip revealed");
    assert_eq!(canvas.find_node(1).unwrap().rows.len(), 3, "all outputs now shown");
    // Click again → re-hide.
    let chip2 = canvas.reveal_chip_rect(vp, 1).expect("chip still shows (hideable)");
    canvas.on_left_button_down(vp, chip2.x + chip2.w * 0.5, chip2.y + chip2.h * 0.5, 0.0, false);
    assert_eq!(canvas.revealed_ports.get(&1).copied(), Some(false), "chip re-hidden");
    assert_eq!(canvas.find_node(1).unwrap().rows.len(), 1, "back to the wired output");
}

#[test]
fn no_reveal_chip_when_nothing_hideable() {
    let (canvas, vp) = expanded_dist(false);
    assert!(
        canvas.reveal_chip_rect(vp, 1).is_none(),
        "a fresh node hides nothing, so no chip"
    );
}

#[test]
fn hidden_outputs_have_no_row_so_hit_test_skips_them() {
    // `port_under` skips an expanded socket whose `output_row_of` is `None`; with
    // `a` wired, only `a` keeps a row, so b/c can't be wire-drag targets.
    let (canvas, _vp) = expanded_dist(true);
    let dist = canvas.find_node(1).unwrap();
    assert!(dist.output_row_of(0).is_some(), "wired output a keeps its row");
    assert!(dist.output_row_of(1).is_none(), "hidden output b has no row");
    assert!(dist.output_row_of(2).is_none(), "hidden output c has no row");
}

// ─── Phase 5: wire-driven / outer-driven state + read-only lockout ───────────

/// Expand node 1 of an arbitrary snapshot and return a generous viewport.
fn expanded_canvas_from(snap: GraphSnapshot) -> (GraphCanvas, Rect) {
    let mut canvas = GraphCanvas::new();
    canvas.collapsed.insert(1, false);
    canvas.set_snapshot(&snap);
    (canvas, Rect::new(0.0, 0.0, 1200.0, 800.0))
}

/// REPRO (glTF import "have to scroll down to see the graph on open"): an
/// import-shaped generator graph — a wide fan-in where many producer nodes all
/// feed one `render` node — with every node at `editor_pos: None` (the state an
/// import leaves). On editor open the canvas must frame the WHOLE graph in the
/// viewport (`apply_pending_fit`), so every node lands on-screen. Reproduces the
/// off-screen symptom if any node's fitted screen box falls outside the viewport.
#[test]
fn import_shaped_graph_fits_all_nodes_on_open() {
    // input + envmap + camera + sun + (mesh,mat,tex)×2 = 10 producers, all at
    // depth 0, feeding `render` (depth 1) → `final` (depth 2). Mirrors
    // gltf_import::assemble_import_graph's shape.
    // A node with a caller-chosen set of input/output ports (the bare `node()`
    // helper hard-codes one `in`/`out`, which would make the fan-in wires target
    // ports that don't exist on `render` — an artifact that exaggerates the
    // layout drift). Here `render` gets its real 9 inputs so port offsets are
    // legitimate.
    let ported = |id: u32, ty: &str, handle: &str, ins: &[&str], outs: &[&str]| {
        let mut n = node(id, ty, Some(handle));
        n.inputs = ins.iter().map(|p| port(p)).collect();
        n.outputs = outs.iter().map(|p| port(p)).collect();
        n.title = handle.to_string();
        n
    };
    let nodes = vec![
        ported(0, "system.generator_input", "input", &[], &["out"]),
        ported(1, "node.bake_environment", "envmap", &[], &["envmap"]),
        ported(2, "node.orbit_camera", "camera", &[], &["out"]),
        ported(3, "node.light", "sun", &[], &["out"]),
        ported(4, "node.gltf_mesh_source", "mesh_0", &[], &["vertices"]),
        ported(5, "node.pbr_material", "mat_0", &[], &["out"]),
        ported(6, "node.gltf_texture_source", "tex_0", &[], &["out"]),
        ported(7, "node.gltf_mesh_source", "mesh_1", &[], &["vertices"]),
        ported(8, "node.pbr_material", "mat_1", &[], &["out"]),
        ported(9, "node.gltf_texture_source", "tex_1", &[], &["out"]),
        ported(
            10,
            "node.render_scene",
            "render",
            &[
                "camera", "envmap", "light_0", "mesh_0", "material_0", "base_color_map_0",
                "mesh_1", "material_1", "base_color_map_1",
            ],
            &["color"],
        ),
        ported(11, "system.final_output", "final", &["in"], &[]),
    ];
    // Every node keeps editor_pos: None — the import's state (asserted here so a
    // future helper default change doesn't silently defeat the repro).
    assert!(nodes.iter().all(|n| n.editor_pos.is_none()));
    let wires = vec![
        wire(2, "out", 10, "camera"),
        wire(1, "envmap", 10, "envmap"),
        wire(3, "out", 10, "light_0"),
        wire(4, "vertices", 10, "mesh_0"),
        wire(5, "out", 10, "material_0"),
        wire(6, "out", 10, "base_color_map_0"),
        wire(7, "vertices", 10, "mesh_1"),
        wire(8, "out", 10, "material_1"),
        wire(9, "out", 10, "base_color_map_1"),
        wire(10, "color", 11, "in"),
    ];
    let snap = GraphSnapshot { nodes, wires, outer_routings: Vec::new() };

    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);
    // Realistic editor canvas viewport (window minus sidebar + card lane).
    let viewport = Rect::new(0.0, 0.0, 900.0, 800.0);
    canvas.apply_pending_fit(viewport);

    // Every laid-out node's screen box must sit inside the viewport.
    let offenders: Vec<String> = canvas
        .nodes
        .iter()
        .filter_map(|n| {
            let (sx, sy) = canvas.to_screen(viewport, n.pos_graph.0, n.pos_graph.1);
            let bottom = sy + n.height() * canvas.zoom;
            let right = sx + NODE_WIDTH * canvas.zoom;
            let inside = sx >= viewport.x - 1.0
                && sy >= viewport.y - 1.0
                && right <= viewport.x + viewport.w + 1.0
                && bottom <= viewport.y + viewport.h + 1.0;
            (!inside).then(|| {
                format!("{} screen=({sx:.0},{sy:.0}..{bottom:.0})", n.handle.as_deref().unwrap_or("?"))
            })
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "apply_pending_fit left {} node(s) outside the {}×{} viewport (zoom={:.3}); a dangling \
         node must not balloon the bbox past what the fit can frame:\n  {}",
        offenders.len(),
        viewport.w,
        viewport.h,
        canvas.zoom,
        offenders.join("\n  "),
    );
}

/// A tall fan-in: `count` producer nodes all feeding one sink, then sink →
/// final. Big `count` makes the graph tall enough that zoom-to-fit must go
/// below the interactive scroll floor to frame it (the 8-object-import shape).
fn tall_fan_in(count: u32) -> GraphSnapshot {
    let mut nodes = vec![node(0, "system.generator_input", Some("input"))];
    let mut wires = Vec::new();
    let sink_id = count + 1;
    let mut sink_inputs = Vec::new();
    for i in 0..count {
        let id = i + 1;
        nodes.push(node(id, "node.producer", Some(&format!("p{i}"))));
        let port_name = format!("in{i}");
        wires.push(wire(id, "out", sink_id, &port_name));
        sink_inputs.push(port_name);
    }
    let mut sink = node(sink_id, "node.sink", Some("sink"));
    sink.inputs = sink_inputs.iter().map(|p| port(p)).collect();
    sink.outputs = vec![port("out")];
    nodes.push(sink);
    let final_id = sink_id + 1;
    nodes.push(node(final_id, "system.final_output", Some("final")));
    wires.push(wire(sink_id, "out", final_id, "in"));
    GraphSnapshot { nodes, wires, outer_routings: Vec::new() }
}

/// Peter's "on zoom it scrolls to an empty graph" report: after zoom-to-fit
/// parks a tall graph below the interactive scroll-zoom floor, the very first
/// scroll used to snap the zoom up to that floor (a ~5x jump) and re-anchor,
/// leaping the view off the graph. Zoom-to-fit and scroll now share one range
/// (`MIN_ZOOM`..`MAX_ZOOM`), so any fitted zoom is reachable and a scroll moves
/// it proportionally instead of snapping.
#[test]
fn fitted_zoom_is_reachable_and_scroll_does_not_snap() {
    let snap = tall_fan_in(24); // ~24-node column → fits well below the old 0.25 floor
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);
    let viewport = Rect::new(0.0, 0.0, 900.0, 800.0);
    canvas.apply_pending_fit(viewport);

    let fitted = canvas.zoom;
    assert!(
        fitted < 0.25,
        "fixture must be tall enough to fit below the old floor (got zoom {fitted:.3})"
    );
    assert!(
        (MIN_ZOOM..=MAX_ZOOM).contains(&fitted),
        "a fitted zoom must be inside the interactive range so the user can reach it \
         (zoom {fitted:.3}, range {MIN_ZOOM}..{MAX_ZOOM})"
    );

    // A small scroll from the fitted zoom must move proportionally, not snap to a
    // higher floor. exp(dy*0.0015) for dy=20 is ~1.03, so the zoom should change
    // by only a few percent — never the ~5x leap the old 0.25 clamp caused.
    canvas.cursor = (450.0, 400.0);
    canvas.on_scroll(viewport, 20.0);
    let after = canvas.zoom;
    assert!(
        after > fitted && after < fitted * 1.2,
        "scroll from a sub-floor fitted zoom must be proportional, not a snap: {fitted:.3} -> {after:.3}"
    );
}

/// Peter's second ask: Cmd+L (`request_relayout`) must reframe as well as
/// reformat — a format that leaves the tidy graph scrolled off-screen isn't
/// much use. After scrolling far away, a relayout must arm the fit so the next
/// present brings every node back into view.
#[test]
fn relayout_reframes_the_graph_into_view() {
    let snap = tall_fan_in(6);
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);
    let viewport = Rect::new(0.0, 0.0, 900.0, 800.0);
    canvas.apply_pending_fit(viewport);

    // Scroll the view far off the graph, as if the user got lost.
    canvas.pan = (-9000.0, -9000.0);
    canvas.zoom = 1.0;
    // Cmd+L.
    canvas.request_relayout();
    canvas.apply_pending_fit(viewport);

    let offenders: Vec<&str> = canvas
        .nodes
        .iter()
        .filter(|n| {
            let (sx, sy) = canvas.to_screen(viewport, n.pos_graph.0, n.pos_graph.1);
            let bottom = sy + n.height() * canvas.zoom;
            let right = sx + NODE_WIDTH * canvas.zoom;
            !(sx >= viewport.x - 1.0
                && sy >= viewport.y - 1.0
                && right <= viewport.x + viewport.w + 1.0
                && bottom <= viewport.y + viewport.h + 1.0)
        })
        .map(|n| n.handle.as_deref().unwrap_or("?"))
        .collect();
    assert!(
        offenders.is_empty(),
        "Cmd+L relayout must bring every node into view, but these stayed off-screen: {offenders:?}"
    );
}

/// each node draws in its OWN increasing depth band, and its output
/// preview is painted inline within that band — so a node stacked above (a
/// higher band, drawn later) occludes the preview of one below it. A `Painter`
/// that records the depth of every rect/image draw proves the ordering without
/// pixels: two preview nodes must occupy two distinct increasing bands, each
/// preview sharing its node's band, and the earlier preview's band strictly
/// below the later node's body band (the occlusion condition).
#[test]
fn node_previews_render_in_per_node_depth_bands() {
    use crate::draw::{Depth, Painter};
    use crate::transform2d::Affine2;

    #[derive(Default)]
    struct Rec {
        stack: Vec<Depth>,
        rect_depths: Vec<i32>,
        image_depths: Vec<i32>,
    }
    impl Rec {
        fn cur(&self) -> i32 {
            self.stack.last().copied().unwrap_or(Depth::BASE).0
        }
    }
    impl Painter for Rec {
        fn draw_rect(&mut self, _: f32, _: f32, _: f32, _: f32, _: Color32) {
            self.rect_depths.push(self.cur());
        }
        fn draw_rounded_rect(&mut self, _: f32, _: f32, _: f32, _: f32, _: Color32, _: f32) {
            self.rect_depths.push(self.cur());
        }
        fn draw_bordered_rect(
            &mut self,
            _: f32,
            _: f32,
            _: f32,
            _: f32,
            _: Color32,
            _: f32,
            _: f32,
            _: Color32,
        ) {
            self.rect_depths.push(self.cur());
        }
        fn draw_line(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: Color32) {}
        fn draw_text(&mut self, _: f32, _: f32, _: &str, _: f32, _: [u8; 4]) {}
        fn draw_image_uv(
            &mut self,
            _: f32,
            _: f32,
            _: f32,
            _: f32,
            _: crate::node::TextureHandle,
            _: [f32; 4],
            _: f32,
        ) {
            self.image_depths.push(self.cur());
        }
        fn push_immediate_clip(&mut self, _: f32, _: f32, _: f32, _: f32) {}
        fn pop_immediate_clip(&mut self) {}
        fn push_depth(&mut self, depth: Depth) {
            self.stack.push(depth);
        }
        fn pop_depth(&mut self) {
            self.stack.pop();
        }
        fn push_transform(&mut self, _: Affine2) {}
        fn pop_transform(&mut self) {}
    }

    // Two image-output nodes (Texture2D out + a stable id → each emits a preview).
    let snap = GraphSnapshot {
        nodes: vec![node(0, "a", Some("na")), node(1, "b", Some("nb"))],
        wires: vec![wire(0, "out", 1, "in")],
        outer_routings: Vec::new(),
    };
    let (mut canvas, viewport) = expanded_canvas_from(snap);
    canvas.apply_pending_fit(viewport); // frame both nodes on-screen

    // Give both nodes a preview source (handle + full-texture UV), so the canvas
    // paints their previews inline. Keyed by preview_node_id (== node_id here).
    let handle = crate::node::texture_handle_for_key("test-atlas");
    let mut src = ahash::AHashMap::new();
    src.insert(manifold_foundation::NodeId::new("na"), (handle, [0.0, 0.0, 1.0, 1.0]));
    src.insert(manifold_foundation::NodeId::new("nb"), (handle, [0.0, 0.0, 1.0, 1.0]));
    canvas.set_node_preview_src(src);

    let mut rec = Rec::default();
    canvas.render(&mut rec, viewport);

    let content = Depth::CONTENT.0;
    // Node chrome draws above CONTENT; wires/header/bg are at/below it. Distinct
    // node-band depths, in order:
    let mut bands: Vec<i32> = rec.rect_depths.iter().copied().filter(|d| *d > content).collect();
    bands.sort_unstable();
    bands.dedup();
    assert_eq!(
        bands,
        vec![content + 1, content + 2],
        "two nodes must occupy two distinct increasing depth bands, got {bands:?}"
    );

    // Each preview is painted in a node band (not one shared top layer), and the
    // two previews land in the two distinct bands.
    let mut previews: Vec<i32> = rec.image_depths.clone();
    previews.sort_unstable();
    previews.dedup();
    assert_eq!(
        previews,
        vec![content + 1, content + 2],
        "each node's preview must draw in its own band, got {previews:?}"
    );

    // The occlusion condition: the lower node's preview band is strictly below
    // the upper node's body band, so the upper body draws over the lower preview.
    assert!(
        previews[0] < bands[1],
        "an earlier node's preview ({}) must sit below a later node's body band ({})",
        previews[0],
        bands[1],
    );
}

/// Node 1 (`uv`) with a scalar param `translate` shadowed by a same-named input
/// port, fed by a wire from node 2. Optionally add an outer routing for it.
fn wire_driven_snapshot(with_outer: bool) -> GraphSnapshot {
    let mut n1 = node(1, "node.uv", Some("uv"));
    n1.inputs = vec![port("translate")];
    n1.outputs = vec![port("out")];
    n1.parameters = vec![float_param("translate", 0.5)];
    let n2 = node(2, "node.src", Some("src"));
    GraphSnapshot {
        nodes: vec![n1, n2],
        wires: vec![wire(2, "out", 1, "translate")],
        outer_routings: if with_outer {
            vec![crate::graph_view::OuterParamRouting {
                outer_label: "Macro 1".to_string(),
                outer_param_id: "p0".to_string(),
                node_handle: "uv".to_string(),
                inner_param: "translate".to_string(),
                source: crate::graph_view::OuterParamSource::User,
            }]
        } else {
            Vec::new()
        },
    }
}

fn outer_driven_snapshot() -> GraphSnapshot {
    let mut n1 = node(1, "node.exposure", Some("gain"));
    n1.parameters = vec![float_param("amount", 0.5)];
    GraphSnapshot {
        nodes: vec![n1],
        wires: Vec::new(),
        outer_routings: vec![crate::graph_view::OuterParamRouting {
            outer_label: "Master".to_string(),
            outer_param_id: "p0".to_string(),
            node_handle: "gain".to_string(),
            inner_param: "amount".to_string(),
            source: crate::graph_view::OuterParamSource::User,
        }],
    }
}

#[test]
fn wire_on_same_named_port_marks_param_wire_driven() {
    let (canvas, _vp) = expanded_canvas_from(wire_driven_snapshot(false));
    let p = &canvas.find_node(1).unwrap().params[0];
    assert!(p.wire_driven, "an input wire on the shadow port drives the param");
    assert!(p.outer_driver.is_none(), "no outer routing");
}

#[test]
fn wire_driven_row_is_read_only_no_scrub() {
    let (mut canvas, vp) = expanded_canvas_from(wire_driven_snapshot(false));
    // Press the value side of the wire-driven row.
    let row = canvas.param_row_rect(vp, 1, 0).expect("row rect");
    canvas.on_left_button_down(vp, row.x + row.w * 0.7, row.y + row.h * 0.5, 0.0, false);
    // It selects the node but starts no scrub and emits nothing.
    assert_eq!(canvas.selected_ids(), vec![1], "still selects");
    assert!(
        !canvas.drag.is_active(),
        "wire-driven row starts no scrub"
    );
    assert!(canvas.drain_edits().is_empty(), "and emits no command");
}

#[test]
fn wire_driven_row_expose_glyph_is_dead() {
    let (mut canvas, vp) = expanded_canvas_from(wire_driven_snapshot(false));
    // float param is exposable, so the glyph is a target — but wire-driven, so
    // the click must not emit a toggle.
    let (cx, cy) = glyph_centre(&canvas, vp, 0);
    canvas.on_left_button_down(vp, cx, cy, 0.0, false);
    assert!(
        canvas
            .drain_edits()
            .iter()
            .all(|a| !matches!(a, GraphEditCommand::ToggleNodeParamExpose { .. })),
        "wire-driven expose glyph emits no toggle"
    );
}

#[test]
fn outer_routing_marks_param_outer_driven_but_editable() {
    let (mut canvas, vp) = expanded_canvas_from(outer_driven_snapshot());
    let p = &canvas.find_node(1).unwrap().params[0];
    assert_eq!(p.outer_driver.as_deref(), Some("Master"), "carries the label");
    assert!(!p.wire_driven, "no wire");
    // Outer-driven rows STAY editable — pressing the value starts a scrub.
    let row = canvas.param_row_rect(vp, 1, 0).unwrap();
    canvas.on_left_button_down(vp, row.x + row.w * 0.7, row.y + row.h * 0.5, 0.0, false);
    assert!(
        matches!(canvas.drag.payload(), Some(CanvasDrag::ParamScrub { .. })),
        "outer-driven row is still scrubbable"
    );
}

#[test]
fn wire_wins_when_both_wire_and_outer_drive_a_param() {
    let (canvas, _vp) = expanded_canvas_from(wire_driven_snapshot(true));
    let p = &canvas.find_node(1).unwrap().params[0];
    // Both apply, but the wire short-circuits the binding path, so it wins:
    // the row is read-only. The outer label is still resolved (for reference),
    // but render surfaces "← wired" first.
    assert!(p.wire_driven, "wire present → read-only");
    assert_eq!(p.outer_driver.as_deref(), Some("Macro 1"), "outer still resolved");
}

#[test]
fn click_on_wire_driven_row_highlights_feeding_wire() {
    let (mut canvas, vp) = expanded_canvas_from(wire_driven_snapshot(false));
    let row = canvas.param_row_rect(vp, 1, 0).expect("row rect");
    // Same press as `wire_driven_row_is_read_only_no_scrub` — the click still
    // consumes (selects, starts no scrub), but now also names the wire that
    // feeds the row (D5) so it's attributable instead of just inert.
    canvas.on_left_button_down(vp, row.x + row.w * 0.7, row.y + row.h * 0.5, 0.0, false);
    assert_eq!(
        canvas.highlighted_wire,
        Some((1, "translate".to_string())),
        "click on the wire-driven row highlights the wire landing on it"
    );
    // A click on a different node is a new interaction — the highlight from
    // the previous click doesn't linger (empty-space clicks start a Pan
    // session instead, so this exercises the actual `select_single` /
    // `click_select` clear path rather than the release-time deselect).
    let n2 = canvas.find_node(2).unwrap();
    let (nx, ny) = canvas.to_screen(vp, n2.pos_graph.0, n2.pos_graph.1);
    canvas.on_left_button_down(vp, nx + 10.0, ny + 10.0, 1.0, false);
    assert!(
        canvas.highlighted_wire.is_none(),
        "a click on another node clears the highlight"
    );
}

#[test]
fn wire_driven_row_carries_source_and_keeps_outer_attribution_too() {
    // Wire-only: `driven_by` names the feeding node/port (D5's hover text).
    let (canvas, _vp) = expanded_canvas_from(wire_driven_snapshot(false));
    let p = &canvas.find_node(1).unwrap().params[0];
    assert_eq!(
        p.driven_by.as_deref(),
        Some("driven by src.out"),
        "hover attribution names the wire's source node.port"
    );

    // Both wire AND outer-card binding present (D6): the wire wins for
    // interactivity, but the card mapping must stay discoverable in the data
    // the render pass reads — not silently dropped once a wire lands too.
    let (canvas, _vp) = expanded_canvas_from(wire_driven_snapshot(true));
    let p = &canvas.find_node(1).unwrap().params[0];
    assert!(p.wire_driven, "wire wins for interactivity");
    assert!(p.driven_by.is_some(), "and still names the wire source");
    assert_eq!(
        p.outer_driver.as_deref(),
        Some("Macro 1"),
        "card-binding attribution remains visible alongside the wire hint"
    );
}

#[test]
fn removing_the_wire_reclaims_the_param() {
    // Wire present → read-only; then the same topology minus the wire → editable.
    let (mut canvas, _vp) = expanded_canvas_from(wire_driven_snapshot(false));
    assert!(canvas.find_node(1).unwrap().params[0].wire_driven);
    let mut unwired = wire_driven_snapshot(false);
    unwired.wires.clear();
    canvas.set_snapshot(&unwired);
    assert!(
        !canvas.find_node(1).unwrap().params[0].wire_driven,
        "no wire → param editable again"
    );
}

// ── P2 motion: marquee fade / connect pop / error shake (`tick`) ────────

#[test]
fn marquee_fades_in_while_dragging_and_out_after_release() {
    let mut canvas = GraphCanvas::new();
    canvas.drag.start(CanvasDrag::Marquee, crate::node::Vec2::new(10.0, 10.0));
    canvas.cursor = (50.0, 40.0);

    assert!(canvas.tick(16.0), "still animating toward alpha 1");
    assert!(canvas.marquee_alpha.value() > 0.0, "fading in");
    assert_eq!(
        canvas.marquee_last_rect,
        Some((10.0, 10.0, 40.0, 30.0)),
        "rect tracks origin→cursor while live"
    );

    // Run the fade-in to completion.
    for _ in 0..20 {
        canvas.tick(20.0);
    }
    assert_eq!(canvas.marquee_alpha.value(), 1.0, "fully faded in");

    // Release: the drag resets to idle (mirroring on_left_button_up), but the
    // rect must survive so the fade-OUT frames have something to draw.
    canvas.drag.cancel();
    assert!(canvas.tick(16.0), "now easing back toward 0");
    assert!(canvas.marquee_alpha.value() < 1.0, "fading out");
    assert!(canvas.marquee_last_rect.is_some(), "rect held onto through the fade-out");

    for _ in 0..20 {
        canvas.tick(20.0);
    }
    assert_eq!(canvas.marquee_alpha.value(), 0.0, "fully faded out");
}

#[test]
fn valid_wire_drop_fires_connect_pop_not_error_shake() {
    let mut canvas = GraphCanvas::new();
    canvas.fire_connect_pop(100.0, 200.0);
    assert_eq!(canvas.connect_pop_pos, (100.0, 200.0));
    assert!(canvas.connect_pop.progress().is_some(), "pop is live");
    assert!(canvas.error_shake.progress().is_none(), "shake untouched");

    assert!(canvas.tick(16.0), "pop still animating");
    for _ in 0..30 {
        canvas.tick(20.0);
    }
    assert!(canvas.connect_pop.progress().is_none(), "pop finishes on its own");
}

#[test]
fn invalid_wire_drop_fires_error_shake_not_connect_pop() {
    let mut canvas = GraphCanvas::new();
    canvas.fire_error_shake(30.0, 40.0);
    assert_eq!(canvas.error_shake_pos, (30.0, 40.0));
    assert!(canvas.error_shake.progress().is_some(), "shake is live");
    assert!(canvas.connect_pop.progress().is_none(), "pop untouched");

    for _ in 0..30 {
        canvas.tick(20.0);
    }
    assert!(canvas.error_shake.progress().is_none(), "shake finishes on its own");
}

#[test]
fn dropping_a_wire_on_empty_canvas_fires_error_shake() {
    // End-to-end through the real interaction path, not just the direct
    // fire_* calls above: begin a WireFrom drag, release over empty canvas.
    let mut canvas = GraphCanvas::new();
    let snap = wire_driven_snapshot(false);
    canvas.set_default_expanded(true);
    canvas.set_snapshot(&snap);
    let vp = Rect::new(0.0, 0.0, 2000.0, 2000.0);

    canvas.drag.start(
        CanvasDrag::WireFrom { from_node: 1, from_port: "out".into() },
        crate::node::Vec2::ZERO,
    );
    canvas.on_left_button_up(vp, 9999.0, 9999.0); // far off any node/port
    assert!(canvas.error_shake.progress().is_some(), "invalid drop shakes");
    assert!(canvas.connect_pop.progress().is_none());
}

// ── D17 "wire→port magnetize" + "flow pulse" ────────────────────────────

#[test]
fn wire_magnet_eases_the_ghost_endpoint_onto_a_nearby_input_port() {
    let mut canvas = GraphCanvas::new();
    canvas.set_default_expanded(true);
    canvas.set_snapshot(&wire_driven_snapshot(false));
    let vp = Rect::new(0.0, 0.0, 2000.0, 2000.0);

    let (in_gx, in_gy) = canvas.find_node(1).unwrap().input_port_pos_graph(0);
    let (in_sx, in_sy) = canvas.to_screen(vp, in_gx, in_gy);

    // Drag from node 2's output, cursor placed exactly on node 1's input —
    // well within `port_under`'s hit radius.
    canvas.drag.start(
        CanvasDrag::WireFrom { from_node: 2, from_port: "out".into() },
        crate::node::Vec2::ZERO,
    );
    canvas.cursor = (in_sx, in_sy);
    canvas.tick_wire_magnet(vp);
    assert!(canvas.wire_magnet_live);

    // Settle deterministically with an explicit dt (not wall-clock — same
    // discipline `DropdownPanel`'s own tests use for its entrance tween).
    canvas.wire_magnet_x.tick(color::MOTION_MED_MS);
    canvas.wire_magnet_y.tick(color::MOTION_MED_MS);
    let (ex, ey) = canvas.wire_ghost_endpoint();
    assert!(
        (ex - in_sx).abs() < 0.01 && (ey - in_sy).abs() < 0.01,
        "settles exactly onto the port: ({ex},{ey}) vs ({in_sx},{in_sy})"
    );
}

#[test]
fn wire_magnet_tracks_the_cursor_directly_when_no_port_is_near() {
    let mut canvas = GraphCanvas::new();
    canvas.set_default_expanded(true);
    canvas.set_snapshot(&wire_driven_snapshot(false));
    let vp = Rect::new(0.0, 0.0, 2000.0, 2000.0);

    canvas.drag.start(
        CanvasDrag::WireFrom { from_node: 2, from_port: "out".into() },
        crate::node::Vec2::ZERO,
    );
    canvas.cursor = (1900.0, 1900.0); // far from every port
    canvas.tick_wire_magnet(vp);
    let (ex, ey) = canvas.wire_ghost_endpoint();
    assert_eq!((ex, ey), (1900.0, 1900.0), "no lag when nothing is in magnet range");
}

#[test]
fn wire_flow_pulse_fires_and_settles() {
    let mut canvas = GraphCanvas::new();
    canvas.fire_wire_flow_pulse((0.0, 0.0), (100.0, 50.0));
    assert!(canvas.wire_flow_pulse.progress().is_some());
    assert_eq!(canvas.wire_flow_pulse_from, (0.0, 0.0));
    assert_eq!(canvas.wire_flow_pulse_to, (100.0, 50.0));

    for _ in 0..20 {
        canvas.tick(20.0);
    }
    assert!(canvas.wire_flow_pulse.progress().is_none(), "pulse finishes on its own");
}

#[test]
fn connecting_a_wire_end_to_end_fires_the_flow_pulse_with_real_port_geometry() {
    // Through the real interaction path (not just the direct `fire_*` call
    // above): begin a WireFrom drag, release exactly on the real target
    // port, and check the pulse's captured geometry matches the two ports.
    let mut canvas = GraphCanvas::new();
    canvas.set_default_expanded(true);
    canvas.set_snapshot(&wire_driven_snapshot(false));
    let vp = Rect::new(0.0, 0.0, 2000.0, 2000.0);

    let (out_gx, out_gy) = canvas.find_node(2).unwrap().output_port_pos_graph(0);
    let (out_sx, out_sy) = canvas.to_screen(vp, out_gx, out_gy);
    let (in_gx, in_gy) = canvas.find_node(1).unwrap().input_port_pos_graph(0);
    let (in_sx, in_sy) = canvas.to_screen(vp, in_gx, in_gy);

    canvas.drag.start(
        CanvasDrag::WireFrom { from_node: 2, from_port: "out".into() },
        crate::node::Vec2::ZERO,
    );
    canvas.on_left_button_up(vp, in_sx, in_sy);

    assert!(canvas.wire_flow_pulse.progress().is_some(), "connect fires the pulse");
    assert!((canvas.wire_flow_pulse_from.0 - out_sx).abs() < 0.01);
    assert!((canvas.wire_flow_pulse_from.1 - out_sy).abs() < 0.01);
    assert!((canvas.wire_flow_pulse_to.0 - in_sx).abs() < 0.01);
    assert!((canvas.wire_flow_pulse_to.1 - in_sy).abs() < 0.01);
}

// ── HitTargets (UI_AUTOMATION_DESIGN.md P1) ──────────────────────

#[test]
fn graph_canvas_targets_enumerates_nodes_ports_and_wires_with_payload_ids() {
    use super::hit::GraphCanvasTargets;
    use crate::hit_targets::HitTargets;

    let snap = GraphSnapshot {
        nodes: vec![
            node(0, "system.source", Some("source")),
            node(2, "system.final_output", Some("final")),
        ],
        wires: vec![wire(0, "out", 2, "in")],
        outer_routings: Vec::new(),
    };
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);
    let viewport = Rect::new(0.0, 0.0, 1000.0, 800.0);
    canvas.apply_pending_fit(viewport);

    let targets = GraphCanvasTargets { canvas: &canvas, viewport };
    assert_eq!(targets.surface_id(), "graph_canvas");
    let mut out = Vec::new();
    targets.enumerate(&mut out);

    let nodes: Vec<_> = out.iter().filter(|e| e.kind == "node").collect();
    assert_eq!(nodes.len(), 2, "one entry per node hit_test can return");
    assert!(nodes.iter().any(|n| n.payload == "scope=/node=0"));
    assert!(nodes.iter().any(|n| n.payload == "scope=/node=2"));

    let ports: Vec<_> = out.iter().filter(|e| e.kind == "port").collect();
    assert!(!ports.is_empty(), "every node's in/out sockets enumerate");

    let wires: Vec<_> = out.iter().filter(|e| e.kind == "wire").collect();
    assert_eq!(wires.len(), 1);
    assert_eq!(wires[0].payload, "scope=/from=0:out/to=2:in");
}

// ─── D6: group-face param rows (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN §2) ─────
//
// A group box mirrors, as live rows, every already-exposed card param whose
// binding target resolves to a node inside it (transitively through nested
// groups). Fixtures below reuse `grouped_snapshot`'s id scheme (group id 10,
// handle "tweak"; body: group_input(0) → inner(1, handle "inner") →
// group_output(2)) — only difference is `inner` now carries a param and the
// root snapshot carries the `OuterParamRouting` that exposes it.

/// One `OuterParamRouting` entry: an outer card slider (`outer_param_id`,
/// `outer_label`) routed into `node_handle`'s `inner_param`.
fn outer_routing(
    outer_param_id: &str,
    outer_label: &str,
    node_handle: &str,
    inner_param: &str,
) -> crate::graph_view::OuterParamRouting {
    crate::graph_view::OuterParamRouting {
        outer_label: outer_label.to_string(),
        outer_param_id: outer_param_id.to_string(),
        node_handle: node_handle.to_string(),
        inner_param: inner_param.to_string(),
        source: crate::graph_view::OuterParamSource::User,
    }
}

/// `grouped_snapshot`'s shape, but `inner` (inside group "tweak") carries a
/// Float param "amount" at `current`, exposed on the card as "card_amount"
/// ("Amount"). One group, one level.
fn grouped_snapshot_with_exposed_param(current: f32) -> GraphSnapshot {
    let mut snap = grouped_snapshot();
    let group = snap.nodes.iter_mut().find(|n| n.type_id == GROUP_TYPE_ID).unwrap();
    let body = group.group.as_mut().unwrap();
    let inner = body.nodes.iter_mut().find(|n| n.node_handle.as_deref() == Some("inner")).unwrap();
    inner.parameters = vec![float_param("amount", current)];
    snap.outer_routings = vec![outer_routing("card_amount", "Amount", "inner", "amount")];
    snap
}

/// Two nested groups: root → "outer" (handle) → body contains "inner_group"
/// (handle) → its body contains `leaf` (handle "leaf") carrying the exposed
/// Float param "amount". Card exposes it as "card_amount". Tests the
/// currently-viewed-level rule: at root, "outer"'s box carries the row; enter
/// "outer" and "inner_group"'s box carries it instead — never both at once.
fn nested_group_snapshot_with_exposed_param(current: f32) -> GraphSnapshot {
    let mut leaf = node(1, "node.blur", Some("leaf"));
    leaf.parameters = vec![float_param("amount", current)];
    let inner_body = GroupSnapshot {
        nodes: vec![node(0, "system.group_input", None), leaf, node(2, "system.group_output", None)],
        wires: vec![wire(0, "src", 1, "in"), wire(1, "out", 2, "out")],
        tint: None,
    };
    let mut inner_group = node(20, GROUP_TYPE_ID, Some("inner_group"));
    inner_group.inputs = vec![port("src")];
    inner_group.outputs = vec![port("out")];
    inner_group.group = Some(Box::new(inner_body));

    let outer_body = GroupSnapshot {
        nodes: vec![node(0, "system.group_input", None), inner_group, node(2, "system.group_output", None)],
        wires: vec![wire(0, "src", 20, "src"), wire(20, "out", 2, "out")],
        tint: None,
    };
    let mut outer_group = node(10, GROUP_TYPE_ID, Some("outer"));
    outer_group.inputs = vec![port("src")];
    outer_group.outputs = vec![port("out")];
    outer_group.group = Some(Box::new(outer_body));

    GraphSnapshot {
        nodes: vec![
            node(0, "system.source", Some("source")),
            outer_group,
            node(2, "system.final_output", Some("final")),
        ],
        wires: vec![wire(0, "out", 10, "src"), wire(10, "out", 2, "in")],
        outer_routings: vec![outer_routing("card_amount", "Amount", "leaf", "amount")],
    }
}

#[test]
fn group_face_row_appears_for_inner_targeted_card_param() {
    let mut canvas = GraphCanvas::new();
    canvas.collapsed.insert(10, false); // expand the group before the full rebuild
    canvas.set_snapshot(&grouped_snapshot_with_exposed_param(0.25));

    let group = canvas.find_node(10).expect("group node");
    assert_eq!(group.params.len(), 1, "one mirrored row for the exposed inner param");
    let row = &group.params[0];
    assert_eq!(row.label, "Amount", "row label is the OUTER card label, not the inner param name");
    assert_eq!(row.value, "0.25");
    assert_eq!(
        row.outer_param_id.as_deref(),
        Some("card_amount"),
        "row carries the routing's own ParamId for scrub dispatch"
    );
    // A param with no exposing routing draws no row: build_group_param_rows is
    // exposed-only, never the deleted authoring picker.
    assert!(group.rows.iter().any(|r| matches!(r, NodeRow::Param { .. })));
}

#[test]
fn group_face_row_absent_when_no_outer_routing_targets_the_group() {
    let mut snap = grouped_snapshot();
    // Give `inner` a param but expose NOTHING — no `OuterParamRouting` at all.
    let group = snap.nodes.iter_mut().find(|n| n.type_id == GROUP_TYPE_ID).unwrap();
    let body = group.group.as_mut().unwrap();
    let inner = body.nodes.iter_mut().find(|n| n.node_handle.as_deref() == Some("inner")).unwrap();
    inner.parameters = vec![float_param("amount", 0.25)];
    let mut canvas = GraphCanvas::new();
    canvas.collapsed.insert(10, false);
    canvas.set_snapshot(&snap);
    assert!(
        canvas.find_node(10).unwrap().params.is_empty(),
        "an un-exposed inner param mirrors nothing on the group face"
    );
}

#[test]
fn collapsed_group_with_rows_shows_a_compact_params_chip() {
    let mut canvas = GraphCanvas::new();
    // Force collapsed regardless of `default_collapsed` (canvas-wide default,
    // not this test's concern) — D6's collapse switch is per-node.
    canvas.collapsed.insert(10, true);
    canvas.set_snapshot(&grouped_snapshot_with_exposed_param(0.25));
    let group = canvas.find_node(10).expect("group node");
    assert!(group.collapsed, "sanity: this group is collapsed");
    assert_eq!(
        group.summary.as_deref(),
        Some("1 params"),
        "collapsed group carries a compact count chip, not a size threshold"
    );
}

/// Scrub the group-face row and assert it emits `GraphEditCommand::SetOuterParam`
/// carrying the routing's own `ParamId` string and the scrubbed value — the
/// SAME `(ParamId, value)` pair a card slider scrubbing this exact exposed
/// param would carry into its own `PanelAction::ParamChanged(GraphParamTarget,
/// ParamId, f32)` (`param_card.rs`'s `ParamChanged` construction). The
/// enum-wrapping into `PanelAction` itself is app-side glue
/// (`manifold-app/src/app_render.rs`'s `SetOuterParam` arm, documented there);
/// this crate boundary is where the invariant is actually decidable, so the
/// mirrored `PanelAction::ParamChanged` is reconstructed here from the same
/// two values and compared field-by-field against what the card path would
/// build for that id/value (PanelAction has no `PartialEq`, hence the
/// destructure instead of `assert_eq!` on the whole action).
#[test]
fn group_face_scrub_emits_set_outer_param_matching_card_value() {
    let mut canvas = GraphCanvas::new();
    canvas.collapsed.insert(10, false);
    canvas.set_snapshot(&grouped_snapshot_with_exposed_param(0.25));

    let row = canvas.param_row_rect(Rect::new(0.0, 0.0, 1200.0, 800.0), 10, 0).expect("row rect");
    let vp = Rect::new(0.0, 0.0, 1200.0, 800.0);
    let px = row.x + row.w * 0.7;
    canvas.on_left_button_down(vp, px, row.y + row.h * 0.5, 0.0, false);
    assert!(
        matches!(canvas.drag.payload(), Some(CanvasDrag::ParamScrub { outer_param_id: Some(_), .. })),
        "the row's CanvasDrag carries the outer_param_id marker"
    );
    // +120px on a (0,1)-range Float, same PARAM_SCRUB_FULL_RANGE_PX math the
    // inner node's own row scrub uses: 0.25 → 0.75.
    canvas.on_pointer_move(vp, px + 120.0, row.y + row.h * 0.5);

    let (outer_param_id, new_value) = canvas
        .drain_edits()
        .into_iter()
        .find_map(|a| match a {
            GraphEditCommand::SetOuterParam { outer_param_id, new_value } => {
                Some((outer_param_id, new_value))
            }
            _ => None,
        })
        .expect("scrub emits SetOuterParam, not SetGraphNodeParam");
    assert_eq!(outer_param_id, "card_amount", "same ParamId string the card's own binding carries");
    assert!((new_value - 0.75).abs() < 1e-3, "same scrub math as an ordinary node-face row");

    // The card-path equivalent: what `param_card.rs`'s own ParamChanged arm
    // would build for this exact (id, value) pair.
    let target = GraphParamTarget::Effect(0);
    let mirrored = PanelAction::Params(ParamsAction::ParamChanged(
        target,
        manifold_foundation::ParamId::from(outer_param_id.clone()),
        new_value,
    ));
    let card_would_emit = PanelAction::Params(ParamsAction::ParamChanged(
        GraphParamTarget::Effect(0),
        manifold_foundation::ParamId::from("card_amount".to_string()),
        0.75,
    ));
    match (mirrored, card_would_emit) {
        (
            PanelAction::Params(ParamsAction::ParamChanged(mt, mid, mv)),
            PanelAction::Params(ParamsAction::ParamChanged(ct, cid, cv)),
        ) => {
            assert_eq!(mt, ct, "same GraphParamTarget");
            assert_eq!(mid, cid, "same ParamId");
            assert!((mv - cv).abs() < 1e-3, "same value");
        }
        _ => unreachable!(),
    }
}

#[test]
fn group_face_row_wire_driven_when_inner_param_is_wire_fed() {
    let mut snap = grouped_snapshot_with_exposed_param(0.25);
    // Feed `inner`'s `amount` port from a same-level wire (port-shadows-param) —
    // add a source node inside the body wired to `inner`'s (implicit) `amount`
    // input port. `node()` gives every node an `in`/`out` port pair named
    // plainly, so wire the body's `group_input` "src" straight into an
    // `amount`-named port by naming it via a synthetic wire (the row's
    // wire-driven check only cares about `to_port == "amount"`, not that a
    // declared port exists — mirrors the ordinary-node check).
    let group = snap.nodes.iter_mut().find(|n| n.type_id == GROUP_TYPE_ID).unwrap();
    let body = group.group.as_mut().unwrap();
    body.wires.push(wire(0, "src", 1, "amount"));

    let mut canvas = GraphCanvas::new();
    canvas.collapsed.insert(10, false);
    canvas.set_snapshot(&snap);
    let row = &canvas.find_node(10).unwrap().params[0];
    assert!(row.wire_driven, "a wire feeding the target inner param locks the mirrored row too");
}

#[test]
fn nested_group_row_lives_on_the_currently_viewed_level_only() {
    let snap = nested_group_snapshot_with_exposed_param(0.25);
    let mut canvas = GraphCanvas::new();
    canvas.collapsed.insert(10, false); // expand "outer" at root
    canvas.set_snapshot(&snap);

    // At root: "outer" (id 10) carries the mirrored row (leaf is transitively
    // inside it); there is no OTHER box at this level that could also carry it.
    assert_eq!(canvas.nodes.len(), 3, "source, outer, final — one level");
    let outer = canvas.find_node(10).expect("outer group at root");
    assert_eq!(outer.params.len(), 1, "outer carries the row while viewed from root");

    // Descend into "outer": the level swaps to its body — "inner_group" (id
    // 20) is now the visible group box, and it, not "outer" (no longer even
    // present as a NodeView at this level), carries the row.
    canvas.enter_group(10);
    canvas.collapsed.insert(20, false);
    canvas.set_snapshot(&snap);
    assert!(
        canvas.find_node(10).is_none(),
        "outer's own box isn't rendered once you're inside it — never two boxes at once"
    );
    let inner_group = canvas.find_node(20).expect("inner_group visible one level down");
    assert_eq!(inner_group.params.len(), 1, "the row re-homed one level down with the group that now visibly contains leaf");
    assert_eq!(inner_group.params[0].label, "Amount");
}

// ── scene build P5: "+ Object" / "+ Light" action rows, same-pair ribbons ──

#[test]
fn render_scene_node_gets_add_object_and_add_light_action_rows() {
    let mut n = node(1, "node.render_scene", Some("render"));
    n.parameters = vec![float_param("objects", 2.0), float_param("lights", 1.0)];
    let snap = GraphSnapshot {
        nodes: vec![n],
        wires: Vec::new(),
        outer_routings: Vec::new(),
    };
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);

    let view = canvas.find_node(1).expect("render_scene node view");
    assert!(view.is_render_scene);

    let objects_row = view
        .rows
        .iter()
        .position(|r| matches!(r, NodeRow::Param { param, .. } if view.params[*param].name == "objects"))
        .expect("objects param row");
    assert!(
        matches!(view.rows[objects_row + 1], NodeRow::Action(NodeActionKind::AddSceneObject)),
        "+ Object button spliced right after the objects row"
    );

    let lights_row = view
        .rows
        .iter()
        .position(|r| matches!(r, NodeRow::Param { param, .. } if view.params[*param].name == "lights"))
        .expect("lights param row");
    assert!(
        matches!(view.rows[lights_row + 1], NodeRow::Action(NodeActionKind::AddSceneLight)),
        "+ Light button spliced right after the lights row"
    );
}

#[test]
fn ordinary_node_gets_no_action_rows() {
    let mut n = node(1, "node.blur", Some("blur"));
    n.parameters = vec![float_param("amount", 0.5)];
    let snap = GraphSnapshot {
        nodes: vec![n],
        wires: Vec::new(),
        outer_routings: Vec::new(),
    };
    let mut canvas = GraphCanvas::new();
    canvas.set_snapshot(&snap);

    let view = canvas.find_node(1).expect("blur node view");
    assert!(!view.is_render_scene);
    assert!(
        !view.rows.iter().any(|r| matches!(r, NodeRow::Action(_))),
        "only render_scene gets gesture-button rows"
    );
}

#[test]
fn group_wires_by_pair_collapses_shared_endpoints_leaves_singletons_alone() {
    let wires = [
        WireView { from_node: 1, from_port: "vertices".into(), to_node: 9, to_port: "mesh_0".into() },
        WireView { from_node: 1, from_port: "material".into(), to_node: 9, to_port: "material_0".into() },
        WireView { from_node: 1, from_port: "transform".into(), to_node: 9, to_port: "transform_0".into() },
        WireView { from_node: 2, from_port: "out".into(), to_node: 9, to_port: "camera".into() },
    ];
    let groups = group_wires_by_pair(wires.iter());
    assert_eq!(groups.len(), 2, "two distinct (from_node, to_node) pairs");

    let ribboned = groups.iter().find(|(k, _)| *k == (1, 9)).expect("the 3-wire pair");
    assert_eq!(ribboned.1.len(), 3, "all three same-pair wires grouped into one ribbon");

    let singleton = groups.iter().find(|(k, _)| *k == (2, 9)).expect("the 1-wire pair");
    assert_eq!(singleton.1.len(), 1, "a lone wire between a pair stays its own group (no ribbon)");
}

// ─── UI_WIDGET_UNIFICATION P4: boundary hit-geometry pins for the canvas's
//     single-host modal editors (EnumDropdown::option_at, VecEditor::channel_at,
//     TableEditor::cell_at). The dismiss/swallow behavior at each editor's own
//     boundary is already pinned above (pressing_outside_open_*); these add the
//     boundary-coordinate cases the doc calls out by name plus the
//     cross-editor modal-priority case (one press both dismisses the open
//     editor and acts on the newly-clicked row). ───────────────────────────

#[test]
fn option_at_pins_first_last_and_boundary_rows() {
    let (mut canvas, vp) = expanded_canvas(enum_param("mode", 0.0));
    press_row_value(&mut canvas, vp, 0);
    let dd = canvas.enum_dropdown.as_ref().expect("dropdown open");
    assert_eq!(dd.options.len(), 3, "A/B/C");

    let r0 = dd.option_rect(0);
    let r2 = dd.option_rect(2);
    // Inside the first row's top-left corner.
    assert_eq!(dd.option_at(r0.x + 1.0, r0.y + 1.0), Some(0), "first option, near-corner");
    // Inside the last row's bottom edge (just above it — the row is
    // half-open [top, top+h)).
    assert_eq!(dd.option_at(r2.x + 1.0, r2.y + r2.h - 1.0), Some(2), "last option, near-bottom-edge");
    // One pixel past the list's bottom edge: outside `contains()`, so `None`.
    assert_eq!(dd.option_at(r2.x + 1.0, r2.y + r2.h + 1.0), None, "past the list bottom");
    // Above the list (inside the anchor row it opened from): `None`.
    assert_eq!(dd.option_at(dd.anchor.x + 1.0, dd.anchor.y + 1.0), None, "above the list, on the anchor row");
}

#[test]
fn channel_at_pins_first_last_channel_and_header_row() {
    let (mut canvas, vp) = expanded_canvas(color_param("tint", [0.1, 0.2, 0.3, 0.4]));
    press_row_value(&mut canvas, vp, 0);
    let ed = canvas.vec_editor.as_ref().expect("editor open");
    assert_eq!(ed.components, 4, "RGBA");

    let ch0 = ed.channel_rect(0);
    let ch3 = ed.channel_rect(3);
    assert_eq!(ed.channel_at(ch0.x + 1.0, ch0.y + 1.0), Some(0), "first channel row");
    assert_eq!(
        ed.channel_at(ch3.x + 1.0, ch3.y + ch3.h - 1.0),
        Some(3),
        "last channel row, near its bottom edge"
    );
    // The swatch header row sits above channel 0 — not itself a channel.
    let sw = ed.swatch_rect().unwrap();
    assert_eq!(ed.channel_at(sw.x + 1.0, sw.y + 1.0), None, "header row is not a channel");
    // Past the panel entirely.
    assert_eq!(ed.channel_at(ch3.x + 1.0, ch3.y + ch3.h + 50.0), None, "past the panel");
}

#[test]
fn cell_at_pins_corner_cells_and_the_header_line() {
    let rows = vec![vec![0.0, 1.0, 2.0], vec![10.0, 11.0, 12.0]];
    let (mut canvas, vp) = expanded_canvas(table_param("grid", rows));
    press_row_value(&mut canvas, vp, 0);
    let ed = canvas.table_editor.as_ref().expect("grid editor open");
    assert_eq!((ed.rows, ed.cols), (2, 3));

    let top_left = ed.cell_rect(0, 0);
    let bottom_right = ed.cell_rect(1, 2);
    assert_eq!(
        ed.cell_at(top_left.x + 1.0, top_left.y + 1.0),
        Some((0, 0)),
        "top-left corner cell"
    );
    assert_eq!(
        ed.cell_at(bottom_right.x + bottom_right.w - 1.0, bottom_right.y + bottom_right.h - 1.0),
        Some((1, 2)),
        "bottom-right corner cell, near its own boundary"
    );
    // The header line (between the anchor row and the grid) is not a cell.
    let panel = ed.panel_rect();
    assert_eq!(ed.cell_at(panel.x + 1.0, panel.y + 1.0), None, "header line is not a cell");
    // Past the last column, same row band.
    assert_eq!(
        ed.cell_at(bottom_right.x + bottom_right.w + 50.0, bottom_right.y + 1.0),
        None,
        "past the last column"
    );
}

#[test]
fn pressing_far_outside_dismisses_then_a_second_rows_value_opens_its_own_editor() {
    // Two rows: an enum on row 0, a color on row 1. The open dropdown's option
    // list stacks directly below row 0's anchor — which is exactly where row
    // 1 sits in the static layout, so a single press there lands ON the open
    // list, not on row 1 underneath it (modal-over-canvas priority, D2/D17).
    // Pin the two-press sequence instead: an out-of-bounds press dismisses
    // with no stray command, then a fresh press on row 1's value opens ITS
    // OWN editor — each editor's modal claim is independent, never sticky
    // across a dismissal.
    let mut canvas = GraphCanvas::new();
    canvas.collapsed.insert(1, false);
    canvas.set_snapshot(&snapshot_with_param_node(
        "gain",
        vec![enum_param("mode", 0.0), color_param("tint", [0.1, 0.2, 0.3, 1.0])],
    ));
    let vp = Rect::new(0.0, 0.0, 1200.0, 800.0);

    press_row_value(&mut canvas, vp, 0);
    assert!(canvas.enum_dropdown.is_some(), "row 0 dropdown open");

    // Far from both the row and the open list — empty canvas, dismisses.
    canvas.on_left_button_down(vp, 1000.0, 700.0, 0.0, false);
    assert!(canvas.enum_dropdown.is_none(), "out-of-bounds press dismissed the modal");
    assert!(
        drained_set_param(&mut canvas).is_none(),
        "dismissal alone emits no value command"
    );
    assert!(canvas.vec_editor.is_none(), "dismissal does not open a different editor by itself");

    press_row_value(&mut canvas, vp, 1);
    assert!(canvas.vec_editor.is_some(), "a fresh press on row 1's value opens its own editor");
}

// ─── UI_WIDGET_UNIFICATION P5d: the contract's last dead stop — a
//     double-click on a numeric row's value-cell zone opens the type-in
//     instead of arming a scrub; every other press on the row keeps
//     scrubbing exactly as before. ─────────────────────────────────────

#[test]
fn zero_move_press_release_on_a_numeric_row_emits_no_command() {
    // VERIFY-AT-IMPL (P5d brief): confirms the premise the double-click
    // layering relies on — an ordinary click-with-no-drag on the value box
    // arms then releases a scrub with nothing committed.
    let (mut canvas, vp) = expanded_canvas(float_param("amount", 0.5));
    let row = canvas.param_row_rect(vp, 1, 0).unwrap();
    let z = canvas.param_slider_zones(vp, 1).unwrap();
    let x = z.value_cell.x + z.value_cell.width * 0.5;
    let y = row.y + row.h * 0.5;

    canvas.on_left_button_down(vp, x, y, 1.0, false);
    assert!(matches!(canvas.drag.payload(), Some(CanvasDrag::ParamScrub { .. })));
    canvas.on_left_button_up(vp, x, y);
    assert!(canvas.drain_edits().is_empty(), "zero-move press+release emits nothing");
}

#[test]
fn double_click_on_the_value_cell_opens_the_type_in_instead_of_scrubbing() {
    let (mut canvas, vp) = expanded_canvas(float_param("amount", 0.5));
    let row = canvas.param_row_rect(vp, 1, 0).unwrap();
    let z = canvas.param_slider_zones(vp, 1).unwrap();
    let x = z.value_cell.x + z.value_cell.width * 0.5;
    let y = row.y + row.h * 0.5;

    canvas.on_left_button_down(vp, x, y, 1.0, false);
    canvas.on_left_button_up(vp, x, y);
    canvas.drain_edits();

    // Second press within the double-click time/radius window (I8's
    // single-sourced constants), same value cell.
    canvas.on_left_button_down(vp, x, y, 1.05, false);
    assert!(
        !matches!(canvas.drag.payload(), Some(CanvasDrag::ParamScrub { .. })),
        "double-click opens the type-in, not a second scrub"
    );
    let cmd = canvas
        .drain_edits()
        .into_iter()
        .find_map(|a| match a {
            GraphEditCommand::EditGraphNodeNumericParam {
                node_id,
                param_name,
                current,
                min,
                max,
                whole_numbers,
                outer_param_id,
                ..
            } => Some((node_id, param_name, current, min, max, whole_numbers, outer_param_id)),
            _ => None,
        })
        .expect("double-click emits EditGraphNodeNumericParam");
    assert_eq!(cmd.0, 1, "node id");
    assert_eq!(cmd.1, "amount", "param name");
    assert!((cmd.2 - 0.5).abs() < 1e-6, "prefilled with the current value");
    assert!(cmd.6.is_none(), "not a group-face mirror row");
}

#[test]
fn single_click_on_the_value_cell_still_scrubs() {
    let (mut canvas, vp) = expanded_canvas(float_param("amount", 0.5));
    let row = canvas.param_row_rect(vp, 1, 0).unwrap();
    let z = canvas.param_slider_zones(vp, 1).unwrap();
    let x = z.value_cell.x + z.value_cell.width * 0.5;
    let y = row.y + row.h * 0.5;

    canvas.on_left_button_down(vp, x, y, 1.0, false);
    assert!(matches!(canvas.drag.payload(), Some(CanvasDrag::ParamScrub { .. })));
    assert!(canvas.drain_edits().is_empty(), "opening a scrub emits nothing on its own");
}

#[test]
fn double_click_outside_the_value_cell_still_scrubs() {
    let (mut canvas, vp) = expanded_canvas(float_param("amount", 0.5));
    let row = canvas.param_row_rect(vp, 1, 0).unwrap();
    let z = canvas.param_slider_zones(vp, 1).unwrap();
    // Inside the TRACK zone, left of the value cell — never the type-in's
    // zone, so double-clicking here keeps scrubbing (D13: EditValue is
    // owned by ValueCell alone).
    let x = z.track.x + z.track.width * 0.5;
    let y = row.y + row.h * 0.5;

    canvas.on_left_button_down(vp, x, y, 1.0, false);
    canvas.on_left_button_up(vp, x, y);
    canvas.drain_edits();
    canvas.on_left_button_down(vp, x, y, 1.05, false);
    assert!(
        matches!(canvas.drag.payload(), Some(CanvasDrag::ParamScrub { .. })),
        "double-click outside the value cell still scrubs"
    );
    assert!(
        canvas.drain_edits().iter().all(|a| !matches!(a, GraphEditCommand::EditGraphNodeNumericParam { .. })),
        "no type-in command from a double-click outside the value cell"
    );
}
