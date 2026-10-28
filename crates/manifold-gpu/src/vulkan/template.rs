//! Gated templates (design section 7): P1 encodes them as indirect
//! dispatches, P6 records them once. Not built yet.

// Stub bodies name the phase that builds them (design section 9); their
// parameters are unused until then.
#![allow(unused_variables)]

use super::*;
use crate::*;

pub struct GatedRecorder<'s> {
    _store: std::marker::PhantomData<&'s mut GpuTemplateStore>,
}

pub struct TemplateTicket {
    _reserved: (),
}

pub struct GpuTemplateStore {
    _reserved: (),
}

impl GpuTemplateStore {
    pub fn new(device: &GpuDevice) -> Self {
        todo!("vulkan P1")
    }

    pub fn stats(&self) -> GpuTemplateStats {
        todo!("vulkan P1")
    }

    /// Slots held, idle or in flight.
    pub fn slots(&self) -> usize {
        todo!("vulkan P1")
    }
}

impl GpuEncoder {
    /// Walk `body` once as one round of exactly `commands` gated
    /// dispatches the GPU runs `copies` times in chunked executes, and find
    /// or build its slot in `store`.
    pub fn prepare_template(
        &mut self,
        store: &mut GpuTemplateStore,
        at: TemplateRanges<'_>,
        commands: u32,
        copies: u32,
        body: impl FnOnce(&mut GatedRecorder<'_>) -> Result<(), String>,
    ) -> Result<TemplateTicket, String> {
        todo!("vulkan P1")
    }

    /// Run a prepared template here: the pending frame-replay stretch first,
    /// then the slot's executes on a compute encoder that declares every
    /// buffer the slot names, a buffer barrier around and between executes.
    pub fn execute_template(
        &mut self,
        store: &mut GpuTemplateStore,
        ticket: TemplateTicket,
    ) -> Result<(), String> {
        todo!("vulkan P1")
    }
}
