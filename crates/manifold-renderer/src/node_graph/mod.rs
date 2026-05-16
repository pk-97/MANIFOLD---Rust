//! Effect & generator graph system.
//!
//! See `docs/NODE_GRAPH_SYSTEM.md` for the full architecture overview.
//!
//! This module currently defines only the core abstractions — the [`EffectNode`]
//! trait, port and parameter types, and graph-level identifiers. The graph
//! runtime (topological sort, execution plan, lifetime planner, resource
//! bindings) lands in subsequent steps.

pub mod atomic;
mod backend;
mod bindings;
mod boundary_nodes;
mod chain_spec;
pub mod composites;
mod effect_node;
mod execution;
mod execution_plan;
mod graph;
mod metadata;
mod metal_backend;
mod palette;
mod param_binding;
mod parameters;
mod persistence;
mod ports;
pub mod primitive;
pub mod primitives;
mod snapshot;
mod state_store;
mod validation;

pub use backend::{Backend, MockBackend};
pub use bindings::{NodeInputs, NodeOutputs, Slot};
pub use boundary_nodes::{FINAL_OUTPUT_TYPE_ID, FinalOutput, SOURCE_TYPE_ID, Source};
pub use effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, FrameTime, NodeInstanceId, NodeRequires,
    NodeWire, ParamValues,
};
pub use execution::Executor;
pub use execution_plan::{ExecutionPlan, ExecutionStep, ResourceId, compile};
pub use chain_spec::{
    BindingParityError, ChainSpec, SkipMode, SpecValidationError, SpliceResult, chain_spec_by_id,
    lookup_handle, splice_def_into_chain, validate_all_specs, validate_binding_spec_parity,
};
pub use graph::{Graph, NodeInstance};
pub use metadata::metadata_by_id;
pub use metal_backend::MetalBackend;
pub use palette::{catalog_graph_def_for, palette_atoms, PaletteAtom};
pub use param_binding::{
    BindingCacheEntry, LastAppliedCache, ParamBinding, ParamConvert, ParamId, ParamTarget,
    UserParamBindingRuntime, apply_binding_defaults, apply_param_bindings, binding_value,
    outer_routings_from_bindings, user_binding_to_runtime,
};
pub use parameters::{ParamDef, ParamType, ParamValue};
pub use persistence::{
    EffectGraphDefExt, GRAPH_DOCUMENT_VERSION, GraphDocument, LoadError, NodeConstructor,
    NodeDocument, PrimitiveRegistry, SerializedParamValue, WireDocument, WireSide,
};
pub use ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
pub use primitive::{Primitive, PrimitiveDescription, PrimitiveSpec};
pub use snapshot::{
    GraphSnapshot, NodeSnapshot, OuterParamRouting, ParamSnapshot, ParamSnapshotKind,
    PortKindSnapshot, PortSnapshot, WireSnapshot,
};
pub use state_store::{NodeState, OwnerKey, StateStore};
pub use validation::{GraphError, topological_sort, validate};
