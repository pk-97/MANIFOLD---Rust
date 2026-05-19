//! Port type system for the effect graph.
//!
//! A [`NodePort`] is one labelled connection point on an
//! [`EffectNode`](crate::node_graph::EffectNode). Whether it consumes data
//! ([`NodeInput`]) or produces it ([`NodeOutput`]) is determined by [`PortKind`].
//! The aliases [`NodeInput`] and [`NodeOutput`] document intent at the call site
//! without changing the underlying type.

/// What kind of data flows through a port.
///
/// `Array` is the storage-buffer wire type used by particle, mesh, line, and
/// audio primitives — the underlying `MTLBuffer` carries `count` items of a
/// fixed-layout struct, accessed by index. Connection validation matches on
/// `(item_size, item_align)`; the shader owns the per-byte interpretation.
/// See `docs/BUFFER_PORT_PLAN.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortType {
    Texture2D,
    Texture3D,
    Scalar(ScalarType),
    Array(ArrayType),
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

/// Layout descriptor for [`PortType::Array`] wires.
///
/// Two `Array` ports can connect iff their `(item_size, item_align)` pairs
/// match. The shader on either side owns the per-byte interpretation —
/// canonical struct layouts live in
/// [`crate::generators::compute_common`](../generators/compute_common/index.html)
/// (`Particle`, etc.) with `#[repr(C)]` and `bytemuck::Pod`. The `primitive!`
/// macro provides `Array<Particle>` syntactic sugar that expands to
/// `ArrayType { item_size: size_of::<Particle>(), item_align: align_of::<Particle>() }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArrayType {
    pub item_size: u32,
    pub item_align: u32,
}

impl ArrayType {
    /// Construct an `ArrayType` from a struct's compile-time layout.
    pub const fn of<T: bytemuck::Pod>() -> Self {
        Self {
            item_size: std::mem::size_of::<T>() as u32,
            item_align: std::mem::align_of::<T>() as u32,
        }
    }
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
