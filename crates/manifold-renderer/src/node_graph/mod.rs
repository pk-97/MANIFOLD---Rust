//! Effect & generator graph system.
//!
//! See `docs/NODE_GRAPH_SYSTEM.md` for the full architecture overview.
//!
//! This module currently defines only the core abstractions — the [`EffectNode`]
//! trait, port and parameter types, and graph-level identifiers. The graph
//! runtime (topological sort, execution plan, lifetime planner, resource
//! bindings) lands in subsequent steps.

mod effect_node;
mod parameters;
mod ports;

pub use effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, FrameTime, NodeInstanceId, NodeWire,
    ParamValues,
};
pub use parameters::{ParamDef, ParamType, ParamValue};
pub use ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
