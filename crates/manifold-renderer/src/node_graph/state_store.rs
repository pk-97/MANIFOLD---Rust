//! Per-instance, per-owner state for stateful nodes.
//!
//! Stateful nodes (Bloom mip chains, Feedback prev-frame buffers, etc.)
//! need persistent GPU resources that survive across frames. Today the
//! `PostProcessEffect` impls hold these inside themselves, keyed by
//! `owner_key: i64`. That conflates *behavior* (the effect's evaluate
//! logic) with *state* (the resources backing it), and makes effects
//! stateful singletons in `EffectRegistry`.
//!
//! [`StateStore`] inverts the relationship: state lives in a runtime-owned
//! container keyed by `(NodeInstanceId, OwnerKey)`. Nodes are pure
//! behavior, accessed by reference through [`EffectNodeContext`]. State is
//! cleaned up in one place, lifecycle is uniform across all stateful
//! nodes, and the future graph runtime can reason about state as a
//! first-class resource (see `docs/EFFECT_RUNTIME_UNIFICATION.md` §5.3).
//!
//! ## Type erasure
//!
//! Each (node, owner) bucket holds a `Box<dyn AnyState>`. Nodes implement
//! [`NodeState`] for their concrete state type and downcast on access.
//! Mismatched downcasts return `None` rather than panicking — the most
//! common cause is a node id collision, which is a host bug worth
//! surfacing as a missing-state error rather than a type-mismatch crash.

use ahash::AHashMap;
use std::any::Any;

use crate::node_graph::effect_node::NodeInstanceId;

/// Owner identity, matching the `PresetContext::owner_key` shape:
/// `0` for master, `layer_index + 1` for a layer, `hash(clip_id)` for a
/// clip. Stays as a typedef rather than a newtype so existing call sites
/// can flow `i64` through unchanged.
pub type OwnerKey = i64;

/// Persistent per-instance, per-owner state for a stateful node.
///
/// Implemented by node-defined state types (e.g. `FeedbackState` holding
/// a previous-frame `RenderTarget`). Lives across frames; cleaned up
/// when the owner is destroyed via [`StateStore::cleanup_owner`] or the
/// store is reset via [`StateStore::cleanup_all`].
pub trait NodeState: Send + 'static {
    /// Optional cleanup hook called before the state is dropped. Default
    /// is a no-op — most types release resources via their fields' `Drop`
    /// impls and don't need this. Override to flush async work, return
    /// resources to a pool, etc.
    fn cleanup(&mut self) {}
}

/// Type-erased state container. One [`Box<dyn AnyState>`] per
/// `(NodeInstanceId, OwnerKey)` bucket.
///
/// Owned by the host (typically a graph-backed effect like
/// `StylizedFeedbackFX`). The host calls [`StateStore::cleanup_owner`]
/// from its `cleanup_owner_state` hook to mirror the legacy lifecycle.
pub struct StateStore {
    buckets: AHashMap<(NodeInstanceId, OwnerKey), Box<dyn AnyState>>,
}

impl Default for StateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StateStore {
    pub fn new() -> Self {
        Self {
            buckets: AHashMap::default(),
        }
    }

    /// Number of `(node, owner)` buckets currently allocated. Diagnostics
    /// only — production code shouldn't branch on this.
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }

    /// Borrow a node's state for an owner, downcast to `T`. Returns
    /// `None` if no entry exists or the stored type doesn't match `T`.
    pub fn get<T: NodeState>(
        &mut self,
        node_id: NodeInstanceId,
        owner_key: OwnerKey,
    ) -> Option<&mut T> {
        self.buckets
            .get_mut(&(node_id, owner_key))?
            .as_any_mut()
            .downcast_mut::<T>()
    }

    /// Insert state for `(node_id, owner_key)`, replacing any existing
    /// entry. The replaced state's `cleanup` hook fires before its drop.
    pub fn insert<T: NodeState>(&mut self, node_id: NodeInstanceId, owner_key: OwnerKey, state: T) {
        if let Some(mut prev) = self.buckets.insert((node_id, owner_key), Box::new(state)) {
            prev.cleanup();
        }
    }

    /// Drop all state for a specific owner. Called when a clip / layer is
    /// destroyed or stops playback — mirrors the legacy
    /// `cleanup_owner_state(owner_key)` hook.
    pub fn cleanup_owner(&mut self, owner_key: OwnerKey) {
        self.buckets.retain(|(_, ok), state| {
            if *ok == owner_key {
                state.cleanup();
                false
            } else {
                true
            }
        });
    }

    /// Drop all state. Called on shutdown / project close — mirrors
    /// `clear_state` and `cleanup_all_owners` semantics.
    pub fn cleanup_all(&mut self) {
        for (_, mut state) in self.buckets.drain() {
            state.cleanup();
        }
    }
}

/// Internal trait that provides type-erasure (via `Any`) plus the
/// `cleanup` hook delegation. Implemented blanket-style for every
/// [`NodeState`].
trait AnyState: Send {
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn cleanup(&mut self);
}

impl<T: NodeState> AnyState for T {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn cleanup(&mut self) {
        T::cleanup(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CounterState {
        value: u32,
        cleanup_calls: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }

    impl NodeState for CounterState {
        fn cleanup(&mut self) {
            self.cleanup_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct OtherState {
        text: String,
    }

    impl NodeState for OtherState {}

    fn node(id: u32) -> NodeInstanceId {
        NodeInstanceId(id)
    }

    #[test]
    fn insert_and_get_round_trip() {
        let mut store = StateStore::new();
        let cleanup = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        store.insert(
            node(1),
            42,
            CounterState {
                value: 7,
                cleanup_calls: cleanup.clone(),
            },
        );
        let got = store.get::<CounterState>(node(1), 42).unwrap();
        assert_eq!(got.value, 7);
        got.value = 99;
        assert_eq!(store.get::<CounterState>(node(1), 42).unwrap().value, 99);
    }

    #[test]
    fn missing_entry_returns_none() {
        let mut store = StateStore::new();
        assert!(store.get::<CounterState>(node(1), 42).is_none());
    }

    #[test]
    fn type_mismatch_returns_none() {
        let mut store = StateStore::new();
        store.insert(
            node(1),
            42,
            OtherState {
                text: "hi".to_string(),
            },
        );
        // Stored as OtherState; querying as CounterState yields None.
        assert!(store.get::<CounterState>(node(1), 42).is_none());
        // But the right type still works.
        assert_eq!(store.get::<OtherState>(node(1), 42).unwrap().text, "hi");
    }

    #[test]
    fn distinct_node_or_owner_keys_are_independent() {
        let mut store = StateStore::new();
        let c = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        store.insert(
            node(1),
            42,
            CounterState {
                value: 1,
                cleanup_calls: c.clone(),
            },
        );
        store.insert(
            node(2),
            42,
            CounterState {
                value: 2,
                cleanup_calls: c.clone(),
            },
        );
        store.insert(
            node(1),
            43,
            CounterState {
                value: 3,
                cleanup_calls: c.clone(),
            },
        );
        assert_eq!(store.get::<CounterState>(node(1), 42).unwrap().value, 1);
        assert_eq!(store.get::<CounterState>(node(2), 42).unwrap().value, 2);
        assert_eq!(store.get::<CounterState>(node(1), 43).unwrap().value, 3);
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn cleanup_owner_drops_only_matching_entries() {
        let mut store = StateStore::new();
        let c = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        store.insert(
            node(1),
            42,
            CounterState {
                value: 1,
                cleanup_calls: c.clone(),
            },
        );
        store.insert(
            node(2),
            42,
            CounterState {
                value: 2,
                cleanup_calls: c.clone(),
            },
        );
        store.insert(
            node(1),
            43,
            CounterState {
                value: 3,
                cleanup_calls: c.clone(),
            },
        );

        store.cleanup_owner(42);
        assert_eq!(c.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(store.get::<CounterState>(node(1), 42).is_none());
        assert!(store.get::<CounterState>(node(2), 42).is_none());
        assert!(store.get::<CounterState>(node(1), 43).is_some());
    }

    #[test]
    fn cleanup_all_drops_everything() {
        let mut store = StateStore::new();
        let c = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        store.insert(
            node(1),
            42,
            CounterState {
                value: 1,
                cleanup_calls: c.clone(),
            },
        );
        store.insert(
            node(2),
            43,
            CounterState {
                value: 2,
                cleanup_calls: c.clone(),
            },
        );
        store.cleanup_all();
        assert_eq!(c.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(store.is_empty());
    }

    #[test]
    fn replacing_state_runs_cleanup_on_old_value() {
        let mut store = StateStore::new();
        let c = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        store.insert(
            node(1),
            42,
            CounterState {
                value: 1,
                cleanup_calls: c.clone(),
            },
        );
        store.insert(
            node(1),
            42,
            CounterState {
                value: 2,
                cleanup_calls: c.clone(),
            },
        );
        // Old state's cleanup should have fired.
        assert_eq!(c.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(store.get::<CounterState>(node(1), 42).unwrap().value, 2);
    }
}
