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
/// `(item_size, item_align, item_kind)`; the shader owns the per-byte
/// interpretation. See `docs/BUFFER_PORT_PLAN.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortType {
    Texture2D,
    Texture3D,
    Scalar(ScalarType),
    Array(ArrayType),
    /// CPU-only struct wire carrying a [`Camera`](crate::node_graph::camera::Camera).
    /// Produced by `node.camera_orbit` (and future camera-source primitives),
    /// consumed by every 3D rendering primitive as a single `camera: Camera`
    /// input instead of N separate scalar params.
    Camera,
    /// CPU-only struct wire carrying a [`Light`](crate::node_graph::light::Light).
    /// Produced by `node.light` (sun + point modes), consumed optionally by 3D
    /// rendering primitives (which generate shadow maps internally when wired)
    /// and by shading atoms (`lambert_directional`, `cook_torrance_specular`,
    /// etc., which use the light's direction/colour/attenuation instead of
    /// their scattered scalar params). Same lifetime model as `Camera`.
    Light,
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

/// Semantic tag carried on every [`ArrayType`] wire.
///
/// Wire validation requires producer and consumer to agree on `ItemKind` in
/// addition to `(item_size, item_align)`. Two buffers can be byte-identical but
/// carry different conventions — the canonical case is `CurvePoint` (2D point
/// in origin-centered pre-aspect curve space, the contract `node.render_lines`
/// expects) vs. a hypothetical `ScreenPoint` (2D point already shifted into
/// `[0, 1]` screen space). Both are 8 bytes of `vec2<f32>`; both would have
/// connected silently under the old size/align-only check. With `ItemKind` on
/// the wire they don't.
///
/// New variants ship one at a time as new conventions enter the primitive
/// library. Anonymous is the deliberate opt-out for genuinely untyped raw
/// buffers (escape-hatch nodes, ad-hoc scratch) — it matches only other
/// Anonymous wires of the same size/align. The `every_array_port_declares_a_kind`
/// CI sweep test in `crate::node_graph::primitive` walks the registry and
/// refuses any conventional Array port that lands as Anonymous, so the opt-out
/// stays deliberate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// Raw byte buffer with no declared semantic. Matches only other
    /// Anonymous wires of the same size/align. Use when the buffer is
    /// scratch state local to a single primitive or its escape-hatch
    /// WGSL nodes — anything that flows between curated primitives
    /// should carry a real kind.
    Anonymous,
    /// 2D point in origin-centered pre-aspect curve space. The contract
    /// every line-renderer (`node.render_lines`) consumes; produced by
    /// `pack_curve_xy`, `project_3d`, `project_4d`,
    /// `concentric_outlines`, `polygon_shape`. Origin is the visual
    /// centre; aspect correction + screen offset live in the consumer.
    CurvePoint,
    /// 3D mesh vertex with surface normal. 32 bytes. Produced by
    /// `generate_grid_mesh` / `generate_cube_mesh` / `wireframe_shape`;
    /// consumed by the `node.render_3d_mesh` family and `project_3d`.
    MeshVertex,
    /// 4D vertex in homogeneous hypercube space, before 4D rotation
    /// and projection-to-3D. Produced by `generate_tesseract_vertices`
    /// (closed polytope) or `pack_vec4` (parametric-surface authoring,
    /// see Duocylinder); consumed by `rotate_4d` / `project_4d`.
    Vec4Vertex,
    /// Explicit `(a, b)` edge between two vertices in a sibling
    /// `Array<CurvePoint>` or `Array<MeshVertex>` buffer. The topology
    /// wire that lets line-renderers draw arbitrary wireframes.
    EdgePair,
    /// Per-instance transform for instanced mesh rendering. 32 bytes.
    /// Produced by `generate_instance_transforms`; consumed by
    /// `render_instanced_3d_mesh`, `digital_plants_render`,
    /// `neighbor_smooth`.
    InstanceTransform,
    /// Particle struct (position + velocity + life + color, 64 bytes)
    /// flowing through every node in the particle-sim chain.
    Particle,
    /// Detected blob (bounding box) emitted by the FFI blob detector
    /// and consumed by overlay-render primitives.
    Blob,
    /// Raw `u32` slot — scatter / accumulator buffers and grid
    /// indices. Distinct from `Anonymous` because the convention
    /// "one u32 per cell" is real and shared across the
    /// `scatter_*` / `resolve_*` family.
    U32Slot,
    /// Raw `f32` slot — variable-length numeric arrays, e.g. per-
    /// instance rotation angles emitted by `cycle_table_row` and
    /// `scalar_array_accumulator`, consumed by primitives that take
    /// a target-pose buffer (e.g. `nested_cubes_geometry`).
    F32Slot,
    /// Raw `vec2<f32>` slot — variable-length 2D vector arrays, e.g.
    /// per-instance UV coordinates emitted by `grid_uv_field`,
    /// consumed by per-instance noise samplers and topology-wrap
    /// primitives. Distinct from `CurvePoint` because the convention
    /// here is "raw 2D data with no declared coordinate space"; if a
    /// specific space (origin-centered curve, screen, world) becomes
    /// load-bearing, promote to a named struct + named `ItemKind`.
    Vec2Slot,
}

/// Compile-time descriptor for an item type that flows through an
/// [`ArrayType`] wire. Every canonical item struct in the primitive
/// library implements this; the `Array(T)` syntax in the
/// [`primitive!`](crate::primitive) macro requires it.
///
/// Two implementations of `KnownItem` with the same `ITEM_KIND` are
/// declared interchangeable; the wire validator checks the kind in
/// addition to size/align. Adding a new conventional item type means
/// (a) adding a variant to [`ItemKind`], (b) implementing this trait
/// for the struct, and (c) consumers / producers picking up the new
/// kind via `Array(NewType)` in their `primitive!` declaration.
pub trait KnownItem: bytemuck::Pod {
    const ITEM_KIND: ItemKind;
}

impl KnownItem for u32 {
    const ITEM_KIND: ItemKind = ItemKind::U32Slot;
}

impl KnownItem for f32 {
    const ITEM_KIND: ItemKind = ItemKind::F32Slot;
}

impl KnownItem for [f32; 2] {
    const ITEM_KIND: ItemKind = ItemKind::Vec2Slot;
}

/// Layout descriptor for [`PortType::Array`] wires.
///
/// Two `Array` ports can connect iff their `(item_size, item_align,
/// item_kind)` triples match. The shader on either side owns the
/// per-byte interpretation — canonical struct layouts live in
/// [`crate::generators::compute_common`](../generators/compute_common/index.html)
/// (`Particle`) and [`crate::generators::mesh_common`](../generators/mesh_common/index.html)
/// (`CurvePoint`, `MeshVertex`, `EdgePair`, …) with `#[repr(C)]` and
/// `bytemuck::Pod` and a [`KnownItem`] impl. The `primitive!` macro
/// provides `Array<Particle>` syntactic sugar that expands to
/// `ArrayType::of_known::<Particle>()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArrayType {
    pub item_size: u32,
    pub item_align: u32,
    pub item_kind: ItemKind,
}

impl ArrayType {
    /// Construct an `ArrayType` from a struct's compile-time layout,
    /// carrying [`ItemKind::Anonymous`]. Use [`ArrayType::of_known`]
    /// instead whenever the item has a declared convention — the
    /// macro does this automatically for every `Array(T)` declaration.
    pub const fn of<T: bytemuck::Pod>() -> Self {
        Self {
            item_size: std::mem::size_of::<T>() as u32,
            item_align: std::mem::align_of::<T>() as u32,
            item_kind: ItemKind::Anonymous,
        }
    }

    /// Construct an `ArrayType` from a struct's compile-time layout,
    /// tagging it with the struct's declared [`ItemKind`]. The
    /// `primitive!` macro emits this for every `Array(T)` port so
    /// authors don't pick the kind manually — picking the wrong kind
    /// would either fail to compile (no `KnownItem` impl) or fail
    /// wire validation downstream.
    pub const fn of_known<T: KnownItem>() -> Self {
        Self {
            item_size: std::mem::size_of::<T>() as u32,
            item_align: std::mem::align_of::<T>() as u32,
            item_kind: T::ITEM_KIND,
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
