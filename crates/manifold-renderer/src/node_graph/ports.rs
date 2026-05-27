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
    /// CPU-only struct wire carrying a [`Material`](crate::node_graph::material::Material).
    /// Produced by `node.{unlit,phong,pbr,cel}_material` atoms, consumed by
    /// 3D mesh renderers as a single `material: Material` input describing the
    /// shaded surface (kind + base colour + roughness/metallic/etc.).
    /// Same CPU-struct lifetime model as `Camera` / `Light`.
    Material,
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
    /// Named typed channels per sample, in std430 order. Populated for
    /// migrated typed families per `docs/CHANNEL_TYPE_SYSTEM.md` §6;
    /// the default `&[]` keeps pre-migration types compiling unchanged.
    /// When non-empty, [`ArrayType::of_known`] folds this into the
    /// wire's `specs` so the new validator path runs end-to-end on
    /// `Array(T)` syntax (no JSON change needed; existing primitives
    /// declaring `Array(T)` automatically gain Channels-aware
    /// validation as their family migrates).
    const SPECS: &'static [ChannelSpec] = &[];
}

impl KnownItem for u32 {
    const ITEM_KIND: ItemKind = ItemKind::U32Slot;
    // Single-channel `value: U32` — the canonical convention for
    // bare scalar arrays (scatter accumulators, grid indices, ID
    // streams) per `docs/CHANNEL_TYPE_SYSTEM.md` §6.8.
    const SPECS: &'static [ChannelSpec] = &[ChannelSpec {
        name: ChannelName::from_str("value"),
        ty: ChannelElementType::U32,
    }];
}

impl KnownItem for f32 {
    const ITEM_KIND: ItemKind = ItemKind::F32Slot;
    const SPECS: &'static [ChannelSpec] = &[ChannelSpec {
        name: ChannelName::from_str("value"),
        ty: ChannelElementType::F32,
    }];
}

impl KnownItem for [f32; 2] {
    const ITEM_KIND: ItemKind = ItemKind::Vec2Slot;
    // Paired scalars (x, y) at 4-byte alignment, not a single Vec2F —
    // preserves byte parity with the existing `[f32; 2]` layout per
    // the §6.8 / §13(3) resolution.
    const SPECS: &'static [ChannelSpec] = &[
        ChannelSpec { name: ChannelName::from_str("x"), ty: ChannelElementType::F32 },
        ChannelSpec { name: ChannelName::from_str("y"), ty: ChannelElementType::F32 },
    ];
}

/// Layout descriptor for [`PortType::Array`] wires.
///
/// Two `Array` ports can connect iff their type identity matches. The
/// wire's type identity is *transitioning* across this migration:
///
/// - Pre-migration (and still the fallback path for Phase 1-3): the
///   `(item_size, item_align, item_kind)` triple is the identity, with
///   an asymmetric coercion at the `Anonymous` boundary (see
///   `port_types_compatible` in `validation.rs`).
/// - Post-migration (Phase 3+): the `specs` slice is the identity — a
///   list of named typed [`ChannelSpec`]s describing the per-sample
///   channels. See `docs/CHANNEL_TYPE_SYSTEM.md`.
///
/// Phase 1 reshapes this struct to carry BOTH the legacy triple AND
/// the new `specs` + `match_mode` fields. Per-primitive `_SPECS`
/// constants populate `specs` as each typed family migrates in Phase
/// 3; `item_kind` deletes in Phase 4 alongside the cast atoms and the
/// `Anonymous` coercion path.
///
/// The shader on either side owns the per-byte interpretation —
/// canonical struct layouts live in
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
    /// Named typed channels per sample. Empty for legacy (pre-Phase-3)
    /// declarations; populated by `_SPECS` constants as typed families
    /// migrate. When non-empty on BOTH the producer and consumer of a
    /// wire, drives the validator's [`channels_compatible`] check.
    /// When empty on either side, the validator falls back to the
    /// legacy `item_kind` + size + align match (with the Anonymous
    /// coercion). See `docs/CHANNEL_TYPE_SYSTEM.md` §4-§5.
    pub specs: &'static [ChannelSpec],
    /// Wire-validator matching policy for this port's Channels
    /// signature. Default [`MatchMode::Exact`]; [`MatchMode::Permissive`]
    /// is the opt-in for generic transform operators (`rename_channel`,
    /// `reorder_channels`, etc.) whose input port accepts any Channels
    /// signature. Only consulted when `specs` is non-empty.
    pub match_mode: MatchMode,
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
            specs: &[],
            match_mode: MatchMode::Exact,
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
            // Phase 3: pull SPECS from the trait. For typed families
            // that have migrated (Particle, MeshVertex, EdgePair, etc.)
            // this lights up the Channels-aware validator path on every
            // existing `Array(T)` declaration without touching any
            // primitive call site. Pre-migration types use the default
            // `&[]` and continue through the legacy `item_kind` path.
            specs: T::SPECS,
            match_mode: MatchMode::Exact,
        }
    }

    /// Construct a Channels-shaped `ArrayType` from a static slice of
    /// channel specs. `item_size` and `item_align` are derived from the
    /// specs via std430 layout rules; the legacy `item_kind` is set to
    /// [`ItemKind::Anonymous`] so existing `port_types_compatible`
    /// behaviour for typed wires is unaffected during Phase 1-3.
    ///
    /// Phase 3 wires this onto each typed family's `_SPECS` constant.
    /// Phase 4 deletes `item_kind` entirely and this becomes the only
    /// constructor for non-Pod-only ports.
    pub const fn of_channels(
        specs: &'static [ChannelSpec],
        match_mode: MatchMode,
    ) -> Self {
        let (item_size, item_align) = std430_stride_and_align(specs);
        Self {
            item_size,
            item_align,
            item_kind: ItemKind::Anonymous,
            specs,
            match_mode,
        }
    }
}

// ─── Channel type system (per docs/CHANNEL_TYPE_SYSTEM.md §4) ─────────

/// A channel's name, interned at compile time via const FNV-1a-64
/// hashing of the source string. Comparison is u64 equality; the
/// original string is recovered from the `well_known` registry via
/// [`ChannelName::debug_name`] for display and error messages.
///
/// Hashing happens in `const` context so port type declarations stay
/// `const` — the `primitive!` macro can declare `Channels[x: F32, y: F32]`
/// in a `static` port array with no runtime allocation. See
/// `docs/CHANNEL_TYPE_SYSTEM.md` §4.2 for the collision analysis
/// (~2.7e-14 probability across the expected name set; gated by the
/// `well_known_channels!`-emitted collision test).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelName(u64);

impl ChannelName {
    /// Compile-time-evaluable constructor. Produces a stable u64 FNV-1a
    /// hash of the source string; the same string always produces the
    /// same `ChannelName` regardless of where it's declared.
    pub const fn from_str(s: &'static str) -> Self {
        Self(const_fnv1a_64(s.as_bytes()))
    }

    /// Raw u64 hash. Use sparingly — comparison via `==` is preferred.
    /// Exposed so the `well_known_channels!` macro and error formatters
    /// can render `0x{:016x}` representations when a name isn't in the
    /// debug registry.
    pub const fn hash(self) -> u64 {
        self.0
    }

    /// Best-effort lookup of the source string. Returns `Some` for
    /// names declared in the `well_known` registry; returns `None` for
    /// runtime-introduced names (e.g. `wgsl_compute` shader field names
    /// — Phase 4 extends the registry to a runtime hashmap covering
    /// those). Used by `Display` impls and validator error messages.
    pub fn debug_name(self) -> Option<&'static str> {
        crate::node_graph::channel_names::debug_name(self)
    }
}

const fn const_fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 14695981039346656037;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(1099511628211);
        i += 1;
    }
    hash
}

/// The closed set of element types a channel can carry. Closed by
/// design — adding a variant requires updating the std430 layout
/// calculator, the WGSL emitter (future fusion compiler), the macro
/// type-keyword recognizer, and the test matrix. Each new type is a
/// deliberate decision, not a generic-over-T extension. See
/// `docs/CHANNEL_TYPE_SYSTEM.md` §4.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelElementType {
    F32,
    I32,
    U32,
    Vec2F,
    Vec3F,
    Vec4F,
}

impl ChannelElementType {
    /// Std430 size in bytes.
    pub const fn size(self) -> u32 {
        match self {
            Self::F32 | Self::I32 | Self::U32 => 4,
            Self::Vec2F => 8,
            Self::Vec3F => 12,
            Self::Vec4F => 16,
        }
    }

    /// Std430 alignment in bytes. Note: Vec3F has 12-byte payload but
    /// 16-byte alignment — the standard std430 trap that produces the
    /// `_pad0` fields in `Particle` and `MeshVertex`.
    pub const fn alignment(self) -> u32 {
        match self {
            Self::F32 | Self::I32 | Self::U32 => 4,
            Self::Vec2F => 8,
            Self::Vec3F | Self::Vec4F => 16,
        }
    }
}

/// One named typed slot on a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChannelSpec {
    pub name: ChannelName,
    pub ty: ChannelElementType,
}

/// Wire-validator matching policy for a Channels port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatchMode {
    /// Producer and consumer signatures must be identical (same specs
    /// in the same order). Default for every primitive port.
    Exact,
    /// Consumer accepts any Channels signature. Reserved for generic
    /// transform operators (`rename_channel`, `reorder_channels`,
    /// `select_channels`, `channel_math`, etc.). A `pub const`
    /// allow-list in `validation.rs` enumerates which primitive
    /// `TYPE_ID`s may legitimately declare Permissive on a port, and a
    /// test enforces the allow-list — see §11.4.
    Permissive,
}

/// Per-channel byte offsets and total sample stride for a Channels
/// signature, computed per WGSL std430 rules. Pure function of the
/// specs slice. See `docs/CHANNEL_TYPE_SYSTEM.md` §4.4.
///
/// Returns `(per_channel_offsets, sample_stride_bytes, sample_alignment)`.
pub fn std430_layout(specs: &[ChannelSpec]) -> (Vec<u32>, u32, u32) {
    let mut offset: u32 = 0;
    let mut max_align: u32 = 4;
    let mut offsets = Vec::with_capacity(specs.len());
    for spec in specs {
        let align = spec.ty.alignment();
        let size = spec.ty.size();
        offset = round_up_align(offset, align);
        offsets.push(offset);
        offset += size;
        if align > max_align {
            max_align = align;
        }
    }
    let stride = round_up_align(offset, max_align);
    (offsets, stride, max_align)
}

/// Just the per-sample stride. Cheaper than [`std430_layout`] when the
/// offsets aren't needed.
pub fn std430_stride(specs: &[ChannelSpec]) -> u32 {
    std430_stride_and_align(specs).0
}

/// `const fn` variant for use in `ArrayType::of_channels`. Returns
/// `(stride, align)`. Identical math to [`std430_layout`] but allocates
/// nothing — `const fn` can't return a `Vec`.
pub const fn std430_stride_and_align(specs: &[ChannelSpec]) -> (u32, u32) {
    let mut offset: u32 = 0;
    let mut max_align: u32 = 4;
    let mut i = 0;
    while i < specs.len() {
        let align = specs[i].ty.alignment();
        let size = specs[i].ty.size();
        offset = round_up_align(offset, align);
        offset += size;
        if align > max_align {
            max_align = align;
        }
        i += 1;
    }
    let stride = round_up_align(offset, max_align);
    (stride, max_align)
}

const fn round_up_align(value: u32, align: u32) -> u32 {
    (value + align - 1) & !(align - 1)
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

#[cfg(test)]
mod channel_layout_tests {
    use super::*;

    const X: ChannelName = ChannelName::from_str("x");
    const Y: ChannelName = ChannelName::from_str("y");
    const Z: ChannelName = ChannelName::from_str("z");
    const POSITION: ChannelName = ChannelName::from_str("position");
    const VELOCITY: ChannelName = ChannelName::from_str("velocity");
    const COLOR: ChannelName = ChannelName::from_str("color");
    const LIFE: ChannelName = ChannelName::from_str("life");
    const AGE: ChannelName = ChannelName::from_str("age");
    const NORMAL: ChannelName = ChannelName::from_str("normal");
    const UV: ChannelName = ChannelName::from_str("uv");

    #[test]
    fn fnv_const_hash_is_stable_across_calls() {
        let a = ChannelName::from_str("x");
        let b = ChannelName::from_str("x");
        assert_eq!(a, b);
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn fnv_distinguishes_distinct_strings() {
        let x = ChannelName::from_str("x");
        let y = ChannelName::from_str("y");
        assert_ne!(x, y);
    }

    #[test]
    fn element_type_sizes_and_alignments() {
        assert_eq!(ChannelElementType::F32.size(), 4);
        assert_eq!(ChannelElementType::F32.alignment(), 4);
        assert_eq!(ChannelElementType::I32.size(), 4);
        assert_eq!(ChannelElementType::U32.size(), 4);
        assert_eq!(ChannelElementType::Vec2F.size(), 8);
        assert_eq!(ChannelElementType::Vec2F.alignment(), 8);
        assert_eq!(ChannelElementType::Vec3F.size(), 12);
        assert_eq!(ChannelElementType::Vec3F.alignment(), 16, "std430 vec3 trap");
        assert_eq!(ChannelElementType::Vec4F.size(), 16);
        assert_eq!(ChannelElementType::Vec4F.alignment(), 16);
    }

    #[test]
    fn std430_layout_curve_point_two_scalars() {
        // CurvePoint equivalent: [x: F32, y: F32]. 4-byte aligned per
        // the §6.3 / §13(3) resolution (paired scalars, not Vec2F).
        const SPECS: &[ChannelSpec] = &[
            ChannelSpec { name: X, ty: ChannelElementType::F32 },
            ChannelSpec { name: Y, ty: ChannelElementType::F32 },
        ];
        let (offsets, stride, align) = std430_layout(SPECS);
        assert_eq!(offsets, vec![0, 4]);
        assert_eq!(stride, 8);
        assert_eq!(align, 4);
    }

    #[test]
    fn std430_layout_edge_pair_two_u32() {
        const A: ChannelName = ChannelName::from_str("a_index");
        const B: ChannelName = ChannelName::from_str("b_index");
        const SPECS: &[ChannelSpec] = &[
            ChannelSpec { name: A, ty: ChannelElementType::U32 },
            ChannelSpec { name: B, ty: ChannelElementType::U32 },
        ];
        let (offsets, stride, align) = std430_layout(SPECS);
        assert_eq!(offsets, vec![0, 4]);
        assert_eq!(stride, 8);
        assert_eq!(align, 4);
    }

    #[test]
    fn std430_layout_mesh_vertex_pos_normal_uv() {
        // MeshVertex equivalent — Vec3F position + Vec3F normal + Vec2F uv.
        // Should match the existing struct's 48-byte size (16 + 16 + 16 with
        // tail-pad to 16-align).
        const SPECS: &[ChannelSpec] = &[
            ChannelSpec { name: POSITION, ty: ChannelElementType::Vec3F },
            ChannelSpec { name: NORMAL, ty: ChannelElementType::Vec3F },
            ChannelSpec { name: UV, ty: ChannelElementType::Vec2F },
        ];
        let (offsets, stride, align) = std430_layout(SPECS);
        assert_eq!(
            offsets,
            vec![0, 16, 32],
            "position at 0, normal at 16 (vec3→vec3 align gap), uv at 32 (vec3→vec2 align gap)"
        );
        assert_eq!(stride, 48, "tail-pad to max-align 16");
        assert_eq!(align, 16);
    }

    #[test]
    fn std430_layout_particle_64_bytes() {
        // Particle: Vec3F position + Vec3F velocity + F32 life + F32 age + Vec4F color.
        // Must produce 64-byte sample stride matching the existing
        // `#[repr(C)] struct Particle`.
        const SPECS: &[ChannelSpec] = &[
            ChannelSpec { name: POSITION, ty: ChannelElementType::Vec3F },
            ChannelSpec { name: VELOCITY, ty: ChannelElementType::Vec3F },
            ChannelSpec { name: LIFE, ty: ChannelElementType::F32 },
            ChannelSpec { name: AGE, ty: ChannelElementType::F32 },
            ChannelSpec { name: COLOR, ty: ChannelElementType::Vec4F },
        ];
        let (offsets, stride, align) = std430_layout(SPECS);
        assert_eq!(
            offsets,
            vec![0, 16, 28, 32, 48],
            "position 0, velocity 16, life 28, age 32, color 48"
        );
        assert_eq!(stride, 64);
        assert_eq!(align, 16);
    }

    #[test]
    fn std430_stride_matches_stride_and_align() {
        const SPECS: &[ChannelSpec] = &[
            ChannelSpec { name: POSITION, ty: ChannelElementType::Vec3F },
            ChannelSpec { name: COLOR, ty: ChannelElementType::Vec4F },
        ];
        let (_, full_stride, full_align) = std430_layout(SPECS);
        let (cs_stride, cs_align) = std430_stride_and_align(SPECS);
        let cheap = std430_stride(SPECS);
        assert_eq!(full_stride, cs_stride);
        assert_eq!(full_stride, cheap);
        assert_eq!(full_align, cs_align);
    }

    #[test]
    fn array_type_of_channels_derives_size_and_align() {
        const SPECS: &[ChannelSpec] = &[
            ChannelSpec { name: X, ty: ChannelElementType::Vec3F },
            ChannelSpec { name: Y, ty: ChannelElementType::F32 },
            ChannelSpec { name: Z, ty: ChannelElementType::F32 },
        ];
        let array_type = ArrayType::of_channels(SPECS, MatchMode::Exact);
        let (_, expected_stride, expected_align) = std430_layout(SPECS);
        assert_eq!(array_type.item_size, expected_stride);
        assert_eq!(array_type.item_align, expected_align);
        assert_eq!(array_type.specs, SPECS);
        assert_eq!(array_type.match_mode, MatchMode::Exact);
        assert_eq!(
            array_type.item_kind,
            ItemKind::Anonymous,
            "Phase 1-3: Channels-shaped ArrayTypes carry Anonymous kind to stay neutral with the existing validator"
        );
    }
}
