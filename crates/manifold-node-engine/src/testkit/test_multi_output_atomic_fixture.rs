//! TEST-ONLY FIXTURE for BUG-agfh (Codegen: buffer atom with several outputs,
//! one atomic): the shape `node.grid_to_matter` takes once it reports rigid-body
//! reactions — an aliased pointwise particle output next to an atomic
//! fixed-point side output the body `atomicAdd`s into.
//!
//! Each live particle loses `drag` of its velocity; the removed momentum,
//! scaled by `fixed_point_scale` and truncated to `i32`, lands in bin
//! `idx % MOMENTUM_BINS` of `momentum` (three words per bin: x, y, z). Dead
//! particles (life <= 0) pass through and add nothing.
//!
//! Hand-rolled `PrimitiveSpec`, not `crate::primitive!`, for the reason
//! `test_camera_pointwise_fixture` gives: the macro auto-registers into the
//! global `PrimitiveFactory` / `NodeDescriptor` inventories, which the catalog
//! freshness tests walk. Tests add it to a registry explicitly with
//! `PrimitiveRegistry::register(TYPE_ID, …)`.

#![cfg(test)]

use std::borrow::Cow;
use std::sync::OnceLock;

use manifold_gpu::{GpuBinding, GpuComputePipeline};

use crate::particles::Particle;
use crate::exec::effect_node::{EffectNodeContext, EffectNodeType};
use crate::freeze::classify::FusionKind;
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::ports::{ArrayType, ChannelElementType, ChannelName, ChannelSpec, MatchMode, NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::primitive::{Primitive, PrimitiveSpec};
use crate::primitives::standalone_pipeline::standalone_pipeline;

pub const TYPE_ID: &str = "test.multi_output_atomic";
/// Accumulator bins; each holds three `i32` words (x, y, z).
pub const MOMENTUM_BINS: u32 = 4;
pub const MOMENTUM_WORDS: u32 = MOMENTUM_BINS * 3;

pub struct TestMultiOutputAtomic {
    pub pipeline: Option<GpuComputePipeline>,
}

impl TestMultiOutputAtomic {
    pub fn new() -> Self {
        Self { pipeline: None }
    }
}

impl Default for TestMultiOutputAtomic {
    fn default() -> Self {
        Self::new()
    }
}

const I32_SPECS: &[ChannelSpec] = &[ChannelSpec {
    name: ChannelName::from_str("value"),
    ty: ChannelElementType::I32,
}];

const INPUTS: &[NodeInput] = &[NodePort {
    name: Cow::Borrowed("points"),
    ty: PortType::Array(ArrayType::of_known::<Particle>()),
    kind: PortKind::Input,
    required: true,
}];
const OUTPUTS: &[NodeOutput] = &[
    NodePort {
        name: Cow::Borrowed("points_out"),
        ty: PortType::Array(ArrayType::of_known::<Particle>()),
        kind: PortKind::Output,
        required: false,
    },
    NodePort {
        name: Cow::Borrowed("momentum"),
        ty: PortType::Array(ArrayType::of_channels(I32_SPECS, MatchMode::Exact)),
        kind: PortKind::Output,
        required: false,
    },
];
const PARAMS: &[ParamDef] = &[
    ParamDef {
        name: Cow::Borrowed("drag"),
        label: "Drag",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),
        range: Some((0.0, 1.0)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("fixed_point_scale"),
        label: "Fixed-Point Scale",
        ty: ParamType::Float,
        default: ParamValue::Float(65536.0),
        range: Some((1.0, 1_048_576.0)),
        enum_values: &[],
    },
];

pub const WGSL_BODY: &str = "\
fn body(idx: u32, count: u32, e_points: Element, drag: f32, fixed_point_scale: f32) -> Element {
    var p = e_points;
    if p.life <= 0.0 {
        return p;
    }
    let removed = p.velocity * drag;
    p.velocity = p.velocity - removed;
    let base = (idx % 4u) * 3u;
    atomicAdd(&buf_momentum[base], i32(removed.x * fixed_point_scale));
    atomicAdd(&buf_momentum[base + 1u], i32(removed.y * fixed_point_scale));
    atomicAdd(&buf_momentum[base + 2u], i32(removed.z * fixed_point_scale));
    return p;
}";

impl PrimitiveSpec for TestMultiOutputAtomic {
    const TYPE_ID: &'static str = TYPE_ID;
    const PURPOSE: &'static str = "TEST FIXTURE ONLY (BUG-agfh) — removes `drag` of each live particle's velocity in place and atomically accumulates the removed momentum, fixed point, into a side output.";
    const INPUTS: &'static [NodeInput] = INPUTS;
    const OUTPUTS: &'static [NodeOutput] = OUTPUTS;
    const PARAMS: &'static [ParamDef] = PARAMS;
    // An atomic side output is a region cut (FREEZE_COMPILER_MAP.md section 4,
    // buffer-atom gates): the standalone kernel is still generated from
    // `wgsl_body`, it just never joins a fused region.
    const FUSION_KIND: FusionKind = FusionKind::Boundary;
    const DEPTH_RULE: crate::scene::depth_rule::DepthRule =
        crate::scene::depth_rule::DepthRule::Terminal;
    const WGSL_BODY: Option<&'static str> = Some(WGSL_BODY);
    const ATOMIC_OUTPUTS: &'static [&'static str] = &["momentum"];

    fn cached_type_id() -> &'static EffectNodeType {
        static CELL: OnceLock<EffectNodeType> = OnceLock::new();
        CELL.get_or_init(|| EffectNodeType::new(Self::TYPE_ID))
    }
}

/// Generated uniform layout: the two f32 params in PARAMS order, then the
/// injected `dispatch_count`, padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniforms {
    pub drag: f32,
    pub fixed_point_scale: f32,
    pub dispatch_count: u32,
    pub _pad0: u32,
}

impl Primitive for TestMultiOutputAtomic {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        match port_name {
            "points_out" => input_capacities
                .iter()
                .find(|(p, _)| *p == "points")
                .map(|(_, n)| *n),
            "momentum" => Some(MOMENTUM_WORDS),
            _ => None,
        }
    }

    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        &[("points", "points_out")]
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let drag = ctx.scalar_or_param("drag", 0.5);
        let fixed_point_scale = ctx.scalar_or_param("fixed_point_scale", 65536.0);
        let Some(points) = ctx.inputs.array("points") else { return };
        let Some(momentum) = ctx.outputs.array("momentum") else { return };
        let count = (points.size / std::mem::size_of::<Particle>() as u64) as u32;
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = Uniforms { drag, fixed_point_scale, dispatch_count: count, _pad0: 0 };
        // `points` / `points_out` alias one buffer (aliased_array_io): bind it
        // to the read input (1) and the read_write output (2). Pointwise, so
        // the in-place write is race-free.
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: points, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: points, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: momentum, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            TYPE_ID,
        );
    }
}

/// CPU reference for one dispatch over `points`: the updated particles and
/// the momentum words the kernel adds. Exact for inputs whose products are
/// exact in f32 (power-of-two `drag` and `fixed_point_scale`).
pub fn cpu_reference(points: &[Particle], drag: f32, fixed_point_scale: f32) -> (Vec<Particle>, Vec<i32>) {
    let mut out = points.to_vec();
    let mut momentum = vec![0i32; MOMENTUM_WORDS as usize];
    for (idx, p) in out.iter_mut().enumerate() {
        if p.life <= 0.0 {
            continue;
        }
        let base = (idx % MOMENTUM_BINS as usize) * 3;
        for c in 0..3 {
            let removed = p.velocity[c] * drag;
            p.velocity[c] -= removed;
            momentum[base + c] = momentum[base + c].wrapping_add((removed * fixed_point_scale) as i32);
        }
    }
    (out, momentum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::effect_node::EffectNode;
    use crate::freeze::codegen::standalone_for_spec;

    /// The generated kernel for the shape: the aliased element output is
    /// written by the wrapper, the atomic side output is `array<atomic<i32>>`
    /// written only by the body, and the result parses and validates in naga.
    #[test]
    fn generates_aliased_output_with_atomic_side_output() {
        let wgsl = standalone_for_spec::<TestMultiOutputAtomic>()
            .expect("multi-output atomic shape generates");
        assert!(
            wgsl.contains("var<storage, read_write> buf_points_out: array<Element>;"),
            "plain output bound as a plain element array:\n{wgsl}"
        );
        assert!(
            wgsl.contains("var<storage, read_write> buf_momentum: array<atomic<i32>>;"),
            "atomic side output bound as array<atomic<i32>>:\n{wgsl}"
        );
        assert!(
            wgsl.contains("    buf_points_out[idx] = body("),
            "wrapper writes the one plain output at idx:\n{wgsl}"
        );
        assert!(!wgsl.contains("BufferOutputs"), "one plain output needs no output struct:\n{wgsl}");
        assert!(!wgsl.contains("buf_momentum[idx]"), "the wrapper never writes the atomic output:\n{wgsl}");

        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("generated kernel must parse: {e:?}\n{wgsl}"));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("generated kernel must validate: {e:?}\n{wgsl}"));

        // The Params block matches the Rust uniform run() packs. This is the
        // fixture's own layout proof: uniform_layout_proof can't construct a
        // cfg(test) atom, so it skips this file.
        let params = module
            .global_variables
            .iter()
            .find(|(_, g)| {
                g.space == naga::AddressSpace::Uniform
                    && g.binding.as_ref().is_some_and(|b| b.group == 0 && b.binding == 0)
            })
            .expect("Params uniform at binding 0")
            .1;
        let naga::TypeInner::Struct { members, span } = &module.types[params.ty].inner else {
            panic!("Params binding is not a struct");
        };
        let shader: Vec<(&str, u32)> = members
            .iter()
            .map(|m| (m.name.as_deref().unwrap_or(""), m.offset))
            .collect();
        assert_eq!(
            &shader[..3],
            &[
                ("drag", std::mem::offset_of!(Uniforms, drag) as u32),
                ("fixed_point_scale", std::mem::offset_of!(Uniforms, fixed_point_scale) as u32),
                ("dispatch_count", std::mem::offset_of!(Uniforms, dispatch_count) as u32),
            ],
            "{wgsl}"
        );
        assert_eq!(*span as usize, std::mem::size_of::<Uniforms>(), "{wgsl}");
    }

    /// Boundary, as every atom with an atomic output must be (the
    /// `atomic_output_atoms_are_boundaries` meta-test holds registered atoms
    /// to the same rule).
    #[test]
    fn declares_boundary() {
        let node = TestMultiOutputAtomic::new();
        assert_eq!(node.fusion_kind(), FusionKind::Boundary);
        assert_eq!(node.atomic_outputs(), &["momentum"]);
    }

    #[test]
    fn cpu_reference_moves_removed_momentum_into_bins() {
        let mut p = <Particle as bytemuck::Zeroable>::zeroed();
        p.life = 1.0;
        p.velocity = [1.0, -0.5, 0.25];
        let mut dead = p;
        dead.life = 0.0;
        let (out, m) = cpu_reference(&[p, dead, p, p, p], 0.5, 4.0);
        assert_eq!(out[0].velocity, [0.5, -0.25, 0.125]);
        assert_eq!(out[1].velocity, [1.0, -0.5, 0.25], "dead particle untouched");
        // Particles 0 and 4 share bin 0; the dead particle adds nothing to bin 1.
        assert_eq!(&m[0..3], &[4, -2, 0]);
        assert_eq!(&m[3..6], &[0, 0, 0]);
        assert_eq!(&m[6..9], &[2, -1, 0]);
    }
}
