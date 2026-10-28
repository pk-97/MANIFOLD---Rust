//! Test-only nodes for region proofs that go through the registry (the
//! freeze finder and the `gpu_proofs` binary build graphs from definitions):
//! array sources, a particle substep boundary, a tick publisher and a texture
//! sink that makes the region live. Compiled for unit tests and the
//! `gpu-proofs` feature only.

use manifold_gpu::BufferUse;
use std::borrow::Cow;

use crate::exec::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, NodeRequires, ParamValues,
};
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::particles::Particle;
use crate::persistence::PrimitiveRegistry;
use crate::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};

use crate::exec::substeps::SubstepBoundaryPorts;

pub const PARTICLE_BOUNDARY_PORTS: SubstepBoundaryPorts = SubstepBoundaryPorts {
    seed: "seed",
    capture: "in",
    state: "out",
    iteration_scalars: &["step_dt", "step_index"],
    results: &[],
    clock: None,
};

/// The `step_dt` the particle boundary serves at iteration `i`: distinct
/// per iteration so a shared uniform would be visible in the result.
pub fn particle_step_dt(iteration: u32) -> f32 {
    0.25 * (iteration + 1) as f32
}

fn port(name: &'static str, ty: PortType, kind: PortKind, required: bool) -> NodePort {
    NodePort {
        name: Cow::Borrowed(name),
        ty,
        kind,
        required,
    }
}

fn int_param(name: &'static str, default: f32) -> ParamDef {
    ParamDef {
        name: Cow::Borrowed(name),
        label: name,
        ty: ParamType::Int,
        default: ParamValue::Float(default),
        range: Some((0.0, 1.0e6)),
        enum_values: &[],
    }
}

macro_rules! node_basics {
    () => {
        fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
            crate::scene::depth_rule::DepthRule::Terminal
        }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &self.inputs
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &self.params
        }
    };
}

/// An array whose contents the test writes after pre-allocation;
/// sized by `max_capacity`.
struct ArraySource {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    params: Vec<ParamDef>,
}

impl ArraySource {
    fn new(type_id: &'static str, item: ArrayType) -> Self {
        Self {
            type_id: EffectNodeType::new(type_id),
            inputs: Vec::new(),
            outputs: vec![port("out", PortType::Array(item), PortKind::Output, false)],
            params: vec![int_param("max_capacity", 256.0)],
        }
    }
}

impl EffectNode for ArraySource {
    node_basics!();
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

/// A particle substep boundary with the same shape the MPM state node
/// takes: `seed` copied in once, `out` the persistent state the body
/// mutates, `in` the capture. `iterations` per frame, `step_dt` from
/// [`particle_step_dt`], `step_index` the iteration.
struct ParticleBoundary {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    params: Vec<ParamDef>,
    seeded: bool,
    pending: u32,
}

impl ParticleBoundary {
    fn new() -> Self {
        let particles = PortType::Array(ArrayType::of_known::<Particle>());
        let f32_ty = PortType::Scalar(ScalarType::F32);
        Self {
            type_id: EffectNodeType::new("test.particle_boundary"),
            inputs: vec![
                port("seed", particles, PortKind::Input, true),
                port("in", particles, PortKind::Input, true),
            ],
            outputs: vec![
                port("out", particles, PortKind::Output, false),
                port("step_dt", f32_ty, PortKind::Output, false),
                port("step_index", f32_ty, PortKind::Output, false),
            ],
            params: vec![int_param("iterations", 4.0)],
            seeded: false,
            pending: 0,
        }
    }
}

impl EffectNode for ParticleBoundary {
    node_basics!();
    fn requires(&self) -> NodeRequires {
        NodeRequires {
            gpu_encoder: true,
            state_store: false,
        }
    }
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "out")
            .then(|| {
                input_capacities
                    .iter()
                    .find(|(p, _)| *p == "seed")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }
    fn state_capture_input_ports(&self) -> &[&str] {
        &["in"]
    }
    fn persistent_output_ports(&self) -> &[&str] {
        &["out"]
    }
    fn substep_boundary(&self) -> Option<SubstepBoundaryPorts> {
        Some(PARTICLE_BOUNDARY_PORTS)
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.pending = ctx
            .params
            .get("iterations")
            .and_then(|v| v.as_u32_clamped(0))
            .unwrap_or(0);
        let (Some(seed), Some(out)) = (ctx.inputs.array("seed"), ctx.outputs.array("out")) else {
            return;
        };
        if !self.seeded {
            self.seeded = true;
            let size = seed.size.min(out.size);
            let gpu = ctx
                .gpu
                .as_deref_mut()
                .expect("particle boundary needs a GpuEncoder");
            gpu.native_enc.copy_buffer_to_buffer(seed, out, size);
        }
    }
    fn substep_iteration(&mut self, iteration: u32, scalars: &mut [f32]) -> bool {
        if iteration >= self.pending {
            return false;
        }
        scalars[0] = particle_step_dt(iteration);
        scalars[1] = iteration as f32;
        true
    }
    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // An in-place body already wrote `out`; a fresh-output body is
        // accepted by copy.
        let (Some(candidate), Some(out)) = (ctx.inputs.array("in"), ctx.outputs.array("out"))
        else {
            return;
        };
        if !candidate.ptr_eq(out) {
            let size = candidate.size.min(out.size);
            let gpu = ctx
                .gpu
                .as_deref_mut()
                .expect("particle boundary needs a GpuEncoder");
            gpu.native_enc.copy_buffer_to_buffer(candidate, out, size);
        }
    }
}

/// Copies `in` into a fresh `out` of the same capacity: an ordinary
/// temporary, which the array planner may place in storage another array
/// has released.
struct ParticleCopy {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    params: Vec<ParamDef>,
}

impl EffectNode for ParticleCopy {
    node_basics!();
    fn requires(&self) -> NodeRequires {
        NodeRequires {
            gpu_encoder: true,
            state_store: false,
        }
    }
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "out")
            .then(|| {
                input_capacities
                    .iter()
                    .find(|(p, _)| *p == "in")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(source), Some(copy)) = (ctx.inputs.array("in"), ctx.outputs.array("out")) else {
            return;
        };
        let size = source.size.min(copy.size);
        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("particle copy needs a GpuEncoder");
        gpu.native_enc.copy_buffer_to_buffer(source, copy, size);
    }
}

/// A tick publisher over particles: each frame it copies `in` into one of two
/// buffers it owns and shows the other one, last frame's copy, as `out`. Its
/// output is storage whose copy already finished, as the contract requires,
/// so it may encode on a region's async lane.
struct TickPublisher {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    params: Vec<ParamDef>,
    copies: [Option<manifold_gpu::GpuBuffer>; 2],
    /// The copy the latest evaluate wrote.
    written: usize,
}

impl EffectNode for TickPublisher {
    node_basics!();
    fn requires(&self) -> NodeRequires {
        NodeRequires {
            gpu_encoder: true,
            state_store: false,
        }
    }
    fn tick_publisher(&self) -> bool {
        true
    }
    fn provides_array_output(&self, port_name: &str) -> bool {
        port_name == "out"
    }
    fn provided_array_output(&self, port_name: &str) -> Option<&manifold_gpu::GpuBuffer> {
        if port_name == "out" {
            self.copies[self.written ^ 1].as_ref()
        } else {
            None
        }
    }
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "out")
            .then(|| {
                input_capacities
                    .iter()
                    .find(|(p, _)| *p == "in")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(source) = ctx.inputs.array("in") else {
            return;
        };
        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("tick publisher needs a GpuEncoder");
        if self.copies[0]
            .as_ref()
            .is_none_or(|copy| copy.size != source.size)
        {
            self.copies = std::array::from_fn(|_| {
                let copy = gpu.device.create_buffer(source.size, BufferUse::Upload);
                copy.zero_fill();
                Some(copy)
            });
        }
        self.written ^= 1;
        let copy = self.copies[self.written]
            .as_ref()
            .expect("copies made above");
        gpu.native_enc
            .copy_buffer_to_buffer(source, copy, source.size);
    }
}

/// Consumes particles and yields a texture so the region reaches a final
/// output; draws nothing.
struct ParticleSink {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    params: Vec<ParamDef>,
}

impl EffectNode for ParticleSink {
    node_basics!();
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
}

/// A fusable per-element sum of two f32 arrays whose output follows `a`
/// alone while it declares the default capacity (the min of its inputs):
/// the dishonest declaration the freeze compiler's capacity probe must
/// refuse (BUG-2efy, capacity probe admits an output that follows one
/// input). Never run.
struct FollowFirst {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    params: Vec<ParamDef>,
}

impl EffectNode for FollowFirst {
    node_basics!();
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
    fn fusion_kind(&self) -> crate::freeze::classify::FusionKind {
        crate::freeze::classify::FusionKind::MultiInputCoincident
    }
    fn wgsl_body(&self) -> Option<&'static str> {
        Some(
            "fn body(idx: u32, count: u32, e_a: f32, e_b: f32) -> f32 {\n    return e_a + e_b;\n}\n",
        )
    }
    fn array_output_capacity(
        &self,
        port: &str,
        _: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "a")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }
}

/// Register `type_id` as a test array source of `item`s, `max_capacity`
/// long: for record types this crate does not know.
pub fn register_array_source(
    registry: &mut PrimitiveRegistry,
    type_id: &'static str,
    item: ArrayType,
) {
    registry.register(type_id, move || Box::new(ArraySource::new(type_id, item)));
}

pub fn register_substep_test_nodes(registry: &mut PrimitiveRegistry) {
    registry.register("test.follow_first", || {
        let f32s = || PortType::Array(ArrayType::of_known::<f32>());
        Box::new(FollowFirst {
            type_id: EffectNodeType::new("test.follow_first"),
            inputs: vec![
                port("a", f32s(), PortKind::Input, true),
                port("b", f32s(), PortKind::Input, true),
            ],
            outputs: vec![port("out", f32s(), PortKind::Output, false)],
            params: Vec::new(),
        })
    });
    registry.register("test.particle_source", || {
        Box::new(ArraySource::new(
            "test.particle_source",
            ArrayType::of_known::<Particle>(),
        ))
    });
    registry.register("test.force_source", || {
        Box::new(ArraySource::new(
            "test.force_source",
            ArrayType::of_known::<[f32; 3]>(),
        ))
    });
    registry.register("test.value_source", || {
        Box::new(ArraySource::new(
            "test.value_source",
            ArrayType::of_known::<f32>(),
        ))
    });
    registry.register("test.liquid_source", || {
        Box::new(ArraySource::new(
            "test.liquid_source",
            ArrayType::of_known::<crate::particles::FluidParticle>(),
        ))
    });
    registry.register("test.count_sink", || {
        Box::new(ParticleSink {
            type_id: EffectNodeType::new("test.count_sink"),
            inputs: vec![port(
                "values",
                PortType::Array(ArrayType::of_known::<u32>()),
                PortKind::Input,
                true,
            )],
            outputs: vec![port("out", PortType::Texture2D, PortKind::Output, false)],
            params: Vec::new(),
        })
    });
    registry.register("test.value_sink", || {
        Box::new(ParticleSink {
            type_id: EffectNodeType::new("test.value_sink"),
            inputs: vec![port(
                "values",
                PortType::Array(ArrayType::of_known::<f32>()),
                PortKind::Input,
                true,
            )],
            outputs: vec![port("out", PortType::Texture2D, PortKind::Output, false)],
            params: Vec::new(),
        })
    });
    registry.register("test.word_source", || {
        Box::new(ArraySource::new(
            "test.word_source",
            ArrayType::of_known::<u32>(),
        ))
    });
    registry.register("test.liquid_sink", || {
        Box::new(ParticleSink {
            type_id: EffectNodeType::new("test.liquid_sink"),
            inputs: vec![port(
                "particles",
                PortType::Array(ArrayType::of_known::<crate::particles::FluidParticle>()),
                PortKind::Input,
                true,
            )],
            outputs: vec![port("out", PortType::Texture2D, PortKind::Output, false)],
            params: Vec::new(),
        })
    });
    registry.register("test.mesh_sink", || {
        Box::new(ParticleSink {
            type_id: EffectNodeType::new("test.mesh_sink"),
            inputs: vec![port(
                "vertices",
                PortType::Array(ArrayType::of_known::<crate::mesh::MeshVertex>()),
                PortKind::Input,
                true,
            )],
            outputs: vec![port("out", PortType::Texture2D, PortKind::Output, false)],
            params: Vec::new(),
        })
    });
    registry.register("test.particle_boundary", || {
        Box::new(ParticleBoundary::new())
    });
    registry.register("test.particle_copy", || {
        let particles = PortType::Array(ArrayType::of_known::<Particle>());
        Box::new(ParticleCopy {
            type_id: EffectNodeType::new("test.particle_copy"),
            inputs: vec![port("in", particles, PortKind::Input, true)],
            outputs: vec![port("out", particles, PortKind::Output, false)],
            params: Vec::new(),
        })
    });
    registry.register("test.tick_publisher", || {
        let particles = PortType::Array(ArrayType::of_known::<Particle>());
        Box::new(TickPublisher {
            type_id: EffectNodeType::new("test.tick_publisher"),
            inputs: vec![port("in", particles, PortKind::Input, true)],
            outputs: vec![port("out", particles, PortKind::Output, false)],
            params: Vec::new(),
            copies: [None, None],
            written: 0,
        })
    });
    registry.register("test.particle_sink", || {
        Box::new(ParticleSink {
            type_id: EffectNodeType::new("test.particle_sink"),
            inputs: vec![port(
                "particles",
                PortType::Array(ArrayType::of_known::<Particle>()),
                PortKind::Input,
                true,
            )],
            outputs: vec![port("out", PortType::Texture2D, PortKind::Output, false)],
            params: Vec::new(),
        })
    });
}
