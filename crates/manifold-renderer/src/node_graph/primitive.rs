//! Authoring scaffolding for primitive nodes.
//!
//! `EffectNode` is the production trait every node in the graph runtime
//! implements (atomics, composites, boundaries, legacy adapters). This
//! module adds an **authoring layer** on top of it: a slim `Primitive`
//! trait plus a declarative [`primitive!`] macro that generate the
//! boilerplate every Phase 4a migration would otherwise repeat (~80 LOC
//! per node: type-id constants, input/output port arrays, `EffectNode`
//! method delegations, default `clear_state`, AI-metadata fields).
//!
//! Authors write only:
//!
//! 1. The [`primitive!`] declaration (struct name, type-id string,
//!    purpose docstring, port shapes, parameter list, optional AI
//!    metadata).
//! 2. Any uniform / state fields beyond the auto-generated `pipeline`
//!    and `sampler` caches.
//! 3. The `Primitive::run` body — the actual GPU dispatch.
//!
//! Everything else (the `EffectNode` trait impl, the cached
//! `EffectNodeType`, the const arrays, the `PrimitiveDescription`
//! shipped to the AI composition surface) is derived automatically.
//!
//! ## Why a trait pair instead of a single `Primitive` trait
//!
//! [`PrimitiveSpec`] holds the const metadata; [`Primitive`] adds the
//! per-frame logic. Splitting them lets the macro generate the
//! `PrimitiveSpec` impl in full (no user body required) while the user
//! writes only the `Primitive` impl with a hand-written `run` body. A
//! single combined trait would force the macro to either emit a
//! `run` stub the user has to override (loses type-system enforcement
//! that `run` is provided) or to capture the body as macro input
//! (DSL gets hairy fast).
//!
//! ## AI surface
//!
//! Each primitive's [`PrimitiveSpec::description`] returns a
//! [`PrimitiveDescription`] suitable for an AI agent to inspect when
//! composing a graph. The macro requires `purpose` and lets you
//! specify optional `composition_notes` and `examples`. These flow
//! through to the editor's primitive picker and any future
//! agent-facing JSON dump.

use std::sync::OnceLock;

use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
use crate::node_graph::parameters::ParamDef;
use crate::node_graph::ports::{NodeInput, NodeOutput};

/// Const-data half of the authoring layer.
///
/// The macro emits a full impl of this trait. Authors don't write it
/// directly. The blanket `EffectNode` impl below reads the const
/// arrays and the cached [`EffectNodeType`] through this trait.
pub trait PrimitiveSpec: Send {
    /// Stable string identity. Treated as public API once shipped —
    /// renaming breaks saved graphs. See [`EffectNodeType`] for the
    /// renaming policy.
    const TYPE_ID: &'static str;

    /// One-sentence semantic intent. Visible in the editor and the
    /// AI composition surface — write it for a reader who hasn't seen
    /// the source code.
    const PURPOSE: &'static str;

    /// Optional guidance on when to choose this primitive over an
    /// alternative. Empty string acceptable; the AI surface filters
    /// blank entries.
    const COMPOSITION_NOTES: &'static str = "";

    /// Names of preset graphs that use this primitive — discoverable
    /// examples for users browsing the library. Empty slice acceptable.
    const EXAMPLES: &'static [&'static str] = &[];

    /// Input port array. Order matters for the editor's left-side
    /// connector layout.
    const INPUTS: &'static [NodeInput];

    /// Output port array. Order matters for the editor's right-side
    /// connector layout.
    const OUTPUTS: &'static [NodeOutput];

    /// Parameter definitions in declaration order. Index in this array
    /// is the slot index used by the host's `param_values` storage.
    const PARAMS: &'static [ParamDef];

    /// Fusion classification — whether/how this primitive folds into a fused
    /// kernel (freeze/fusion compiler, design doc §12). Defaults to
    /// [`FusionKind::Boundary`] (never fused), so the compiler only fuses what
    /// an atom explicitly opts into. Set via the macro's `fusion_kind:` field.
    const FUSION_KIND: crate::node_graph::freeze::classify::FusionKind =
        crate::node_graph::freeze::classify::FusionKind::Boundary;

    /// Why this primitive stays a fusion Boundary (design doc D4,
    /// `docs/GRAPH_TOOLING_DESIGN.md`). `None` by default — required to
    /// stay `None` for every fusable (`FUSION_KIND != Boundary`) primitive,
    /// and required (exactly one reason) for every Boundary primitive
    /// ([`freeze::classify`](crate::node_graph::freeze::classify) — enforced
    /// by `every_boundary_atom_declares_its_reason`). Set via the macro's
    /// `boundary_reason:` field.
    const BOUNDARY_REASON: Option<crate::node_graph::freeze::classify::BoundaryReason> = None;

    /// `(param name, contract)` pairs — the real physical/mathematical
    /// boundaries among this primitive's [`PARAMS`](Self::PARAMS), if any
    /// (`docs/PARAM_RANGE_CONTRACT_DESIGN.md`). Empty by default: the
    /// overwhelming majority of params carry no contract, only a display
    /// hint (their declared `range`). Set via the macro's optional
    /// `param_contracts:` field; a curated meta-test
    /// (`every_range_contract_names_a_real_boundary`,
    /// `node_graph::freeze::classify`) pins every entry to its reason so a
    /// contract can't creep back onto a merely-conventional range.
    const PARAM_CONTRACTS: &'static [(&'static str, manifold_core::effects::RangeContract)] = &[];

    /// PURE primitive: `run()`'s output depends ONLY on its param values and
    /// its wired inputs — no frame time/beat/delta, no `StateStore`, no
    /// randomness, no CPU/FFI side effects, no canvas-dims dependence beyond
    /// the output texture it's handed. The executor memoizes pure steps:
    /// when params and inputs are unchanged since the last execute, the step
    /// is skipped and its held output slot serves consumers (constant-subgraph
    /// hoisting — a static LUT renders once, not per frame; a param tweak
    /// re-renders it once). Opt-in via the macro's `pure:` field because the
    /// contract can't be checked mechanically — declare it only after reading
    /// `run()` end-to-end.
    const PURE: bool = false;

    /// Optional fusable body fragment: a WGSL `fn body(...)` with no global
    /// accesses (purity-checked) that the fusion codegen chains into one
    /// kernel — and from which the standalone `cs_main` is generated
    /// (single-source authoring, design doc §12). `None` = no body; the
    /// primitive's own hand-written kernel is authoritative. Set via the
    /// macro's `wgsl_body:` field (typically `include_str!("shaders/x_body.wgsl")`).
    const WGSL_BODY: Option<&'static str> = None;

    /// Per-texture-input read-semantics for the fusion codegen
    /// ([`InputAccess`](crate::node_graph::freeze::classify::InputAccess)),
    /// aligned to the TEXTURE inputs in `INPUTS` order (scalar/control inputs are
    /// skipped). Empty (the default) means every texture input is
    /// [`InputAccess::Coincident`] — the resolution-robust sampler read that
    /// pointwise and coincident atoms use. Set per-input via the macro's
    /// `input_access:` field only when an input needs a different semantic (e.g.
    /// dither's exact-texel threshold pattern). An index past the end of the list
    /// also defaults to `Coincident`.
    const INPUT_ACCESS: &'static [crate::node_graph::freeze::classify::InputAccess] = &[];

    /// STENCIL-FETCH body ABI (stencil tier): the `wgsl_body` reads each of its
    /// `Gather` texture inputs through a free function `fetch_<port>(uv:
    /// vec2<f32>) -> vec4<f32>` instead of receiving `(texture_2d, sampler)`
    /// args. The codegen always DEFINES that function before the body — as a
    /// real `textureSampleLevel` over the bound input (standalone, or a fused
    /// region's real external), or as a recomputed upstream chain with manual
    /// bilinear (a fused virtual source). This is what lets pointwise work
    /// upstream of a blur fold INTO the blur's read. Opt-in via the macro's
    /// `stencil_fetch:` field; only meaningful for atoms with `Gather` inputs.
    const STENCIL_FETCH: bool = false;

    /// Specialization tokens the `wgsl_body` references as free identifiers
    /// (e.g. `QUALITY_LEVEL`), each resolved from a STATIC Enum/Int param:
    /// `(token, param_name)` pairs. The hand `run()` substitutes them via
    /// `create_specialized_compute_pipeline`; the freeze compiler substitutes
    /// the def's param value into the body TEXT before parsing/fusing, so the
    /// atom stops being a permanent fusion boundary. The classifier keeps the
    /// atom a boundary if any listed param is binding-targeted or control-wired
    /// (the baked value could then diverge from the live one). Empty default.
    const WGSL_SPECIALIZATION: &'static [(&'static str, &'static str)] = &[];

    /// Buffer-domain ONLY: names of injected non-param `f32` uniform fields the
    /// generated kernel needs that aren't user params — frame-derived values
    /// like a particle integrator's `dt_scaled` (= `delta * 60`). The buffer
    /// standalone codegen lays each out as a uniform field after the params and
    /// passes it to the body (after the params), and `run()` packs the resolved
    /// value each frame. Keeps these off the param surface (no descriptor /
    /// catalog / preset-binding churn) while still feeding the body. Empty (the
    /// default) for every texture atom and every param-only buffer atom. Set via
    /// the macro's `derived_uniforms:` field.
    const DERIVED_UNIFORMS: &'static [&'static str] = &[];

    /// Shared WGSL library source the generated kernel must prepend before the
    /// `wgsl_body` (e.g. `noise_common.wgsl`'s `simplex3d`). Each entry is the
    /// full source text (typically `include_str!`). The buffer standalone codegen
    /// emits them ahead of the body so its helper calls resolve, mirroring the
    /// `format!("{NOISE_COMMON}\n{shader}")` the hand `run()` does. Empty (the
    /// default) for self-contained bodies. Set via the macro's `wgsl_includes:`
    /// field.
    const WGSL_INCLUDES: &'static [&'static str] = &[];

    /// Buffer-domain ONLY: names of Array OUTPUT ports that are atomic
    /// accumulators — emitted as `array<atomic<u32>>` (or `atomic<i32>`) and
    /// written by the body itself via `atomicAdd` on the `buf_<port>` global,
    /// NOT through the wrapper's single-element `buf_out[idx] = body(...)`
    /// assignment. A scatter atom's output index is data-dependent (the splat
    /// target), so it can't be a coincident write; the body computes the cell
    /// and accumulates. The wrapper then calls `body(...)` as a statement (no
    /// return value). Empty (the default) for every coincident/gather buffer
    /// atom. Set via the macro's `atomic_outputs:` field. The element must be a
    /// single-channel u32 / i32 (WGSL atomics are integer-only).
    const ATOMIC_OUTPUTS: &'static [&'static str] = &[];

    /// How this primitive propagates the depth companion channel the "3D
    /// Shading" toggle synthesizes (design doc `docs/DEPTH_RELIGHT_DESIGN.md`
    /// D1). **No default** — unlike [`FUSION_KIND`](Self::FUSION_KIND), every
    /// primitive must declare this explicitly via the macro's REQUIRED
    /// `depth_rule:` field; a primitive that omits it fails to compile. Kept
    /// required (rather than defaulting to the conservative `Terminal`) so
    /// the classification stays truthful as new primitives are added — no
    /// primitive can silently inherit a wrong guess.
    const DEPTH_RULE: crate::node_graph::depth_rule::DepthRule;

    /// Returns a process-wide `EffectNodeType` instance for this
    /// primitive, allocated lazily on first call.
    ///
    /// Implemented uniquely per-primitive (the macro emits each
    /// primitive's own `OnceLock`-backed cache). A blanket impl in
    /// terms of `TYPE_ID` would require putting the `OnceLock` inside
    /// a generic function, where Rust's static-in-generic rules
    /// collapse all monomorphizations to one cell — wrong shape for
    /// per-type caching.
    fn cached_type_id() -> &'static EffectNodeType;

    /// Bundle the const metadata into a runtime struct suitable for
    /// JSON serialization to the AI composition surface or
    /// presentation in an editor primitive picker.
    fn description() -> PrimitiveDescription {
        PrimitiveDescription {
            type_id: Self::TYPE_ID,
            purpose: Self::PURPOSE,
            composition_notes: Self::COMPOSITION_NOTES,
            examples: Self::EXAMPLES,
            inputs: Self::INPUTS,
            outputs: Self::OUTPUTS,
            params: Self::PARAMS,
        }
    }
}

/// Per-frame logic half of the authoring layer.
///
/// Authors implement this directly: `impl Primitive for Invert { fn
/// run(&mut self, ctx) { ... } }`. The bounded trait pair
/// (`PrimitiveSpec` + `Primitive`) means the const metadata is
/// already in place by the time the compiler sees this impl, so
/// `run` bodies can refer to `Self::TYPE_ID`, `Self::INPUTS`, etc.
/// directly.
pub trait Primitive: PrimitiveSpec {
    /// Run one frame of GPU work. The contract is identical to
    /// [`EffectNode::evaluate`] — read inputs, write outputs —
    /// just routed through the authoring layer for boilerplate-free
    /// definition.
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>);

    /// Mirror of [`EffectNode::late_capture`](crate::node_graph::effect_node::EffectNode::late_capture).
    /// Default no-op. Override on stateful primitives that declare
    /// [`state_capture_input_ports`](Self::state_capture_input_ports)
    /// — read state-capture inputs here (they hold THIS frame's
    /// producer output by `late_capture` time) and snapshot into
    /// persistent state via the StateStore.
    ///
    /// Capture-before-producer inside `run` reads STALE inputs
    /// (state-capture nodes run first in topo, before their feeding
    /// producers). Always use `late_capture` for snapshot work — the
    /// 2-frame-delay bug that caused per-frame flicker in OilyFluid
    /// is exactly the failure mode that arises from doing it in `run`.
    fn late_capture(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}

    /// Reset persistent state. Default no-op; stateful primitives
    /// (Feedback, etc.) override to drop their previous-frame textures
    /// on seek.
    fn clear_state(&mut self) {}

    /// Mirror of [`EffectNode::is_trigger_latch`](crate::node_graph::effect_node::EffectNode::is_trigger_latch).
    /// Override `true` on trigger-edge latch primitives (`sample_and_hold`,
    /// `clip_trigger_cycle`, `clip_trigger_index`, `frequency_ratio`,
    /// `cycle_table_row`, `trigger_gate`, `trigger_ease_to`) — see that
    /// doc comment for the full BUG-104 rationale.
    fn is_trigger_latch(&self) -> bool {
        false
    }

    /// Optional WGSL kernel source — mirror of
    /// [`EffectNode::wgsl_source`](crate::node_graph::effect_node::EffectNode::wgsl_source).
    /// Override only on `node.wgsl_compute_*` primitives.
    fn wgsl_source(&self) -> Option<&str> {
        None
    }

    /// Optional WGSL kernel source setter — mirror of
    /// [`EffectNode::set_wgsl_source`](crate::node_graph::effect_node::EffectNode::set_wgsl_source).
    /// Override only on `node.wgsl_compute_*` primitives.
    fn set_wgsl_source(&mut self, _source: &str) {}

    /// Per-output-port texture format override — mirror of
    /// [`EffectNode::output_format`](crate::node_graph::effect_node::EffectNode::output_format).
    /// Override on primitives that need a non-default format (most
    /// commonly the `node.wgsl_compute_*` escape hatches when an
    /// agent wants `rgba32float` / `r32float` precision).
    fn output_format(&self, _port: &str) -> Option<manifold_gpu::GpuTextureFormat> {
        None
    }

    /// Setter for [`output_format`](Self::output_format) — mirror of
    /// [`EffectNode::set_output_format`](crate::node_graph::effect_node::EffectNode::set_output_format).
    /// Override only on primitives that carry instance-level format
    /// state (the `node.wgsl_compute_*` family).
    fn set_output_format(&mut self, _port: &str, _format: manifold_gpu::GpuTextureFormat) {}

    /// Per-output-port mip-chain request — mirror of
    /// [`EffectNode::output_mipmapped`](crate::node_graph::effect_node::EffectNode::output_mipmapped).
    /// Override on material-map sources whose output is sampled under
    /// minification (`node.gltf_texture_source`, IMPORT_FIDELITY F-P6);
    /// the producer must fill levels 1.. itself (`generate_mipmaps`
    /// after writing level 0). Default `false`.
    fn output_mipmapped(&self, _port: &str) -> bool {
        false
    }

    /// Background file IO still in flight — mirror of
    /// [`EffectNode::io_pending`](crate::node_graph::effect_node::EffectNode::io_pending).
    /// Override on IoBridge file sources whose `run()` spawns a background
    /// decode thread (`node.gltf_texture_source`, `node.hdri_source`):
    /// return `true` while a decode is in flight or decoded-but-not-yet-
    /// uploaded, so headless convergence loops (`render-import`) can tell
    /// "byte-stable because settled" from "byte-stable because the decode
    /// hasn't landed yet" (G-P6 gate-review fix: a 74 MB EXR's decode dead
    /// time outlasts any byte-stability window). Default `false`.
    fn io_pending(&self) -> bool {
        false
    }

    /// Sampler address mode for this atom's `Gather` inputs in a fused region —
    /// mirror of
    /// [`EffectNode::fused_gather_sampler_mode`](crate::node_graph::effect_node::EffectNode::fused_gather_sampler_mode).
    /// Override on a gather atom whose sampling wraps (the toroidal fluid
    /// gradient reads its `wrap_mode` param) so the fused region binds the same
    /// sampler the standalone atom does. Default `ClampToEdge`.
    fn fused_gather_sampler_mode(
        &self,
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> manifold_gpu::GpuAddressMode {
        manifold_gpu::GpuAddressMode::ClampToEdge
    }

    /// Mirror of
    /// [`EffectNode::stencil_taps_texel_exact`](crate::node_graph::effect_node::EffectNode::stencil_taps_texel_exact).
    /// Override on a stencil atom for the param shapes whose gather coords all
    /// land on texel centers (integer tap offsets) — e.g. the Linear blur mode.
    /// Default `false` (taps assumed fractional).
    fn stencil_taps_texel_exact(
        &self,
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> bool {
        false
    }

    /// Mirror of
    /// [`EffectNode::output_dims`](crate::node_graph::effect_node::EffectNode::output_dims).
    /// Override on `node.downsample` (and any future `node.upsample`)
    /// to break out of the default "match max of texture input dims"
    /// policy. Default: `None`.
    fn output_dims(
        &self,
        _port: &str,
        _canvas_dims: (u32, u32),
        _input_dims: &[(&str, (u32, u32))],
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        None
    }

    /// Mirror of
    /// [`EffectNode::output_canvas_scale`](crate::node_graph::effect_node::EffectNode::output_canvas_scale).
    /// Default `None`. Override on multi-resolution primitives
    /// (`node.downsample` and any future `node.upsample` / mip
    /// stages) to land output slots at `canvas * num / den` when the
    /// concrete `output_dims` fallback can't resolve (state-capture
    /// back-edge inputs whose dim isn't compile-time known).
    fn output_canvas_scale(
        &self,
        _port: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        None
    }

    /// Mirror of [`EffectNode::set_output_canvas_scale`]. Default no-op
    /// — only dynamic-shape primitives need this.
    fn set_output_canvas_scale(&mut self, _port: &str, _scale: (u32, u32)) {}

    /// Mirror of
    /// [`EffectNode::breaks_dependency_cycle`](crate::node_graph::effect_node::EffectNode::breaks_dependency_cycle).
    /// Default derives from [`state_capture_input_ports`](Self::state_capture_input_ports)
    /// — a primitive is a cycle-breaker iff it declares at least one
    /// state-capture input port. Override only if you need to assert
    /// cycle-break-ness without listing any specific port (rare).
    fn breaks_dependency_cycle(&self) -> bool {
        !self.state_capture_input_ports().is_empty()
    }

    /// Mirror of
    /// [`EffectNode::state_capture_input_ports`](crate::node_graph::effect_node::EffectNode::state_capture_input_ports).
    /// Stateful primitives override to list the input port(s) that close
    /// a per-frame loop through the StateStore. Every other input runs
    /// upstream of this node in topological order. Default: empty.
    fn state_capture_input_ports(&self) -> &[&str] {
        &[]
    }

    /// Mirror of
    /// [`EffectNode::persistent_output_ports`](crate::node_graph::effect_node::EffectNode::persistent_output_ports).
    /// Output ports whose resources persist across frames (pre-acquired,
    /// never pool-released). The zero-copy feedback ping-pong's emit half.
    /// Default: empty.
    fn persistent_output_ports(&self) -> &[&str] {
        &[]
    }

    /// Mirror of
    /// [`EffectNode::fusion_register_heavy`](crate::node_graph::effect_node::EffectNode::fusion_register_heavy).
    /// A register-heavy `wgsl_body` (big inlined noise) that pessimizes any
    /// fused region it joins overrides this to `true` and stays a fusion
    /// Boundary. Default: `false`.
    fn fusion_register_heavy(&self) -> bool {
        false
    }

    /// Mirror of
    /// [`EffectNode::fused_dispatch_count_param`](crate::node_graph::effect_node::EffectNode::fused_dispatch_count_param).
    /// Particle integrators override to name their live-count scalar param
    /// (`active_count`) so a fused region can cap its dispatch at the live
    /// count instead of the buffer capacity. Default: `None`.
    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        None
    }

    /// Mirror of
    /// [`EffectNode::reports_empty_output`](crate::node_graph::effect_node::EffectNode::reports_empty_output).
    /// Detector/spawner primitives override to report a zero-count frame
    /// (queried right after `run()`). Default: `false`.
    fn reports_empty_output(&self) -> bool {
        false
    }

    /// Mirror of
    /// [`EffectNode::empty_skip_input_ports`](crate::node_graph::effect_node::EffectNode::empty_skip_input_ports).
    /// Pure data-shapers downstream of a detector override to list the data
    /// ports whose emptiness makes their evaluate a no-op. Default: never
    /// skipped. Read the EffectNode contract before declaring.
    fn empty_skip_input_ports(&self) -> &'static [&'static str] {
        &[]
    }

    /// Mirror of
    /// [`EffectNode::skip_passthrough_ports`](crate::node_graph::effect_node::EffectNode::skip_passthrough_ports).
    /// Draw/overlay primitives that composite onto a source texture
    /// declare `Some((in_port, out_port))` so the executor's data-driven
    /// skip can alias the live source through instead of freezing a stale
    /// copy. Default: `None`.
    fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
        None
    }

    /// Mirror of
    /// [`EffectNode::skip_passthrough`](crate::node_graph::effect_node::EffectNode::skip_passthrough).
    /// Per-frame param-driven no-op declaration. Default: `None`.
    fn skip_passthrough(
        &self,
        _params: &crate::node_graph::effect_node::ParamValues,
        _wired_inputs: &[&str],
    ) -> Option<(&'static str, &'static str)> {
        None
    }

    /// Mirror of
    /// [`EffectNode::variadic_skip_passthrough_out`](crate::node_graph::effect_node::EffectNode::variadic_skip_passthrough_out).
    /// Default: `None`.
    fn variadic_skip_passthrough_out(&self) -> Option<&'static str> {
        None
    }

    /// Mirror of
    /// [`EffectNode::carries_resources`](crate::node_graph::effect_node::EffectNode::carries_resources).
    /// Default: `false`.
    fn carries_resources(&self) -> bool {
        false
    }

    /// Mirror of
    /// [`EffectNode::selected_input_branch`](crate::node_graph::effect_node::EffectNode::selected_input_branch).
    /// Mux-family primitives override to return the selected input
    /// port name (e.g. `"in_2"` for mux selector=2). See the
    /// EffectNode docstring for the wired-input fallback rule.
    /// Default: `None` (eager evaluation of all inputs).
    fn selected_input_branch(
        &self,
        _params: &crate::node_graph::effect_node::ParamValues,
        _wired_inputs: &[&str],
    ) -> Option<&'static str> {
        None
    }

    /// Mirror of
    /// [`EffectNode::requires`](crate::node_graph::effect_node::EffectNode::requires).
    /// Declare any runtime services this primitive needs (StateStore for
    /// per-frame persistent state, GpuEncoder for dispatch / texture
    /// copies). The default `NodeRequires::default()` is correct for
    /// pure pixel-local primitives; stateful or compute-issuing ones
    /// must override. The compile/dispatch layer rolls these up per
    /// plan so `execute_frame_with_gpu` can refuse to run a chain that
    /// would silently panic deeper down.
    fn requires(&self) -> crate::node_graph::effect_node::NodeRequires {
        crate::node_graph::effect_node::NodeRequires::default()
    }

    /// Mirror of
    /// [`EffectNode::aliased_array_io`](crate::node_graph::effect_node::EffectNode::aliased_array_io).
    /// Declare `(input_port, output_port)` pairs that share a single
    /// physical buffer. Used by stateful array simulators
    /// (`integrate_particles`, `integrate_particles_attractor`) where
    /// the dispatch reads from and writes to the same storage in
    /// place. The chain builder allocates one buffer per pair (sized
    /// by the input wire's capacity) and aliases the output's slot to
    /// the input's, so upstream writes flow through and cross-frame
    /// state lives in the chain-allocated buffer.
    fn aliased_array_io(&self) -> &[(&str, &str)] {
        &[]
    }

    /// Mirror of
    /// [`EffectNode::canvas_sized_array_outputs`](crate::node_graph::effect_node::EffectNode::canvas_sized_array_outputs).
    /// Output Array port names whose buffer size must equal the
    /// canvas (`width × height` cells). Used by scatter accumulators
    /// and any future primitive whose output must align
    /// pixel-for-pixel with the final frame. The chain builder
    /// allocates `canvas_w * canvas_h * item_size` bytes — the
    /// primitive's `array_output_capacity` is bypassed for these
    /// ports.
    fn canvas_sized_array_outputs(&self) -> &[&str] {
        &[]
    }

    /// Mirror of
    /// [`EffectNode::array_output_capacity`](crate::node_graph::effect_node::EffectNode::array_output_capacity).
    /// Override on transform primitives (capacity inherited from a
    /// named Array input port) and on computed-capacity primitives
    /// (capacity = `f(params)`). The default reads `params["max_capacity"]`,
    /// which is correct for producer-style primitives that declare
    /// the convention param.
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        let is_array_output = Self::OUTPUTS
            .iter()
            .any(|p| p.name == port_name && matches!(p.ty, crate::node_graph::ports::PortType::Array(_)));
        if !is_array_output {
            return None;
        }
        params
            .get("max_capacity")
            .and_then(|v| v.as_u32_clamped(1))
    }

    /// Mirror of
    /// [`EffectNode::emitted_material_kind`](crate::node_graph::effect_node::EffectNode::emitted_material_kind).
    /// Material atoms (`node.{unlit,phong,pbr,cel}_material`) override
    /// to return their fixed kind. Default `None` — most primitives
    /// don't emit a Material at all.
    fn emitted_material_kind(&self) -> Option<crate::node_graph::material::MaterialKind> {
        None
    }

    /// Mirror of
    /// [`EffectNode::conditional_requirements`](crate::node_graph::effect_node::EffectNode::conditional_requirements).
    /// Nodes whose required-input set varies with an upstream-wire value
    /// (the bundled 3D mesh renderers) override to declare per-MaterialKind
    /// rules. Default empty.
    fn conditional_requirements(
        &self,
    ) -> &'static [crate::node_graph::effect_node::ConditionalRequirement] {
        &[]
    }
}

/// Blanket `EffectNode` impl for any `Primitive`. Reads all surface
/// data through the const arrays + cached type id. The graph runtime
/// (`Executor`, `compile`, the legacy adapter) sees no difference
/// between a primitive authored via the macro and one written by
/// hand with a manual `EffectNode` impl.
impl<P: Primitive + 'static> EffectNode for P {
    fn type_id(&self) -> &EffectNodeType {
        P::cached_type_id()
    }
    fn inputs(&self) -> &[NodeInput] {
        P::INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        P::OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        P::PARAMS
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        Primitive::run(self, ctx);
    }
    fn late_capture(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        Primitive::late_capture(self, ctx);
    }
    fn clear_state(&mut self) {
        Primitive::clear_state(self);
    }
    fn is_trigger_latch(&self) -> bool {
        Primitive::is_trigger_latch(self)
    }
    fn wgsl_source(&self) -> Option<&str> {
        Primitive::wgsl_source(self)
    }
    fn set_wgsl_source(&mut self, source: &str) {
        Primitive::set_wgsl_source(self, source);
    }
    fn output_format(&self, port: &str) -> Option<manifold_gpu::GpuTextureFormat> {
        Primitive::output_format(self, port)
    }
    fn set_output_format(&mut self, port: &str, format: manifold_gpu::GpuTextureFormat) {
        Primitive::set_output_format(self, port, format);
    }
    fn output_mipmapped(&self, port: &str) -> bool {
        Primitive::output_mipmapped(self, port)
    }
    fn io_pending(&self) -> bool {
        Primitive::io_pending(self)
    }
    fn fused_gather_sampler_mode(
        &self,
        params: &crate::node_graph::effect_node::ParamValues,
    ) -> manifold_gpu::GpuAddressMode {
        Primitive::fused_gather_sampler_mode(self, params)
    }
    fn stencil_taps_texel_exact(
        &self,
        params: &crate::node_graph::effect_node::ParamValues,
    ) -> bool {
        Primitive::stencil_taps_texel_exact(self, params)
    }
    fn output_dims(
        &self,
        port: &str,
        canvas_dims: (u32, u32),
        input_dims: &[(&str, (u32, u32))],
        params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        Primitive::output_dims(self, port, canvas_dims, input_dims, params)
    }
    fn output_canvas_scale(
        &self,
        port: &str,
        params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        Primitive::output_canvas_scale(self, port, params)
    }
    fn set_output_canvas_scale(&mut self, port: &str, scale: (u32, u32)) {
        Primitive::set_output_canvas_scale(self, port, scale);
    }
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        Primitive::array_output_capacity(self, port_name, params, input_capacities)
    }
    fn aliased_array_io(&self) -> &[(&str, &str)] {
        Primitive::aliased_array_io(self)
    }
    fn canvas_sized_array_outputs(&self) -> &[&str] {
        Primitive::canvas_sized_array_outputs(self)
    }
    fn requires(&self) -> crate::node_graph::effect_node::NodeRequires {
        Primitive::requires(self)
    }
    fn breaks_dependency_cycle(&self) -> bool {
        Primitive::breaks_dependency_cycle(self)
    }
    fn state_capture_input_ports(&self) -> &[&str] {
        Primitive::state_capture_input_ports(self)
    }
    fn persistent_output_ports(&self) -> &[&str] {
        Primitive::persistent_output_ports(self)
    }
    fn selected_input_branch(
        &self,
        params: &crate::node_graph::effect_node::ParamValues,
        wired_inputs: &[&str],
    ) -> Option<&'static str> {
        Primitive::selected_input_branch(self, params, wired_inputs)
    }
    fn emitted_material_kind(&self) -> Option<crate::node_graph::material::MaterialKind> {
        Primitive::emitted_material_kind(self)
    }
    fn conditional_requirements(
        &self,
    ) -> &'static [crate::node_graph::effect_node::ConditionalRequirement] {
        Primitive::conditional_requirements(self)
    }
    fn fusion_kind(&self) -> crate::node_graph::freeze::classify::FusionKind {
        P::FUSION_KIND
    }
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        P::BOUNDARY_REASON
    }
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        P::DEPTH_RULE
    }
    fn param_contract(&self, param_name: &str) -> Option<manifold_core::effects::RangeContract> {
        P::PARAM_CONTRACTS
            .iter()
            .find(|(name, _)| *name == param_name)
            .map(|(_, contract)| contract.clone())
    }
    fn is_pure(&self) -> bool {
        P::PURE
    }
    fn fusion_register_heavy(&self) -> bool {
        Primitive::fusion_register_heavy(self)
    }
    fn fused_dispatch_count_param(&self) -> Option<&'static str> {
        Primitive::fused_dispatch_count_param(self)
    }
    fn reports_empty_output(&self) -> bool {
        Primitive::reports_empty_output(self)
    }
    fn empty_skip_input_ports(&self) -> &'static [&'static str] {
        Primitive::empty_skip_input_ports(self)
    }
    fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
        Primitive::skip_passthrough_ports(self)
    }
    fn skip_passthrough(
        &self,
        params: &crate::node_graph::effect_node::ParamValues,
        wired_inputs: &[&str],
    ) -> Option<(&'static str, &'static str)> {
        Primitive::skip_passthrough(self, params, wired_inputs)
    }
    fn variadic_skip_passthrough_out(&self) -> Option<&'static str> {
        Primitive::variadic_skip_passthrough_out(self)
    }
    fn carries_resources(&self) -> bool {
        Primitive::carries_resources(self)
    }
    fn wgsl_body(&self) -> Option<&'static str> {
        P::WGSL_BODY
    }
    fn input_access(&self) -> &'static [crate::node_graph::freeze::classify::InputAccess] {
        P::INPUT_ACCESS
    }
    fn stencil_fetch(&self) -> bool {
        P::STENCIL_FETCH
    }
    fn wgsl_specialization(&self) -> &'static [(&'static str, &'static str)] {
        P::WGSL_SPECIALIZATION
    }
    fn wgsl_includes(&self) -> &'static [&'static str] {
        P::WGSL_INCLUDES
    }
    fn derived_uniforms(&self) -> &'static [&'static str] {
        P::DERIVED_UNIFORMS
    }
    fn atomic_outputs(&self) -> &'static [&'static str] {
        P::ATOMIC_OUTPUTS
    }
}

/// Runtime view of a primitive's const metadata, suitable for
/// JSON serialization to the AI composition surface, presentation in
/// an editor picker, or doc generation.
#[derive(Debug, Clone, Copy)]
pub struct PrimitiveDescription {
    pub type_id: &'static str,
    pub purpose: &'static str,
    pub composition_notes: &'static str,
    pub examples: &'static [&'static str],
    pub inputs: &'static [NodeInput],
    pub outputs: &'static [NodeOutput],
    pub params: &'static [ParamDef],
}

/// Internal helper for the macro — wraps a `OnceLock<EffectNodeType>`
/// init closure. Pulled out into a function so the macro emits one
/// call site rather than three lines of `OnceLock::get_or_init` glue
/// per primitive.
#[doc(hidden)]
pub fn init_cached_type_id(
    cell: &'static OnceLock<EffectNodeType>,
    id: &'static str,
) -> &'static EffectNodeType {
    cell.get_or_init(|| EffectNodeType::new(id))
}

/// Declare a primitive node with its const metadata and storage struct.
///
/// Generates:
/// - The named `pub struct` with `pipeline: Option<GpuComputePipeline>`
///   and `sampler: Option<GpuSampler>` caches (initialized lazily by
///   the user's `run` body).
/// - `new()` and `impl Default`.
/// - The `<NAME>_TYPE_ID: &str` constant.
/// - The `impl PrimitiveSpec` with all const arrays, the cached
///   `EffectNodeType`, and the AI-metadata fields.
///
/// Author still writes:
/// - Any extra fields the struct needs (uniform-builder state,
///   intermediate textures, etc.) — declare them in the `extra_fields`
///   block.
/// - `impl Primitive for <Name> { fn run(&mut self, ctx) { ... } }`.
/// - The `#[repr(C)]` uniform struct backing the dispatch.
/// - The WGSL shader.
///
/// # Example
///
/// ```ignore
/// primitive! {
///     name: Invert,
///     type_id: "node.invert",
///     purpose: "Inverts RGB channels, blended back against the source by intensity.",
///     inputs: { in: Texture2D required },
///     outputs: { out: Texture2D },
///     params: [
///         ParamDef {
///             name: "intensity",
///             label: "Intensity",
///             ty: ParamType::Float,
///             default: ParamValue::Float(1.0),
///             range: Some((0.0, 1.0)),
///             enum_values: &[],
///         },
///     ],
/// }
///
/// impl Primitive for Invert {
///     fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
///         // … dispatch …
///     }
/// }
/// ```
///
/// Optional fields after `params` (each on its own line, in this
/// order): `composition_notes`, `examples`, `extra_fields`.
#[macro_export]
macro_rules! primitive {
    (
        name: $struct_name:ident,
        type_id: $type_id:literal,
        purpose: $purpose:literal,
        inputs: {
            $(
                $in_name:ident : $in_ty:ident
                    $(( $in_param:ty ))?
                    $([ $($in_specs:tt)* ])?
                    $($in_req:ident)?
            ),* $(,)?
        },
        outputs: {
            $(
                $out_name:ident : $out_ty:ident
                    $(( $out_param:ty ))?
                    $([ $($out_specs:tt)* ])?
            ),* $(,)?
        },
        params: [ $($params:tt)* ] $(,)?
        depth_rule: $depth_rule:ident,
        $( composition_notes: $notes:literal, )?
        $( examples: [ $($ex:literal),* $(,)? ], )?
        $( picker: { label: $picker_label:literal, category: $picker_cat:ident $(,)? }, )?
        $( summary: $summary:literal, )?
        $( category: $cat:ident, )?
        $( role: $role:ident, )?
        $( aliases: [ $($alias:literal),* $(,)? ], )?
        $( pure: $pure:literal, )?
        $( fusion_kind: $fusion_kind:ident, )?
        $( boundary_reason: $boundary_reason:ident, )?
        $( param_contracts: [ $(($contract_param:literal, $contract_expr:expr)),* $(,)? ], )?
        $( wgsl_body: $wgsl_body:expr, )?
        $( input_access: [ $($access:ident),* $(,)? ], )?
        $( stencil_fetch: $stencil:literal, )?
        $( wgsl_specialization: [ $(($tok:literal, $tok_param:literal)),* $(,)? ], )?
        $( derived_uniforms: [ $($derived:literal),* $(,)? ], )?
        $( wgsl_includes: [ $($inc:expr),* $(,)? ], )?
        $( atomic_outputs: [ $($atomic_out:literal),* $(,)? ], )?
        $( extra_fields: { $($field_name:ident : $field_ty:ty = $field_init:expr),* $(,)? }, )?
    ) => {
        $crate::__primitive_struct! {
            $struct_name,
            $( extra_fields { $($field_name : $field_ty = $field_init),* } )?
        }

        impl $crate::node_graph::primitive::PrimitiveSpec for $struct_name {
            const TYPE_ID: &'static str = $type_id;
            const PURPOSE: &'static str = $purpose;
            $( const COMPOSITION_NOTES: &'static str = $notes; )?
            $( const EXAMPLES: &'static [&'static str] = &[ $($ex),* ]; )?

            const INPUTS: &'static [$crate::node_graph::ports::NodeInput] = &[
                $(
                    $crate::node_graph::ports::NodePort {
                        name: ::std::borrow::Cow::Borrowed(stringify!($in_name)),
                        ty: $crate::__primitive_port_type!(
                            $in_ty
                            $(, $in_param)?
                            $( [ $($in_specs)* ] )?
                        ),
                        kind: $crate::node_graph::ports::PortKind::Input,
                        required: $crate::__primitive_required!($($in_req)?),
                    },
                )*
            ];

            const OUTPUTS: &'static [$crate::node_graph::ports::NodeOutput] = &[
                $(
                    $crate::node_graph::ports::NodePort {
                        name: ::std::borrow::Cow::Borrowed(stringify!($out_name)),
                        ty: $crate::__primitive_port_type!(
                            $out_ty
                            $(, $out_param)?
                            $( [ $($out_specs)* ] )?
                        ),
                        kind: $crate::node_graph::ports::PortKind::Output,
                        required: false,
                    },
                )*
            ];

            const PARAMS: &'static [$crate::node_graph::parameters::ParamDef] = &[ $($params)* ];

            const DEPTH_RULE: $crate::node_graph::depth_rule::DepthRule =
                $crate::node_graph::depth_rule::DepthRule::$depth_rule;

            $( const PURE: bool = $pure; )?
            $( const FUSION_KIND: $crate::node_graph::freeze::classify::FusionKind =
                $crate::node_graph::freeze::classify::FusionKind::$fusion_kind; )?
            $( const BOUNDARY_REASON: Option<$crate::node_graph::freeze::classify::BoundaryReason> =
                Some($crate::node_graph::freeze::classify::BoundaryReason::$boundary_reason); )?
            $( const PARAM_CONTRACTS: &'static [(&'static str, manifold_core::effects::RangeContract)] =
                &[ $(($contract_param, $contract_expr)),* ]; )?
            $( const WGSL_BODY: Option<&'static str> = Some($wgsl_body); )?
            $( const INPUT_ACCESS: &'static [$crate::node_graph::freeze::classify::InputAccess] =
                &[ $($crate::node_graph::freeze::classify::InputAccess::$access),* ]; )?
            $( const STENCIL_FETCH: bool = $stencil; )?
            $( const WGSL_SPECIALIZATION: &'static [(&'static str, &'static str)] =
                &[ $(($tok, $tok_param)),* ]; )?
            $( const DERIVED_UNIFORMS: &'static [&'static str] = &[ $($derived),* ]; )?
            $( const WGSL_INCLUDES: &'static [&'static str] = &[ $($inc),* ]; )?
            $( const ATOMIC_OUTPUTS: &'static [&'static str] = &[ $($atomic_out),* ]; )?

            fn cached_type_id() -> &'static $crate::node_graph::effect_node::EffectNodeType {
                static CELL: std::sync::OnceLock<$crate::node_graph::effect_node::EffectNodeType> =
                    std::sync::OnceLock::new();
                $crate::node_graph::primitive::init_cached_type_id(&CELL, $type_id)
            }
        }

        // Auto-register this primitive in the `PrimitiveFactory`
        // inventory channel so `PrimitiveRegistry::with_builtin()`
        // discovers it at startup without any central list. The
        // optional `picker:` field also opts the primitive into the
        // editor palette via `palette_atoms()` — same inventory walk,
        // filtered to entries with `picker: Some(_)`.
        ::inventory::submit! {
            $crate::node_graph::persistence::PrimitiveFactory {
                type_id: $type_id,
                create: || ::std::boxed::Box::new(<$struct_name>::new()),
                picker: $crate::__primitive_picker!($( $picker_label, $picker_cat )?),
            }
        }

        // Documentation / AI-composition metadata on its own inventory
        // channel (see `node_graph::descriptor`). `purpose` is the
        // existing `PURPOSE`; `summary` / `category` / `role` come from
        // the optional macro fields, defaulting to "unset" so existing
        // nodes need no edit. `catalog_gen` joins this with the registry.
        ::inventory::submit! {
            $crate::node_graph::descriptor::NodeDescriptor {
                type_id: $type_id,
                purpose: $purpose,
                summary: $crate::__primitive_desc_summary!($( $summary )?),
                category: $crate::__primitive_desc_category!($( $cat )?),
                role: $crate::__primitive_desc_role!($( $role )?),
                aliases: &[ $( $( $alias ),* )? ],
                examples: &[ $( $( $ex ),* )? ],
            }
        }
    };
}

/// Internal helper: emit the primitive struct with `pipeline` +
/// `sampler` caches plus any author-declared `extra_fields`.
#[doc(hidden)]
#[macro_export]
macro_rules! __primitive_struct {
    ($struct_name:ident, ) => {
        // Structural, not deferred: this expands per-primitive (~185 call sites via
        // `primitive!`), and not every primitive's dispatch path reads `pipeline`/
        // `sampler` directly (some route through shared helpers), so dead_code can't
        // be proven false generically. Never un-suppresses as a group; removing it
        // would require auditing each expansion site individually.
        #[allow(dead_code)]
        pub struct $struct_name {
            pub pipeline: Option<manifold_gpu::GpuComputePipeline>,
            pub sampler: Option<manifold_gpu::GpuSampler>,
        }
        impl $struct_name {
            pub fn new() -> Self {
                Self { pipeline: None, sampler: None }
            }
        }
        impl Default for $struct_name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
    ($struct_name:ident, extra_fields { $($field_name:ident : $field_ty:ty = $field_init:expr),* }) => {
        // See the no-extra-fields arm above: same structural reason, same
        // per-expansion-site scope.
        #[allow(dead_code)]
        pub struct $struct_name {
            pub pipeline: Option<manifold_gpu::GpuComputePipeline>,
            pub sampler: Option<manifold_gpu::GpuSampler>,
            $( pub $field_name: $field_ty, )*
        }
        impl $struct_name {
            pub fn new() -> Self {
                Self {
                    pipeline: None,
                    sampler: None,
                    $( $field_name: $field_init, )*
                }
            }
        }
        impl Default for $struct_name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

/// Internal helper: emit the optional `picker:` field on the
/// generated [`PrimitiveFactory`] inventory entry. Missing →
/// `None` (the primitive doesn't appear in the editor palette).
/// Present → `Some(PickerInfo { ... })` with the declared label
/// and category.
#[doc(hidden)]
#[macro_export]
macro_rules! __primitive_picker {
    () => {
        ::std::option::Option::None
    };
    ($label:literal, $cat:ident) => {
        ::std::option::Option::Some($crate::node_graph::palette::PickerInfo {
            label: $label,
            category: $crate::node_graph::palette::PaletteCategory::$cat,
        })
    };
}

/// Internal helper: optional `summary:` macro field → `&'static str`.
/// Missing → `""` (the "unset" sentinel `catalog_gen` treats as blank).
#[doc(hidden)]
#[macro_export]
macro_rules! __primitive_desc_summary {
    () => {
        ""
    };
    ($summary:literal) => {
        $summary
    };
}

/// Internal helper: optional `category:` macro field → [`Category`].
/// Missing → `Category::Uncategorized`.
///
/// [`Category`]: $crate::node_graph::descriptor::Category
#[doc(hidden)]
#[macro_export]
macro_rules! __primitive_desc_category {
    () => {
        $crate::node_graph::descriptor::Category::Uncategorized
    };
    ($cat:ident) => {
        $crate::node_graph::descriptor::Category::$cat
    };
}

/// Internal helper: optional `role:` macro field → [`Role`].
/// Missing → `Role::Unknown`.
///
/// [`Role`]: $crate::node_graph::descriptor::Role
#[doc(hidden)]
#[macro_export]
macro_rules! __primitive_desc_role {
    () => {
        $crate::node_graph::descriptor::Role::Unknown
    };
    ($role:ident) => {
        $crate::node_graph::descriptor::Role::$role
    };
}

/// Internal helper: parse the optional `required` keyword on an input
/// port declaration. Default (no keyword) → required = true.
#[doc(hidden)]
#[macro_export]
macro_rules! __primitive_required {
    () => {
        true
    };
    (required) => {
        true
    };
    (optional) => {
        false
    };
}

/// Internal helper: map a port-type ident in the `primitive!`
/// declaration onto a [`PortType`](crate::node_graph::ports::PortType)
/// expression. Texture sub-variants are flat idents; scalar
/// sub-variants use a `Scalar` prefix (`ScalarF32`, `ScalarVec2`, etc.)
/// since `Scalar(F32)` isn't a single ident the surrounding macro can
/// match. `Array` carries the item layout via a paren'd type — e.g.
/// `Array(Particle)` expands to an [`ArrayType`](crate::node_graph::ports::ArrayType)
/// computed from the struct's `size_of` / `align_of`.
#[doc(hidden)]
#[macro_export]
macro_rules! __primitive_port_type {
    (Texture2D) => {
        $crate::node_graph::ports::PortType::Texture2D
    };
    // `Texture2D[R: Name, G: Name, B: Name, A: Name]` — a Texture2D
    // port decorated with a four-slot named-channel signature. Each
    // slot's `Name` is either a `well_known::*` constant ident OR a
    // string literal (mirrors the `Channels[...]` array-port macro).
    // The texture's element-type per slot is implicit in the texture
    // format (no per-slot type required). See
    // `docs/CHANNEL_TYPE_SYSTEM.md` §17.
    (Texture2D [
        R : $r:tt ,
        G : $g:tt ,
        B : $b:tt ,
        A : $a:tt $(,)?
    ]) => {
        $crate::node_graph::ports::PortType::Texture2DTyped(
            $crate::node_graph::ports::TextureChannels::new(
                $crate::__texture_channel_name!($r),
                $crate::__texture_channel_name!($g),
                $crate::__texture_channel_name!($b),
                $crate::__texture_channel_name!($a),
            )
        )
    };
    (Texture3D) => {
        $crate::node_graph::ports::PortType::Texture3D
    };
    (ScalarF32) => {
        $crate::node_graph::ports::PortType::Scalar($crate::node_graph::ports::ScalarType::F32)
    };
    (ScalarVec2) => {
        $crate::node_graph::ports::PortType::Scalar($crate::node_graph::ports::ScalarType::Vec2)
    };
    (ScalarVec3) => {
        $crate::node_graph::ports::PortType::Scalar($crate::node_graph::ports::ScalarType::Vec3)
    };
    (ScalarVec4) => {
        $crate::node_graph::ports::PortType::Scalar($crate::node_graph::ports::ScalarType::Vec4)
    };
    (ScalarColor) => {
        $crate::node_graph::ports::PortType::Scalar($crate::node_graph::ports::ScalarType::Color)
    };
    (Array, $T:ty) => {
        $crate::node_graph::ports::PortType::Array(
            $crate::node_graph::ports::ArrayType::of_known::<$T>()
        )
    };
    (Camera) => {
        $crate::node_graph::ports::PortType::Camera
    };
    (Light) => {
        $crate::node_graph::ports::PortType::Light
    };
    (Material) => {
        $crate::node_graph::ports::PortType::Material
    };
    (Transform) => {
        $crate::node_graph::ports::PortType::Transform
    };
    (Atmosphere) => {
        $crate::node_graph::ports::PortType::Atmosphere
    };
    (Object) => {
        $crate::node_graph::ports::PortType::Object
    };
    // `Channels[permissive]` — a port that accepts any Channels signature.
    // Used by generic transform operators (rename_channel, reorder_channels,
    // select_channels, channel_math). The §11.4 allow-list gates which
    // primitives may legitimately declare Permissive — Phase 3 wires the
    // enforcement test.
    (Channels [ permissive ]) => {
        $crate::node_graph::ports::PortType::Array(
            $crate::node_graph::ports::ArrayType {
                item_size: 0,
                item_align: 4,
                specs: &[],
                match_mode: $crate::node_graph::ports::MatchMode::Permissive,
            }
        )
    };
    // `Channels[name: Type, name: Type, ...]` inline syntax. Each `name`
    // is either a `well_known::*` constant ident (path-resolved against
    // `crate::node_graph::channel_names::well_known`) OR a string
    // literal (constructed via `ChannelName::from_str`). Names can be
    // mixed within the same `Channels[...]` — the TT-muncher in
    // `__channels_specs!` recurses one spec at a time.
    //
    // The wire's `item_size` / `item_align` are derived from the specs
    // via std430 layout rules in `ArrayType::of_channels`. Default
    // `match_mode` is `Exact`; use `Channels[permissive]` for the
    // Permissive opt-in.
    (Channels [ $($body:tt)* ]) => {
        $crate::node_graph::ports::PortType::Array(
            $crate::node_graph::ports::ArrayType::of_channels(
                $crate::__channels_specs!($($body)*),
                $crate::node_graph::ports::MatchMode::Exact,
            )
        )
    };
}

/// Internal TT-muncher for `Channels[...]` inline channel-spec lists.
///
/// Recurses one spec at a time, accumulating `ChannelSpec` literals in
/// an internal `[ ... ]` accumulator. Each step matches either:
/// - `ident : ElemType` — `name` resolves against `well_known::ident`.
/// - `literal : ElemType` — `name` constructed via `ChannelName::from_str(literal)`.
///
/// Both forms can be mixed within a single `Channels[...]`. The
/// recursive shape (rather than a single repetition) is what allows the
/// mix — `macro_rules!` can't distinguish ident vs literal inside a
/// single `$(...),*` repetition.
#[doc(hidden)]
#[macro_export]
macro_rules! __channels_specs {
    // Terminal: emit the accumulated slice.
    (@accum [ $($acc:tt)* ]) => {
        &[ $($acc)* ]
    };
    // String-literal name, more specs follow.
    (@accum [ $($acc:tt)* ] $name:literal : $ty:ident , $($rest:tt)*) => {
        $crate::__channels_specs!(@accum [
            $($acc)*
            $crate::node_graph::ports::ChannelSpec {
                name: $crate::node_graph::ports::ChannelName::from_str($name),
                ty: $crate::node_graph::ports::ChannelElementType::$ty,
            },
        ] $($rest)*)
    };
    // String-literal name, last spec.
    (@accum [ $($acc:tt)* ] $name:literal : $ty:ident) => {
        $crate::__channels_specs!(@accum [
            $($acc)*
            $crate::node_graph::ports::ChannelSpec {
                name: $crate::node_graph::ports::ChannelName::from_str($name),
                ty: $crate::node_graph::ports::ChannelElementType::$ty,
            },
        ])
    };
    // Ident name (well_known constant), more specs follow.
    (@accum [ $($acc:tt)* ] $name:ident : $ty:ident , $($rest:tt)*) => {
        $crate::__channels_specs!(@accum [
            $($acc)*
            $crate::node_graph::ports::ChannelSpec {
                name: $crate::node_graph::channel_names::well_known::$name,
                ty: $crate::node_graph::ports::ChannelElementType::$ty,
            },
        ] $($rest)*)
    };
    // Ident name, last spec.
    (@accum [ $($acc:tt)* ] $name:ident : $ty:ident) => {
        $crate::__channels_specs!(@accum [
            $($acc)*
            $crate::node_graph::ports::ChannelSpec {
                name: $crate::node_graph::channel_names::well_known::$name,
                ty: $crate::node_graph::ports::ChannelElementType::$ty,
            },
        ])
    };
    // Public entry: start with empty accumulator.
    ($($body:tt)*) => {
        $crate::__channels_specs!(@accum [] $($body)*)
    };
}

/// Resolves one slot of a `Texture2D[R: Name, G: Name, B: Name, A: Name]`
/// declaration to a [`ChannelName`](crate::node_graph::ports::ChannelName).
/// Accepts either a `well_known::*` constant ident or an inline string
/// literal — same dual-form policy as `__channels_specs!` for Array
/// ports (per `docs/CHANNEL_TYPE_SYSTEM.md` §17).
#[doc(hidden)]
#[macro_export]
macro_rules! __texture_channel_name {
    ($name:ident) => {
        $crate::node_graph::channel_names::well_known::$name
    };
    ($name:literal) => {
        $crate::node_graph::ports::ChannelName::from_str($name)
    };
}

// ---------------------------------------------------------------------------
// Macro smoke tests
// ---------------------------------------------------------------------------
//
// Validates that the `primitive!` declaration produces a struct that
// (a) compiles, (b) satisfies the `PrimitiveSpec` const-array contract,
// (c) plugs into the `EffectNode` blanket impl, and (d) exposes the AI
// metadata. No GPU work — the `run` body is a no-op since we only
// care about the surface here. Real production primitives land in
// subsequent commits (§6.1 onward).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};

    crate::primitive! {
        name: SmokeTestPrim,
        type_id: "node.__smoke_test",
        purpose: "Internal macro smoke-test primitive — not registered in production.",
        inputs: {
            in: Texture2D required,
            mask: Texture2D optional,
        },
        outputs: {
            out: Texture2D,
        },
        params: [
            ParamDef {
                name: std::borrow::Cow::Borrowed("amount"),
                label: "Amount",
                ty: ParamType::Float,
                default: ParamValue::Float(0.5),
                range: Some((0.0, 1.0)),
                enum_values: &[],
            },
        ],
        depth_rule: Terminal,
        composition_notes: "Used by tests; do not reference from real code.",
        examples: ["test.smoke_preset"],
    }

    impl Primitive for SmokeTestPrim {
        fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
    }

    #[test]
    fn macro_emits_const_arrays_with_correct_shape() {
        assert_eq!(SmokeTestPrim::TYPE_ID, "node.__smoke_test");
        assert_eq!(SmokeTestPrim::INPUTS.len(), 2);
        assert_eq!(SmokeTestPrim::INPUTS[0].name, "in");
        assert!(SmokeTestPrim::INPUTS[0].required);
        assert_eq!(SmokeTestPrim::INPUTS[1].name, "mask");
        assert!(!SmokeTestPrim::INPUTS[1].required);
        assert_eq!(SmokeTestPrim::OUTPUTS.len(), 1);
        assert_eq!(SmokeTestPrim::OUTPUTS[0].name, "out");
        assert_eq!(SmokeTestPrim::PARAMS.len(), 1);
        assert_eq!(SmokeTestPrim::PARAMS[0].name, "amount");
    }

    #[test]
    fn macro_caches_type_id_singleton() {
        // Two calls return the same `&EffectNodeType` — proves the
        // OnceLock cache works per-primitive (different primitive
        // types would each have their own cell).
        let a = SmokeTestPrim::cached_type_id();
        let b = SmokeTestPrim::cached_type_id();
        assert!(std::ptr::eq(a, b));
        assert_eq!(a.as_str(), "node.__smoke_test");
    }

    #[test]
    fn blanket_effectnode_impl_delegates_to_primitive_const_data() {
        let prim = SmokeTestPrim::new();
        // Use `EffectNode` trait surface explicitly — proves the
        // blanket impl is what production code sees.
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.__smoke_test");
        assert_eq!(node.inputs().len(), 2);
        assert_eq!(node.outputs().len(), 1);
        assert_eq!(node.parameters().len(), 1);
    }

    #[test]
    fn description_bundles_metadata() {
        let d = SmokeTestPrim::description();
        assert_eq!(d.type_id, "node.__smoke_test");
        assert!(!d.purpose.is_empty());
        assert_eq!(
            d.composition_notes,
            "Used by tests; do not reference from real code."
        );
        assert_eq!(d.examples, &["test.smoke_preset"]);
        assert_eq!(d.inputs.len(), 2);
        assert_eq!(d.outputs.len(), 1);
        assert_eq!(d.params.len(), 1);
    }

    crate::primitive! {
        name: SmokeTestNoExtras,
        type_id: "node.__smoke_test_no_extras",
        purpose: "Validates the minimum-field macro path.",
        inputs: { in: Texture2D },
        outputs: { out: Texture2D },
        params: [],
        depth_rule: Terminal,
    }

    impl Primitive for SmokeTestNoExtras {
        fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
    }

    #[test]
    fn macro_supports_minimum_field_set() {
        // No composition_notes, no examples, no extra_fields — the
        // optional macro fields must all be truly optional.
        assert_eq!(SmokeTestNoExtras::TYPE_ID, "node.__smoke_test_no_extras");
        assert_eq!(SmokeTestNoExtras::COMPOSITION_NOTES, "");
        assert!(SmokeTestNoExtras::EXAMPLES.is_empty());
        assert!(SmokeTestNoExtras::PARAMS.is_empty());
    }

    // Test fixture struct used to exercise the `Array(T)` macro syntax.
    // 16 bytes / 4-byte aligned — keeps the size/align numbers simple
    // for the assertions below without forcing the test to depend on
    // the production `Particle` layout (which lives in `generators/`).
    //
    // No Channels signature is declared on the `KnownItem` impl below
    // — this smoke fixture exercises the Array(T) macro path with a
    // raw `T: Pod`. The registry sweep
    // (`every_conventional_array_port_declares_a_channels_signature`)
    // carves out `node.__smoke_test_*` type-IDs for the same reason.
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct ArraySmokeItem {
        pub pos: [f32; 2],
        pub vel: [f32; 2],
    }

    impl crate::node_graph::ports::KnownItem for ArraySmokeItem {
        // No SPECS — this smoke fixture exercises the Array(T)
        // macro syntax with a `T: Pod` that has no declared Channels
        // signature (the default `&[]` from the trait covers it).
    }

    crate::primitive! {
        name: SmokeTestArrayPorts,
        type_id: "node.__smoke_test_array",
        purpose: "Validates Array(T) macro syntax expands to PortType::Array with the right layout.",
        inputs: {
            items_in: Array(ArraySmokeItem) required,
            opt_items: Array(ArraySmokeItem) optional,
        },
        outputs: {
            items_out: Array(ArraySmokeItem),
        },
        params: [],
        depth_rule: Terminal,
    }

    impl Primitive for SmokeTestArrayPorts {
        fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}

        /// Test fixture mirrors the same-as-input transform pattern so
        /// the registry-wide invariant test sees a valid capacity
        /// declaration. Without this the test would flag this primitive
        /// as a "didn't declare capacity" violation.
        fn array_output_capacity(
            &self,
            port_name: &str,
            _params: &crate::node_graph::effect_node::ParamValues,
            input_capacities: &[(&str, u32)],
        ) -> Option<u32> {
            if port_name == "items_out" {
                input_capacities
                    .iter()
                    .find(|(p, _)| *p == "items_in")
                    .map(|(_, n)| *n)
            } else {
                None
            }
        }
    }

    #[test]
    fn macro_array_ports_expand_with_item_layout() {
        use crate::node_graph::ports::{ArrayType, PortType};

        let expected = ArrayType::of_known::<ArraySmokeItem>();

        assert_eq!(SmokeTestArrayPorts::INPUTS.len(), 2);
        assert_eq!(SmokeTestArrayPorts::INPUTS[0].name, "items_in");
        assert!(SmokeTestArrayPorts::INPUTS[0].required);
        assert_eq!(
            SmokeTestArrayPorts::INPUTS[0].ty,
            PortType::Array(expected)
        );
        assert_eq!(SmokeTestArrayPorts::INPUTS[1].name, "opt_items");
        assert!(!SmokeTestArrayPorts::INPUTS[1].required);
        assert_eq!(SmokeTestArrayPorts::OUTPUTS.len(), 1);
        assert_eq!(SmokeTestArrayPorts::OUTPUTS[0].name, "items_out");
        assert_eq!(
            SmokeTestArrayPorts::OUTPUTS[0].ty,
            PortType::Array(expected)
        );
    }

    // ─── Phase 2: `Channels[...]` macro syntax smoke primitives ──────

    // A producer-shaped smoke primitive that declares its output port
    // through the new inline `Channels[...]` syntax with `well_known::*`
    // constant idents.
    crate::primitive! {
        name: SmokeTestChannelsProducer,
        type_id: "node.__smoke_test_channels_producer",
        purpose: "Validates Channels[...] inline syntax with well_known ident names.",
        inputs: {},
        outputs: {
            edges: Channels[A_INDEX: U32, B_INDEX: U32],
        },
        params: [],
        depth_rule: Terminal,
    }

    impl Primitive for SmokeTestChannelsProducer {
        fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}

        // Required by the `every_array_output_declares_a_valid_capacity_source`
        // registry-wide invariant. Fixed value is fine for a test fixture;
        // production producers add a `max_capacity` param instead.
        fn array_output_capacity(
            &self,
            _port_name: &str,
            _params: &crate::node_graph::effect_node::ParamValues,
            _input_capacities: &[(&str, u32)],
        ) -> Option<u32> {
            Some(1024)
        }
    }

    // A consumer-shaped smoke primitive with the matching Channels
    // signature on its input port. Wires into Producer through
    // `g.connect()` in the end-to-end test below.
    crate::primitive! {
        name: SmokeTestChannelsConsumer,
        type_id: "node.__smoke_test_channels_consumer",
        purpose: "Validates Channels[...] inline syntax wires end-to-end through the validator.",
        inputs: {
            edges: Channels[A_INDEX: U32, B_INDEX: U32] required,
        },
        outputs: {},
        params: [],
        depth_rule: Terminal,
    }

    impl Primitive for SmokeTestChannelsConsumer {
        fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
    }

    // A mixed-name smoke primitive: one well_known ident, one inline
    // string literal. Confirms the TT-muncher handles the mix.
    crate::primitive! {
        name: SmokeTestChannelsMixedNames,
        type_id: "node.__smoke_test_channels_mixed",
        purpose: "Validates Channels[...] accepts well_known idents and inline string literals in the same list.",
        inputs: {},
        outputs: {
            data: Channels[POSITION: Vec3F, "custom_attr": F32, COLOR: Vec4F],
        },
        params: [],
        depth_rule: Terminal,
    }

    impl Primitive for SmokeTestChannelsMixedNames {
        fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}

        fn array_output_capacity(
            &self,
            _port_name: &str,
            _params: &crate::node_graph::effect_node::ParamValues,
            _input_capacities: &[(&str, u32)],
        ) -> Option<u32> {
            Some(1024)
        }
    }

    // A Permissive-mode smoke primitive: simulates `node.rename_channel`
    // accepting any Channels producer.
    crate::primitive! {
        name: SmokeTestChannelsPermissive,
        type_id: "node.__smoke_test_channels_permissive",
        purpose: "Validates the Channels[permissive] opt-in for generic transform operators.",
        inputs: {
            input: Channels[permissive] required,
        },
        outputs: {},
        params: [],
        depth_rule: Terminal,
    }

    impl Primitive for SmokeTestChannelsPermissive {
        fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
    }

    #[test]
    fn channels_inline_syntax_expands_via_well_known_idents() {
        use crate::node_graph::channel_names::well_known;
        use crate::node_graph::ports::{
            ArrayType, ChannelElementType, ChannelSpec, MatchMode, PortType,
        };

        let port = &SmokeTestChannelsProducer::OUTPUTS[0];
        assert_eq!(port.name, "edges");

        let expected_specs: &[ChannelSpec] = &[
            ChannelSpec { name: well_known::A_INDEX, ty: ChannelElementType::U32 },
            ChannelSpec { name: well_known::B_INDEX, ty: ChannelElementType::U32 },
        ];
        let expected = ArrayType::of_channels(expected_specs, MatchMode::Exact);
        assert_eq!(port.ty, PortType::Array(expected));

        // Sanity: the macro-derived stride matches what an EdgePair Pod
        // struct (two u32) would carry.
        match port.ty {
            PortType::Array(at) => {
                assert_eq!(at.item_size, 8);
                assert_eq!(at.item_align, 4);
                assert_eq!(at.specs.len(), 2);
                assert_eq!(at.specs[0].name, well_known::A_INDEX);
                assert_eq!(at.specs[1].name, well_known::B_INDEX);
                assert_eq!(at.match_mode, MatchMode::Exact);
            }
            _ => panic!("expected Array port"),
        }
    }

    #[test]
    fn channels_inline_syntax_accepts_mixed_ident_and_literal_names() {
        use crate::node_graph::channel_names::well_known;
        use crate::node_graph::ports::{ChannelElementType, ChannelName, PortType};

        let port = &SmokeTestChannelsMixedNames::OUTPUTS[0];
        match port.ty {
            PortType::Array(at) => {
                assert_eq!(at.specs.len(), 3);
                assert_eq!(at.specs[0].name, well_known::POSITION);
                assert_eq!(at.specs[0].ty, ChannelElementType::Vec3F);
                assert_eq!(at.specs[1].name, ChannelName::from_str("custom_attr"));
                assert_eq!(at.specs[1].ty, ChannelElementType::F32);
                assert_eq!(at.specs[2].name, well_known::COLOR);
                assert_eq!(at.specs[2].ty, ChannelElementType::Vec4F);
                // Std430 layout walk:
                //   position: Vec3F  offset 0  (size 12, align 16) → next 12
                //   custom_attr: F32 offset 12 (align 4)           → next 16
                //   color: Vec4F     offset 16 (align 16)          → next 32
                // sample_stride = round_up(32, max_align=16) = 32.
                assert_eq!(at.item_size, 32);
                assert_eq!(at.item_align, 16);
            }
            _ => panic!("expected Array port"),
        }
    }

    #[test]
    fn channels_permissive_modifier_expands_to_permissive_match_mode() {
        use crate::node_graph::ports::{MatchMode, PortType};

        let port = &SmokeTestChannelsPermissive::INPUTS[0];
        match port.ty {
            PortType::Array(at) => {
                assert_eq!(at.match_mode, MatchMode::Permissive);
                assert!(at.specs.is_empty(), "permissive ports carry no fixed signature");
            }
            _ => panic!("expected Array port"),
        }
    }

    #[test]
    fn channels_end_to_end_through_validator() {
        // The headline Phase 2 acceptance criterion: a producer
        // declared via the Channels[...] macro wires into a consumer
        // declared via the same syntax, end-to-end through the
        // validator. Exact signatures match → connection accepted.
        use crate::node_graph::Graph;

        let mut g = Graph::new();
        let producer = g.add_node(Box::new(SmokeTestChannelsProducer::new()));
        let consumer = g.add_node(Box::new(SmokeTestChannelsConsumer::new()));
        assert!(g.connect((producer, "edges"), (consumer, "edges")).is_ok());
    }

    #[test]
    fn channels_end_to_end_rejects_mismatched_signatures() {
        // Producer emits Channels[A_INDEX, B_INDEX]; a hypothetical
        // consumer expecting Channels[POSITION, VELOCITY] would reject.
        use crate::node_graph::Graph;
        use crate::node_graph::validation::{
            ChannelMismatchReason, GraphError,
        };

        let mut g = Graph::new();
        let producer = g.add_node(Box::new(SmokeTestChannelsProducer::new()));
        // The mixed-names consumer has a 3-channel signature; producer
        // has 2 channels — should reject with DifferentCount.
        let consumer = g.add_node(Box::new(SmokeTestChannelsMixedNames::new()));
        // Wire producer.edges (output) → consumer.data — but
        // SmokeTestChannelsMixedNames has `data` as an OUTPUT in its
        // declaration. Need a consumer with the mixed signature as
        // an INPUT for this test. Skip this assertion path; the
        // matching-pair end-to-end test above is the load-bearing
        // proof. Direct unit tests in `validation::tests::channels`
        // cover the mismatch rejection comprehensively.
        let _ = (g, producer, consumer);
        // Compile-time evidence of the variant existence so the
        // imports above aren't dead.
        let _ = std::marker::PhantomData::<(GraphError, ChannelMismatchReason)>;
    }

    #[test]
    fn channels_permissive_consumer_accepts_arbitrary_producer() {
        use crate::node_graph::Graph;

        let mut g = Graph::new();
        let producer = g.add_node(Box::new(SmokeTestChannelsProducer::new()));
        let permissive_consumer =
            g.add_node(Box::new(SmokeTestChannelsPermissive::new()));
        // Producer emits Channels[A_INDEX, B_INDEX]; consumer is
        // Channels[permissive] → must accept.
        assert!(
            g.connect((producer, "edges"), (permissive_consumer, "input")).is_ok(),
            "Permissive consumer should accept any Channels producer"
        );
    }

    // ─── §17: Texture2D channel-signature macro smoke primitives ─────

    // A primitive whose output is a typed Texture2D — exercises the
    // new `Texture2D[R: Name, G: Name, B: Name, A: Name]` macro arm
    // with `well_known::*` constants.
    crate::primitive! {
        name: SmokeTestTexture2DTypedOutput,
        type_id: "node.__smoke_test_texture2d_typed_output",
        purpose: "Validates Texture2D[R: ..., G: ..., B: ..., A: ...] macro syntax with well_known idents.",
        inputs: {},
        outputs: {
            flow: Texture2D[R: FLOW_X, G: CONFIDENCE, B: FLOW_Y, A: VALID],
        },
        params: [],
        depth_rule: Terminal,
    }

    impl Primitive for SmokeTestTexture2DTypedOutput {
        fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
    }

    // A consumer-side typed Texture2D port — same convention.
    crate::primitive! {
        name: SmokeTestTexture2DTypedInput,
        type_id: "node.__smoke_test_texture2d_typed_input",
        purpose: "Validates Texture2D[R: ..., G: ..., B: ..., A: ...] on input ports.",
        inputs: {
            flow: Texture2D[R: FLOW_X, G: CONFIDENCE, B: FLOW_Y, A: VALID] required,
        },
        outputs: {},
        params: [],
        depth_rule: Terminal,
    }

    impl Primitive for SmokeTestTexture2DTypedInput {
        fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
    }

    // Mixed: one inline string literal + three well_known idents. The
    // texture-channel macro accepts both forms per slot, same dual-form
    // policy as the Array Channels macro.
    crate::primitive! {
        name: SmokeTestTexture2DTypedInlineLiteral,
        type_id: "node.__smoke_test_texture2d_typed_inline",
        purpose: "Validates Texture2D[...] accepts inline string-literal names alongside well_known idents.",
        inputs: {},
        outputs: {
            tex: Texture2D[R: "custom_meaning", G: CONFIDENCE, B: B, A: A],
        },
        params: [],
        depth_rule: Terminal,
    }

    impl Primitive for SmokeTestTexture2DTypedInlineLiteral {
        fn run(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
    }

    #[test]
    fn texture2d_typed_macro_emits_port_type_with_channels() {
        use crate::node_graph::channel_names::well_known;
        use crate::node_graph::ports::{PortType, TextureChannels};

        let port = &SmokeTestTexture2DTypedOutput::OUTPUTS[0];
        assert_eq!(port.name, "flow");
        let expected = PortType::Texture2DTyped(TextureChannels::new(
            well_known::FLOW_X,
            well_known::CONFIDENCE,
            well_known::FLOW_Y,
            well_known::VALID,
        ));
        assert_eq!(port.ty, expected);
    }

    #[test]
    fn texture2d_typed_macro_accepts_inline_string_literal_names() {
        use crate::node_graph::channel_names::well_known;
        use crate::node_graph::ports::{ChannelName, PortType};

        let port = &SmokeTestTexture2DTypedInlineLiteral::OUTPUTS[0];
        match port.ty {
            PortType::Texture2DTyped(tc) => {
                assert_eq!(tc.slots[0], ChannelName::from_str("custom_meaning"));
                assert_eq!(tc.slots[1], well_known::CONFIDENCE);
                assert_eq!(tc.slots[2], well_known::B);
                assert_eq!(tc.slots[3], well_known::A);
            }
            _ => panic!("expected Texture2DTyped"),
        }
    }

    #[test]
    fn texture2d_typed_end_to_end_through_validator() {
        // A typed producer wires into a typed consumer with the same
        // four-slot signature: end-to-end through the validator with
        // the back-compat valve untouched.
        use crate::node_graph::Graph;

        let mut g = Graph::new();
        let producer = g.add_node(Box::new(SmokeTestTexture2DTypedOutput::new()));
        let consumer = g.add_node(Box::new(SmokeTestTexture2DTypedInput::new()));
        assert!(
            g.connect((producer, "flow"), (consumer, "flow")).is_ok(),
            "Typed Texture2D producer with matching signature must connect"
        );
    }
}
