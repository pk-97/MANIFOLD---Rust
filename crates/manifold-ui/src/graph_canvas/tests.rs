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
        matches!(canvas.drag_mode, DragMode::VecScrub { channel: 1, .. }),
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
        !matches!(canvas.drag_mode, DragMode::VecScrub { .. }),
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
        matches!(canvas.drag_mode, DragMode::None),
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
        matches!(canvas.drag_mode, DragMode::ParamScrub { .. }),
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
    canvas.drag_mode = DragMode::Marquee { origin_screen: (10.0, 10.0) };
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

    // Release: drag_mode resets to None (mirroring on_left_button_up), but the
    // rect must survive so the fade-OUT frames have something to draw.
    canvas.drag_mode = DragMode::None;
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

    canvas.drag_mode = DragMode::WireFrom { from_node: 1, from_port: "out".into() };
    canvas.on_left_button_up(vp, 9999.0, 9999.0); // far off any node/port
    assert!(canvas.error_shake.progress().is_some(), "invalid drop shakes");
    assert!(canvas.connect_pop.progress().is_none());
}
