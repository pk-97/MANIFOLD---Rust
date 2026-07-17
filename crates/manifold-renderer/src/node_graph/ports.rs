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
/// the wire's Channels signature (per `docs/CHANNEL_TYPE_SYSTEM.md`); the
/// shader owns the per-byte interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortType {
    /// Untyped Texture2D — the back-compat default. The four RGBA slots
    /// carry whatever the producer packs into them; consumers reading
    /// the texture rely on prose `composition_notes` to know the layout.
    /// Connects to any other [`Texture2D`](PortType::Texture2D) or
    /// [`Texture2DTyped`](PortType::Texture2DTyped) endpoint (the
    /// migration valve — see `docs/CHANNEL_TYPE_SYSTEM.md` §17).
    Texture2D,
    /// Texture2D decorated with a four-slot named-channel signature
    /// (one [`ChannelName`] per RGBA slot). The validator enforces
    /// exact-match between two typed endpoints and surfaces a per-slot
    /// diff on mismatch; an untyped consumer / producer on the other
    /// side connects through the back-compat valve. See
    /// `docs/CHANNEL_TYPE_SYSTEM.md` §17 for the texture-channel
    /// extension to the Channel type system.
    Texture2DTyped(TextureChannels),
    Texture3D,
    Scalar(ScalarType),
    Array(ArrayType),
    /// CPU-only struct wire carrying a [`Camera`](crate::node_graph::camera::Camera).
    /// Produced by `node.orbit_camera` (and future camera-source primitives),
    /// consumed by every 3D rendering primitive as a single `camera: Camera`
    /// input instead of N separate scalar params.
    Camera,
    /// CPU-only struct wire carrying a [`Light`](crate::node_graph::light::Light).
    /// Produced by `node.light` (sun + point modes), consumed optionally by 3D
    /// rendering primitives (which generate shadow maps internally when wired)
    /// and by shading atoms (`lambert_directional`, `blinn_specular`,
    /// etc., which use the light's direction/colour/attenuation instead of
    /// their scattered scalar params). Same lifetime model as `Camera`.
    Light,
    /// CPU-only struct wire carrying a [`Material`](crate::node_graph::material::Material).
    /// Produced by `node.{unlit,phong,pbr,cel}_material` atoms, consumed by
    /// 3D mesh renderers as a single `material: Material` input describing the
    /// shaded surface (kind + base colour + roughness/metallic/etc.).
    /// Same CPU-struct lifetime model as `Camera` / `Light`.
    Material,
    /// CPU-only struct wire carrying a [`Transform`](crate::node_graph::transform::Transform)
    /// (local TRS: `pos` / `rot_euler` radians / `scale`). Produced by
    /// `node.transform_3d`, consumed by `render_scene`'s `transform_n` ports
    /// as a single struct input replacing nine per-object TRS params. Same
    /// CPU-struct lifetime model as `Camera` / `Light` / `Material` — no GPU
    /// resource on the wire, matrices are composed by consumers, never
    /// carried on the wire itself.
    Transform,
    /// CPU-only struct wire carrying an
    /// [`Atmosphere`](crate::node_graph::atmosphere::Atmosphere) — scene-wide
    /// exponential depth fog + ambient/sky tint. Produced by `node.atmosphere`,
    /// consumed by `render_scene`'s optional `atmosphere` input (REALTIME_3D §5
    /// P3). Same CPU-struct lifetime model as `Camera` / `Light` / `Material` /
    /// `Transform`; unwired = fog off = byte-identical to no atmosphere.
    Atmosphere,
    /// CPU-only struct wire carrying a
    /// [`SceneObject`](crate::node_graph::scene_object::SceneObject) — the
    /// bundle of transform + material + mesh/map/instance [`Slot`](crate::node_graph::bindings::Slot)s
    /// that together make up one scene object (SCENE_OBJECT_AND_PANEL_V2_DESIGN
    /// D1–D3). Produced exclusively by `node.scene_object`; consumed
    /// exclusively by `render_scene`'s `object_k` ports.
    ///
    /// **Single-hop invariant: `Object` wires never chain.** `node.scene_object`
    /// is the sole producer and declares no `Object` input; every legal
    /// consumer is a renderer boundary node (`render_scene` today — extend
    /// this doc comment if a second renderer ever consumes `Object`).
    /// Enforced by the `object_port_single_hop` registry-walk test.
    Object,
}

impl PortType {
    /// True for [`Texture2D`](PortType::Texture2D) and
    /// [`Texture2DTyped`](PortType::Texture2DTyped). Used by the
    /// execution planner and pool keyer to treat both texture variants
    /// uniformly — the channel signature is a validator concern, not
    /// a runtime / GPU one.
    pub fn is_texture_2d(self) -> bool {
        matches!(self, PortType::Texture2D | PortType::Texture2DTyped(_))
    }
}

/// Four-slot RGBA channel signature for a [`PortType::Texture2DTyped`]
/// port. Each slot names what the producer packs into that texel
/// component; consumers wired to a typed producer declare the same
/// signature and the validator enforces exact match per slot.
///
/// The element type of each slot is implicit in the texture's format
/// (`Rgba16Float` → four F32 slots, `R8Unorm` → one F32 slot with the
/// rest ignored, etc.) so [`TextureChannels`] carries only names. The
/// names interned through the same FNV-1a-64 const-hash mechanism as
/// [`ChannelSpec::name`] and resolved against the shared `well_known`
/// channel-name registry. See `docs/CHANNEL_TYPE_SYSTEM.md` §17.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureChannels {
    /// Channel names in R, G, B, A order.
    pub slots: [ChannelName; 4],
}

impl TextureChannels {
    /// Construct a four-slot signature from explicit per-slot names.
    /// Use `well_known::*` constants for canonical names; inline
    /// `ChannelName::from_str("custom")` is the deliberate exception
    /// for genuinely local meanings.
    pub const fn new(r: ChannelName, g: ChannelName, b: ChannelName, a: ChannelName) -> Self {
        Self {
            slots: [r, g, b, a],
        }
    }
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

// `pub enum ItemKind` deleted in Phase 4b. The wire's semantic
// identity is now carried entirely by the [`ArrayType::specs`] slice
// (a list of named typed [`ChannelSpec`]s); the legacy
// `(item_size, item_align, item_kind)` triple collapses to just the
// (size, align) pair plus the specs. See
// `docs/CHANNEL_TYPE_SYSTEM.md` §5.

/// Compile-time descriptor for an item type that flows through an
/// [`ArrayType`] wire. Every canonical item struct in the primitive
/// library implements this; the `Array(T)` syntax in the
/// [`primitive!`](crate::primitive) macro requires it.
///
/// Implementations supply a [`SPECS`](KnownItem::SPECS) slice naming
/// the per-sample channels; [`ArrayType::of_known`] folds it into the
/// wire type so every `Array(T)` declaration becomes Channels-typed
/// without any per-primitive macro change. Adding a new conventional
/// item type means writing the `#[repr(C)]` struct + a `KnownItem`
/// impl that points `SPECS` at a `const _SPECS: &[ChannelSpec]`
/// describing the same byte layout.
pub trait KnownItem: bytemuck::Pod {
    /// Named typed channels per sample, in std430 order. See
    /// `docs/CHANNEL_TYPE_SYSTEM.md` §6 for the per-typed-family
    /// signatures.
    const SPECS: &'static [ChannelSpec] = &[];
}

impl KnownItem for u32 {
    // Single-channel `value: U32` — the canonical convention for
    // bare scalar arrays (scatter accumulators, grid indices, ID
    // streams) per `docs/CHANNEL_TYPE_SYSTEM.md` §6.8.
    const SPECS: &'static [ChannelSpec] = &[ChannelSpec {
        name: ChannelName::from_str("value"),
        ty: ChannelElementType::U32,
    }];
}

impl KnownItem for f32 {
    const SPECS: &'static [ChannelSpec] = &[ChannelSpec {
        name: ChannelName::from_str("value"),
        ty: ChannelElementType::F32,
    }];
}

impl KnownItem for [f32; 2] {
    // Paired scalars (x, y) at 4-byte alignment, not a single Vec2F —
    // preserves byte parity with the existing `[f32; 2]` layout per
    // the §6.8 / §13(3) resolution.
    const SPECS: &'static [ChannelSpec] = &[
        ChannelSpec { name: ChannelName::from_str("x"), ty: ChannelElementType::F32 },
        ChannelSpec { name: ChannelName::from_str("y"), ty: ChannelElementType::F32 },
    ];
}

impl KnownItem for [f32; 3] {
    // Triple scalars (x, y, z) at 4-byte alignment — three separate
    // F32 channels, NOT a single Vec3F (whose std430 stride would pad
    // to 16). Stride is 12. The 3D analog of `[f32; 2]`: the per-
    // particle force buffer the FluidSim3D integrator chain accumulates
    // into. The matching WGSL storage element is a packed
    // `struct { x: f32, y: f32, z: f32 }` (stride 12), read/written via
    // `.x/.y/.z` — NOT `vec3<f32>` (stride 16).
    const SPECS: &'static [ChannelSpec] = &[
        ChannelSpec { name: ChannelName::from_str("x"), ty: ChannelElementType::F32 },
        ChannelSpec { name: ChannelName::from_str("y"), ty: ChannelElementType::F32 },
        ChannelSpec { name: ChannelName::from_str("z"), ty: ChannelElementType::F32 },
    ];
}

/// Layout descriptor for [`PortType::Array`] wires.
///
/// Two `Array` ports can connect iff their type identity matches. The
/// identity is the [`specs`](ArrayType::specs) slice (a list of named
/// typed [`ChannelSpec`]s describing the per-sample channels) plus
/// the validator policy carried in [`match_mode`](ArrayType::match_mode).
/// `item_size` and `item_align` are derived from `specs` via std430
/// layout rules — they stay on the struct for fast lookup at the GPU
/// bind site but are not part of the type identity beyond what specs
/// already encodes.
///
/// See `docs/CHANNEL_TYPE_SYSTEM.md` for the type-system contract.
///
/// The shader on either side owns the per-byte interpretation —
/// canonical struct layouts live in
/// [`crate::generators::compute_common`](../generators/compute_common/index.html)
/// (`Particle`) and [`crate::generators::mesh_common`](../generators/mesh_common/index.html)
/// (`CurvePoint`, `MeshVertex`, `EdgePair`, …) with `#[repr(C)]` and
/// `bytemuck::Pod` and a [`KnownItem`] impl. The `primitive!` macro
/// provides `Array<Particle>` syntactic sugar that expands to
/// `ArrayType::of_known::<Particle>()`; inline `Channels[name: Type, …]`
/// syntax is the equivalent for ad-hoc signatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArrayType {
    pub item_size: u32,
    pub item_align: u32,
    /// Named typed channels per sample, in std430 order. Drives the
    /// validator's [`channels_compatible`] check. See
    /// `docs/CHANNEL_TYPE_SYSTEM.md` §4-§5.
    pub specs: &'static [ChannelSpec],
    /// Wire-validator matching policy. Default [`MatchMode::Exact`];
    /// [`MatchMode::Permissive`] is the opt-in for generic transform
    /// operators (`rename_channel`, `reorder_channels`, etc.) whose
    /// input port accepts any Channels signature.
    pub match_mode: MatchMode,
}

impl ArrayType {
    /// Construct an `ArrayType` for a `T: Pod` with no declared
    /// Channels signature — the wire carries raw bytes of `T`'s size
    /// and alignment. Use [`ArrayType::of_known`] instead whenever
    /// the item has a `KnownItem` impl with a `SPECS` constant; the
    /// `primitive!` macro does this automatically for every
    /// `Array(T)` declaration.
    ///
    /// Surviving callers (post-cast-atom-deletion): the few WGSL-
    /// escape-hatch primitives whose output is genuinely untyped
    /// (raw atomic scratch slots, etc.). The validator's raw-byte
    /// fallback in `port_types_compatible` accepts these against
    /// other empty-specs Array wires of matching size+align.
    pub const fn of<T: bytemuck::Pod>() -> Self {
        Self {
            item_size: std::mem::size_of::<T>() as u32,
            item_align: std::mem::align_of::<T>() as u32,
            specs: &[],
            match_mode: MatchMode::Exact,
        }
    }

    /// Construct an `ArrayType` from a struct's compile-time layout,
    /// folding the struct's `KnownItem::SPECS` into the wire's
    /// Channels signature. The `primitive!` macro emits this for
    /// every `Array(T)` port declaration; downstream wires validate
    /// through the Channels-aware path in `validation::channels_compatible`.
    pub const fn of_known<T: KnownItem>() -> Self {
        Self {
            item_size: std::mem::size_of::<T>() as u32,
            item_align: std::mem::align_of::<T>() as u32,
            specs: T::SPECS,
            match_mode: MatchMode::Exact,
        }
    }

    /// Construct a Channels-shaped `ArrayType` from a static slice of
    /// channel specs. `item_size` and `item_align` are derived from
    /// the specs via std430 layout rules. The canonical constructor
    /// for ad-hoc signatures declared via inline `Channels[name: Type, …]`
    /// macro syntax (per `docs/CHANNEL_TYPE_SYSTEM.md` §12.1).
    pub const fn of_channels(
        specs: &'static [ChannelSpec],
        match_mode: MatchMode,
    ) -> Self {
        let (item_size, item_align) = std430_stride_and_align(specs);
        Self {
            item_size,
            item_align,
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
#[derive(Debug, Clone)]
pub struct NodePort {
    /// Stable port name. Treated as public API once the node ships — the save
    /// format references ports by name when describing wires, so renames
    /// invalidate saved graphs.
    ///
    /// `Cow<'static, str>` so static primitives keep borrowing a `&'static str`
    /// (zero cost, const-constructible in the `primitive!` macro's port
    /// tables) while variadic nodes (`render_scene`, the muxes) can format an
    /// owned name per dynamic port — no arbitrary port-count cap. This is why
    /// `NodePort` is no longer `Copy`.
    pub name: std::borrow::Cow<'static, str>,

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
    fn texture_channels_new_orders_slots_rgba() {
        const R: ChannelName = ChannelName::from_str("r");
        const G: ChannelName = ChannelName::from_str("g");
        const B: ChannelName = ChannelName::from_str("b");
        const A: ChannelName = ChannelName::from_str("a");
        let tc = TextureChannels::new(R, G, B, A);
        assert_eq!(tc.slots, [R, G, B, A]);
    }

    #[test]
    fn texture_channels_equal_when_slots_match() {
        let a = TextureChannels::new(
            ChannelName::from_str("flow_x"),
            ChannelName::from_str("confidence"),
            ChannelName::from_str("flow_y"),
            ChannelName::from_str("valid"),
        );
        let b = TextureChannels::new(
            ChannelName::from_str("flow_x"),
            ChannelName::from_str("confidence"),
            ChannelName::from_str("flow_y"),
            ChannelName::from_str("valid"),
        );
        assert_eq!(a, b);
    }

    #[test]
    fn texture_channels_differ_when_any_slot_differs() {
        // MiDaS convention vs Watercolor convention — the exact bug
        // that motivated the texture channel extension.
        let watercolor = TextureChannels::new(
            ChannelName::from_str("flow_x"),
            ChannelName::from_str("confidence"),
            ChannelName::from_str("flow_y"),
            ChannelName::from_str("valid"),
        );
        let midas = TextureChannels::new(
            ChannelName::from_str("flow_x"),
            ChannelName::from_str("flow_y"),
            ChannelName::from_str("confidence"),
            ChannelName::from_str("valid"),
        );
        assert_ne!(watercolor, midas);
    }

    #[test]
    fn port_type_is_texture_2d_covers_both_variants() {
        assert!(PortType::Texture2D.is_texture_2d());
        let typed = PortType::Texture2DTyped(TextureChannels::new(
            ChannelName::from_str("r"),
            ChannelName::from_str("g"),
            ChannelName::from_str("b"),
            ChannelName::from_str("a"),
        ));
        assert!(typed.is_texture_2d());
        assert!(!PortType::Texture3D.is_texture_2d());
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
    }
}

/// SCENE_OBJECT_AND_PANEL_V2_DESIGN.md's single-hop invariant, enforced
/// registry-wide: `Object` wires never chain — `node.scene_object` is the
/// sole producer (takes no `Object` input); every legal consumer is a
/// renderer boundary node (`render_scene` from P2 on — extend
/// `ALLOWED_OBJECT_CONSUMERS` the day a second one ships).
#[cfg(test)]
mod object_port_single_hop_tests {
    use crate::node_graph::persistence::PrimitiveFactory;
    use crate::node_graph::ports::PortType;

    /// `type_id`s allowed to declare an `Object`-typed INPUT port. Extending
    /// this list is itself the design's named escalation trigger ("any need
    /// for a second Object consumer/producer" — §8) — don't add to it
    /// without re-reading the design doc.
    const ALLOWED_OBJECT_CONSUMERS: &[&str] = &["node.render_scene"];

    #[test]
    fn object_port_single_hop() {
        for factory in inventory::iter::<PrimitiveFactory> {
            let node = (factory.create)();
            let has_object_output = node.outputs().iter().any(|o| o.ty == PortType::Object);
            let has_object_input = node.inputs().iter().any(|i| i.ty == PortType::Object);

            assert!(
                !has_object_output || factory.type_id == "node.scene_object",
                "{} declares an Object output but is not node.scene_object — \
                 node.scene_object must be the sole Object producer",
                factory.type_id,
            );
            assert!(
                !has_object_input || ALLOWED_OBJECT_CONSUMERS.contains(&factory.type_id),
                "{} declares an Object input but isn't in ALLOWED_OBJECT_CONSUMERS \
                 — a second Object consumer is a design escalation, not a \
                 mechanical addition (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md §8)",
                factory.type_id,
            );
        }
    }

    #[test]
    fn scene_object_itself_takes_no_object_input() {
        // Restates the invariant at the single known producer, so a
        // regression here fails loudly and specifically instead of only
        // showing up as a generic registry-walk failure.
        let node = crate::node_graph::primitives::SceneObjectNode::new();
        assert!(
            !crate::node_graph::EffectNode::inputs(&node)
                .iter()
                .any(|i| i.ty == PortType::Object),
            "node.scene_object must not take an Object input — Object wires never chain"
        );
    }
}
