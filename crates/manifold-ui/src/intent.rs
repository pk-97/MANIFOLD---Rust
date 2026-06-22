//! Node-intent dispatch — the friendly, position-robust replacement for
//! per-panel `event.node_id == self.some_id` matching.
//!
//! A panel attaches an *intent* to a node at build time (what this region means
//! for a given gesture) instead of re-deriving it from stored ids at handle
//! time. A single central resolver folds a hit node up its parent chain to the
//! nearest ancestor carrying an intent for the gesture, then emits that
//! `PanelAction`. The fold-up is what kills dead zones: a right-click on a
//! slider's non-interactive fill resolves to the owning card.
//!
//! See `docs/NODE_INTENT_DISPATCH.md` for the full design + migration plan.

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

/// Per-node intent: which `PanelAction` each gesture maps to. Most nodes carry
/// none, so the registry stores these sparsely (boxed, behind `Option`).
#[derive(Default, Clone)]
pub struct NodeIntent {
    pub click: Option<PanelAction>,
    pub double_click: Option<PanelAction>,
    pub right_click: Option<PanelAction>,
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

impl NodeIntent {
    fn action_for(&self, g: Gesture) -> Option<&PanelAction> {
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

/// Dense, node-id-indexed intent store (`id == index`, parallel to the SoA
/// tree). Cleared at build start and repopulated as panels create nodes.
#[derive(Default)]
pub struct IntentRegistry {
    slots: Vec<Option<Box<NodeIntent>>>,
}

impl IntentRegistry {
    pub fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Drop all intent. Call once at the start of each tree build.
    pub fn clear(&mut self) {
        self.slots.clear();
    }

    fn slot_mut(&mut self, node: NodeId) -> &mut NodeIntent {
        let idx = node.index();
        if idx >= self.slots.len() {
            self.slots.resize_with(idx + 1, || None);
        }
        self.slots[idx].get_or_insert_with(|| Box::new(NodeIntent::default()))
    }

    /// Map a gesture on `node` to `action`.
    pub fn on(&mut self, node: NodeId, g: Gesture, action: PanelAction) {
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

    fn get(&self, node: NodeId) -> Option<&NodeIntent> {
        self.slots
            .get(node.index())
            .and_then(|s| s.as_deref())
            .filter(|i| !i.is_empty())
    }

    /// Fold up from `hit` toward the root, returning the first ancestor's action
    /// for `g`. A `claims_area` node with no matching gesture intent stops the
    /// walk and absorbs the gesture (returns `None`) — it deliberately shadows
    /// outer intents so an inner region can opt out.
    pub fn resolve(&self, tree: &UITree, hit: Option<NodeId>, g: Gesture) -> Option<PanelAction> {
        let mut cur = hit;
        while let Some(node) = cur {
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
        intents.on(card, Gesture::RightClick, PanelAction::PlayPause);

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
        intents.on(card, Gesture::RightClick, PanelAction::PlayPause);

        let track = tree.add_button(Some(card), 0.0, 0.0, 200.0, 30.0, style(), "");
        intents.on(track, Gesture::RightClick, PanelAction::Stop);

        // The track's own intent resolves before the card's.
        let action = intents.resolve(&tree, Some(track), Gesture::RightClick);
        assert!(matches!(action, Some(PanelAction::Stop)));
    }

    #[test]
    fn claimed_region_without_gesture_absorbs() {
        let mut tree = UITree::new();
        let mut intents = IntentRegistry::new();

        // Outer carries a right-click intent.
        let outer = tree.add_panel(None, 0.0, 0.0, 200.0, 100.0, style());
        intents.on(outer, Gesture::RightClick, PanelAction::PlayPause);

        // Inner claims its area but only for Click — right-click is absorbed,
        // not leaked to `outer`.
        let inner = tree.add_panel(Some(outer), 0.0, 0.0, 50.0, 50.0, style());
        intents.claim_area(inner);
        intents.on(inner, Gesture::Click, PanelAction::PlayPause);

        assert!(
            intents
                .resolve(&tree, Some(inner), Gesture::RightClick)
                .is_none(),
            "claimed region must shadow the outer right-click intent"
        );
    }

    #[test]
    fn miss_resolves_to_nothing() {
        let tree = UITree::new();
        let intents = IntentRegistry::new();
        assert!(intents.resolve(&tree, None, Gesture::RightClick).is_none());
    }
}
