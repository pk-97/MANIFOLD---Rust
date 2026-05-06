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
pub mod composites;
mod effect_node;
mod execution;
mod execution_plan;
mod graph;
mod legacy_adapter;
mod metal_backend;
mod parameters;
mod ports;
pub mod primitives;
mod snapshot;
mod state_store;
mod validation;

pub use backend::{Backend, MockBackend};
pub use bindings::{NodeInputs, NodeOutputs, Slot};
pub use boundary_nodes::{FinalOutput, Source, FINAL_OUTPUT_TYPE_ID, SOURCE_TYPE_ID};
pub use effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, FrameTime, NodeInstanceId, NodeWire,
    ParamValues,
};
pub use execution::Executor;
pub use metal_backend::MetalBackend;
pub use execution_plan::{compile, ExecutionPlan, ExecutionStep, ResourceId};
pub use graph::{Graph, NodeInstance};
pub use legacy_adapter::{metadata_by_id, LegacyPostProcessNode, LEGACY_TYPE_ID_PREFIX};
pub use parameters::{ParamDef, ParamType, ParamValue};
pub use ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
pub use snapshot::{GraphSnapshot, NodeSnapshot, PortKindSnapshot, PortSnapshot, WireSnapshot};
pub use state_store::{NodeState, OwnerKey, StateStore};
pub use validation::{topological_sort, validate, GraphError};
