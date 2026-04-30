//! Parameter types exposed by an [`EffectNode`](crate::node_graph::EffectNode).
//!
//! Parameters are the user-facing knobs on a node: numeric values, booleans,
//! vectors, colours, enums. Each `EffectNode` declares its parameters
//! statically via `EffectNode::parameters()`. Per-instance values live
//! separately in `ParamValues` and are supplied to the node each frame.
//!
//! Parameters are also the public surface of a graph: the per-parameter
//! "expose" flag (V2 graph editor) decides which internal node parameters
//! become slots on the outer effect card. The modulation system targets
//! exposed parameters by name, identical to today's effect parameters.

/// Type of a parameter knob on an [`EffectNode`](crate::node_graph::EffectNode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamType {
    Float,
    Int,
    Bool,
    Vec2,
    Vec3,
    Vec4,
    Color,
    /// Index into the parameter definition's `enum_values` list.
    Enum,
}

/// Runtime value of one parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    Color([f32; 4]),
    Enum(u32),
}

/// Static description of one parameter on an
/// [`EffectNode`](crate::node_graph::EffectNode).
#[derive(Debug, Clone)]
pub struct ParamDef {
    /// Stable parameter name. Wire-key for modulation bindings and the
    /// save format. Treated as public API once shipped — never rename in place.
    pub name: &'static str,

    /// Human-readable label for the effect card UI. Free to change.
    pub label: &'static str,

    pub ty: ParamType,
    pub default: ParamValue,

    /// Numeric range `(min, max)`. Applied to `Float` and `Int` types only.
    pub range: Option<(f32, f32)>,

    /// Discrete options for `ParamType::Enum`. Empty for non-Enum types.
    pub enum_values: &'static [&'static str],
}
