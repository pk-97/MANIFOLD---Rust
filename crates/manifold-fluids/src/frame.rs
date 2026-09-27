//! Owner-driven access to the same native substeps used by `FluidWorld::step`.

use std::ffi::c_void;

use manifold_physics::FieldInput;

use super::{FluidError, FluidWorld, FrameStats, NativeFrameStats, Seconds, native_result};

unsafe extern "C" {
    fn manifold_fluids_world_begin_frame(world: *mut c_void, dt: f64) -> i32;
    fn manifold_fluids_world_next_substep(world: *mut c_void, dt: *mut f64) -> i32;
    fn manifold_fluids_world_advance_substep(world: *mut c_void, dt: f64) -> i32;
    fn manifold_fluids_world_finish_frame(world: *mut c_void, stats: *mut NativeFrameStats) -> i32;
    fn manifold_fluids_world_abort_frame(world: *mut c_void);
}

/// An unpublished fluid frame, exclusively borrowed by its simulation owner.
///
/// Between substeps the owner can advance another physics backend. Collider
/// motion must be supplied before beginning the frame. This does not itself
/// exchange liquid/body reactions. Dropping an unfinished frame invalidates
/// the native world: rebuild it before reuse.
/// A partly advanced world cannot supply a surface or whitewater snapshot.
#[must_use = "finish the frame after all substeps, or dropping it invalidates the world"]
pub struct FluidFrame<'a> {
    world: &'a mut FluidWorld,
    finished: bool,
}

impl FluidWorld {
    pub fn begin_frame(&mut self, dt: Seconds) -> Result<FluidFrame<'_>, FluidError> {
        self.begin_frame_with_fields(dt, &[])
    }

    /// Use the existing field preparation for the whole frame interval. Impulse
    /// fields are distributed over that interval exactly as in `step_with_fields`.
    pub fn begin_frame_with_fields(
        &mut self,
        dt: Seconds,
        fields: &[FieldInput<'_>],
    ) -> Result<FluidFrame<'_>, FluidError> {
        self.prepare_step_fields(dt, fields)?;
        let ok = unsafe { manifold_fluids_world_begin_frame(self.native, dt.0) };
        native_result(ok, "beginning the fluid frame")?;
        Ok(FluidFrame {
            world: self,
            finished: false,
        })
    }
}

impl FluidFrame<'_> {
    /// Return the native stability bound for the next substep. The owner may
    /// choose a smaller duration to satisfy another backend. Repeated queries
    /// retain the same offer until it is consumed. `None` means finish is ready.
    pub fn next_substep(&mut self) -> Result<Option<Seconds>, FluidError> {
        let mut dt = 0.0;
        let ok = unsafe { manifold_fluids_world_next_substep(self.world.native, &mut dt) };
        native_result(ok, "selecting the fluid substep")?;
        if !dt.is_finite() || dt < 0.0 {
            return Err(FluidError::native(
                "native fluid substep duration is invalid",
            ));
        }
        Ok((dt > 0.0).then_some(Seconds(dt)))
    }

    /// Consume a duration no larger than the last native offer. This frame's
    /// substep budget is a failure boundary; it never forces an oversized step.
    pub fn advance(&mut self, dt: Seconds) -> Result<(), FluidError> {
        if !dt.0.is_finite() || dt.0 <= 0.0 {
            return Err(FluidError::input(
                "substep duration must be finite and positive",
            ));
        }
        let ok = unsafe { manifold_fluids_world_advance_substep(self.world.native, dt.0) };
        native_result(ok, "advancing the fluid substep")
    }

    /// Publish frame statistics only after every native substep has completed.
    pub fn finish(mut self) -> Result<FrameStats, FluidError> {
        let mut stats = NativeFrameStats::default();
        let ok = unsafe { manifold_fluids_world_finish_frame(self.world.native, &mut stats) };
        native_result(ok, "finishing the fluid frame")?;
        self.finished = true;
        Ok(stats.into())
    }
}

impl Drop for FluidFrame<'_> {
    fn drop(&mut self) {
        if !self.finished {
            unsafe { manifold_fluids_world_abort_frame(self.world.native) };
        }
    }
}

#[cfg(test)]
mod tests;
