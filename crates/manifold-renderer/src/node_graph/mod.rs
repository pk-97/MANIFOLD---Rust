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
mod bundled_presets;
mod chain_spec;
pub mod composites;
mod effect_node;
mod execution;
mod execution_plan;
mod graph;
mod loaded_preset_view;
mod metal_backend;
mod palette;
mod param_binding;
mod parameters;
mod persistence;
pub mod ports;
pub mod primitive;
pub mod primitives;
mod snapshot;
mod state_store;
mod validation;

pub use backend::{Backend, MockBackend};
pub use bindings::{NodeInputs, NodeOutputs, Slot};
pub use boundary_nodes::{
    FINAL_OUTPUT_TYPE_ID, FinalOutput, GENERATOR_INPUT_TYPE_ID, GeneratorInput, SOURCE_TYPE_ID,
    Source,
};
pub use bundled_presets::{
    bundled_preset_def, bundled_preset_json, bundled_preset_type_ids,
};
pub use effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, FrameTime, NodeInstanceId, NodeRequires,
    NodeWire, ParamValues,
};
pub use execution::Executor;
pub use execution_plan::{ExecutionPlan, ExecutionStep, ResourceId, compile};
pub use chain_spec::{SkipMode, SpliceResult, is_skipped_for, splice_def_into_chain};
pub use graph::{Graph, NodeInstance};
pub use loaded_preset_view::{
    LoadedPresetView, loaded_preset_view_by_id, outer_routings_from_view, snapshot_for_view,
};
pub use metal_backend::MetalBackend;
pub use palette::{catalog_graph_def_for, palette_atoms, PaletteAtom};
pub use param_binding::{
    BindingCacheEntry, BindingSource, LastAppliedCache, ParamBinding, ParamConvert, ParamId,
    ParamTarget, ResolvedBinding, ResolvedTarget, apply_binding_defaults, apply_bindings,
    binding_value, outer_routings_from_bindings,
};
pub use parameters::{ParamDef, ParamType, ParamValue};
pub use persistence::{
    EffectGraphDefExt, GRAPH_DOCUMENT_VERSION, GraphDocument, LoadError, NodeConstructor,
    NodeDocument, PrimitiveRegistry, SerializedParamValue, WireDocument, WireSide,
};
pub use ports::{
    ArrayType, ItemKind, KnownItem, NodeInput, NodeOutput, NodePort, PortKind, PortType,
    ScalarType,
};
pub use primitive::{Primitive, PrimitiveDescription, PrimitiveSpec};
pub use snapshot::{
    GraphSnapshot, NodeSnapshot, OuterParamRouting, OuterParamSource, ParamSnapshot,
    ParamSnapshotKind, PortKindSnapshot, PortSnapshot, WireSnapshot,
};
pub use state_store::{NodeState, OwnerKey, StateStore};
pub use validation::{GraphError, topological_sort, validate};
