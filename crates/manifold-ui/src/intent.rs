//! Node-intent dispatch — the friendly, position-robust replacement for
//! per-panel `event.node_id == self.some_id` matching.
//!
//! A panel attaches an *intent* to a node at build time (what this region means
//! for a given gesture) instead of re-deriving it from stored ids at handle
//! time. A single central resolver folds a hit node up its parent chain to the
//! nearest ancestor carrying an intent for the gesture, then emits that action.
//! The fold-up is what kills dead zones: a right-click on a slider's
//! non-interactive fill resolves to the owning card.
//!
//! The registry is generic over the **action type** `A` (`IntentRegistry<A>` /
//! `NodeIntent<A>`), defaulting to [`PanelAction`] so every chrome panel and the
//! main-window resolve path read as `IntentRegistry` unchanged. The graph-editor
//! sidebar instantiates it as `IntentRegistry<GraphEditCommand>` to fold its own
//! click dispatch onto the same machinery (UI Architecture Overhaul Phase 6).
//!
//! Entries are keyed by the full [`NodeId`], generation included. A panel may
//! register against an id it kept from an earlier build of its region; that
//! entry is inert. It can never be found from a live node, even one that now
//! sits in the same region and slot, because the live node carries a different
//! generation.
//!
// !

#[cfg(test)]
use crate::TransportAction;
use crate::node::NodeId;
use crate::panels::PanelAction;
use crate::tree::UITree;

/// Discrete pointer gestures that carry intent. Drag/scroll/hover stay in the
/// stateful `handle_event` path — intent dispatch is for node→action gestures.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Gesture {
    Click,
    DoubleClick,
    RightClick,
}

/// Per-node intent: which action `A` each gesture maps to. Most nodes carry
/// none, so the registry stores only the nodes that carry one, inline in a
/// map that keeps its capacity across rebuilds (no allocation per entry).
/// Generic over the action type (default [`PanelAction`]); the graph-editor
/// sidebar uses `NodeIntent<GraphEditCommand>`.
pub struct NodeIntent<A = PanelAction> {
    pub click: Option<A>,
    pub double_click: Option<A>,
    pub right_click: Option<A>,
    /// When set, this node claims its whole area for fold-up: a gesture on any
    /// non-intent descendant resolves here. Container backgrounds (card body,
    /// panel bg) set this so their padding/gaps are live, not dead.
    ///
    /// `claims_area` does not by itself produce an action — it only stops the
    /// fold-up walk from passing through. The node must also carry the relevant
    /// gesture intent to fire. (A claim with no matching gesture intent means
    /// "absorb the gesture here and do nothing" — an explicit dead stop.)
    pub claims_area: bool,
}

// Hand-rolled so the impl carries no `A: Default` bound — an empty intent is
// "all `None`" regardless of whether the action type is `Default`.
impl<A> Default for NodeIntent<A> {
    fn default() -> Self {
        Self {
            click: None,
            double_click: None,
            right_click: None,
            claims_area: false,
        }
    }
}

impl<A> NodeIntent<A> {
    fn action_for(&self, g: Gesture) -> Option<&A> {
        match g {
            Gesture::Click => self.click.as_ref(),
            Gesture::DoubleClick => self.double_click.as_ref(),
            Gesture::RightClick => self.right_click.as_ref(),
        }
    }

    fn is_empty(&self) -> bool {
        self.click.is_none()
            && self.double_click.is_none()
            && self.right_click.is_none()
            && !self.claims_area
    }
}

/// Sparse intent store keyed by the full [`NodeId`] (region, slot and
/// generation). Cleared at build start and repopulated as panels create nodes.
/// Generic over the action type (default [`PanelAction`]).
///
/// Contract: a registration against a stale id is inert. The key carries the
/// generation, so a stale id and the live node that reuses its slot never
/// share an entry.
pub struct IntentRegistry<A = PanelAction> {
    entries: ahash::AHashMap<NodeId, NodeIntent<A>>,
}

// Hand-rolled to avoid a spurious `A: Default` bound.
impl<A> Default for IntentRegistry<A> {
    fn default() -> Self {
        Self {
            entries: ahash::AHashMap::new(),
        }
    }
}

impl<A> IntentRegistry<A> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop all intent, keeping the map's capacity. Call once at the start of
    /// each tree build.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// How many registered entries name a node that is not live in `tree`.
    /// Such entries are inert, but each one is a panel registering ids it
    /// kept from an earlier build.
    pub fn dead_entry_count(&self, tree: &UITree) -> usize {
        self.entries.keys().filter(|&&id| !tree.is_live(id)).count()
    }

    fn slot_mut(&mut self, node: NodeId) -> &mut NodeIntent<A> {
        self.entries.entry(node).or_default()
    }

    /// Map a gesture on `node` to `action`.
    pub fn on(&mut self, node: NodeId, g: Gesture, action: A) {
        let slot = self.slot_mut(node);
        match g {
            Gesture::Click => slot.click = Some(action),
            Gesture::DoubleClick => slot.double_click = Some(action),
            Gesture::RightClick => slot.right_click = Some(action),
        }
    }

    /// Mark `node` as claiming its whole area for fold-up resolution.
    pub fn claim_area(&mut self, node: NodeId) {
        self.slot_mut(node).claims_area = true;
    }

    fn get(&self, node: NodeId) -> Option<&NodeIntent<A>> {
        self.entries.get(&node).filter(|i| !i.is_empty())
    }

    /// Fold up from `hit` toward the root, returning the first ancestor's action
    /// for `g`. A `claims_area` node with no matching gesture intent stops the
    /// walk and absorbs the gesture (returns `None`) — it deliberately shadows
    /// outer intents so an inner region can opt out.
    pub fn resolve(&self, tree: &UITree, hit: Option<NodeId>, g: Gesture) -> Option<A>
    where
        A: Clone,
    {
        let mut cur = hit;
        while let Some(node) = cur {
            if !tree.is_live(node) {
                return None;
            }
            if let Some(intent) = self.get(node) {
                if let Some(action) = intent.action_for(g) {
                    return Some(action.clone());
                }
                if intent.claims_area {
                    // Claimed but no action for this gesture: stop here.
                    return None;
                }
            }
            cur = tree.parent_of(node);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::UIStyle;

    fn style() -> UIStyle {
        UIStyle::default()
    }

    // Build: root → card(bg, claims area, right-click=A) → row → slider track.
    // Right-click on the track (which has its own intent) hits the track action;
    // right-click on a sibling with no intent folds up to the card.
    #[test]
    fn fold_up_resolves_to_owning_card() {
        let mut tree = UITree::new();
        let mut intents = IntentRegistry::new();

        let card = tree.add_panel(None, 0.0, 0.0, 200.0, 100.0, style());
        intents.claim_area(card);
        intents.on(
            card,
            Gesture::RightClick,
            PanelAction::Transport(TransportAction::PlayPause),
        );

        let row = tree.add_panel(Some(card), 0.0, 0.0, 200.0, 30.0, style());
        let fill = tree.add_panel(Some(row), 0.0, 0.0, 100.0, 30.0, style());

        // A non-intent descendant (the slider fill) folds up to the card.
        assert!(
            intents
                .resolve(&tree, Some(fill), Gesture::RightClick)
                .is_some(),
            "right-click on dead fill should fold up to the card"
        );
    }

    #[test]
    fn specific_node_intent_wins_over_ancestor() {
        let mut tree = UITree::new();
        let mut intents = IntentRegistry::new();

        let card = tree.add_panel(None, 0.0, 0.0, 200.0, 100.0, style());
        intents.claim_area(card);
        intents.on(
            card,
            Gesture::RightClick,
            PanelAction::Transport(TransportAction::PlayPause),
        );

        let track = tree.add_button(Some(card), 0.0, 0.0, 200.0, 30.0, style(), "");
        intents.on(
            track,
            Gesture::RightClick,
            PanelAction::Transport(TransportAction::Stop),
        );

        // The track's own intent resolves before the card's.
        let action = intents.resolve(&tree, Some(track), Gesture::RightClick);
        assert!(matches!(
            action,
            Some(PanelAction::Transport(TransportAction::Stop))
        ));
    }

    #[test]
    fn claimed_region_without_gesture_absorbs() {
        let mut tree = UITree::new();
        let mut intents = IntentRegistry::new();

        // Outer carries a right-click intent.
        let outer = tree.add_panel(None, 0.0, 0.0, 200.0, 100.0, style());
        intents.on(
            outer,
            Gesture::RightClick,
            PanelAction::Transport(TransportAction::PlayPause),
        );

        // Inner claims its area but only for Click — right-click is absorbed,
        // not leaked to `outer`.
        let inner = tree.add_panel(Some(outer), 0.0, 0.0, 50.0, 50.0, style());
        intents.claim_area(inner);
        intents.on(
            inner,
            Gesture::Click,
            PanelAction::Transport(TransportAction::PlayPause),
        );

        assert!(
            intents
                .resolve(&tree, Some(inner), Gesture::RightClick)
                .is_none(),
            "claimed region must shadow the outer right-click intent"
        );
    }

    /// A panel that re-registers an id from an earlier build of its region
    /// must not hand its action to the live node now at that slot.
    #[test]
    fn stale_id_registration_is_inert_after_slot_reuse() {
        let mut tree = UITree::new();
        let mut intents = IntentRegistry::new();

        let stale = tree.add_button(None, 0.0, 0.0, 80.0, 30.0, style(), "");
        tree.clear_region(stale.region());
        let live = tree.add_button(None, 0.0, 0.0, 80.0, 30.0, style(), "");
        assert_eq!(
            (stale.region(), stale.slot()),
            (live.region(), live.slot()),
            "slot is reused"
        );
        assert_ne!(stale, live);

        intents.clear();
        intents.on(
            stale,
            Gesture::Click,
            PanelAction::Transport(TransportAction::Stop),
        );

        assert!(intents.resolve(&tree, Some(live), Gesture::Click).is_none());
        assert_eq!(intents.dead_entry_count(&tree), 1);

        intents.on(
            live,
            Gesture::Click,
            PanelAction::Transport(TransportAction::PlayPause),
        );
        assert!(matches!(
            intents.resolve(&tree, Some(live), Gesture::Click),
            Some(PanelAction::Transport(TransportAction::PlayPause))
        ));
    }

    #[test]
    fn miss_resolves_to_nothing() {
        let tree = UITree::new();
        // `None` hit constrains nothing, so name the action type explicitly.
        let intents = IntentRegistry::<PanelAction>::new();
        assert!(intents.resolve(&tree, None, Gesture::RightClick).is_none());
    }

    /// A registry parameterised over a *non-`PanelAction`* action type resolves
    /// (and folds up) exactly like the default one. Proves Phase 6.1's generic
    /// param is real — the graph-editor sidebar's `IntentRegistry<GraphEditCommand>`
    /// rides this same code, not a fork.
    #[test]
    fn generic_action_type_resolves_and_folds_up() {
        #[derive(Clone, PartialEq, Debug)]
        enum MyAction {
            Open(u32),
            Close,
        }

        let mut tree = UITree::new();
        let mut intents: IntentRegistry<MyAction> = IntentRegistry::new();

        let card = tree.add_panel(None, 0.0, 0.0, 200.0, 100.0, style());
        intents.claim_area(card);
        intents.on(card, Gesture::Click, MyAction::Close);

        let row = tree.add_panel(Some(card), 0.0, 0.0, 200.0, 30.0, style());
        let button = tree.add_button(Some(row), 0.0, 0.0, 80.0, 30.0, style(), "");
        intents.on(button, Gesture::Click, MyAction::Open(7));

        // Direct hit on the button resolves to its own action.
        assert_eq!(
            intents.resolve(&tree, Some(button), Gesture::Click),
            Some(MyAction::Open(7))
        );
        // A dead sibling folds up to the claiming card.
        assert_eq!(
            intents.resolve(&tree, Some(row), Gesture::Click),
            Some(MyAction::Close)
        );
        // A gesture the card doesn't carry is absorbed by the claim (None).
        assert_eq!(intents.resolve(&tree, Some(row), Gesture::RightClick), None);
    }
}
