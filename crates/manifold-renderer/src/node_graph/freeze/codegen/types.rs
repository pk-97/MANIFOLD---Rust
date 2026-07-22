use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::freeze::classify::{FusionKind, InputAccess};
use crate::node_graph::parameters::{ParamDef, ParamType};
use crate::node_graph::ports::{ChannelElementType, ChannelSpec, NodeInput, NodeOutput, PortType};

/// Entry-point name every generated kernel uses. Exactly `cs_main`, and no
/// emitted helper/struct may have it as a prefix (the backend's
/// `find_entry_function` tries an exact match first, then prefix-matches —
/// design §12.3 / shader_compiler.rs).
pub const ENTRY: &str = "cs_main";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodegenError {
    /// The atom declares no `wgsl_body` — nothing to wrap.
    NoBody,
    /// The atom's `fusion_kind` is `Boundary` (or otherwise not handled by the
    /// v1 standalone generator).
    NotFusable(FusionKind),
    /// Texture-input arity doesn't match the kind (Pointwise wants 1,
    /// MultiInputCoincident wants ≥2).
    WrongTextureArity { kind: FusionKind, found: usize },
    /// A param type the v1 generator can't lay out as a scalar uniform field
    /// (vec/color/table/string/trigger). Such an atom is simply not fused yet.
    UnsupportedParam { name: &'static str, ty: ParamType },
    /// Two region bodies define a helper with the same name but different
    /// bodies — can't dedup safely. (Doesn't occur in v1; the shared HSV/blend
    /// helpers are byte-identical.)
    HelperCollision(String),
    /// A region node's `InputSource` references an unknown node, a node that
    /// isn't earlier in topo order, or an out-of-range external input.
    BadInput,
    /// An `ATOMIC_OUTPUTS`-marked port resolves to a non-integer element. WGSL
    /// atomics are `u32`/`i32` only, so a multi-channel or float accumulator
    /// can't be emitted as `array<atomic<…>>` — the atom mis-declared its
    /// atomic output.
    AtomicNonInteger { ty: String },
}

/// Map a (scalar) param type to its WGSL type + 4-byte slot. v1 supports the
/// scalar params ColorGrade uses; non-scalar params make the atom un-fusable
/// for now (conservative — the region-grower skips it). `pub(crate)` so the
/// install-side region-grower can pre-screen a candidate atom's params before
/// committing to a fusion.
pub(crate) fn param_wgsl_type(p: &ParamDef) -> Result<&'static str, CodegenError> {
    match p.ty {
        // Angle/Frequency are presentation hints stored as f32 (radians).
        ParamType::Float | ParamType::Angle | ParamType::Frequency => Ok("f32"),
        ParamType::Int => Ok("i32"),
        ParamType::Bool | ParamType::Enum => Ok("u32"),
        other => Err(CodegenError::UnsupportedParam {
            name: crate::node_graph::intern_name(&p.name),
            ty: other,
        }),
    }
}

/// A param name, made safe to use as a WGSL struct-field / identifier. A param
/// can legitimately be named `type` (node.noise), but `type` is a WGSL reserved
/// word, so emitting `struct Params { type: u32 }` / `params.type` fails to
/// compile. Reserved names get a `p_` prefix; everything else passes through
/// unchanged (so existing atoms' generated WGSL — and its pipeline-cache key —
/// is untouched). The fused path namespaces fields as `n{i}_<name>`, which is
/// never reserved, so only the standalone Params struct needs this.
/// How many 4-byte words a param occupies in the merged scalar uniform. Vec3
/// expands to 3 consecutive f32 fields (`<name>_x/_y/_z`); Vec4/Color expand
/// to four (`<name>_x/_y/_z/_w`) — matching how the hand atoms already pack a
/// colour as scalars (e.g. chroma_key's key_r/g/b) — and the body receives it
/// reassembled as a `vec3<f32>`/`vec4<f32>` (2026-07-14, P3 wave 2: the
/// shading-family atoms' `color` params are the first standalone-path users
/// of Vec4/Color; still `param_wgsl_type`-unsupported, so an atom with one of
/// these params stays a `region.rs` cut — the fused MULTI-node path still
/// only carries pure scalar uniform fields).
pub(crate) fn param_word_count(p: &ParamDef) -> Result<usize, CodegenError> {
    match p.ty {
        ParamType::Float | ParamType::Angle | ParamType::Frequency => Ok(1),
        ParamType::Int | ParamType::Bool | ParamType::Enum => Ok(1),
        ParamType::Vec3 => Ok(3),
        ParamType::Vec4 | ParamType::Color => Ok(4),
        other => Err(CodegenError::UnsupportedParam {
            name: crate::node_graph::intern_name(&p.name),
            ty: other,
        }),
    }
}

/// Whether a param can lay out in the fused per-node namespaced uniform AT
/// ALL — scalar (via [`param_wgsl_type`]) OR Vec3/Vec4/Color (P5/D4 lift:
/// Vec3 packs as three consecutive `_x`/`_y`/`_z` f32 fields, Vec4/Color as
/// four `_x`/`_y`/`_z`/`_w`, both already word-aligned — see the struct- and
/// arg-emission blocks in `generate_fused`/`generate_fused_buffer`).
/// Table/String stay unrepresentable (Table needs a fixed-size
/// array-of-vec4 the per-node namespacing doesn't extend to; String has no
/// GPU representation) — `region.rs`'s `classify_node`/`classify_refusal`
/// param gate (cut rule 4) is the only caller; kept `pub(crate)` for the
/// same reason. Distinct from `param_wgsl_type`, which must keep returning
/// `Err` for Vec3/Vec4/Color — its callers (e.g. the `dispatch_count_field`
/// zero-literal) need a single scalar WGSL type name, not a vector.
pub(crate) fn param_is_fusable(p: &ParamDef) -> bool {
    matches!(p.ty, ParamType::Vec3 | ParamType::Vec4 | ParamType::Color) || param_wgsl_type(p).is_ok()
}

pub(crate) fn wgsl_safe_field(name: &str) -> std::borrow::Cow<'_, str> {
    // WGSL keywords a short param name could realistically collide with.
    const RESERVED: &[&str] = &[
        "type", "var", "let", "const", "fn", "struct", "return", "if", "else",
        "for", "while", "loop", "switch", "case", "default", "break", "continue",
        "true", "false", "bool", "i32", "u32", "f32", "f16", "array", "atomic",
        "ptr", "sampler", "texture", "override", "enable", "discard", "vec2",
        "vec3", "vec4", "mat2x2", "mat3x3", "mat4x4",
        // WGSL reserved-for-future word a short param name realistically hits
        // (generate_instance_transforms has a `layout` Enum param). Add others
        // only when an atom actually collides — adding a word here changes the
        // generated WGSL of any atom with a param of that name.
        "layout",
        // `length(v)` is a WGSL builtin function (vector magnitude).
        // node.taper_mesh's committed param name (MESH_DEFORM_AND_CURVE_
        // GEOMETRY_DESIGN.md §3) is literally `length` (the taper falloff
        // span) — collides with the builtin identifier in the generated
        // Params struct field / `params.length` access. Renamed to
        // `p_length` in generated WGSL only; the outward ParamDef.name,
        // JSON param id, and port-shadow port name stay `length`.
        "length",
    ];
    if RESERVED.contains(&name) {
        std::borrow::Cow::Owned(format!("p_{name}"))
    } else {
        std::borrow::Cow::Borrowed(name)
    }
}

pub(super) fn is_texture_input(i: &NodeInput) -> bool {
    matches!(
        i.ty,
        PortType::Texture2D | PortType::Texture2DTyped(_) | PortType::Texture3D
    )
}

pub(super) fn is_texture_port(ty: &PortType) -> bool {
    matches!(
        ty,
        PortType::Texture2D | PortType::Texture2DTyped(_) | PortType::Texture3D
    )
}

/// Texture dimensionality the generated kernel works in. An atom is 3D iff any of
/// its texture ports is a `Texture3D` (volume atoms like blur_3d_separable);
/// otherwise 2D. This selects the texture types, workgroup shape, and the
/// `id`/`uv`/store coordinate forms throughout the wrapper.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TexDim {
    D2,
    D3,
}

/// Threads per axis of every GENERATED 3D-volume kernel — the value behind the
/// `workgroup: "4, 4, 4"` string in [`dim_forms`]. A volume primitive's `run()`
/// MUST size its dispatch grid `div_ceil(VOLUME_WORKGROUP_3D)`; sizing it from a
/// hand shader's historical 8×8×8 grid silently computes only an EIGHTH of the
/// volume (the FluidSim3D "top-right cube" bug, 2026-07-10: edge_slope_3d and
/// swirl_force_3d under-dispatched, so the force field existed in one octant and
/// the whole sim's dynamics parked there). Kernel parity tests can't catch this —
/// they dispatch with the test's own grid — so the constant is shared instead:
/// the kernel emission and every run() read the same number.
pub const VOLUME_WORKGROUP_3D: u32 = 4;

/// Per-dimension WGSL fragments for the iteration wrapper. (Input texture types
/// are emitted per-input, not here, so a 3D input can feed a 2D-output atom.)
pub(super) struct DimForms {
    pub(super) storage_ty: &'static str,
    pub(super) workgroup: &'static str,
    pub(super) guard: &'static str,
    pub(super) uv_expr: &'static str,
    pub(super) dims_arg: &'static str,
    pub(super) store_coord: &'static str,
}

pub(super) fn dim_forms(dim: TexDim) -> DimForms {
    match dim {
        TexDim::D2 => DimForms {
            storage_ty: "texture_storage_2d<rgba16float, write>",
            workgroup: "16, 16",
            guard: "id.x >= dims.x || id.y >= dims.y",
            uv_expr: "(vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims)",
            dims_arg: "vec2<f32>(dims)",
            store_coord: "vec2<i32>(id.xy)",
        },
        TexDim::D3 => DimForms {
            storage_ty: "texture_storage_3d<rgba16float, write>",
            // Must spell out VOLUME_WORKGROUP_3D per axis — a unit test
            // (volume_workgroup_constant_matches_emitted_kernel) pins them together.
            workgroup: "4, 4, 4",
            guard: "id.x >= dims.x || id.y >= dims.y || id.z >= dims.z",
            uv_expr: "(vec3<f32>(id) + 0.5) / vec3<f32>(dims)",
            dims_arg: "vec3<f32>(dims)",
            store_coord: "vec3<i32>(id)",
        },
    }
}


// ===========================================================================
// Buffer-domain standalone codegen: wrap a buffer atom's `wgsl_body` in the
// storage-array iteration boilerplate (element structs synthesized from the
// Channels signature, `var<storage>` bindings, a 1D `@workgroup_size(256)`
// dispatch keyed on an injected element count). The buffer analogue of
// `generate_standalone` — same single-source intent (reproduce the hand kernel
// so it can be deleted), validated per-atom through a buffer-readback oracle.
//
// v1 supports the GATHER body shape (the particle / instance sims' dominant
// form): the body references the input array global(s) `buf_<port>` and computes
// its own indices, returns the output element. This covers neighbor_smooth and
// the random-access particle/instance family. The element-passing COINCIDENT
// shape (`body(elem_in, …) -> elem_out`, fusable) is an additive follow-on for
// the genuinely-pointwise integrators (euler_step) — a second arg-marshalling
// path here, no change to anything below.
// ===========================================================================

/// WGSL scalar / vector type for one channel element type. The std430 layout of
/// a struct built from these reproduces the `#[repr(C)]` element byte layout
/// (the Channels SPECS omit explicit pad fields precisely because WGSL's own
/// std430 alignment re-inserts them — e.g. a `vec3<f32>` field pads to 16).
pub(super) fn channel_wgsl_ty(t: ChannelElementType) -> &'static str {
    match t {
        ChannelElementType::F32 => "f32",
        ChannelElementType::I32 => "i32",
        ChannelElementType::U32 => "u32",
        ChannelElementType::Vec2F => "vec2<f32>",
        ChannelElementType::Vec3F => "vec3<f32>",
        ChannelElementType::Vec4F => "vec4<f32>",
    }
}

/// Resolve an Array port's Channels signature to its WGSL element type name,
/// registering a struct definition when the element has >1 channel.
///
/// - 1 channel (`Array(u32)` / `Array(f32)` — accumulators, scalar streams) →
///   the bare scalar/vector type, no struct (the channel name is the canonical
///   `value`, which carries no WGSL meaning).
/// - ≥2 channels (`Particle`, `InstanceTransform`, `[f32; 2]` force pairs) → a
///   struct named `Element`, `Element2`, … in first-appearance order, deduped by
///   signature so a same-typed in/out pair shares ONE struct (WGSL is nominally
///   typed — `buf_out[i] = body(...)` needs the return type to be the *same*
///   struct the output array holds).
pub(super) fn buffer_element_type(
    specs: &'static [ChannelSpec],
    structs: &mut Vec<(&'static [ChannelSpec], String)>,
) -> String {
    if specs.len() == 1 {
        return channel_wgsl_ty(specs[0].ty).to_string();
    }
    if let Some((_, name)) = structs.iter().find(|(s, _)| *s == specs) {
        return name.clone();
    }
    let name = if structs.is_empty() {
        "Element".to_string()
    } else {
        format!("Element{}", structs.len() + 1)
    };
    structs.push((specs, name.clone()));
    name
}


// ===========================================================================
// Fused multi-atom codegen (build step 3): chain a region of atom bodies into
// ONE kernel. Read the external input(s) once, thread a register through each
// atom's body in topo order (a fork that re-converges in the region is just
// two uses of one register — e.g. ColorGrade's source -> {chain, mix.a}),
// dedup shared helpers, namespace each body, merge params into one uniform,
// write once. Auto-generates the hand-fused colorgrade_fused.wgsl.
// ===========================================================================

/// Where a region node's texture input comes from.
#[derive(Debug, Clone)]
pub enum InputSource {
    /// The region's Nth external input texture (read once into a register).
    External(usize),
    /// Another region node's output register (must appear earlier in topo order).
    Node(NodeInstanceId),
    /// Another region node's output register, but that node is MULTI-OUTPUT (a
    /// struct-return body with ≥2 texture outputs, e.g. voronoi_2d's `out`/
    /// `cell_id` — D4/P6): the register is a `BodyOutputs` struct, not the
    /// value itself, so this names which field to read (`r{j}.<port>`).
    /// Single-output producers keep `Node` (the register IS the value) —
    /// byte-identical for every region that existed before this phase.
    NodeOutput(NodeInstanceId, String),
    /// An OPTIONAL texture input with no wire (pack_channels' unwired b/a). The
    /// body receives a zero vector and its injected `use_<name>` flag is the
    /// literal `0u`, so the value is never read — the same contract the atom's
    /// `run()` fulfils by binding a dummy texture and clearing the flag. Wiring
    /// is static in the def, so the flag folds at codegen time instead of
    /// becoming a uniform field.
    Unwired,
    /// STENCIL tier: a Gather input backed by a VIRTUAL SOURCE — index into
    /// [`FusionRegion::virtual_chains`]. The consumer's `n{i}_fetch_<port>`
    /// recomputes the chain at each tap's bilinear corner texels instead of
    /// sampling a bound texture. Only valid on a stencil member's Gather input.
    Virtual(usize),
}

/// A producer chain recomputed inside a stencil member's fetch (the finder's
/// [`VirtualChain`](super::region::VirtualChain), resolved to region-node
/// indices). The chain members live in [`FusionRegion::nodes`] (so their params
/// join the merged uniform under the same `n{i}_` numbering) but are SKIPPED by
/// `cs_main` — their bodies run per corner texel inside the fetch.
#[derive(Debug, Clone)]
pub struct FusedVirtualChain {
    /// Region-node index of the consuming stencil member.
    pub consumer: usize,
    /// Which of the consumer's `inputs` slots this chain backs (the slot holds
    /// `InputSource::Virtual(chain index)`).
    pub input_index: usize,
    /// Region-node indices of the chain members, topo order (every `Node` input
    /// of a chain member refers to an earlier entry in this list).
    pub members: Vec<usize>,
    /// Region-node index of the chain's OUTPUT member — the value the fetch
    /// returns per corner (q16-rounded unless its storage is fp32, reproducing
    /// the unfused chain's store the consumer sampled).
    pub output: usize,
}

/// One atom inside a fusion region. Borrows its body + params (both available
/// as `&'static` from a type's `PrimitiveSpec` consts, or borrowed from a graph
/// node) for `'a`.
#[derive(Debug, Clone)]
pub struct RegionNode<'a> {
    pub node_id: NodeInstanceId,
    pub fusion_kind: FusionKind,
    pub body: &'a str,
    pub params: &'a [ParamDef],
    /// Texture inputs in body-arg order (Pointwise: 1; MultiInputCoincident: ≥2;
    /// Source: 0). Each pairs with the same index in [`Self::input_access`].
    pub inputs: Vec<InputSource>,
    /// How each input is read, aligned to [`Self::inputs`]. A `Gather` input is
    /// bound as a texture (+ a shared sampler) the body samples itself at a coord
    /// it computes — NOT pre-read into a register. Empty (or short) ⇒ the missing
    /// entries default to `Coincident`, so every all-coincident region keeps
    /// constructing this as `vec![]` and emits byte-identical WGSL.
    pub input_access: Vec<InputAccess>,
    /// The member's full input port defs (texture/array ports in declaration
    /// order, same order as [`Self::inputs`] after filtering to the relevant kind).
    /// Carried so the BUFFER codegen path can read each `Array` input's element
    /// `ChannelSpec`s to synthesize the element struct. The texture path ignores
    /// this (it only needs `inputs` + `input_access`). Empty ⇒ texture-domain
    /// construction stays byte-identical.
    pub node_inputs: &'a [NodeInput],
    /// The member's full output port defs. The buffer codegen reads the output
    /// `Array` element type for the result struct + write. Empty ⇒ texture path.
    pub node_outputs: &'a [NodeOutput],
    /// Shared WGSL library snippets the member's body depends on (noise_common,
    /// …). The buffer codegen prepends the deduped union across the region so
    /// every member's helper calls resolve. Empty for the texture path (texture
    /// atoms inline their helpers).
    pub node_includes: &'a [&'static str],
    /// Frame-derived uniform fields the member's body takes as trailing args
    /// (`dt_scaled`, `frame_count:u32`, a camera's `cam_fwd_x`/`_y`/`_z`, …), in
    /// body-arg order after the params. The standalone path computes these in
    /// run() and packs them; a fused region (buffer OR texture) emits them as
    /// `n{i}_<name>` Params fields + body args (see [`Self::type_id`] /
    /// [`Self::derived_camera_ext`] for how `node.wgsl_compute` recomputes their
    /// VALUES every frame — D7/P0, `docs/CINEMATIC_POST_DESIGN.md`). Empty for
    /// atoms with no frame-derived uniforms.
    pub derived_uniforms: &'static [&'static str],
    /// This member's own `type_id` (e.g. `"node.euler_step_particles"`) — the
    /// registry key `derived_uniform_registry::recompute` uses every frame to
    /// refresh [`Self::derived_uniforms`]'s uniform fields. Only meaningful when
    /// `derived_uniforms` is non-empty; unused (empty string) otherwise.
    pub type_id: String,
    /// Index into the region's distinct Camera externals (see
    /// [`FusionRegion::camera_externals`]) this member's recompute reads, if its
    /// `derived_uniforms` are sourced from a wired `Camera` input port. `None`
    /// for a purely frame-derived member (the time-family) and for any member
    /// with no `derived_uniforms` at all.
    pub derived_camera_ext: Option<usize>,
    /// TEXTURE members only: the WGSL storage-format token the unfused executor
    /// allocates this member's output texture at — `"rgba16float"` (working
    /// default) or `"rgba32float"` (an `outputFormats` fp32 override). When this
    /// member is a region OUTPUT, the fused codegen declares its `dst` storage
    /// texture at this format so the fused kernel writes the same precision the
    /// unfused chain would — the dst half of full-precision in-loop fusion.
    /// Intermediate members keep their format only for completeness (the fused
    /// kernel threads them as f32 registers — no texture). Defaults to
    /// `"rgba16float"`; the buffer path ignores it.
    pub output_storage: &'static str,
    /// STENCIL-FETCH member: the body reads each sampler-`Gather` input through
    /// a free `fetch_<port>(uv)` function instead of `(texture, sampler)` args.
    /// The fused codegen namespaces the member's ENTIRE fragment (helpers call
    /// the fetch, so they can't dedup across members) and emits one
    /// `n{i}_fetch_<port>` per gather input — a real `textureSampleLevel` over
    /// `src_<e>` for an external, or the recomputed virtual-source chain.
    /// `false` keeps the (tex, samp)-args path and prior WGSL byte-identical.
    pub stencil_fetch: bool,
    /// f16-faithful rounding (stencil tier A): wrap this member's body call in
    /// `q16(...)` — a pack2x16float/unpack2x16float round-trip that reproduces
    /// the unfused chain's rgba16float store+load rounding exactly. Set for
    /// in-loop members whose unfused output texture is f16 (see the region
    /// finder); false everywhere else, keeping prior fused WGSL byte-identical.
    pub quantize_f16: bool,
}

/// A maximal fusable region: nodes in topo order, the external inputs they read,
/// and which node register(s) leave the region as its output(s).
#[derive(Debug, Clone)]
pub struct FusionRegion<'a> {
    pub nodes: Vec<RegionNode<'a>>,
    pub num_external_inputs: usize,
    /// The member register(s) the region exposes, in dst-slot order. A region
    /// usually has exactly one (the tail of a linear chain) — fused to a single
    /// `dst` binding, byte-identical to the v1 codegen. A FAN-OUT region (an
    /// interior member feeds two distinct downstream boundaries) has several:
    /// each escaping member's register is stored to its own `dst_<k>` binding,
    /// in this vec's order, and every one is wired to a live consumer by the
    /// install pass (so no store ever lands on an unallocated output). Must be
    /// non-empty; each id must name a node in [`Self::nodes`]. Each entry
    /// carries the escaping PORT NAME (D4/P6): a single-output node has
    /// exactly one entry here (port ignored downstream — the register IS the
    /// value); a MULTI-output node (voronoi_2d) can appear TWICE, once per
    /// distinct escaping port, and the port picks the `BodyOutputs` field
    /// each `dst_<k>` store reads.
    pub outputs: Vec<(NodeInstanceId, String)>,
    /// IN-PLACE buffer regions only: `Some(k)` means the region's single output
    /// must be written back IN PLACE to external input `src_k` (the aliased loop
    /// buffer) instead of a fresh `// @fused_output dst`. The install pass sets
    /// this only when (a) the output traces back through aliased members to that
    /// external input AND (b) that input is part of an `array_feedback` in-place
    /// loop — so aliasing preserves the loop's `in==out` contract WITHOUT the
    /// forward-producer ordering bug the fresh-dst model avoids (a loop buffer has
    /// no forward producer). `None` keeps the fresh-output model (the default, and
    /// the only correct choice for forward-produced regions like DigitalPlants).
    /// Single-output regions only; fan-out (`outputs.len() > 1`) stays fresh.
    pub in_place_alias: Option<usize>,
    /// The address mode of the region's shared gather `samp` sampler, as a WGSL
    /// marker token: `"clamp"` (default, `ClampToEdge` — byte-identical to the
    /// historical fused sampler), `"repeat"`, or `"mirror"`. When a region folds
    /// in a `Gather` atom that wraps (a toroidal fluid gradient), the install
    /// pass resolves the members' agreed mode here, the codegen emits a
    /// `// @sampler_address_mode: <token>` marker on the sampler binding, and
    /// `node.wgsl_compute` creates the sampler at that mode — so a fused gather
    /// samples its edges exactly like the unfused atom. The install pass only
    /// admits gathers that agree on ONE mode, so a single token covers the region.
    pub sampler_address_mode: &'static str,
    /// IN-PLACE buffer regions only: `(member index, param name)` of the live
    /// element count (`active_count`) bounding the whole region's dispatch.
    /// When set, the kernel early-returns at that count and carries a
    /// `// @dispatch_count_param: n{i}_<param>` marker so `node.wgsl_compute`
    /// sizes its grid from the param value instead of the buffer CAPACITY —
    /// matching the standalone integrators, which dispatch only live particles
    /// and leave the pool tail untouched. The install pass sets this only when
    /// every member declares
    /// [`fused_dispatch_count_param`](crate::node_graph::effect_node::EffectNode::fused_dispatch_count_param)
    /// and all those params are driven by ONE producer wire (so a single field
    /// is authoritative). `None` keeps the capacity dispatch.
    pub dispatch_count_field: Option<(usize, &'static str)>,
    /// STENCIL tier: producer chains recomputed inside stencil members'
    /// fetches. Empty for every non-stencil region (and for the buffer path,
    /// which never carries one).
    pub virtual_chains: Vec<FusedVirtualChain>,
    /// CROSS-RESOLUTION externals (workstream 4): external slots whose producer
    /// lives at a different element space than the region grid and are read
    /// `Coincident`. cs_main pre-reads these via `textureSampleLevel(src_e, samp,
    /// uv, 0.0)` (the resolution-robust standalone read) instead of `textureLoad`
    /// at its own canvas coord — a half-res producer would misread the latter.
    /// Forces the shared `samp` to exist. Empty ⇒ every external textureLoad'd,
    /// byte-identical to the v1 codegen.
    pub sampled_externals: Vec<usize>,
    /// Count of distinct `camera_ext_N` CPU-struct external ports this region's
    /// members' derived-uniform recomputes need (D7/P0). The codegen emits one
    /// `// @camera_external: camera_ext_N` marker per index `0..camera_externals`
    /// — `node.wgsl_compute` reads the marker to synthesize a Camera-typed input
    /// port of that name (never a GPU binding: Camera has no WGSL
    /// representation, so this is a DECLARED, non-introspected port — the only
    /// way a CPU-struct external can reach a fused node whose entire persisted
    /// shape is its WGSL text). Zero for every region with no camera-derived
    /// member — the overwhelmingly common case, byte-identical to prior codegen.
    pub camera_externals: usize,
}

/// Result of fusing a region: the kernel + the ordered uniform field list
/// (node + param) so the caller can pack the merged uniform / gather live
/// values (DD-A5 per-source descriptor; step 4 gathers from inst.params).
#[derive(Debug, Clone)]
pub struct GeneratedFusion {
    pub wgsl: String,
    pub param_order: Vec<(NodeInstanceId, &'static str)>,
}

