//! Phase 2a.5 — prove the Chrome API on param-card-shaped UI.
//!
//! `param_card` is the most interaction-dense panel in the app: a header
//! (drag handle, name, badges, ON/OFF), N parameter rows (label, slider, value,
//! D/E buttons), and per-param drawers that change the card's structure. This
//! test reconstructs that shape on the Chrome API and asserts the four
//! properties the live Phase-2b cutover will rely on:
//!
//!  1. a value-only change reconciles in place (ids stable, no rebuild),
//!  2. a structural change (open a drawer) reports `NeedsRebuild`,
//!  3. intents populate from the description and fold up correctly,
//!  4. `validate` catches an unwired control at build.
//!
//! The live `param_card` rewrite-and-delete is the first task of Phase 2b (it
//! needs a runtime visual pass); this proves the foundation is correct on the
//! hardest real card shape first. See `docs/CHROME_API_DESIGN.md`.

use manifold_ui::chrome::{validate, Align, ChromeHost, Pad, Reconcile, Sizing, View};
use manifold_ui::intent::{Gesture, IntentRegistry};
use manifold_ui::node::{Color32, FontWeight, Rect, UIFlags, UINodeType, Vec2};
use manifold_ui::text::TextMeasure;
use manifold_ui::tree::{UITree, ZTier};
use manifold_ui::{GraphParamTarget, PanelAction};
use manifold_foundation::ParamId;

// ── Layout constants (param-card feel) ──────────────────────────────
const CARD_PAD: f32 = 4.0;
const HEADER_H: f32 = 28.0;
const ROW_H: f32 = 22.0;
const TRACK_H: f32 = 18.0;
const DRAWER_BTN_H: f32 = 16.0;
const LABEL_W: f32 = 56.0;
const VALUE_W: f32 = 40.0;
const BTN: f32 = 18.0;
const BADGE_W: f32 = 30.0;

// ── Proof card model ────────────────────────────────────────────────

struct ProofParam {
    label: &'static str,
    id: &'static str,
    value: f32,
    driver_open: bool,
}

struct ProofCard {
    effect_index: usize,
    name: String,
    enabled: bool,
    has_drv: bool,
    params: Vec<ProofParam>,
    host: ChromeHost,
}

impl ProofCard {
    fn target(&self) -> GraphParamTarget {
        GraphParamTarget::Effect(self.effect_index)
    }

    /// The whole card, described once. This is the method a migrated panel
    /// writes; `build`/`update` both feed it through the host.
    fn view(&self) -> View {
        let target = self.target();
        let mut col = View::column(2.0)
            .pad(Pad::all(CARD_PAD))
            .bg(Color32::new(28, 28, 30, 255))
            .claims_area()
            .on_right_click(PanelAction::CardRightClicked(target.clone()))
            .child(self.header_view());
        for (pi, p) in self.params.iter().enumerate() {
            col = col.child(self.param_row_view(target.clone(), pi, p));
        }
        col
    }

    fn header_view(&self) -> View {
        View::row(4.0)
            .h(Sizing::Fixed(HEADER_H))
            .cross_align(Align::Center)
            // Drag handle — decorative; the reorder drag lives in handle_event.
            .child(View::panel().fixed(14.0, 14.0).bg(Color32::new(80, 80, 84, 255)))
            // Name fills the gap and clips so a long name never spills into badges.
            .child(
                View::label(self.name.clone())
                    .fill_w()
                    .h(Sizing::Fixed(16.0))
                    .clip(),
            )
            // DRV badge — emitted always, shown only when armed (in-place toggle).
            .child(
                View::label("DRV")
                    .fixed(BADGE_W, 14.0)
                    .font(8)
                    .bg(Color32::new(70, 120, 90, 255))
                    .radius(7.0)
                    .visible(self.has_drv),
            )
            .child(
                View::button(if self.enabled { "ON" } else { "OFF" })
                    .fixed(36.0, 18.0)
                    .on_click(PanelAction::EffectToggle(self.effect_index)),
            )
    }

    fn param_row_view(&self, target: GraphParamTarget, _pi: usize, p: &ProofParam) -> View {
        let pid: ParamId = ParamId::from(p.id);
        let mut row_col = View::column(0.0)
            .claims_area()
            .on_right_click(PanelAction::ParamLabelRightClick(target.clone(), pid.clone()))
            .child(
                View::row(4.0)
                    .h(Sizing::Fixed(ROW_H))
                    .cross_align(Align::Center)
                    .child(View::label(p.label).w(Sizing::Fixed(LABEL_W)).h(Sizing::Fixed(16.0)))
                    // Slider drag lives in handle_event → inert (no intent needed).
                    .child(View::slider().fill_w().h(Sizing::Fixed(TRACK_H)).inert())
                    .child(
                        View::label(format!("{:.2}", p.value))
                            .w(Sizing::Fixed(VALUE_W))
                            .h(Sizing::Fixed(16.0))
                            .align_text(manifold_ui::node::TextAlign::Right),
                    )
                    .child(
                        View::button("D")
                            .fixed(BTN, BTN)
                            .on_click(PanelAction::DriverToggle(target.clone(), pid.clone())),
                    )
                    .child(
                        View::button("E")
                            .fixed(BTN, BTN)
                            .on_click(PanelAction::EnvelopeToggle(target.clone(), pid.clone())),
                    ),
            );
        if p.driver_open {
            row_col = row_col.child(self.driver_drawer_view(target, pid));
        }
        row_col
    }

    fn driver_drawer_view(&self, target: GraphParamTarget, pid: ParamId) -> View {
        use manifold_ui::DriverConfigAction;
        View::row(2.0)
            .h(Sizing::Fixed(DRAWER_BTN_H + 4.0))
            .pad(Pad::xy(2.0, 2.0))
            .child(
                View::button("1/4")
                    .fixed(28.0, DRAWER_BTN_H)
                    .on_click(PanelAction::DriverConfig(
                        target.clone(),
                        pid.clone(),
                        DriverConfigAction::BeatDiv(2),
                    )),
            )
            .child(
                View::button("sine")
                    .fixed(34.0, DRAWER_BTN_H)
                    .on_click(PanelAction::DriverConfig(
                        target,
                        pid,
                        DriverConfigAction::Wave(0),
                    )),
            )
    }

    fn build(&mut self, tree: &mut UITree, rect: Rect) {
        let v = self.view();
        // D4: the host mints a root-parented card subtree, so build it inside a
        // region bracket (`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D1/D4). The region
        // container is minted directly on the tree — NOT through the host — so
        // `host.node_id`/`node_count` and the DFS indices these tests assert on
        // are unaffected (the host bases off `tree.count()` at build start). The
        // region rect == the card rect, so the clip is a no-op.
        let region = tree.begin_region(rect, ZTier::Base, "proof_card", UIFlags::empty());
        let start = tree.count();
        self.host.build(tree, &v, rect);
        tree.end_region(region, start);
    }

    fn update(&mut self, tree: &mut UITree, rect: Rect) -> Reconcile {
        let v = self.view();
        self.host.update(tree, &v, rect)
    }
}

// ── Deterministic measurer ──────────────────────────────────────────
struct Mono;
impl TextMeasure for Mono {
    fn measure_text(&self, text: &str, _s: u16, _w: FontWeight) -> Vec2 {
        Vec2::new(text.chars().count() as f32 * 7.0, 14.0)
    }
}

fn fixture(driver_open: bool) -> ProofCard {
    ProofCard {
        effect_index: 3,
        name: "Gaussian Blur".to_string(),
        enabled: true,
        has_drv: false,
        params: vec![
            ProofParam { label: "Radius", id: "blur.radius", value: 1.0, driver_open },
            ProofParam { label: "Sigma", id: "blur.sigma", value: 0.5, driver_open: false },
        ],
        host: ChromeHost::new(),
    }
}

fn tree() -> UITree {
    let mut t = UITree::new();
    t.set_text_measure(Box::new(Mono));
    t
}

fn rect() -> Rect {
    Rect::new(10.0, 20.0, 240.0, 200.0)
}

// ── Tests ───────────────────────────────────────────────────────────

#[test]
fn build_matches_card_structure() {
    let mut t = tree();
    let mut card = fixture(false);
    card.build(&mut t, rect());

    // card(1) + header(row + handle + name + drv + toggle = 5)
    //         + 2 param rows (col + sliderrow + label + slider + value + D + E = 7 each)
    assert_eq!(card.host.node_count(), 1 + 5 + 7 + 7);

    // The card root fills the given rect.
    let root = card.host.node_id(0).unwrap();
    assert_eq!(t.get_bounds(root), rect());

    // Header is a row of fixed height, padded inside the card.
    let header = card.host.node_id(1).unwrap();
    let hb = t.get_bounds(header);
    assert_eq!(hb.x, rect().x + CARD_PAD);
    assert_eq!(hb.height, HEADER_H);

    // Toggle button (DFS index 5) carries "ON" and is interactive.
    let toggle = card.host.node_id(5).unwrap();
    assert_eq!(t.get_node(toggle).unwrap().node_type, UINodeType::Button);
    assert_eq!(t.get_node(toggle).unwrap().text.as_deref(), Some("ON"));

    // The DRV badge (index 4) is emitted but hidden (has_drv = false).
    let badge = card.host.node_id(4).unwrap();
    assert!(!t.has_flag(badge, manifold_ui::node::UIFlags::VISIBLE));

    // First param's value label (index 10) reads "1.00".
    let value0 = card.host.node_id(10).unwrap();
    assert_eq!(t.get_node(value0).unwrap().text.as_deref(), Some("1.00"));
}

#[test]
fn value_change_reconciles_in_place() {
    let mut t = tree();
    let mut card = fixture(false);
    card.build(&mut t, rect());
    let count = t.count();
    let sv = t.structure_version();
    let slider0 = card.host.node_id(9).unwrap();
    let value0 = card.host.node_id(10).unwrap();

    // A new value — structure identical.
    card.params[0].value = 2.5;
    let outcome = card.update(&mut t, rect());

    assert_eq!(outcome, Reconcile::Updated);
    assert_eq!(t.count(), count, "no nodes added/removed");
    assert_eq!(
        t.structure_version(),
        sv,
        "in-place update must not bump structure_version — slider drag + intents stay valid"
    );
    // Same node id, new text.
    assert_eq!(card.host.node_id(9), Some(slider0));
    assert_eq!(t.get_node(value0).unwrap().text.as_deref(), Some("2.50"));
}

#[test]
fn badge_toggle_is_in_place() {
    // Arming a driver flips the badge's visibility — emitted-but-hidden means
    // this is an in-place update, not a structural rebuild.
    let mut t = tree();
    let mut card = fixture(false);
    card.build(&mut t, rect());
    let badge = card.host.node_id(4).unwrap();
    assert!(!t.has_flag(badge, manifold_ui::node::UIFlags::VISIBLE));

    card.has_drv = true;
    assert_eq!(card.update(&mut t, rect()), Reconcile::Updated);
    assert!(t.has_flag(badge, manifold_ui::node::UIFlags::VISIBLE));
}

#[test]
fn opening_drawer_needs_rebuild_then_grows() {
    let mut t = tree();
    let mut card = fixture(false);
    card.build(&mut t, rect());
    let count_before = t.count();

    // Open param 0's driver drawer — adds nodes → structural.
    card.params[0].driver_open = true;
    assert_eq!(card.update(&mut t, rect()), Reconcile::NeedsRebuild);
    assert_eq!(t.count(), count_before, "NeedsRebuild leaves the tree untouched");

    // The app re-runs build() for the panel range (here: fresh tree).
    let mut t2 = tree();
    let mut card2 = fixture(true);
    card2.build(&mut t2, rect());
    // +3 nodes: drawer row + two drawer buttons.
    assert_eq!(card2.host.node_count(), 1 + 5 + 10 + 7);
}

#[test]
fn intents_resolve_and_fold_up() {
    let mut t = tree();
    let mut card = fixture(false);
    card.build(&mut t, rect());

    let mut reg = IntentRegistry::new();
    card.host.register_intents(&mut reg);

    // Click the ON/OFF toggle → EffectToggle(3).
    let toggle = card.host.node_id(5).unwrap();
    assert!(matches!(
        reg.resolve(&t, Some(toggle), Gesture::Click),
        Some(PanelAction::EffectToggle(3))
    ));

    // Click the D button → DriverToggle for param 0.
    let d_btn = card.host.node_id(11).unwrap();
    assert!(matches!(
        reg.resolve(&t, Some(d_btn), Gesture::Click),
        Some(PanelAction::DriverToggle(GraphParamTarget::Effect(3), _))
    ));

    // Right-click the inert slider folds up to the param row's menu, not the card.
    let slider0 = card.host.node_id(9).unwrap();
    assert!(matches!(
        reg.resolve(&t, Some(slider0), Gesture::RightClick),
        Some(PanelAction::ParamLabelRightClick(GraphParamTarget::Effect(3), _))
    ));

    // Right-click the header drag handle (no row claim above it) folds to the card.
    let handle = card.host.node_id(2).unwrap();
    assert!(matches!(
        reg.resolve(&t, Some(handle), Gesture::RightClick),
        Some(PanelAction::CardRightClicked(GraphParamTarget::Effect(3)))
    ));

    // Left-click the slider is absorbed by the row's claim (no click intent there).
    assert!(reg.resolve(&t, Some(slider0), Gesture::Click).is_none());
}

#[test]
fn validate_catches_unwired_control() {
    // A card view with a stray button that has no intent and isn't inert.
    let bad = View::column(2.0)
        .child(View::button("ghost")) // unwired
        .child(View::slider().inert());
    let warnings = validate(&bad);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("ghost"));

    // The real card view is clean.
    let card = fixture(true);
    assert!(validate(&card.view()).is_empty());
}
