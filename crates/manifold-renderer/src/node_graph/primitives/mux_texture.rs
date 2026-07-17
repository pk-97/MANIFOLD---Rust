//! `node.switch_texture` — dynamic N-way texture selector.
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

use std::borrow::Cow;
use std::sync::OnceLock;

use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSampler, GpuSamplerDesc};

use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, ParamValues,
};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::node_graph::primitive::PrimitiveDescription;

pub const MUX_TEXTURE_TYPE_ID: &str = "node.switch_texture";

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
    name: Cow::Borrowed("selector"),
    ty: PortType::Scalar(ScalarType::F32),
    kind: PortKind::Input,
    required: true,
};

const MUX_OUTPUTS: [NodeOutput; 1] = [NodePort {
    name: Cow::Borrowed("out"),
    ty: PortType::Texture2D,
    kind: PortKind::Output,
    required: false,
}];

const MUX_PARAMS: [ParamDef; 2] = [
    ParamDef {
        name: Cow::Borrowed("selector"),
        label: "Selector",
        ty: ParamType::Float,
        default: ParamValue::Float(0.0),
        range: Some((0.0, (MAX_INPUTS - 1) as f32)),
        enum_values: &[],
    },
    ParamDef {
        name: Cow::Borrowed("num_inputs"),
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
    /// Wired-selector LATCH: the selector value `evaluate` resolved last
    /// frame. `selected_input_branch` runs during liveness — before any node
    /// evaluates — so a wired selector's current-frame value is unknowable
    /// there. Pruning AND the texture pick both use this latched value, which
    /// keeps them consistent: a selector edge lands one frame later instead
    /// of ever reading a branch whose producer was pruned (a stale or unbound
    /// texture on stage). `None` until the first wired evaluate — that frame
    /// every branch stays live.
    ///
    /// Memo interaction: the latch makes a wired mux's output depend on LAST
    /// frame's wire value, which would poison a memo hold — but a wired
    /// selector's producer chain (trigger logic, LFOs) is never pure, so the
    /// hoistable closure never includes a wired-selector mux and the memoizer
    /// never holds one. The `is_pure` declaration below is only ever
    /// exercised on the inline-selector shape, where the latch is `None`.
    latched_selector: Option<f32>,
    pipeline: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3b/BUG-197 — hash of
    /// (effective selector index actually rendered, selected source slot's
    /// write generation, selected source texture identity, output texture
    /// identity, executor rebuild epoch) from the last frame this node
    /// actually dispatched (or cleared). A full-key match means every input
    /// this evaluate reads is provably the same content it read last time,
    /// so the dispatch (or the clear-fallback) is skipped and
    /// `mark_outputs_unchanged()` is declared instead — this is what lets
    /// `render_scene`'s IBL cache key (D7/P3) actually stabilize across a
    /// real glTF import's `bake_environment -> switch_texture -> render_scene`
    /// chain, where this node used to re-emit unconditionally every frame.
    /// The unwired-fallback (`in_0`) and all-unwired (clear-to-black) cases
    /// fold into the same key (a distinct hash arm for "no source bound")
    /// rather than special-casing them — same rule as any resolved source.
    last_gate_key: Option<u64>,
}

impl MuxTexture {
    pub fn new() -> Self {
        let mut m = Self {
            inputs: Vec::new(),
            latched_selector: None,
            pipeline: None,
            sampler: None,
            last_gate_key: None,
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
                name: std::borrow::Cow::Borrowed(name),
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
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        Some(crate::node_graph::freeze::classify::BoundaryReason::Blocked)
    }

    /// PARAM_RANGE_CONTRACT_DESIGN.md D6/§2 mechanical grant: both params on
    /// this hand-`impl EffectNode` primitive address/size a discrete
    /// resource, evidenced by this file's own defensive clamps (curated in
    /// `freeze::classify::tests::every_range_contract_names_a_real_boundary`).
    /// `selector` — `resolve_selector_index` (this file, line 197) rounds and
    /// clamps to `[0, num_inputs)`; the absolute reachable index space is
    /// `[0, MAX_INPUTS - 1]`. `num_inputs` — `rebuild_ports` (this file, line
    /// 149) clamps `n` to `[1, MAX_INPUTS]` before slicing the static
    /// `IN_PORT_NAMES` table.
    fn param_contract(&self, param_name: &str) -> Option<manifold_core::effects::RangeContract> {
        match param_name {
            "selector" => Some(manifold_core::effects::RangeContract {
                min: Some(0.0),
                max: Some((MAX_INPUTS - 1) as f32),
                reason: manifold_core::effects::RangeReason::Index,
            }),
            "num_inputs" => Some(manifold_core::effects::RangeContract {
                min: Some(1.0),
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
        &MUX_OUTPUTS
    }

    fn parameters(&self) -> &[ParamDef] {
        &MUX_PARAMS
    }

    /// PURE: the output is a copy of the selected input — selector param (or
    /// wired scalar) + input textures fully determine it. No time, no state.
    /// With a static selector and static (hoistable) inputs the whole
    /// ramp-constellation → mux chain goes quiet (Infrared's palette bank).
    fn is_pure(&self) -> bool {
        true
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
        // Wire-driven selector: its current-frame value can't be known at
        // liveness time, but the LATCHED value (last frame's resolved wire,
        // which `evaluate` will also render with this frame) can prune the
        // other branches — BasicShapes' trigger-wired selector stops paying
        // for all three shapes. First frame (no latch yet): every branch
        // runs.
        if wired_inputs.contains(&"selector") {
            let latched = self.latched_selector?;
            return Some(IN_PORT_NAMES[resolve_selector_index(latched, self.num_inputs())]);
        }
        let selector = params
            .get("selector")
            .and_then(|v| v.as_scalar())
            .unwrap_or(0.0);
        Some(IN_PORT_NAMES[resolve_selector_index(selector, self.num_inputs())])
    }

    /// Passthrough alias: the selected-branch copy is a per-pixel identity, so
    /// when the selector is INLINE (unwired) the executor aliases the chosen
    /// input's texture onto the output — zero GPU work — instead of running
    /// the full-canvas sampled copy. The executor only installs the alias when
    /// the slots' dims + format match (a 256×1 LUT ramp muxed up to canvas
    /// still resamples through `evaluate`). Wired selectors keep the dispatch:
    /// `evaluate` is where the selector latch updates, and an aliased skip
    /// would freeze it.
    fn skip_passthrough(
        &self,
        params: &ParamValues,
        wired_inputs: &[&str],
    ) -> Option<(&'static str, &'static str)> {
        if wired_inputs.contains(&"selector") {
            return None;
        }
        let selector = params.get("selector").and_then(|v| v.as_scalar())?;
        let port = IN_PORT_NAMES[resolve_selector_index(selector, self.num_inputs())];
        // Alias only when the selected slot is actually wired — the unwired
        // fallback (in_0 / clear-to-black) must keep running evaluate.
        if !wired_inputs.contains(&port) {
            return None;
        }
        Some((port, "out"))
    }

    /// The alias source is dynamic (any wired `in_N`), so the planner extends
    /// every wired texture input's lifetime to `out`'s last reader.
    fn variadic_skip_passthrough_out(&self) -> Option<&'static str> {
        Some("out")
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        // Read the selector. Port-shadows-param: wired value overrides the
        // inline param, otherwise the param drives the choice. A WIRED
        // selector renders with the LATCHED (last frame's) value — the same
        // value `selected_input_branch` pruned with this frame — so a pruned
        // branch is never read; the selector edge lands one frame later.
        let selector = match ctx.inputs.scalar("selector") {
            Some(ParamValue::Float(f)) => {
                let effective = self.latched_selector.unwrap_or(f);
                self.latched_selector = Some(f);
                effective
            }
            _ => {
                self.latched_selector = None;
                match ctx.params.get("selector") {
                    Some(ParamValue::Float(f)) => *f,
                    _ => 0.0,
                }
            }
        };
        let idx = resolve_selector_index(selector, self.num_inputs());

        // Selected input, falling back to in_0 if the selected slot is unwired.
        // Kept as (port name, texture) so the gate below can key on the
        // ACTUAL slot that fed this frame's output, not just `idx` — a
        // fallback to in_0 uses a different physical slot than a direct
        // idx==0 selection would, even though both render the same branch.
        let selected = ctx
            .inputs
            .texture_2d(IN_PORT_NAMES[idx])
            .map(|t| (IN_PORT_NAMES[idx], t))
            .or_else(|| ctx.inputs.texture_2d("in_0").map(|t| ("in_0", t)));

        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out.width, out.height);
        let out_identity = out.identity_key();

        // RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3b/BUG-197: gate this
        // evaluate's dispatch (or clear-fallback) on everything that
        // determines its output content — the effective selector index,
        // the selected source's write generation + physical identity (the
        // `last_mip_identity`/`ibl_cache_key` precedent: identity alone is
        // a stale-serving trap on an in-place-rewriting producer, so the
        // generation term is load-bearing, not decorative), the output
        // texture's own identity (pool recycling), and the executor rebuild
        // epoch (never compare a generation number alone across executor
        // lifetimes — `NodeInputs::slot_generation`'s own doc comment). The
        // all-unwired clear-to-black path participates via its own hash arm
        // rather than a sentinel value that could collide with a real
        // (generation, identity) pair.
        use std::hash::{Hash, Hasher};
        let mut hasher = ahash::AHasher::default();
        idx.hash(&mut hasher);
        match selected {
            Some((port, tex)) => {
                ctx.inputs.slot_generation(port).hash(&mut hasher);
                hasher.write_usize(tex.identity_key());
            }
            None => hasher.write_u8(0xFF),
        }
        hasher.write_usize(out_identity);
        hasher.write_u64(ctx.rebuild_epoch);
        let gate_key = hasher.finish();

        if self.last_gate_key == Some(gate_key) {
            ctx.mark_outputs_unchanged();
            return;
        }
        self.last_gate_key = Some(gate_key);

        // Every in_N unwired: clear the output to opaque black so the gap is
        // visually obvious instead of leaving sticky pool contents behind.
        let Some(source) = selected.map(|(_, tex)| tex) else {
            let gpu = ctx.gpu_encoder();
            gpu.clear_texture(out, 0.0, 0.0, 0.0, 1.0);
            return;
        };

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(MUX_TEXTURE_WGSL, "cs_main", "node.switch_texture")
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
            "node.switch_texture",
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
        p.insert(std::borrow::Cow::Borrowed("num_inputs"), ParamValue::Float(num_inputs));
        p.insert(std::borrow::Cow::Borrowed("selector"), ParamValue::Float(selector));
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
        assert_eq!(node.type_id().as_str(), "node.switch_texture");
    }
}

/// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3b/BUG-197 gate: the
/// evaluate-path dispatch skip, exercised directly (no `Graph`/`Executor`
/// needed — bindings are constructed by hand, same shape as
/// `gltf_texture_source`'s and `bake_equirect_envmap`'s own P1/P3
/// gpu_tests modules).
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::{FrameTime, MetalBackend};
    use crate::render_target::RenderTarget;
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::{GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};

    fn frame_time() -> FrameTime {
        FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 }
    }

    fn solid_rgba16f(w: u32, h: u32, rgba: [f32; 4]) -> Vec<u8> {
        let px: [u16; 4] =
            [f16::from_f32(rgba[0]).to_bits(), f16::from_f32(rgba[1]).to_bits(), f16::from_f32(rgba[2]).to_bits(), f16::from_f32(rgba[3]).to_bits()];
        let mut bytes = Vec::with_capacity((w * h) as usize * 8);
        for _ in 0..(w * h) {
            for v in px {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        bytes
    }

    fn upload_solid_texture(device: &GpuDevice, w: u32, h: u32, rgba: [f32; 4], label: &str) -> manifold_gpu::GpuTexture {
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label,
            mip_levels: 1,
        });
        device.upload_texture(&tex, &solid_rgba16f(w, h, rgba));
        tex
    }

    fn readback(device: &GpuDevice, backend: &MetalBackend, slot: Slot, w: u32, h: u32) -> Vec<u8> {
        let tex = backend.texture_2d(slot).expect("texture retained");
        let bytes_per_row = w * 8; // Rgba16Float
        let total = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total);
        let mut enc = device.create_encoder("mux-texture-readback");
        enc.copy_texture_to_buffer(tex, &readback_buf, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        unsafe { std::slice::from_raw_parts(ptr, total as usize) }.to_vec()
    }

    #[allow(clippy::too_many_arguments)]
    fn run_once(
        prim: &mut MuxTexture,
        backend: &MetalBackend,
        device: &GpuDevice,
        input_scratch: &[(&'static str, Slot)],
        output_scratch: &[(&'static str, Slot)],
        generations: &[u64],
        params: &ParamValues,
        time: FrameTime,
    ) -> bool {
        let mut scalar_ws = Vec::new();
        let mut camera_ws = Vec::new();
        let mut light_ws = Vec::new();
        let mut material_ws = Vec::new();
        let mut transform_ws = Vec::new();
        let mut atmosphere_ws = Vec::new();
        let mut object_ws = Vec::new();
        let backend_ref: &dyn Backend = backend;
        let inputs = NodeInputs::new(input_scratch, backend_ref, generations);
        let outputs = NodeOutputs::new(
            output_scratch,
            backend_ref,
            &mut scalar_ws,
            &mut camera_ws,
            &mut light_ws,
            &mut material_ws,
            &mut transform_ws,
            &mut atmosphere_ws,
            &mut object_ws,
        );
        let mut native_enc = device.create_encoder("mux-texture-test");
        let unchanged;
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, device);
            let mut ctx = EffectNodeContext::new(time, params, inputs, outputs, Some(&mut gpu));
            prim.evaluate(&mut ctx);
            unchanged = ctx.outputs_unchanged;
        }
        native_enc.commit_and_wait_completed();
        unchanged
    }

    fn base_params(num_inputs: f32, selector: f32) -> ParamValues {
        let mut p = ParamValues::default();
        p.insert(Cow::Borrowed("num_inputs"), ParamValue::Float(num_inputs));
        p.insert(Cow::Borrowed("selector"), ParamValue::Float(selector));
        p
    }

    /// I4 (static inline selector, static source): frame 2's output is
    /// bit-identical to frame 1's, and the dispatch skip
    /// (`mark_outputs_unchanged`) fires on frame 2.
    #[test]
    fn frame2_matches_frame1_on_static_inline_selector_and_declares_unchanged() {
        let device = crate::test_device();
        let (w, h) = (8u32, 8u32);
        let format = GpuTextureFormat::Rgba16Float;
        let mut backend = MetalBackend::new(device.arc(), w, h, format);

        let src = upload_solid_texture(&device, w, h, [1.0, 0.0, 0.0, 1.0], "mux-src");
        let r_in0 = ResourceId(0);
        let in0_target = RenderTarget::view_of(src, "view");
        let in0_slot = backend.pre_bind_texture_2d(r_in0, in0_target);
        let r_out = ResourceId(1);
        let out_target = RenderTarget::new(&device, w, h, format, "mux-out");
        let out_slot = backend.pre_bind_texture_2d(r_out, out_target);

        let input_scratch: Vec<(&'static str, Slot)> = vec![("in_0", in0_slot)];
        let output_scratch: Vec<(&'static str, Slot)> = vec![("out", out_slot)];
        let params = base_params(8.0, 0.0);
        let mut prim = MuxTexture::new();

        let unchanged1 =
            run_once(&mut prim, &backend, &device, &input_scratch, &output_scratch, &[], &params, frame_time());
        assert!(!unchanged1, "first frame must actually dispatch (no prior gate key)");
        let frame1 = readback(&device, &backend, out_slot, w, h);

        let unchanged2 =
            run_once(&mut prim, &backend, &device, &input_scratch, &output_scratch, &[], &params, frame_time());
        assert!(unchanged2, "static frame must declare mark_outputs_unchanged");
        let frame2 = readback(&device, &backend, out_slot, w, h);
        assert_eq!(frame1, frame2, "frame 2 must be bit-identical to frame 1 on a static source");
    }

    /// I2-analog (the documented staleness trap this design guards
    /// against): the SAME physical source texture (identity unchanged)
    /// silently rewritten in place, with its slot's write generation
    /// bumped to signal the change — exactly R1's gated-source contract.
    /// The gate must re-dispatch on the generation bump (not go stale on
    /// identity alone), then re-declare unchanged once the generation
    /// holds steady again.
    #[test]
    fn source_generation_bump_without_identity_change_forces_refresh() {
        let device = crate::test_device();
        let (w, h) = (8u32, 8u32);
        let format = GpuTextureFormat::Rgba16Float;
        let mut backend = MetalBackend::new(device.arc(), w, h, format);

        let src = upload_solid_texture(&device, w, h, [1.0, 0.0, 0.0, 1.0], "mux-src");
        let r_in0 = ResourceId(0);
        let in0_slot = backend.pre_bind_texture_2d(r_in0, RenderTarget::view_of(src, "view"));
        let r_out = ResourceId(1);
        let out_slot = backend.pre_bind_texture_2d(r_out, RenderTarget::new(&device, w, h, format, "mux-out"));

        let input_scratch: Vec<(&'static str, Slot)> = vec![("in_0", in0_slot)];
        let output_scratch: Vec<(&'static str, Slot)> = vec![("out", out_slot)];
        let params = base_params(8.0, 0.0);
        let mut prim = MuxTexture::new();

        // Frame 1: generation 1 (index by in0_slot.0).
        let mut gens = vec![0u64; (in0_slot.0 as usize) + 1];
        gens[in0_slot.0 as usize] = 1;
        run_once(&mut prim, &backend, &device, &input_scratch, &output_scratch, &gens, &params, frame_time());
        let frame1 = readback(&device, &backend, out_slot, w, h);

        // Frame 2: SAME texture object, rewritten in place to green, and the
        // generation bumps to 2 — the gate must not go stale on identity.
        let same_tex = backend.texture_2d(in0_slot).expect("bound").clone();
        device.upload_texture(&same_tex, &solid_rgba16f(w, h, [0.0, 1.0, 0.0, 1.0]));
        gens[in0_slot.0 as usize] = 2;
        let unchanged2 =
            run_once(&mut prim, &backend, &device, &input_scratch, &output_scratch, &gens, &params, frame_time());
        assert!(!unchanged2, "a generation bump must force a re-dispatch even with unchanged identity");
        let frame2 = readback(&device, &backend, out_slot, w, h);
        assert_ne!(frame1, frame2, "the rewritten content must actually reach the output");

        // Frame 3: generation holds at 2 — must settle back to unchanged.
        let unchanged3 =
            run_once(&mut prim, &backend, &device, &input_scratch, &output_scratch, &gens, &params, frame_time());
        assert!(unchanged3, "a held generation must re-declare unchanged");
        let frame3 = readback(&device, &backend, out_slot, w, h);
        assert_eq!(frame2, frame3, "frame 3 must be bit-identical to frame 2 once the generation holds");
    }

    /// Changing the INLINE selector (unwired) must switch branches and
    /// must NOT be gated as unchanged — the new branch's content must
    /// reach the output the same frame the selector changes.
    #[test]
    fn inline_selector_change_switches_branch_and_forces_refresh() {
        let device = crate::test_device();
        let (w, h) = (8u32, 8u32);
        let format = GpuTextureFormat::Rgba16Float;
        let mut backend = MetalBackend::new(device.arc(), w, h, format);

        let src0 = upload_solid_texture(&device, w, h, [1.0, 0.0, 0.0, 1.0], "mux-src0");
        let src1 = upload_solid_texture(&device, w, h, [0.0, 0.0, 1.0, 1.0], "mux-src1");
        let r_in0 = ResourceId(0);
        let in0_slot = backend.pre_bind_texture_2d(r_in0, RenderTarget::view_of(src0, "view"));
        let r_in1 = ResourceId(1);
        let in1_slot = backend.pre_bind_texture_2d(r_in1, RenderTarget::view_of(src1, "view"));
        let r_out = ResourceId(2);
        let out_slot = backend.pre_bind_texture_2d(r_out, RenderTarget::new(&device, w, h, format, "mux-out"));

        let input_scratch: Vec<(&'static str, Slot)> = vec![("in_0", in0_slot), ("in_1", in1_slot)];
        let output_scratch: Vec<(&'static str, Slot)> = vec![("out", out_slot)];
        let mut prim = MuxTexture::new();

        run_once(&mut prim, &backend, &device, &input_scratch, &output_scratch, &[], &base_params(8.0, 0.0), frame_time());
        let frame_branch0 = readback(&device, &backend, out_slot, w, h);

        let unchanged =
            run_once(&mut prim, &backend, &device, &input_scratch, &output_scratch, &[], &base_params(8.0, 1.0), frame_time());
        assert!(!unchanged, "a selector flip must NOT be gated as unchanged");
        let frame_branch1 = readback(&device, &backend, out_slot, w, h);
        assert_ne!(frame_branch0, frame_branch1, "switching branches must change the output");

        // Must match a FRESH render of branch 1 from scratch.
        let mut backend_fresh = MetalBackend::new(device.arc(), w, h, format);
        let src1_fresh = upload_solid_texture(&device, w, h, [0.0, 0.0, 1.0, 1.0], "mux-src1-fresh");
        let in0_fresh = backend_fresh.pre_bind_texture_2d(r_in0, RenderTarget::new(&device, w, h, format, "unused"));
        let in1_fresh = backend_fresh.pre_bind_texture_2d(r_in1, RenderTarget::view_of(src1_fresh, "view"));
        let out_fresh = backend_fresh.pre_bind_texture_2d(r_out, RenderTarget::new(&device, w, h, format, "mux-out-fresh"));
        let input_scratch_fresh: Vec<(&'static str, Slot)> = vec![("in_0", in0_fresh), ("in_1", in1_fresh)];
        let output_scratch_fresh: Vec<(&'static str, Slot)> = vec![("out", out_fresh)];
        let mut prim_fresh = MuxTexture::new();
        run_once(&mut prim_fresh, &backend_fresh, &device, &input_scratch_fresh, &output_scratch_fresh, &[], &base_params(8.0, 1.0), frame_time());
        let fresh = readback(&device, &backend_fresh, out_fresh, w, h);
        assert_eq!(frame_branch1, fresh, "a live selector flip must match a fresh executor built with that selector");
    }

    /// Changing a WIRED selector renders with the LATCHED (previous
    /// frame's) value — existing, designed behavior (the doc comment on
    /// `latched_selector`) — this test pins that the new evaluate-path gate
    /// did not break that one-frame lag.
    #[test]
    fn wired_selector_change_uses_latched_value_one_frame_lag() {
        let device = crate::test_device();
        let (w, h) = (8u32, 8u32);
        let format = GpuTextureFormat::Rgba16Float;
        let mut backend = MetalBackend::new(device.arc(), w, h, format);

        let src0 = upload_solid_texture(&device, w, h, [1.0, 0.0, 0.0, 1.0], "mux-src0");
        let src1 = upload_solid_texture(&device, w, h, [0.0, 0.0, 1.0, 1.0], "mux-src1");
        let r_in0 = ResourceId(0);
        let in0_slot = backend.pre_bind_texture_2d(r_in0, RenderTarget::view_of(src0, "view"));
        let r_in1 = ResourceId(1);
        let in1_slot = backend.pre_bind_texture_2d(r_in1, RenderTarget::view_of(src1, "view"));
        let r_out = ResourceId(2);
        let out_slot = backend.pre_bind_texture_2d(r_out, RenderTarget::new(&device, w, h, format, "mux-out"));
        // Selector is WIRED: pick an arbitrary slot number distinct from
        // the texture slots above and drive it via `set_scalar`.
        let selector_slot = Slot(1000);

        let input_scratch: Vec<(&'static str, Slot)> =
            vec![("selector", selector_slot), ("in_0", in0_slot), ("in_1", in1_slot)];
        let output_scratch: Vec<(&'static str, Slot)> = vec![("out", out_slot)];
        let params = base_params(8.0, 0.0);
        let mut prim = MuxTexture::new();

        // Frame 1: wire = 0 (branch 0). No latch yet, so `evaluate` reads
        // the CURRENT wire value this first time.
        backend.set_scalar(selector_slot, ParamValue::Float(0.0));
        run_once(&mut prim, &backend, &device, &input_scratch, &output_scratch, &[], &params, frame_time());
        let frame1 = readback(&device, &backend, out_slot, w, h);

        // Frame 2: wire flips to 1 — must still render branch 0 (the
        // LATCHED value from frame 1), not branch 1 yet.
        backend.set_scalar(selector_slot, ParamValue::Float(1.0));
        run_once(&mut prim, &backend, &device, &input_scratch, &output_scratch, &[], &params, frame_time());
        let frame2 = readback(&device, &backend, out_slot, w, h);
        assert_eq!(frame1, frame2, "the frame the wire flips must still render the LATCHED (old) branch");

        // Frame 3: wire holds at 1 — the latch has caught up, branch 1 now.
        backend.set_scalar(selector_slot, ParamValue::Float(1.0));
        run_once(&mut prim, &backend, &device, &input_scratch, &output_scratch, &[], &params, frame_time());
        let frame3 = readback(&device, &backend, out_slot, w, h);
        assert_ne!(frame2, frame3, "one frame after the wire flip, the latch must have caught up to branch 1");
    }
}
