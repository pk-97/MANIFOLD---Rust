//! The reconciler — applies a [`View`] description to the [`UITree`], deciding
//! per frame whether it is a fresh structural build or an in-place update.
//!
//! A [`ChromeHost`] is a panel field. The panel writes one `view()` method;
//! `build()` and `update()` both feed that description here. This collapses the
//! old build-then-sync dual write: there is one description, and the host emits
//! the minimal mutations.
//!
//! ## Build vs update vs rebuild
//!
//! - **[`ChromeHost::build`]** appends fresh nodes at the tree tail (the panel
//!   contract: `build()` runs only when the tree is truncated to this panel's
//!   start). It records the assigned [`NodeId`]s and a structural signature.
//! - **[`ChromeHost::update`]** solves the new description and compares its
//!   signature to the retained one. Same structure → in-place `set_*` on the
//!   retained ids (no `add_node`, no `structure_version` bump, so drag state and
//!   intents survive). Different structure → returns [`Reconcile::NeedsRebuild`]
//!   and touches nothing; the app re-runs `build()` for the affected range, the
//!   same way it already handles a collapse or drawer toggle.

#[cfg(test)]
use crate::TransportAction;
use crate::chrome::layout::{self, LaidNode};
use crate::chrome::view::View;
#[cfg(debug_assertions)]
use crate::chrome::view::validate;
use crate::intent::{Gesture, IntentRegistry};
use crate::node::{NodeId, Rect, UIFlags};
use crate::panels::PanelAction;
use crate::slider::{BitmapSlider, SliderNodeIds};
use crate::tree::UITree;

/// Outcome of an [`ChromeHost::update`] reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconcile {
    /// Structure matched; nodes updated in place (ids preserved).
    Updated,
    /// Structure changed; caller must re-run [`ChromeHost::build`] for this
    /// panel's range. The tree was left untouched.
    NeedsRebuild,
}

/// Materialise a [`View`] into `tree` at `rect` once, with no retained reconcile
/// state — for full-rebuild panels (e.g. the inspector's scroll columns) that
/// re-emit their whole subtree every frame and so can't carry a stateful
/// [`ChromeHost`]. Returns the `(key, id)` pairs for nodes that set a
/// [`View::key`], letting the caller recover an interactive node id (e.g. a
/// button) without hand-rolling `add_node`. Slider slots are not built here;
/// a caller with a [`View::slider_row`] uses [`materialize_sliders`].
pub fn materialize(tree: &mut UITree, root: &View, rect: Rect) -> Vec<(u64, NodeId)> {
    materialize_sliders(tree, root, rect).0
}

/// Nodes that set a [`View::key`], by key.
pub type KeyedNodes = Vec<(u64, NodeId)>;
/// Materialised slider ids, by their slot's key.
pub type KeyedSliders = Vec<(u64, SliderNodeIds)>;

/// [`materialize`] that also builds each [`View::slider_row`] slot, returning
/// the keyed slider ids. The caller owns the value and drag through a
/// `SliderDragState`, the same split as [`ChromeHost`].
pub fn materialize_sliders(
    tree: &mut UITree,
    root: &View,
    rect: Rect,
) -> (KeyedNodes, KeyedSliders) {
    #[cfg(debug_assertions)]
    {
        let warnings = validate(root);
        debug_assert!(
            warnings.is_empty(),
            "Chrome validation failed:\n{}",
            warnings.join("\n")
        );
    }

    let mut scratch: Vec<LaidNode> = Vec::new();
    layout::solve_into(root, rect, tree.measurer(), &mut scratch);

    let mut ids: Vec<NodeId> = Vec::with_capacity(scratch.len());
    let mut keyed: Vec<(u64, NodeId)> = Vec::new();
    for n in &scratch {
        // Parents always precede their children in laid order, so `ids[p]` is set.
        let parent_id = n.parent.map(|p| ids[p]);
        let mut extra = UIFlags::empty();
        if n.interactive {
            extra |= UIFlags::INTERACTIVE;
        }
        if n.clips {
            extra |= UIFlags::CLIPS_CHILDREN;
        }
        if n.disabled {
            extra |= UIFlags::DISABLED;
        }
        // `View::identity` (opt-in, distinct from the lookup-only `key`)
        // pins the node's durable WidgetId — reorder-stable identity for
        // roots whose siblings can shift (card roots, D4). Plain `key`
        // deliberately does NOT salt the WidgetId: keys are only
        // sibling-unique per host, and hosts can share a tree parent.
        let id = match n.identity {
            Some(k) => tree.add_node_keyed(
                parent_id,
                n.rect,
                n.kind,
                n.style,
                n.text.as_deref(),
                extra,
                k,
            ),
            None => tree.add_node(parent_id, n.rect, n.kind, n.style, n.text.as_deref(), extra),
        };
        if !n.visible {
            tree.set_visible(id, false);
        }
        if let Some(name) = n.name {
            tree.set_name(id, name);
        }
        if let Some(k) = n.key {
            keyed.push((k, id));
        }
        ids.push(id);
    }
    let mut sliders: Vec<(u64, SliderNodeIds)> = Vec::new();
    for n in &scratch {
        let (Some(spec), Some(key)) = (n.slider.as_ref(), n.key) else {
            continue;
        };
        let built = BitmapSlider::build(
            tree,
            None,
            n.rect,
            spec.label.as_deref(),
            spec.value,
            &spec.value_text,
            &spec.colors,
            spec.font_size,
            spec.label_width,
            spec.default,
            spec.reset.clone(),
            None,
        );
        sliders.push((key, built.ids));
    }
    (keyed, sliders)
}

/// Retained reconciliation state for one panel built on the Chrome API.
#[derive(Default)]
pub struct ChromeHost {
    /// Node ids assigned during build, in DFS pre-order. Empty until built.
    ids: Vec<NodeId>,
    /// The laid tree currently realised in the [`UITree`] — drives intent
    /// population and is the structure the next update diffs against.
    laid: Vec<LaidNode>,
    /// Reusable solve buffer, swapped with `laid` each frame (no per-frame alloc).
    scratch: Vec<LaidNode>,
    /// Index of the first node in the tree (== `ids[0].index()` when built).
    first_node: usize,
    signature: u64,
    built: bool,
    /// Sliders materialised at build, by their slot's [`View::key`], plus each
    /// slider's right-click reset action. The panel resolves the ids to drive
    /// the slider's value + drag through its `SliderDragState`; the host owns
    /// the slider's structure, not its value. The reset travels alongside so
    /// [`Self::register_slider_resets`] can replay every one without a panel
    /// having to re-derive it.
    slider_ids: Vec<(u64, SliderNodeIds, PanelAction)>,
}

impl ChromeHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fresh structural build at the current tree tail. Records ids + signature.
    /// Runs [`validate`] in debug — an unwired control fails here.
    pub fn build(&mut self, tree: &mut UITree, root: &View, rect: Rect) {
        self.build_under(tree, root, rect, None);
    }

    /// Materialize beneath an existing parent, preserving inherited widget identity.
    pub fn build_under(
        &mut self,
        tree: &mut UITree,
        root: &View,
        rect: Rect,
        parent: Option<NodeId>,
    ) {
        layout::solve_into(root, rect, tree.measurer(), &mut self.scratch);

        #[cfg(debug_assertions)]
        {
            let warnings = validate(root);
            debug_assert!(
                warnings.is_empty(),
                "Chrome validation failed:\n{}",
                warnings.join("\n")
            );
        }

        self.first_node = tree.count();
        self.ids.clear();
        self.ids.reserve(self.scratch.len());
        for i in 0..self.scratch.len() {
            let parent_id = self.scratch[i].parent.map(|p| self.ids[p]).or(parent);
            let id = {
                let n = &self.scratch[i];
                let mut extra = UIFlags::empty();
                if n.interactive {
                    extra |= UIFlags::INTERACTIVE;
                }
                if n.clips {
                    extra |= UIFlags::CLIPS_CHILDREN;
                }
                if n.disabled {
                    extra |= UIFlags::DISABLED;
                }
                // `View::identity` pins the durable WidgetId (D4 card-root
                // identity) — see the materialize loop's twin note above.
                match n.identity {
                    Some(k) => tree.add_node_keyed(
                        parent_id,
                        n.rect,
                        n.kind,
                        n.style,
                        n.text.as_deref(),
                        extra,
                        k,
                    ),
                    None => {
                        tree.add_node(parent_id, n.rect, n.kind, n.style, n.text.as_deref(), extra)
                    }
                }
            };
            if !self.scratch[i].visible {
                tree.set_visible(id, false);
            }
            if let Some(name) = self.scratch[i].name {
                tree.set_name(id, name);
            }
            self.ids.push(id);
        }

        // Materialise any declarative sliders into their laid slot rects. The
        // slot node above is a transparent placeholder; the BitmapSlider's
        // (interactive) sub-nodes are appended on top and tracked by key.
        self.slider_ids.clear();
        for i in 0..self.scratch.len() {
            let Some(spec) = self.scratch[i].slider.clone() else {
                continue;
            };
            let rect = self.scratch[i].rect;
            let key = self.scratch[i].key;
            let built = BitmapSlider::build(
                tree,
                None,
                rect,
                spec.label.as_deref(),
                spec.value,
                &spec.value_text,
                &spec.colors,
                spec.font_size,
                spec.label_width,
                spec.default,
                spec.reset.clone(),
                None,
            );
            if let Some(k) = key {
                self.slider_ids.push((k, built.ids, built.reset));
            }
        }

        self.signature = signature(&self.scratch);
        std::mem::swap(&mut self.laid, &mut self.scratch);
        self.built = true;
    }

    /// In-place update if the structure is unchanged; otherwise
    /// [`Reconcile::NeedsRebuild`] with the tree left untouched.
    pub fn update(&mut self, tree: &mut UITree, root: &View, rect: Rect) -> Reconcile {
        manifold_foundation::work_count::record_work(
            manifold_foundation::work_count::WorkKind::ChromeLayout,
        );
        layout::solve_into(root, rect, tree.measurer(), &mut self.scratch);

        if !self.built
            || self.scratch.len() != self.ids.len()
            || signature(&self.scratch) != self.signature
        {
            return Reconcile::NeedsRebuild;
        }

        for i in 0..self.ids.len() {
            let id = self.ids[i];
            let n = &self.scratch[i];
            tree.set_bounds(id, n.rect);
            if let Some(t) = &n.text {
                tree.set_text(id, t);
            }
            tree.set_style(id, n.style);
            tree.set_visible(id, n.visible);
            if n.disabled {
                tree.set_flag(id, UIFlags::DISABLED);
            } else {
                tree.clear_flag(id, UIFlags::DISABLED);
            }
        }
        // A materialised slider sits on top of its slot, outside the laid
        // nodes; it moves with the slot. Holders re-read `slider_ids`.
        for i in 0..self.scratch.len() {
            let (Some(spec), Some(key)) = (self.scratch[i].slider.as_ref(), self.scratch[i].key)
            else {
                continue;
            };
            let rect = self.scratch[i].rect;
            if self.laid[i].rect == rect {
                continue;
            }
            if let Some((_, ids, _)) = self.slider_ids.iter_mut().find(|(k, _, _)| *k == key) {
                BitmapSlider::relayout(tree, ids, rect, spec.label_width, spec.value);
            }
        }

        std::mem::swap(&mut self.laid, &mut self.scratch);
        Reconcile::Updated
    }

    /// Populate the intent registry from the retained description. Delegate
    /// target for a panel's `register_intents`.
    pub fn register_intents(&self, reg: &mut IntentRegistry) {
        for (i, node) in self.laid.iter().enumerate() {
            let id = self.ids[i];
            if node.intent.claims_area {
                reg.claim_area(id);
            }
            if let Some(a) = &node.intent.click {
                reg.on(id, Gesture::Click, a.clone());
            }
            if let Some(a) = &node.intent.double_click {
                reg.on(id, Gesture::DoubleClick, a.clone());
            }
            if let Some(a) = &node.intent.right_click {
                reg.on(id, Gesture::RightClick, a.clone());
            }
        }
    }

    /// Return the retained click intent for a materialised node.
    pub fn click_action(&self, node: NodeId) -> Option<&PanelAction> {
        let i = node.slot().checked_sub(self.first_node)?;
        (self.ids.get(i) == Some(&node))
            .then(|| self.laid.get(i)?.intent.click.as_ref())
            .flatten()
    }

    // ── Accessors (Panel trait glue) ────────────────────────────────

    pub fn is_built(&self) -> bool {
        self.built
    }

    pub fn first_node(&self) -> usize {
        if self.built {
            self.first_node
        } else {
            usize::MAX
        }
    }

    pub fn node_count(&self) -> usize {
        self.ids.len()
    }

    /// Node id at DFS index `i` (the order views were described in).
    pub fn node_id(&self, i: usize) -> Option<NodeId> {
        self.ids.get(i).copied()
    }

    /// Resolve the tree node id of the first node carrying `key` (set via
    /// [`View::key`](crate::chrome::view::View::key)). The stable way for a
    /// panel to hand a specific element to overlay anchoring without hoarding a
    /// `self.*_id` field — the id survives in-place updates and is re-resolved
    /// after a rebuild. O(n) over this panel's nodes; called on interaction, not
    /// per frame.
    pub fn node_id_for_key(&self, key: u64) -> Option<NodeId> {
        self.laid
            .iter()
            .position(|n| n.key == Some(key))
            .and_then(|i| self.ids.get(i).copied())
    }

    /// Write the text of the node built under `key` in place, without laying
    /// the panel out: a live value in a fixed-size slot, like a slider's
    /// value. The panel keeps the value its next description shows, so a
    /// later update or rebuild writes the same text. No-op before the build
    /// or for an unknown key. Allocates nothing once the text has its
    /// length.
    pub fn set_text_by_key(&mut self, tree: &mut UITree, key: u64, text: &str) {
        if !self.built {
            return;
        }
        let Some(i) = self.laid.iter().position(|n| n.key == Some(key)) else {
            return;
        };
        let laid = &mut self.laid[i].text;
        if laid.as_deref() == Some(text) {
            return;
        }
        match laid {
            Some(t) => {
                t.clear();
                t.push_str(text);
            }
            None => *laid = Some(text.to_owned()),
        }
        tree.set_text(self.ids[i], text);
    }

    /// The materialised [`BitmapSlider`] ids for the slot built under `key`
    /// (set on the [`View::slider_row`]). A panel hands these to its
    /// `SliderDragState` to drive the value + drag.
    pub fn slider_ids(&self, key: u64) -> Option<SliderNodeIds> {
        self.slider_ids
            .iter()
            .find(|(k, _, _)| *k == key)
            .map(|(_, ids, _)| *ids)
    }

    /// Register every materialised slider's right-click reset. Panels delegate
    /// here instead of hand-registering slider tracks (BUG-061 follow-through,
    /// BUG-070) — the host owns the full set of slots it materialised, so this
    /// replay can't skip one the way a panel's own hand-written loop could.
    /// Walks the contract via [`BitmapSlider::register_track_reset`]
    /// instead of hand-emitting the
    /// `Gesture::RightClick` registration itself.
    pub fn register_slider_resets(&self, reg: &mut IntentRegistry) {
        for (_key, ids, reset) in &self.slider_ids {
            BitmapSlider::register_track_reset(ids, reset, reg);
        }
    }
}

/// FNV-1a step.
#[inline]
fn fnv(h: u64, x: u64) -> u64 {
    (h ^ x).wrapping_mul(0x100000001b3)
}

/// Structural fingerprint: node count + per-node (kind, parent, interactive,
/// clips, intent *shape*). Excludes bounds / text / style / visibility / intent
/// *payload* — those are in-place updatable. Adding or removing a node, changing
/// a kind, or changing which gestures a node carries all force a rebuild (the
/// registry and tree shape must change); a value, color, or visibility change
/// does not.
fn signature(laid: &[LaidNode]) -> u64 {
    let mut h = fnv(0xcbf29ce484222325, laid.len() as u64);
    for n in laid {
        h = fnv(h, n.kind as u64);
        h = fnv(h, n.parent.map(|p| p as u64).unwrap_or(u64::MAX));
        h = fnv(h, n.interactive as u64);
        h = fnv(h, n.clips as u64);
        let shape = (n.intent.click.is_some() as u64)
            | ((n.intent.double_click.is_some() as u64) << 1)
            | ((n.intent.right_click.is_some() as u64) << 2)
            | ((n.intent.claims_area as u64) << 3);
        h = fnv(h, shape);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::view::Sizing;
    use crate::node::{FontWeight, UINodeType, Vec2};
    use crate::panels::PanelAction;
    use crate::text::TextMeasure;

    struct Mono;
    impl TextMeasure for Mono {
        fn measure_text(&self, text: &str, _s: u16, _w: FontWeight) -> Vec2 {
            Vec2::new(text.chars().count() as f32 * 10.0, 16.0)
        }
    }

    fn tree() -> UITree {
        let mut t = UITree::new();
        t.set_text_measure(Box::new(Mono));
        t
    }

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 200.0, 100.0)
    }

    // A simple card: column(bg) → [ label(value), button ].
    fn card(value: &str) -> View {
        View::column(2.0)
            .bg(crate::node::Color32::new(20, 20, 20, 255))
            .child(View::label(value).fill_w().h(Sizing::Fixed(16.0)))
            .child(
                View::button("OK")
                    .fixed(40.0, 18.0)
                    .on_click(PanelAction::Transport(TransportAction::Stop)),
            )
    }

    #[test]
    fn build_assigns_contiguous_ids_from_tail() {
        let mut t = tree();
        // Pre-existing node so the panel starts mid-tree.
        t.add_panel(None, 0.0, 0.0, 10.0, 10.0, crate::node::UIStyle::default());

        let mut host = ChromeHost::new();
        host.build(&mut t, &card("1.0"), rect());

        assert_eq!(host.first_node(), 1);
        assert_eq!(host.node_count(), 3); // column, label, button
        assert_eq!(host.node_id(0).map(|n| n.slot()), Some(1));
        assert_eq!(host.node_id(2).map(|n| n.slot()), Some(3));
        assert_eq!(t.count(), 4);
    }

    #[test]
    fn value_change_updates_in_place() {
        let mut t = tree();
        let mut host = ChromeHost::new();
        host.build(&mut t, &card("1.0"), rect());
        let count_after_build = t.count();
        let label_id = host.node_id(1).unwrap();
        let sv = t.structure_version();

        let outcome = host.update(&mut t, &card("2.5"), rect());

        assert_eq!(outcome, Reconcile::Updated);
        assert_eq!(t.count(), count_after_build, "no nodes added");
        assert_eq!(
            t.structure_version(),
            sv,
            "in-place update must not bump structure_version (ids/intents stay valid)"
        );
        assert_eq!(t.get_node(label_id).unwrap().text.as_deref(), Some("2.5"));
    }

    #[test]
    fn shape_change_reports_a_rebuild_without_mutating() {
        let mut t = tree();
        let mut host = ChromeHost::new();
        host.build(&mut t, &card("1.0"), rect());
        let count = t.count();
        let sv = t.structure_version();

        // One extra child → different structure.
        let bigger = card("1.0").child(View::label("extra"));
        let outcome = host.update(&mut t, &bigger, rect());

        assert_eq!(outcome, Reconcile::NeedsRebuild);
        assert_eq!(t.count(), count, "tree untouched on NeedsRebuild");
        assert_eq!(t.structure_version(), sv);
    }

    #[test]
    fn intent_shape_change_forces_rebuild() {
        let mut t = tree();
        let mut host = ChromeHost::new();
        host.build(&mut t, &card("1.0"), rect());

        // Same shape, but the button loses its click intent → registry must change.
        let no_intent = View::column(2.0)
            .child(View::label("1.0").fill_w().h(Sizing::Fixed(16.0)))
            .child(View::button("OK").fixed(40.0, 18.0).inert());
        assert_eq!(
            host.update(&mut t, &no_intent, rect()),
            Reconcile::NeedsRebuild
        );
    }

    #[test]
    fn intents_populate_from_description() {
        let mut t = tree();
        let mut host = ChromeHost::new();
        host.build(&mut t, &card("1.0"), rect());

        let mut reg = IntentRegistry::new();
        host.register_intents(&mut reg);

        let button_id = host.node_id(2).unwrap();
        let action = reg.resolve(&t, Some(button_id), Gesture::Click);
        assert!(matches!(
            action,
            Some(PanelAction::Transport(TransportAction::Stop))
        ));
    }

    #[test]
    fn intents_reject_node_id_after_region_rebuild() {
        let mut t = tree();
        let mut host = ChromeHost::new();
        host.build(&mut t, &card("1.0"), rect());
        let old_button = host.node_id(2).unwrap();

        t.clear_region(old_button.region());
        host.build(&mut t, &card("2.0"), rect());

        let mut reg = IntentRegistry::new();
        host.register_intents(&mut reg);
        assert!(reg.resolve(&t, Some(old_button), Gesture::Click).is_none());
    }

    #[test]
    fn hidden_node_is_emitted_but_invisible() {
        let mut t = tree();
        let mut host = ChromeHost::new();
        let v = View::column(0.0)
            .child(View::label("shown").h(Sizing::Fixed(16.0)))
            .child(View::label("gone").h(Sizing::Fixed(16.0)).hidden());
        host.build(&mut t, &v, rect());

        let hidden_id = host.node_id(2).unwrap();
        assert!(!t.has_flag(hidden_id, UIFlags::VISIBLE));
        // Toggling visibility is an in-place update (same structure).
        let shown = View::column(0.0)
            .child(View::label("shown").h(Sizing::Fixed(16.0)))
            .child(View::label("gone").h(Sizing::Fixed(16.0)));
        assert_eq!(host.update(&mut t, &shown, rect()), Reconcile::Updated);
        assert!(t.has_flag(hidden_id, UIFlags::VISIBLE));
    }

    // A row whose slider slot starts after a label of variable width.
    fn slider_row(name: &str) -> View {
        let spec = crate::chrome::view::SliderSpec {
            label: Some("Opacity".to_string()),
            value: 0.5,
            default: 1.0,
            value_text: "0.50".to_string(),
            colors: crate::slider::SliderColors::default_slider(),
            font_size: 12,
            label_width: 40.0,
            reset: PanelAction::Transport(TransportAction::Stop),
        };
        View::row(4.0)
            .fill_w()
            .h(Sizing::Fixed(20.0))
            .child(View::label(name))
            .child(
                View::slider_row(spec)
                    .fill_w()
                    .h(Sizing::Fixed(20.0))
                    .key(7),
            )
    }

    #[test]
    fn an_in_place_update_moves_a_slider_with_its_slot() {
        let bounds = |t: &UITree, ids: SliderNodeIds| {
            [
                ids.label.map(|l| t.get_bounds(l)),
                Some(t.get_bounds(ids.track)),
                Some(t.get_bounds(ids.value_text)),
            ]
        };
        let mut fresh_tree = tree();
        let mut fresh = ChromeHost::new();
        fresh.build(&mut fresh_tree, &slider_row("a longer name"), rect());
        let want = bounds(&fresh_tree, fresh.slider_ids(7).unwrap());

        let mut t = tree();
        let mut host = ChromeHost::new();
        host.build(&mut t, &slider_row("ab"), rect());
        assert_eq!(
            host.update(&mut t, &slider_row("a longer name"), rect()),
            Reconcile::Updated
        );
        let ids = host.slider_ids(7).unwrap();
        assert_eq!(bounds(&t, ids), want);
        assert_eq!(ids.track_span, fresh.slider_ids(7).unwrap().track_span);
    }

    #[test]
    fn parent_links_preserved_in_tree() {
        let mut t = tree();
        let mut host = ChromeHost::new();
        host.build(&mut t, &card("1.0"), rect());
        let col = host.node_id(0).unwrap();
        let label = host.node_id(1).unwrap();
        let button = host.node_id(2).unwrap();
        assert_eq!(t.parent_of(col), None);
        assert_eq!(t.parent_of(label), Some(col));
        assert_eq!(t.parent_of(button), Some(col));
        // Sanity: kinds match the description.
        assert_eq!(t.get_node(button).unwrap().node_type, UINodeType::Button);
    }
}
