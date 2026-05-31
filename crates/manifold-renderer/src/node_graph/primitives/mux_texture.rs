//! `node.mux_texture` — dynamic N-way texture selector.
//!
//! A variadic-input primitive: the `num_inputs` param sets how many
//! `Texture2D` inputs (`in_0` … `in_{num_inputs-1}`) the node exposes, and a
//! scalar `selector` (Scalar F32) chooses one of them. The selector rounds to
//! the nearest integer, clamps to `[0, num_inputs)`, and forwards the matching
//! input's contents into the output. Changing `num_inputs` rebuilds the port
//! list (via [`EffectNode::reconfigure`]) so the node grows / shrinks in the
//! editor and `compile()` sees the new shape.
//!
//! Designed for clip-trigger-style preset cycling (Plasma's 8 patterns,
//! ConcentricTunnel's 6 shapes) and any "pick 1 of N textures" selection
//! (Infrared's 10 palette ramps). The host wires
//! `system.generator_input.trigger_count → selector` and each sub-graph
//! variant into `in_0..in_N`. `num_inputs` defaults to 8, so every preset
//! authored against the previous fixed `in_0..in_7` shape loads unchanged.
//!
//! Cost model: short-circuits via `selected_input_branch`. When the
//! `selector` input port is UNWIRED, the executor reads the inline `selector`
//! param, resolves it to `in_N`, and prunes the producer subgraphs for every
//! other `in_K` from this frame's dispatch — the 5-mode oily-fluid case goes
//! from 5× render cost to 1×. When the selector port IS wired (selector value
//! depends on runtime-computed scalars), every input subgraph runs since we
//! can't predict the selected branch without first running its producer.
//! Live-perform with an outer-card selector slider — the dominant case — uses
//! inline params so the optimisation fires.

use std::sync::OnceLock;

use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc};

use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, ParamValues,
};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::primitive::PrimitiveDescription;

pub const MUX_TEXTURE_TYPE_ID: &str = "node.mux_texture";

/// Hard cap on input count — bounds the static port-name table below. The
/// node only ever exposes `num_inputs` of these; 32 is far beyond any real
/// live-switch need (Infrared's 10 palettes is the current high-water mark).
const MAX_INPUTS: usize = 32;

/// Default input count. 8 matches the previous fixed `in_0..in_7` shape, so
/// every preset authored before this node went dynamic loads with the same
/// ports and zero migration.
const DEFAULT_INPUTS: u32 = 8;

/// Static port-name table. A dynamic-port node can't `format!` a
/// `&'static str` per instance, so the names live here and the live port
/// list (and `selected_input_branch`) slice `&IN_PORT_NAMES[..num_inputs]`.
const IN_PORT_NAMES: [&str; MAX_INPUTS] = [
    "in_0", "in_1", "in_2", "in_3", "in_4", "in_5", "in_6", "in_7", "in_8",
    "in_9", "in_10", "in_11", "in_12", "in_13", "in_14", "in_15", "in_16",
    "in_17", "in_18", "in_19", "in_20", "in_21", "in_22", "in_23", "in_24",
    "in_25", "in_26", "in_27", "in_28", "in_29", "in_30", "in_31",
];

const MUX_TEXTURE_WGSL: &str = r"
@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let color = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    textureStore(output_tex, vec2<i32>(id.xy), color);
}
";

const SELECTOR_INPUT: NodeInput = NodePort {
    name: "selector",
    ty: PortType::Scalar(ScalarType::F32),
    kind: PortKind::Input,
    required: true,
};

const MUX_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: "out",
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const MUX_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: "selector",
        label: "Selector",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, (MAX_INPUTS - 1) as f32)),
        enum_values: &[],
    },
    ParamDef {
        name: "num_inputs",
        label: "Input Count",
        ty: ParamType::Int,
        default: ParamValue::Float(DEFAULT_INPUTS as f32),
        range: Some((2.0, MAX_INPUTS as f32)),
        enum_values: &[],
    },
];

pub struct MuxTexture {
    /// Live input ports: `[selector, in_0, …, in_{num_inputs-1}]`. Rebuilt by
    /// `reconfigure` whenever `num_inputs` changes.
    inputs: Vec<NodeInput>,
    pipeline: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
}

impl MuxTexture {
    pub fn new() -> Self {
        let mut m = Self {
            inputs: Vec::new(),
            pipeline: None,
            sampler: None,
        };
        m.rebuild_ports(DEFAULT_INPUTS);
        m
    }

    /// (Re)build the input port list for `n` texture inputs (clamped to
    /// `[1, MAX_INPUTS]`). Always leads with the required `selector` port.
    fn rebuild_ports(&mut self, n: u32) {
        let n = n.clamp(1, MAX_INPUTS as u32) as usize;
        let mut ports = Vec::with_capacity(n + 1);
        ports.push(SELECTOR_INPUT);
        for &name in &IN_PORT_NAMES[..n] {
            ports.push(NodePort {
                name,
                ty: PortType::Texture2D,
                kind: PortKind::Input,
                required: false,
            });
        }
        self.inputs = ports;
    }

    /// Number of `in_N` texture ports currently exposed (excludes the
    /// leading `selector` port).
    fn num_inputs(&self) -> usize {
        self.inputs.len().saturating_sub(1)
    }

    /// AI-composition surface metadata.
    pub fn description() -> PrimitiveDescription {
        PrimitiveDescription {
            type_id: MUX_TEXTURE_TYPE_ID,
            purpose: "Dynamic N-way Texture2D selector. `num_inputs` sets how many in_0..in_N ports exist; the `selector` scalar (rounded, clamped to [0, num_inputs)) routes the matching input to the output. Use for clip-trigger preset cycling and any pick-1-of-N texture selection (e.g. Infrared's 10 palette ramps) — wire generator_input.trigger_count to selector and each variant sub-graph to in_0..in_N.",
            composition_notes: "num_inputs (default 8) rebuilds the port list, so the node grows/shrinks in the editor. Selector rounds to nearest int, clamps to [0, num_inputs). selector is port-shadows-param: the inline param drives the choice when no wire is connected. If the selected in_N isn't wired the node falls back to in_0; if every in_N is unwired the output clears to opaque black. Acts as a switch at the executor level via selected_input_branch — with the selector port unwired, only the selected branch's producer subgraph dispatches each frame.",
            examples: &[],
            inputs: &[],
            outputs: &MUX_OUTPUTS,
            params: &MUX_PARAMS,
        }
    }
}

impl Default for MuxTexture {
    fn default() -> Self {
        Self::new()
    }
}

fn cached_type_id() -> &'static EffectNodeType {
    static CELL: OnceLock<EffectNodeType> = OnceLock::new();
    CELL.get_or_init(|| EffectNodeType::new(MUX_TEXTURE_TYPE_ID))
}

/// Resolve a selector value to an `in_N` index, clamped to `[0, count)`.
/// Shared by `evaluate` (texture pick) and `selected_input_branch`
/// (executor live-set pruning) so the two can't drift on rounding.
fn resolve_selector_index(selector: f32, count: usize) -> usize {
    let max = count.saturating_sub(1) as f32;
    selector.round().clamp(0.0, max) as usize
}

impl EffectNode for MuxTexture {
    fn type_id(&self) -> &EffectNodeType {
        cached_type_id()
    }

    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }

    fn outputs(&self) -> &[NodeOutput] {
        &MUX_OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &MUX_PARAMS
    }

    fn reconfigure(&mut self, params: &ParamValues) {
        let n = params
            .get("num_inputs")
            .and_then(|v| v.as_scalar())
            .map(|f| f.round().max(1.0) as u32)
            .unwrap_or(DEFAULT_INPUTS);
        if n as usize != self.num_inputs() {
            self.rebuild_ports(n);
        }
    }

    fn selected_input_branch(
        &self,
        params: &ParamValues,
        wired_inputs: &[&str],
    ) -> Option<&'static str> {
        // Wire-driven selector: can't pre-resolve. Every branch must run so
        // whichever the wire eventually resolves to is ready.
        if wired_inputs.contains(&"selector") {
            return None;
        }
        let selector = params
            .get("selector")
            .and_then(|v| v.as_scalar())
            .unwrap_or(0.0);
        Some(IN_PORT_NAMES[resolve_selector_index(selector, self.num_inputs())])
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Read the selector. Port-shadows-param: wired value overrides the
        // inline param, otherwise the param drives the choice.
        let selector = match ctx.inputs.scalar("selector") {
            Some(ParamValue::Float(f)) => f,
            _ => match ctx.params.get("selector") {
                Some(ParamValue::Float(f)) => *f,
                _ => 0.0,
            },
        };
        let idx = resolve_selector_index(selector, self.num_inputs());

        // Selected input, falling back to in_0 if the selected slot is unwired.
        let source = ctx
            .inputs
            .texture_2d(IN_PORT_NAMES[idx])
            .or_else(|| ctx.inputs.texture_2d("in_0"));

        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out.width, out.height);

        // Every in_N unwired: clear the output to opaque black so the gap is
        // visually obvious instead of leaving sticky pool contents behind.
        let Some(source) = source else {
            let gpu = ctx.gpu_encoder();
            gpu.clear_texture(out, 0.0, 0.0, 0.0, 1.0);
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(MUX_TEXTURE_WGSL, "cs_main", "node.mux_texture")
        });
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Texture {
                    binding: 0,
                    texture: source,
                },
                GpuBinding::Sampler {
                    binding: 1,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: out,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.mux_texture",
        );
    }
}

inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: MUX_TEXTURE_TYPE_ID,
        create: || Box::new(MuxTexture::new()),
        picker: Some(crate::node_graph::palette::PickerInfo {
            label: "Switch (texture)",
            category: crate::node_graph::palette::PaletteCategory::Atom,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params_with(num_inputs: f32, selector: f32) -> ParamValues {
        let mut p = ParamValues::default();
        p.insert("num_inputs", ParamValue::Float(num_inputs));
        p.insert("selector", ParamValue::Float(selector));
        p
    }

    #[test]
    fn defaults_to_eight_inputs_plus_selector() {
        let m = MuxTexture::new();
        // selector + in_0..in_7
        assert_eq!(m.inputs().len(), 9);
        assert_eq!(m.inputs()[0].name, "selector");
        assert!(m.inputs()[0].required);
        for (i, port) in m.inputs().iter().enumerate().skip(1) {
            assert_eq!(port.name, IN_PORT_NAMES[i - 1]);
            assert!(!port.required);
            assert_eq!(port.ty, PortType::Texture2D);
        }
    }

    #[test]
    fn reconfigure_grows_and_shrinks_the_port_list() {
        let mut m = MuxTexture::new();
        m.reconfigure(&params_with(12.0, 0.0));
        assert_eq!(m.num_inputs(), 12);
        assert_eq!(m.inputs().len(), 13);
        assert_eq!(m.inputs().last().unwrap().name, "in_11");

        m.reconfigure(&params_with(3.0, 0.0));
        assert_eq!(m.num_inputs(), 3);
        assert_eq!(m.inputs().last().unwrap().name, "in_2");
    }

    #[test]
    fn reconfigure_clamps_to_max_inputs() {
        let mut m = MuxTexture::new();
        m.reconfigure(&params_with(999.0, 0.0));
        assert_eq!(m.num_inputs(), MAX_INPUTS);
    }

    #[test]
    fn selector_resolves_and_clamps_within_current_count() {
        let mut m = MuxTexture::new();
        m.reconfigure(&params_with(10.0, 0.0));
        let node: &dyn EffectNode = &m;
        // In range.
        assert_eq!(node.selected_input_branch(&params_with(10.0, 9.0), &[]), Some("in_9"));
        // Over the current count clamps to the last input.
        assert_eq!(node.selected_input_branch(&params_with(10.0, 50.0), &[]), Some("in_9"));
        // Rounds.
        assert_eq!(node.selected_input_branch(&params_with(10.0, 2.6), &[]), Some("in_3"));
        // Negative clamps to in_0.
        assert_eq!(node.selected_input_branch(&params_with(10.0, -5.0), &[]), Some("in_0"));
    }

    #[test]
    fn wired_selector_disables_short_circuit() {
        let m = MuxTexture::new();
        let node: &dyn EffectNode = &m;
        let wired = ["selector", "in_0", "in_1"];
        assert_eq!(node.selected_input_branch(&params_with(8.0, 2.0), &wired), None);
    }

    #[test]
    fn registers_with_palette_type_id() {
        let m = MuxTexture::new();
        let node: &dyn EffectNode = &m;
        assert_eq!(node.type_id().as_str(), "node.mux_texture");
    }
}
