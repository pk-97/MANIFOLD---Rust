//! Pure-logic tests for the group-aware canvas. Everything that isn't
//! pixels is exercised here so a misbehaving canvas points to rendering
//! (eyes only), not logic. Per the handoff doc's debug-friendly mandate.
use super::*;
// Items used only by tests are imported directly from their module rather than
// re-exported crate-wide from `mod.rs` (which would read as unused in a
// non-test build).
use super::hit::{ports_compatible, rects_overlap};
use super::layout::LayeredLayout;
use crate::graph_view::{
    GraphSnapshot, GroupSnapshot, NodeSnapshot, PortKindSnapshot, PortSnapshot, WireSnapshot,
};

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
    // Each preview-strip rect is the 16:9 image area and has positive size.
    assert!(thumbs.iter().all(|(_, _, _, w, h)| *w > 0.0 && *h > 0.0));
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
    let mut n = node(1, "node.gain", Some(handle));
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
    canvas.drag_mode = DragMode::ParamScrub {
        node_id: 1,
        param_name: "amount".to_string(),
        range: (0.0, 1.0),
        start_value: 0.25,
        is_int: false,
        press_origin_x: 0.0,
    };
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
    let mut n = node(2, "node.gain", Some("other"));
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
    assert_eq!(emitted, Some(Some(GROUP_TINT_PALETTE[0])));
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

// ── Connection type feedback ────────────────────────────────────

#[test]
fn ports_compatible_is_colour_category_equality() {
    // Same category → compatible (the ghost wire reads green).
    assert!(ports_compatible(PORT_TEXTURE2D_COLOR, PORT_TEXTURE2D_COLOR));
    // Cross-category → incompatible (red), so a mis-wire is caught pre-drop.
    assert!(!ports_compatible(PORT_TEXTURE2D_COLOR, PORT_SCALAR_COLOR));
    assert!(!ports_compatible(PORT_SCALAR_COLOR, PORT_ARRAY_COLOR));
}
