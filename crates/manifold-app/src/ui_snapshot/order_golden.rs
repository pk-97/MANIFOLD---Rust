//! `paint_and_hit_order_match_full_build` (UI_INVALIDATION_DESIGN.md INV-14).
//!
//! Builds the main window with every stacked surface open at once (a
//! dropdown, the Ableton picker, a toast, the warmup overlay, a card-drag
//! ghost and a hovered control with a tooltip) and records what the tree
//! paints and what it hits: the region-tier paint walk, each atlas panel's
//! node run, each overlay's node run, the ghost pass, and a hit-test grid
//! over the whole window. Each section is hashed against
//! `order_golden.txt`, recorded on the flat tree before per-region storage,
//! so the arenas must reproduce today's order exactly. Region rebuilds of
//! every main region must reproduce it too.
//!
//! Re-record with `MANIFOLD_RECORD_ORDER_GOLDEN=1`. On a mismatch the full
//! sections are written next to the golden as `order_golden.<section>.txt`
//! for diffing.

use std::fmt::Write as _;

use manifold_ui::input::PointerAction;
use manifold_ui::node::WidgetId;
use manifold_ui::node::{UIFlags, UINode, Vec2};
use manifold_ui::tree::TraversalEvent;

use super::fixtures;
use super::{LOGICAL_H, LOGICAL_W, sync_build};
use crate::frame_plan::{MainRegion, RegionMask};
use crate::ui_frame::{UiFrameSignals, apply_ui_frame_invalidations};
use crate::ui_root::UIRoot;

const GOLDEN: &str = "src/ui_snapshot/order_golden.txt";
const HIT_STEP: f32 = 8.0;

fn describe(n: &UINode) -> String {
    let mask = UIFlags::VISIBLE
        | UIFlags::INTERACTIVE
        | UIFlags::CLIPS_CHILDREN
        | UIFlags::DISABLED
        | UIFlags::HOVERED
        | UIFlags::ALLOW_OVERFLOW;
    let b = n.bounds;
    format!(
        "{:?} {:.1},{:.1},{:.1},{:.1} {:?} f{:x} bg{:?}",
        n.node_type,
        b.x,
        b.y,
        b.width,
        b.height,
        n.text,
        (n.flags & mask).bits(),
        n.style.bg_color,
    )
}

fn event(out: &mut Vec<String>, ev: TraversalEvent) {
    match ev {
        TraversalEvent::Node(n) => out.push(describe(n)),
        TraversalEvent::PushClip(r) => out.push(format!(
            "push {:.1},{:.1},{:.1},{:.1}",
            r.x, r.y, r.width, r.height
        )),
        TraversalEvent::PopClip => out.push("pop".into()),
    }
}

/// Every section the tree paints or hits, in order.
fn record(ui: &UIRoot) -> Vec<(&'static str, Vec<String>)> {
    let tree = &ui.tree;
    let mut paint = Vec::new();
    tree.traverse(|ev| event(&mut paint, ev));

    let mut panels = Vec::new();
    let (mut subs, mut infos) = (Vec::new(), Vec::new());
    ui.panel_cache_info_into(&mut subs, &mut infos);
    for info in &infos {
        panels.push(format!("slot {:?}", info.slot));
        tree.traverse_flat_range(info.region, info.start, info.end, false, |ev| {
            event(&mut panels, ev)
        });
        for (s, e) in info.sub_regions.into_iter().flatten() {
            panels.push(format!("sub {}", e - s));
            tree.traverse_flat_range(info.region, *s, *e, false, |ev| event(&mut panels, ev));
        }
    }

    let mut overlays = Vec::new();
    let overlay_region = MainRegion::Overlays.id();
    for &(start, end) in &ui.overlay_draw {
        overlays.push("overlay".into());
        tree.traverse_flat_range(overlay_region, start, end, false, |ev| {
            event(&mut overlays, ev)
        });
    }

    let mut ghost = Vec::new();
    if let Some(root) = ui.inspector.card_drag_ghost_root() {
        tree.traverse_range(root.region(), 0, usize::MAX, |ev| event(&mut ghost, ev));
    }

    let mut hits = Vec::new();
    let mut y = 0.0;
    while y < LOGICAL_H {
        let mut x = 0.0;
        while x < LOGICAL_W {
            let hit = tree
                .hit_test(Vec2::new(x, y))
                .and_then(|id| tree.get_node(id))
                .map(describe);
            hits.push(hit.unwrap_or_default());
            x += HIT_STEP;
        }
        y += HIT_STEP;
    }
    vec![
        ("paint", paint),
        ("panels", panels),
        ("overlays", overlays),
        ("ghost", ghost),
        ("hits", hits),
    ]
}

fn fnv(lines: &[String]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for line in lines {
        for b in line.bytes().chain(std::iter::once(b'\n')) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn summary(sections: &[(&'static str, Vec<String>)]) -> String {
    let mut out = String::new();
    for (name, lines) in sections {
        let _ = writeln!(out, "{name} {} {:016x}", lines.len(), fnv(lines));
    }
    out
}

/// The automation scene with the flowers layer's cards in the inspector and
/// every stacked surface open, built once.
fn stacked_surfaces() -> UIRoot {
    let mut data = fixtures::build("automation").expect("automation scene");
    data.active = Some(1);
    let mut ui = UIRoot::new();
    ui.resize(LOGICAL_W, LOGICAL_H);
    sync_build(&mut ui, &data, 24.0);

    ui.warmup = Some(manifold_core::WarmupProgress {
        done: 2,
        total: 5,
        label: "Flowers".into(),
    });
    ui.toast.show("Undo: Move Clip");
    let items = vec![
        manifold_ui::panels::dropdown::DropdownItem::new("Normal"),
        manifold_ui::panels::dropdown::DropdownItem::new("Add"),
        manifold_ui::panels::dropdown::DropdownItem::new("Multiply"),
    ];
    let trigger = manifold_ui::node::Rect::new(400.0, 300.0, 120.0, 20.0);
    ui.dropdown.open(items, trigger, 120.0, &mut ui.tree);
    use manifold_ui::panels::ableton_picker::{
        AbletonPickerSession, PickerDevice, PickerMacro, PickerTrack,
    };
    let session = AbletonPickerSession {
        rack_tracks: vec![PickerTrack {
            track_id: 1,
            track_name: "Drums".into(),
            devices: vec![PickerDevice {
                device_id: 7,
                device_name: "Rack".into(),
                device_class_name: "InstrumentGroupDevice".into(),
                macros: (0..4)
                    .map(|i| PickerMacro {
                        param_id: i,
                        name: format!("Macro {i}"),
                    })
                    .collect(),
            }],
        }],
    };
    ui.ableton_picker.open(session, Vec2::new(700.0, 200.0));
    ui.build();
    ui
}

/// [`stacked_surfaces`] with a hovered inspector control and a live
/// card-drag ghost.
fn stacked_ui() -> UIRoot {
    let mut ui = stacked_surfaces();
    // Hover an inspector control. The tooltip itself is an immediate pass;
    // the hover flag is what the tree paints.
    let insp = ui.layout.inspector();
    let hover = ui
        .tree
        .all_nodes()
        .find(|n| {
            n.flags.contains(UIFlags::INTERACTIVE | UIFlags::VISIBLE)
                && n.bounds.width > 2.0
                && insp.contains(Vec2::new(n.bounds.x + 1.0, n.bounds.y + 1.0))
        })
        .map(|n| n.bounds);
    let hover = hover.expect("a visible inspector control");
    ui.pointer_event(
        Vec2::new(hover.x + 1.0, hover.y + 1.0),
        PointerAction::Move,
        0.0,
    );

    // Grab an effect card by its drag handle and move the ghost.
    let ids: Vec<_> = ui
        .tree
        .all_nodes()
        .filter(|n| n.flags.contains(UIFlags::INTERACTIVE))
        .map(|n| n.id)
        .collect();
    let started = ids
        .into_iter()
        .any(|id| ui.inspector.try_begin_card_drag(Some(id), &mut ui.tree));
    assert!(started, "an effect card drag handle in the inspector");
    let insp = ui.layout.inspector();
    ui.inspector.update_card_drag(
        Vec2::new(insp.x + insp.width * 0.5, insp.y + 200.0),
        &mut ui.tree,
    );
    ui
}

fn check(sections: &[(&'static str, Vec<String>)], when: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    let got = summary(sections);
    if std::env::var_os("MANIFOLD_RECORD_ORDER_GOLDEN").is_some() {
        std::fs::write(&path, &got).expect("write order golden");
        return;
    }
    let want = std::fs::read_to_string(&path).expect("order golden recorded on the flat tree");
    if got != want {
        for (name, lines) in sections {
            let out = path.with_file_name(format!("order_golden.{name}.txt"));
            let _ = std::fs::write(out, lines.join("\n"));
        }
        panic!("paint or hit order moved ({when}):\n--- golden\n{want}--- now\n{got}");
    }
}

#[test]
fn paint_and_hit_order_match_full_build() {
    let ui = stacked_ui();
    let sections = record(&ui);
    for (name, lines) in &sections {
        assert!(!lines.is_empty(), "{name} recorded nothing");
    }
    check(&sections, "full build");
}

fn widget_ids(ui: &UIRoot) -> Vec<WidgetId> {
    ui.tree
        .all_nodes()
        .map(|n| ui.tree.widget_of(n.id))
        .collect()
}

/// Each main region rebuilt alone through the frame seam reproduces the full
/// build's paint and hit order and every widget id, its own and the other
/// regions' (INV-14, D7).
#[test]
fn each_region_rebuilt_alone_matches_full_build() {
    let mut ui = stacked_surfaces();
    let want = summary(&record(&ui));
    let want_ids = widget_ids(&ui);
    for region in MainRegion::ALL {
        let mut rebuild = RegionMask::default();
        rebuild.insert(region);
        let mut signals = UiFrameSignals {
            rebuild,
            ..Default::default()
        };
        apply_ui_frame_invalidations(&mut ui, None, &mut signals);
        assert_eq!(
            summary(&record(&ui)),
            want,
            "{region:?} rebuilt alone moved paint or hit order"
        );
        assert!(
            widget_ids(&ui) == want_ids,
            "{region:?} rebuilt alone changed a widget id"
        );
    }
}

/// Widget ids are unique across every region's arena of a real main-window
/// build (INV-18): press, hover and focus resolve through one map.
#[test]
fn widget_ids_unique_across_main_regions() {
    let ui = stacked_ui();
    let mut seen = std::collections::HashSet::new();
    for n in ui.tree.all_nodes() {
        let id = ui.tree.widget_of(n.id);
        assert!(
            seen.insert(id),
            "widget id {id:?} minted twice (node {:?})",
            n.id
        );
    }
}

/// Every node of an atlas region paints into the atlas: each lies in one
/// panel range, except region roots (which paint nothing).
#[test]
fn atlas_ranges_cover_every_region_node() {
    let ui = stacked_ui();
    let (mut subs, mut infos) = (Vec::new(), Vec::new());
    ui.panel_cache_info_into(&mut subs, &mut infos);
    for region in MainRegion::ALL
        .into_iter()
        .filter(|r| r.panel_slot().is_some())
    {
        let id = region.id();
        for slot in 0..ui.tree.region_len(id) {
            let covering = infos
                .iter()
                .filter(|i| i.region == id && (i.start..i.end).contains(&slot))
                .count();
            let node = ui.tree.id_in(id, slot);
            if ui.tree.parent_of(node).is_none() {
                continue;
            }
            assert_eq!(
                covering, 1,
                "{region:?} slot {slot} lies in {covering} panel ranges"
            );
        }
    }
}
