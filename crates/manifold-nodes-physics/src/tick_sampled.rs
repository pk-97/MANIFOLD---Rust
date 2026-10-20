//! Simulations that read their force field at each tick's start, found on a
//! graph node by registration, never by naming its type. The host's
//! physics history replay asks each one for the coming ticks' starts and
//! replays only its force field, speed and roles at those times; every other
//! input is setup.

use std::any::TypeId;
use std::sync::LazyLock;

use ahash::AHashMap;
use manifold_node_engine::exec::effect_node::EffectNode;

/// A simulation whose forces are sampled per tick, whatever the display rate.
pub trait TickSampled: Send {
    /// Transport times in `(from, until]` the physics history replay must
    /// sample exactly before the next frame: each coming tick's start.
    fn request_physics_samples(&mut self, from: f64, until: f64, out: &mut Vec<f64>);
}

/// The inputs a tick-sampled simulation's history replay evaluates; the rest
/// are setup, held as they stand.
pub fn replays_input(port: &str) -> bool {
    matches!(port, "acceleration_field" | "speed" | "air_drag") || port.starts_with("role_")
}

/// Checked cast for one concrete tick-sampled node type.
pub struct TickSampledRegistration {
    type_id: TypeId,
    get_mut: fn(&mut dyn EffectNode) -> &mut dyn TickSampled,
}

impl TickSampledRegistration {
    pub const fn new<T: EffectNode + TickSampled + 'static>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            get_mut: |node| node.as_any_mut().downcast_mut::<T>().expect("registered tick-sampled node type"),
        }
    }
}

inventory::collect!(TickSampledRegistration);

static REGISTRATIONS: LazyLock<AHashMap<TypeId, &'static TickSampledRegistration>> = LazyLock::new(|| {
    let mut registrations = AHashMap::new();
    for entry in inventory::iter::<TickSampledRegistration> {
        assert!(registrations.insert(entry.type_id, entry).is_none(), "duplicate tick-sampled registration");
    }
    registrations
});

/// Whether `node` reads its forces per tick.
pub fn is_tick_sampled(node: &dyn EffectNode) -> bool {
    REGISTRATIONS.contains_key(&node.as_any().type_id())
}

pub fn get_mut(node: &mut dyn EffectNode) -> Option<&mut dyn TickSampled> {
    let type_id = node.as_any().type_id();
    REGISTRATIONS.get(&type_id).map(|entry| (entry.get_mut)(node))
}
