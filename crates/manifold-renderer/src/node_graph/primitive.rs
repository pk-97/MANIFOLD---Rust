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
        inputs: { $( $in_name:ident : $in_ty:ident $($in_req:tt)? ),* $(,)? },
        outputs: { $( $out_name:ident : $out_ty:ident ),* $(,)? },
        params: [ $($params:tt)* ] $(,)?
        $( composition_notes: $notes:literal, )?
        $( examples: [ $($ex:literal),* $(,)? ], )?
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
                        ty: $crate::node_graph::ports::PortType::$in_ty,
                        kind: $crate::node_graph::ports::PortKind::Input,
                        required: $crate::__primitive_required!($($in_req)?),
                    },
                )*
            ];

            const OUTPUTS: &'static [$crate::node_graph::ports::NodeOutput] = &[
                $(
                    $crate::node_graph::ports::NodePort {
                        name: stringify!($out_name),
                        ty: $crate::node_graph::ports::PortType::$out_ty,
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
}
