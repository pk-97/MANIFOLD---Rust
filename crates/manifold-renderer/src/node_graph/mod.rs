//! Effect & generator graph system.
//!
//! See `docs/NODE_GRAPH_SYSTEM.md` for the full architecture overview.
//!
//! This module currently defines only the core abstractions — the [`EffectNode`]
//! trait, port and parameter types, and graph-level identifiers. The graph
//! runtime (topological sort, execution plan, lifetime planner, resource
//! bindings) lands in subsequent steps.

mod bindings;
mod effect_node;
mod execution;
mod execution_plan;
mod graph;
mod parameters;
mod ports;
mod validation;

pub use bindings::{NodeInputs, NodeOutputs, Slot};
pub use effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, FrameTime, NodeInstanceId, NodeWire,
    ParamValues,
};
pub use execution::{Executor, ResourcePool};
pub use execution_plan::{compile, ExecutionPlan, ExecutionStep, ResourceId};
pub use graph::{Graph, NodeInstance};
pub use parameters::{ParamDef, ParamType, ParamValue};
pub use ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
pub use validation::{topological_sort, validate, GraphError};
