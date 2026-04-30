//! Port type system for the effect graph.
//!
//! A [`NodePort`] is one labelled connection point on an
//! [`EffectNode`](crate::node_graph::EffectNode). Whether it consumes data
//! ([`NodeInput`]) or produces it ([`NodeOutput`]) is determined by [`PortKind`].
//! The aliases [`NodeInput`] and [`NodeOutput`] document intent at the call site
//! without changing the underlying type.

/// What kind of data flows through a port.
///
/// The V1 set is intentionally small. `Buffer` ports (particle positions, mesh
/// data, audio waveforms) defer to V2 — adding a port type later requires every
/// existing node to potentially understand it, so the V1 set stays tight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortType {
    Texture2D,
    Texture3D,
    Scalar(ScalarType),
}

/// Sub-types for [`PortType::Scalar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalarType {
    F32,
    Vec2,
    Vec3,
    Vec4,
    Color,
}

/// Whether a port consumes data or produces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortKind {
    Input,
    Output,
}

/// One labelled connection point on an [`EffectNode`](crate::node_graph::EffectNode).
///
/// [`NodeInput`] and [`NodeOutput`] are type aliases that read more clearly at
/// the call site. The underlying struct is the same.
#[derive(Debug, Clone, Copy)]
pub struct NodePort {
    /// Stable port name. Treated as public API once the node ships — the save
    /// format references ports by name when describing wires, so renames
    /// invalidate saved graphs.
    pub name: &'static str,

    pub ty: PortType,
    pub kind: PortKind,

    /// Only meaningful for inputs. Outputs ignore this field.
    /// An input with `required = false` may be left unwired.
    pub required: bool,
}

/// A [`NodePort`] with `kind = PortKind::Input`. Type alias for clarity.
pub type NodeInput = NodePort;

/// A [`NodePort`] with `kind = PortKind::Output`. Type alias for clarity.
pub type NodeOutput = NodePort;
