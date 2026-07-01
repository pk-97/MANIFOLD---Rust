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
    // Each preview-strip rect is the image area and has positive size.
    assert!(thumbs.iter().all(|(_, _, _, w, h)| *w > 0.0 && *h > 0.0));
}

#[test]
fn preview_screen_size_follows_project_aspect() {
    // Landscape (16:9) is width-bound: full inner width, short — the historical
    // behaviour, unchanged.
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
    assert!(matches!(canvas.drag_mode, DragMode::ParamScrub { .. }));
    assert!(
        canvas
            .drain_edits()
            .iter()
            .all(|a| !matches!(a, GraphEditCommand::ToggleNodeParamExpose { .. })),
        "row-body press must not toggle exposure"
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
