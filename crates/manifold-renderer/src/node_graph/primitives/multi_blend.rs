//! `node.multi_blend` — per-pixel weighted-sum of N textures.
//!
//! The dynamic-input generalisation of the old fixed-five `node.texture_sum_5`:
//! `out = (in_0 + in_1 + … + in_{N-1}) / divisor`, all channels. `num_inputs`
//! sets how many `Texture2D` ports (`in_0` … `in_{N-1}`) the node exposes,
//! rebuilt via [`EffectNode::reconfigure`] exactly like `node.switch_texture`.
//! `divisor = 1` (default) is a plain compose-add; `divisor = N` averages.
//!
//! Unwired inputs simply drop out of the sum (contribute nothing), so the
//! node is forgiving to wire incrementally. The summing shader is generated
//! for the number of *wired* inputs and the pipeline is cached per count, so
//! a 2-input blend and an 8-input blend each compile a tight kernel with no
//! dead taps.

use std::borrow::Cow;
use ahash::AHashMap;
use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc};

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, ParamValues};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};

pub const MULTI_BLEND_TYPE_ID: &str = "node.multi_blend";

/// Hard cap on input count — bounds the static port-name table. 8 covers any
/// real multi-tap composite (the old fixed node summed five).
const MAX_INPUTS: usize = 8;

/// Default input count. 5 matches the old `node.texture_sum_5` shape.
const DEFAULT_INPUTS: u32 = 5;

/// Static port-name table — a dynamic-port node can't `format!` a
/// `&'static str` per instance, so the live port list slices this.
const IN_PORT_NAMES: [&str; MAX_INPUTS] = [
    "in_0", "in_1", "in_2", "in_3", "in_4", "in_5", "in_6", "in_7",
];

const MULTI_BLEND_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const MULTI_BLEND_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: Cow::Borrowed("num_inputs"),
        label: "Input Count",
        ty: ParamType::Int,
        default: ParamValue::Float(DEFAULT_INPUTS as f32),
        range: Some((2.0, MAX_INPUTS as f32)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("divisor"),
        label: "Divisor",
        ty: ParamType::Float,
        default: ParamValue::Float(1.0),
        range: Some((0.0, 100.0)),
        enum_values: &[],
    },
];

pub struct MultiBlend {
    /// Live input ports: `in_0 … in_{num_inputs-1}`. Rebuilt by `reconfigure`.
    inputs: Vec<NodeInput>,
    /// Sum kernels keyed by wired-input count. Compiled lazily on first use.
    pipelines: AHashMap<usize, GpuComputePipeline>,
    sampler: Option<GpuSampler>,
}

impl MultiBlend {
    pub fn new() -> Self {
        let mut m = Self {
            inputs: Vec::new(),
            pipelines: AHashMap::new(),
            sampler: None,
        };
        m.rebuild_ports(DEFAULT_INPUTS);
        m
    }

    /// (Re)build the input port list for `n` texture inputs, clamped to
    /// `[2, MAX_INPUTS]`. All inputs are optional — an unwired input drops
    /// out of the sum rather than blocking the dispatch.
    fn rebuild_ports(&mut self, n: u32) {
        let n = n.clamp(2, MAX_INPUTS as u32) as usize;
        let mut ports = Vec::with_capacity(n);
        for &name in &IN_PORT_NAMES[..n] {
            ports.push(NodePort {
                name: std::borrow::Cow::Borrowed(name),
                ty: PortType::Texture2D,
                kind: PortKind::Input,
                required: false,
            });
        }
        self.inputs = ports;
    }

    fn num_inputs(&self) -> usize {
        self.inputs.len()
    }

    /// Generate a summing kernel for exactly `k` texture inputs. Bindings:
    /// 0 = uniform (divisor), 1 = sampler, 2..2+k = the k textures,
    /// 2+k = the storage output.
    fn shader_for(k: usize) -> String {
        let mut s = String::new();
        s.push_str("struct U { divisor: f32, _p0: f32, _p1: f32, _p2: f32, };\n");
        s.push_str("@group(0) @binding(0) var<uniform> u: U;\n");
        s.push_str("@group(0) @binding(1) var samp: sampler;\n");
        for i in 0..k {
            s.push_str(&format!(
                "@group(0) @binding({}) var t{}: texture_2d<f32>;\n",
                2 + i,
                i
            ));
        }
        s.push_str(&format!(
            "@group(0) @binding({}) var output_tex: texture_storage_2d<rgba16float, write>;\n",
            2 + k
        ));
        s.push_str("@compute @workgroup_size(16, 16)\n");
        s.push_str("fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {\n");
        s.push_str("    let dims = textureDimensions(output_tex);\n");
        s.push_str("    if id.x >= dims.x || id.y >= dims.y { return; }\n");
        s.push_str("    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);\n");
        s.push_str("    var sum = vec4<f32>(0.0, 0.0, 0.0, 0.0);\n");
        for i in 0..k {
            s.push_str(&format!(
                "    sum = sum + textureSampleLevel(t{}, samp, uv, 0.0);\n",
                i
            ));
        }
        // Divide-by-zero clamps the output to 0 (matches the old texture_sum_5).
        s.push_str("    var outc = vec4<f32>(0.0, 0.0, 0.0, 0.0);\n");
        s.push_str("    if abs(u.divisor) > 1e-6 { outc = sum / u.divisor; }\n");
        s.push_str("    textureStore(output_tex, vec2<i32>(id.xy), outc);\n");
        s.push_str("}\n");
        s
    }
}

impl Default for MultiBlend {
    fn default() -> Self {
        Self::new()
    }
}

fn cached_type_id() -> &'static EffectNodeType {
    use std::sync::OnceLock;
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(MULTI_BLEND_TYPE_ID))
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MultiBlendUniforms {
    divisor: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

impl EffectNode for MultiBlend {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::CombineNearest
    }
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        Some(crate::node_graph::freeze::classify::BoundaryReason::Blocked)
    }

    /// PARAM_RANGE_CONTRACT_DESIGN.md D6/§2 mechanical grant: `num_inputs`
    /// sizes the live port allocation — `reconfigure` (this file, line 191)
    /// clamps to `[2, MAX_INPUTS]` before `rebuild_ports` slices the static
    /// `IN_PORT_NAMES` table.
    fn param_contract(&self, param_name: &str) -> Option<manifold_core::effects::RangeContract> {
        match param_name {
            "num_inputs" => Some(manifold_core::effects::RangeContract {
                min: Some(2.0),
                max: Some(MAX_INPUTS as f32),
                reason: manifold_core::effects::RangeReason::Count,
            }),
            _ => None,
        }
    }

    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }

    fn outputs(&self) -> &[NodeOutput] {
        &MULTI_BLEND_OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &MULTI_BLEND_PARAMS
    }

    fn reconfigure(&mut self, params: &ParamValues) {
        let n = params
            .get("num_inputs")
            .and_then(|v| v.as_scalar())
            .map(|f| f.round().max(2.0) as u32)
            .unwrap_or(DEFAULT_INPUTS);
        if n.clamp(2, MAX_INPUTS as u32) as usize != self.num_inputs() {
            self.rebuild_ports(n);
        }
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let divisor = match ctx.params.get("divisor") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };

        // Collect the wired inputs in port order. Unwired inputs drop out.
        let mut sources = Vec::with_capacity(self.num_inputs());
        for &name in &IN_PORT_NAMES[..self.num_inputs()] {
            if let Some(tex) = ctx.inputs.texture_2d(name) {
                sources.push(tex);
            }
        }

        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out.width, out.height);
        if w == 0 || h == 0 {
            return;
        }

        // No inputs wired: clear to opaque black so the gap is visible rather
        // than leaving stale pool contents behind.
        let k = sources.len();
        if k == 0 {
            let gpu = ctx.gpu_encoder();
            gpu.clear_texture(out, 0.0, 0.0, 0.0, 1.0);
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipelines.entry(k).or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(&MultiBlend::shader_for(k), "cs_main", "node.multi_blend")
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        let uniforms = MultiBlendUniforms {
            divisor,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };

        let mut bindings = Vec::with_capacity(k + 3);
        bindings.push(GpuBinding::Bytes {
            binding: 0,
            data: bytemuck::bytes_of(&uniforms),
        });
        bindings.push(GpuBinding::Sampler {
            binding: 1,
            sampler,
        });
        for (i, tex) in sources.iter().enumerate() {
            bindings.push(GpuBinding::Texture {
                binding: 2 + i as u32,
                texture: tex,
            });
        }
        bindings.push(GpuBinding::Texture {
            binding: 2 + k as u32,
            texture: out,
        });

        gpu.native_enc.dispatch_compute(
            pipeline,
            &bindings,
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.multi_blend",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: MULTI_BLEND_TYPE_ID,
        create: || Box::new(MultiBlend::new()),
        picker: Some(crate::node_graph::palette::PickerInfo {
            label: "Multi Blend",
            category: crate::node_graph::palette::PaletteCategory::Atom,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params_with(num_inputs: f32) -> ParamValues {
        let mut p = ParamValues::default();
        p.insert(std::borrow::Cow::Borrowed("num_inputs"), ParamValue::Float(num_inputs));
        p
    }

    #[test]
    fn defaults_to_five_optional_inputs() {
        let m = MultiBlend::new();
        assert_eq!(m.inputs().len(), 5);
        for (i, port) in m.inputs().iter().enumerate() {
            assert_eq!(port.name, IN_PORT_NAMES[i]);
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Texture2D);
        }
    }

    #[test]
    fn reconfigure_grows_and_shrinks_and_clamps() {
        let mut m = MultiBlend::new();
        m.reconfigure(&params_with(8.0));
        assert_eq!(m.num_inputs(), 8);
        assert_eq!(m.inputs().last().unwrap().name, "in_7");

        m.reconfigure(&params_with(2.0));
        assert_eq!(m.num_inputs(), 2);

        m.reconfigure(&params_with(999.0));
        assert_eq!(m.num_inputs(), MAX_INPUTS);
    }

    #[test]
    fn shader_declares_k_textures_and_sums_them() {
        let src = MultiBlend::shader_for(3);
        assert!(src.contains("var t0: texture_2d<f32>"));
        assert!(src.contains("var t2: texture_2d<f32>"));
        assert!(!src.contains("var t3:"));
        // output binding follows the k textures (2 + k).
        assert!(src.contains("@binding(5) var output_tex"));
        assert_eq!(src.matches("textureSampleLevel").count(), 3);
    }

    #[test]
    fn registers_with_palette_type_id() {
        let m = MultiBlend::new();
        let node: &dyn EffectNode = &m;
        assert_eq!(node.type_id().as_str(), "node.multi_blend");
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! The summing kernel is generated per input count, so no `include_str!`
    //! and no preset exercises it. Compile every variant on the real GPU —
    //! Metal compiles the WGSL at pipeline creation, so a malformed generated
    //! shader fails here rather than silently at first live use.
    use super::*;

    #[test]
    fn generated_shaders_compile_for_every_input_count() {
        let device = crate::test_device();
        for k in 1..=MAX_INPUTS {
            let src = MultiBlend::shader_for(k);
            let _ = device.create_compute_pipeline(&src, "cs_main", "node.multi_blend test");
        }
    }
}
