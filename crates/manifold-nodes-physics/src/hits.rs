//! Simulations in this crate that take scene hits (a swarm), found on a graph
//! node by registration, never by naming its type. The host's impulse
//! runtime stamps, admits and drains them exactly as it does a native
//! simulation's; each hit is a velocity field sampled where the simulation's
//! particles are, applied once on the tick it lands on.

use std::any::TypeId;
use std::sync::LazyLock;

use ahash::AHashMap;
use manifold_node_engine::exec::effect_node::{EffectNode, SimStep};
use manifold_physics::input::{AppliedEvent, EventStamp};
use manifold_physics::{FieldValue, TickStamp};

/// A simulation that takes scene hits on its own fixed-tick clock.
pub trait TakesHits: Send {
    /// The epoch stamps must carry, once the clock has started.
    fn hit_epoch(&self) -> Option<u64>;

    /// `Ok(false)`: still preparing, the hit waits. `Err`: refused, with why.
    fn hit_readiness(&self) -> Result<bool, String>;

    /// Stamp a hit fired at `played` against the clock this frame's
    /// `step` accepted.
    fn hit_stamp(
        &self,
        played: manifold_physics::PlayedTime,
        sequence: u64,
        step: SimStep,
    ) -> Result<EventStamp, String>;

    /// Queue one hit: a velocity change in m/s, by position.
    fn enqueue_hit(&mut self, stamp: EventStamp, field: FieldValue) -> Result<TickStamp, String>;

    /// Receipts of hits applied by committed frames.
    fn drain_applied_hits(&mut self, consume: &mut dyn FnMut(AppliedEvent<FieldValue>));

    /// Stamps of hits discarded (a held clock, a failed frame). Returns the
    /// count of further discards past the bounded receipt list.
    fn drain_discarded_hits(&mut self, consume: &mut dyn FnMut(EventStamp)) -> u64;
}

/// Checked casts for one concrete hit-taking node type.
pub struct TakesHitsRegistration {
    type_id: TypeId,
    get: fn(&dyn EffectNode) -> &dyn TakesHits,
    get_mut: fn(&mut dyn EffectNode) -> &mut dyn TakesHits,
}

impl TakesHitsRegistration {
    pub const fn new<T: EffectNode + TakesHits + 'static>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            get: |node| {
                node.as_any()
                    .downcast_ref::<T>()
                    .expect("registered hit-taking node type")
            },
            get_mut: |node| {
                node.as_any_mut()
                    .downcast_mut::<T>()
                    .expect("registered hit-taking node type")
            },
        }
    }
}

inventory::collect!(TakesHitsRegistration);

static REGISTRATIONS: LazyLock<AHashMap<TypeId, &'static TakesHitsRegistration>> =
    LazyLock::new(|| {
        let mut registrations = AHashMap::new();
        for entry in inventory::iter::<TakesHitsRegistration> {
            assert!(
                registrations.insert(entry.type_id, entry).is_none(),
                "duplicate hit-taking registration"
            );
        }
        registrations
    });

pub fn get(node: &dyn EffectNode) -> Option<&dyn TakesHits> {
    REGISTRATIONS
        .get(&node.as_any().type_id())
        .map(|entry| (entry.get)(node))
}

pub fn get_mut(node: &mut dyn EffectNode) -> Option<&mut dyn TakesHits> {
    let type_id = node.as_any().type_id();
    REGISTRATIONS
        .get(&type_id)
        .map(|entry| (entry.get_mut)(node))
}
