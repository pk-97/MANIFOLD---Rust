//! [`FluidSim2D`] — the V1 hero atomic node.
//!
//! Demonstrates the **atomic-with-rich-ports** pattern that's the whole
//! point of allowing decomposition to be opt-in: the simulation kernel
//! stays opaque (you can't crack open the cog and see the advection /
//! pressure-projection passes), but its *internal data* is exposed as
//! auxiliary output ports so other nodes can use it.
//!
//! Casual users wire `source → composited` and ignore everything else —
//! it acts like any other one-in-one-out effect/generator. Power users
//! wire `density` into a Threshold → Blend overlay, drive `force_field`
//! from another generator, modulate `dye_color` with audio, etc.
//!
//! Also the first node with **optional** inputs (every input is optional;
//! FluidSim used as a generator leaves `source` unwired) and the first
//! node with a **scalar input port** (`dye_color` is `Scalar(Vec3)` so
//! it can be driven by another node's output, not just by a parameter).

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use std::borrow::Cow;

pub const FLUID_SIM_2D_TYPE_ID: &str = "atomic.fluid_sim_2d";

const FLUID_SIM_2D_INPUTS: [NodeInput; 4] = [
    NodePort {
        name: Cow::Borrowed("source"),
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        // Optional: when wired, FluidSim acts as an effect on the source
        // pixels. When unwired, it's a pure generator (renders density only).
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("force_field"),
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("spawn_mask"),
        ty: PortType::Texture2D,
        kind: PortKind::Input,
        required: false,
    },
    NodePort {
        // Scalar input: per-frame dye colour, possibly driven by audio /
        // another node's output. Falls back to the parameter when unwired.
        name: Cow::Borrowed("dye_color"),
        ty: PortType::Scalar(ScalarType::Vec3),
        kind: PortKind::Input,
        required: false,
    },
];

const FLUID_SIM_2D_OUTPUTS: [NodeOutput; 4] = [
    NodePort {
        // Default output. Most users only wire this.
        name: Cow::Borrowed("composited"),
        ty: PortType::Texture2D,
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("density"),
        ty: PortType::Texture2D,
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("velocity"),
        ty: PortType::Texture2D,
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("pressure"),
        ty: PortType::Texture2D,
        kind: PortKind::Output,
        required: false,
    },
];

const FLUID_SIM_2D_PARAMS: [ParamDef; 5] = [
    ParamDef {
        name: Cow::Borrowed("viscosity"),
        label: "Viscosity",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0001),
        range: Some((0.0, 0.1)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("dissipation"),
        label: "Dissipation",
        ty: ParamType::Float,
        default: ParamValue::Float(0.99),
        range: Some((0.9, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("vorticity"),
        label: "Vorticity",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, 4.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("jacobi_iterations"),
        label: "Pressure Iterations",
        ty: ParamType::Int,
        default: ParamValue::Float(20.0),
        range: Some((1.0, 60.0)),
        enum_values: &[],
    },
    ParamDef {
        // Fallback dye colour when the `dye_color` input port is unwired.
        name: Cow::Borrowed("dye_color_default"),
        label: "Dye Colour (default)",
        ty: ParamType::Vec3,
        default: ParamValue::Vec3([1.0, 0.5, 0.2]),
        range: None,
        enum_values: &[],
    },
];

#[derive(Debug)]
pub struct FluidSim2D {
    type_id: EffectNodeType,
}

impl FluidSim2D {
    pub fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(FLUID_SIM_2D_TYPE_ID),
        }
    }
}

impl Default for FluidSim2D {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectNode for FluidSim2D {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Warp
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &FLUID_SIM_2D_INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &FLUID_SIM_2D_OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &FLUID_SIM_2D_PARAMS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {
        // Stub. Real fluid-simulation kernel (advection + pressure projection
        // + vorticity confinement, with persistent density/velocity grids)
        // arrives in the manifold-gpu integration step.
    }

    fn clear_state(&mut self) {
        // Real impl will clear the density/velocity grids and any
        // accumulator buffers here. Called on seek to prevent stale fluid
        // state from a different point on the timeline.
    }
}
