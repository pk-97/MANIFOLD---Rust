//! Exchange with another solver inside the existing rigid-body tick owner.
//!
//! The owner retains clocks, authored input history and once-only events. It
//! prepares rigid targets and queued continuous forces before requesting each
//! interval, applies the exchange, then advances Box3D for exactly that duration.

use std::convert::Infallible;
use std::fmt::Display;

use crate::{PhysicsWorld, Seconds, TickStamp};

/// A backend participating in the rigid owner's fixed ticks. The returned
/// guard exclusively borrows its native state until the complete tick finishes.
pub trait StepCoupling {
    type Error: Display;
    type Frame<'a>: SubstepExchange<Error = Self::Error>
    where
        Self: 'a;

    fn begin_tick(
        &mut self,
        stamp: TickStamp,
        duration: Seconds,
    ) -> Result<Self::Frame<'_>, Self::Error>;
}

/// One unpublished tick. An owner must consume each selected interval once,
/// step Box3D once for it, then finish before publishing either solver's output.
/// Implementations invalidate incomplete native work when dropped.
pub trait SubstepExchange {
    type Error: Display;

    /// Read fresh rigid state after authored motion and forces are queued.
    /// Return a finite positive interval no greater than `maximum`.
    fn next_substep(
        &mut self,
        rigid: &PhysicsWorld,
        maximum: Seconds,
    ) -> Result<Seconds, Self::Error>;

    /// Advance the other solver and apply its reaction to the rigid world.
    /// Do not advance Box3D here: the existing owner does so immediately after.
    fn exchange(&mut self, rigid: &mut PhysicsWorld, duration: Seconds) -> Result<(), Self::Error>;

    /// Complete the participant and capture the paired native rigid state.
    /// This runs after the final rigid substep and before later authored
    /// edits or release events can change the world. A failed capture must
    /// leave both published outputs at their previous accepted tick.
    fn finish(self, rigid: &PhysicsWorld) -> Result<(), Self::Error>;
}

/// Rigid-only scenes use the identical tick owner without a second backend.
pub struct Uncoupled;

impl StepCoupling for Uncoupled {
    type Error = Infallible;
    type Frame<'a> = Self;

    fn begin_tick(&mut self, _: TickStamp, _: Seconds) -> Result<Self::Frame<'_>, Self::Error> {
        Ok(Self)
    }
}

impl SubstepExchange for Uncoupled {
    type Error = Infallible;

    fn next_substep(&mut self, _: &PhysicsWorld, maximum: Seconds) -> Result<Seconds, Self::Error> {
        Ok(maximum)
    }

    fn exchange(&mut self, _: &mut PhysicsWorld, _: Seconds) -> Result<(), Self::Error> {
        Ok(())
    }

    fn finish(self, _: &PhysicsWorld) -> Result<(), Self::Error> {
        Ok(())
    }
}
