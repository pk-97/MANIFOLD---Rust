//! Native water interfaces borrowed from graph-owned nodes.

use std::any::TypeId;
use std::sync::LazyLock;

use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext};
use ahash::AHashMap;

/// Observations, coupled inputs and impulse admission owned by native simulations.
pub trait PhysicsNode: Send {
    /// Current accepted setup state for native fluid-domain bounds. Nodes that
    /// expose fluid-domain observations return `Some`; all other nodes return
    /// `None`.
    fn fluid_domain_snapshot(&self) -> Option<manifold_node_engine::scene::fluid_domain::FluidDomainSnapshot> {
        None
    }

    /// Resolved rigid inputs from the latest successful graph evaluation.
    /// Pending or invalid inputs must clear the previous observation.
    fn rigid_scene_observation(&self) -> Option<&crate::physics::RigidSceneObservation> {
        None
    }

    /// Configure ownership during graph preparation, before any evaluation.
    fn set_coupled_physics(&mut self, _enabled: bool) {}

    /// Current authored project timing for recorded-take compatibility.
    /// Hosts install an immutable snapshot; native I/O stays on the worker.
    fn set_physics_project_tempo(
        &mut self,
        _tempo: Option<&manifold_node_engine::runtime::preset_context::ProjectTempo>,
    ) {
    }

    /// Transport times in `(from, until]` the physics history replay must
    /// sample exactly before the next frame: a GPU liquid asks for each
    /// coming tick's start, so its forces are evaluated per tick.
    fn request_physics_samples(&mut self, _from: f64, _until: f64, _out: &mut Vec<f64>) {}

    /// Resolve the rigid participant before its paired liquid step. This must
    /// not construct or advance a second native world, or publish outputs.
    fn capture_coupled_rigid(
        &mut self,
        _ctx: &mut EffectNodeContext<'_, '_>,
    ) -> Result<(), String> {
        Err("Primitive does not support coupled rigid input capture".into())
    }

    /// Hand the captured rigid inputs to the liquid's existing worker owner.
    /// A missing observation is pending; an error fails the entire pair.
    fn set_coupled_rigid_inputs(
        &mut self,
        _observation: Option<&crate::physics::RigidSceneObservation>,
        _colliders: manifold_node_engine::scene::impulse::RigidImpulseTargets,
        _error: Option<&str>,
    ) {
    }

    fn coupled_rigid_frame(&self) -> Option<&crate::rigid_coupling::CoupledRigidFrame> {
        None
    }

    /// Latch the rigid result of the liquid step before any scene consumer runs.
    fn accept_coupled_rigid_frame(
        &mut self,
        _frame: Option<&crate::rigid_coupling::CoupledRigidFrame>,
    ) {
    }

    /// Epoch of native impulse inputs accepted by this node, if any.
    fn physics_impulse_epoch(&self) -> Option<u64> {
        None
    }

    /// Timestamp an impulse against the exact native observation accepted at
    /// the supplied transport value, if this node owns such a clock.
    fn physics_impulse_stamp(
        &self,
        _transport: manifold_core::Seconds,
        _sequence: u64,
    ) -> Result<manifold_physics::input::EventStamp, String> {
        Err("node does not expose a native impulse clock".into())
    }

    /// Queue one resolved impulse for a native fixed-tick simulation.
    fn enqueue_physics_impulse(
        &mut self,
        _stamp: manifold_physics::input::EventStamp,
        _impulse: crate::physics_events::ResolvedNodeImpulse,
    ) -> Result<manifold_physics::TickStamp, String> {
        Err("node does not accept physics impulses".into())
    }

    /// Drain native tick-start impulse receipts into the graph-owned sink.
    fn drain_physics_impulses(
        &mut self,
        _consume: &mut dyn FnMut(
            manifold_physics::input::AppliedEvent<crate::physics_events::ResolvedNodeImpulse>,
        ),
    ) {
    }

    /// Drain the stamps of impulses discarded because the simulation's clock
    /// was held (pause, Speed 0); they are never applied.
    fn drain_discarded_impulses(
        &mut self,
        _consume: &mut dyn FnMut(manifold_physics::input::EventStamp),
    ) {
    }
}

/// Checked casts for a concrete native node. Registration does not own node state.
pub struct PhysicsNodeRegistration {
    type_id: TypeId,
    get: fn(&dyn EffectNode) -> &dyn PhysicsNode,
    get_mut: fn(&mut dyn EffectNode) -> &mut dyn PhysicsNode,
}

impl PhysicsNodeRegistration {
    pub const fn new<T: EffectNode + PhysicsNode + 'static>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            get: |node| {
                node.as_any()
                    .downcast_ref::<T>()
                    .expect("registered native node type")
            },
            get_mut: |node| {
                node.as_any_mut()
                    .downcast_mut::<T>()
                    .expect("registered native node type")
            },
        }
    }
}

inventory::collect!(PhysicsNodeRegistration);

static REGISTRATIONS: LazyLock<AHashMap<TypeId, &'static PhysicsNodeRegistration>> =
    LazyLock::new(|| {
        let mut registrations = AHashMap::new();
        for entry in inventory::iter::<PhysicsNodeRegistration> {
            assert!(
                registrations.insert(entry.type_id, entry).is_none(),
                "duplicate native node registration"
            );
        }
        registrations
    });

/// Prepare the immutable index during graph construction, before live evaluation.
pub(crate) fn initialize() {
    LazyLock::force(&REGISTRATIONS);
}

pub fn get(node: &dyn EffectNode) -> Option<&dyn PhysicsNode> {
    REGISTRATIONS
        .get(&node.as_any().type_id())
        .map(|entry| (entry.get)(node))
}

pub fn get_mut(node: &mut dyn EffectNode) -> Option<&mut dyn PhysicsNode> {
    let type_id = node.as_any().type_id();
    REGISTRATIONS
        .get(&type_id)
        .map(|entry| (entry.get_mut)(node))
}
