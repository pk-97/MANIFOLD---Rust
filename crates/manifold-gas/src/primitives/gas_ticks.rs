//! `node.gas_ticks` — the tick loop of a Smoke or Fire: the substep boundary
//! that repeats `node.gas_volume` once a tick of `node.gas_domain`'s clock.
//! With a water or Matter leading the World, the gas's ticks step in the
//! World's loop after theirs. It owns the published fields, which the step
//! writes in place on the frame's last tick, so the drawn gas leaves the loop
//! through this node.

use manifold_gpu::{BufferUse, GpuBuffer};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::exec::substeps::{SubstepBoundaryPorts, SubstepResultPorts};
use manifold_node_engine::primitive::Primitive;

use super::gas_volume::fine_field_bytes;

/// The published state is the region's state; the published velocity its
/// one result. The step reads neither per tick: it writes both on the
/// frame's last tick.
pub const GAS_TICKS_PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts {
    seed: "seed",
    capture: "state_in",
    state: "state",
    iteration_scalars: &[],
    results: &[SubstepResultPorts {
        capture: "velocity_in",
        output: "velocity",
        optional: false,
    }],
    clock: Some("clock"),
};

manifold_node_engine::primitive! {
    name: GasTicks,
    type_id: "node.gas_ticks",
    purpose: "The tick loop of a Smoke or Fire: repeats its body (node.gas_volume) once a tick of the clock wired into clock (node.gas_domain's interval_duration), ticks times a frame. Holds the published fields on the box's fine grid (dims_x, dims_y, dims_z): state (two words a fine cell: density and temperature, then fuel and flame, each pair as two halves) and velocity (u and v, then w and 0, as halves), which the body writes in place on the frame's last tick and hands back through state_in and velocity_in. seed is unused: the gas starts empty.",
    inputs: {
        seed: Array(u32) optional,
        state_in: Array(u32) optional,
        velocity_in: Array(u32) optional,
        clock: ScalarF32 optional,
        ticks: ScalarF32 optional,
        dims_x: ScalarF32 optional, dims_y: ScalarF32 optional, dims_z: ScalarF32 optional,
    },
    outputs: {
        state: Array(u32),
        velocity: Array(u32),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "clock from node.gas_domain's interval_duration, ticks and the fine grid's dims from the same domain. state and velocity feed node.gas_volume's state and velocity, whose state_out and velocity_out come back into state_in and velocity_in; the scene object reads state and velocity.",
    examples: [],
    picker: { label: "Gas Ticks", category: Atom },
    summary: "Repeats a Smoke or Fire's step once a World tick.",
    category: Particles3D,
    role: Filter,
    aliases: ["gas loop", "smoke ticks"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        published: Option<[GpuBuffer; 2]> = None,
        pending: u32 = 0,
    },
}

impl Primitive for GasTicks {
    fn state_capture_input_ports(&self) -> &[&str] {
        &["state_in", "velocity_in"]
    }

    fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
        Some(GAS_TICKS_PORTS)
    }

    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "state" | "velocity")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        let [state, velocity] = self.published.as_ref()?;
        match port {
            "state" => Some(state),
            "velocity" => Some(velocity),
            _ => None,
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, sized to the box at run time.
        matches!(port, "state" | "velocity").then_some(1)
    }

    /// The published fields are the box the domain lays: scene forces and
    /// hits enter at the domain, through the clock.
    fn physical_sources(&self, output: &str) -> Option<&'static [&'static str]> {
        matches!(output, "state" | "velocity").then_some(&["clock"])
    }

    fn substep_iteration(&mut self, iteration: u32, _scalars: &mut [f32]) -> bool {
        iteration < self.pending
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let whole = |name: &str| ctx.scalar_or_param(name, 0.0).round().max(0.0) as u32;
        self.pending = whole("ticks");
        let bytes = fine_field_bytes([whole("dims_x"), whole("dims_y"), whole("dims_z")]);
        if bytes == 0 {
            self.pending = 0;
            return;
        }
        let device = ctx.gpu_encoder().device;
        if self
            .published
            .as_ref()
            .is_none_or(|[state, _]| state.size != bytes)
        {
            // Unwritten until the step's first publish, which a fresh box's
            // first tick makes.
            self.published = Some(std::array::from_fn(|_| {
                device.create_buffer(bytes, BufferUse::Gpu)
            }));
        }
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent {
    use super::GasTicks;
    use crate::primitives::gas_volume::fine_field_bytes;
    use manifold_node_engine::exec::extent::{AtomExtent, ExtentRule, Verdict};
    use manifold_node_engine::primitive::PrimitiveSpec;

    fn gas_ticks(x: &mut AtomExtent<'_>) -> Result<(), Verdict> {
        let whole = |value: f32| value.round().max(0.0) as u32;
        let dims = ["dims_x", "dims_y", "dims_z"].map(|name| whole(x.scalar(name, 0.0)));
        let bytes = fine_field_bytes(dims);
        x.provide("state", bytes);
        x.provide("velocity", bytes);
        x.hold(2 * bytes);
        Ok(())
    }
    inventory::submit! {
        ExtentRule { type_id: <GasTicks as PrimitiveSpec>::TYPE_ID, check: gas_ticks }
    }
}
