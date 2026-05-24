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

    /// Reset persistent state. Default no-op; stateful primitives
    /// (Feedback, MipChain when state-backed) override to drop their
    /// previous-frame textures on seek.
    fn clear_state(&mut self) {}

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
    ) -> Option<(u32, u32)> {
        None
    }

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
    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        &[]
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
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
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
    fn canvas_sized_array_outputs(&self) -> &'static [&'static str] {
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
    fn clear_state(&mut self) {
        Primitive::clear_state(self);
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
    fn output_dims(
        &self,
        port: &str,
        canvas_dims: (u32, u32),
        input_dims: &[(&str, (u32, u32))],
    ) -> Option<(u32, u32)> {
        Primitive::output_dims(self, port, canvas_dims, input_dims)
    }
    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        Primitive::array_output_capacity(self, port_name, params, input_capacities)
    }
    fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
        Primitive::aliased_array_io(self)
    }
    fn canvas_sized_array_outputs(&self) -> &'static [&'static str] {
        Primitive::canvas_sized_array_outputs(self)
    }
    fn requires(&self) -> crate::node_graph::effect_node::NodeRequires {
        Primitive::requires(self)
    }
    fn breaks_dependency_cycle(&self) -> bool {
        Primitive::breaks_dependency_cycle(self)
    }
    fn state_capture_input_ports(&self) -> &'static [&'static str] {
        Primitive::state_capture_input_ports(self)
    }
    fn selected_input_branch(
        &self,
        params: &crate::node_graph::effect_node::ParamValues,
        wired_inputs: &[&str],
    ) -> Option<&'static str> {
        Primitive::selected_input_branch(self, params, wired_inputs)
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
                $in_name:ident : $in_ty:ident $(( $in_param:ty ))? $($in_req:ident)?
            ),* $(,)?
        },
        outputs: {
            $(
                $out_name:ident : $out_ty:ident $(( $out_param:ty ))?
            ),* $(,)?
        },
        params: [ $($params:tt)* ] $(,)?
        $( composition_notes: $notes:literal, )?
        $( examples: [ $($ex:literal),* $(,)? ], )?
        $( picker: { label: $picker_label:literal, category: $picker_cat:ident $(,)? }, )?
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
                        name: stringify!($in_name),
                        ty: $crate::__primitive_port_type!($in_ty $(, $in_param)?),
                        kind: $crate::node_graph::ports::PortKind::Input,
                        required: $crate::__primitive_required!($($in_req)?),
                    },
                )*
            ];

            const OUTPUTS: &'static [$crate::node_graph::ports::NodeOutput] = &[
                $(
                    $crate::node_graph::ports::NodePort {
                        name: stringify!($out_name),
                        ty: $crate::__primitive_port_type!($out_ty $(, $out_param)?),
                        kind: $crate::node_graph::ports::PortKind::Output,
                        required: false,
                    },
                )*
            ];

            const PARAMS: &'static [$crate::node_graph::parameters::ParamDef] = &[ $($params)* ];

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
    };
}

/// Internal helper: emit the primitive struct with `pipeline` +
/// `sampler` caches plus any author-declared `extra_fields`.
#[doc(hidden)]
#[macro_export]
macro_rules! __primitive_struct {
    ($struct_name:ident, ) => {
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
                name: "amount",
                label: "Amount",
                ty: ParamType::Float,
                default: ParamValue::Float(0.5),
                range: Some((0.0, 1.0)),
                enum_values: &[],
            },
        ],
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
    // Tagged `ItemKind::Anonymous` because no convention is asserted
    // for this purely-structural smoke test — the registry sweep
    // (`every_conventional_array_port_declares_a_kind`) carves out
    // the `node.__smoke_test_*` type-IDs for the same reason.
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct ArraySmokeItem {
        pub pos: [f32; 2],
        pub vel: [f32; 2],
    }

    impl crate::node_graph::ports::KnownItem for ArraySmokeItem {
        const ITEM_KIND: crate::node_graph::ports::ItemKind =
            crate::node_graph::ports::ItemKind::Anonymous;
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
}
