//! `node.wgsl_compute` — single dynamic WGSL escape hatch.
//!
//! The shader is the contract. Port shape, uniform layout, workgroup
//! size, binding map, and output texture formats are all derived from
//! the WGSL source via `naga` introspection. JSON ships the source
//! string plus (optionally) a dispatch hint; ports are not redeclared.
//!
//! Replaces the static-binding-shape family
//! (`wgsl_compute_0in_1tex`, `_1tex_1tex`, `_2tex_1tex`) with one node
//! that covers every shape — including the BlackHole-required cases the
//! static variants couldn't reach (multi-texture out, Array<Particle>
//! in/out, atomic-u32 accumulator outputs).
//!
//! ## Binding kinds inferred from WGSL
//!
//! - `var<uniform>` struct → packed param layout; one [`ParamDef`] per
//!   scalar member (`_pad`-prefixed members skipped).
//! - `texture_2d<f32>` (Sampled) → required input Texture2D port.
//! - `sampler` → internal primitive-owned sampler (not a Manifold port).
//! - `texture_storage_2d<F, write>` → output Texture2D port carrying
//!   the format `F` (for backend pre-binding).
//! - `var<storage, read> array<Particle>` → required input Array port.
//! - `var<storage, read_write> array<Particle>` → required input AND
//!   output port with the same name, declared as aliased in
//!   [`aliased_array_io`].
//! - `var<storage, read_write> array<atomic<u32>>` → output Array(u32)
//!   port (atomic accumulator).
//!
//! Unsupported shapes (vec/mat uniform members, Texture3D, non-u32
//! atomics, group != 0) fail validation with a warning and leave the
//! pipeline empty. The entry point that runs is chosen by
//! [`select_compute_entry`]: a single `@compute` entry (any name), else
//! the one named `cs_main`; two-plus `@compute` entries with no `cs_main`
//! are ambiguous and fail (BUG-010 — a stray leftover `@compute fn` in a
//! fragment node used to silently run in place of the real kernel).

#![allow(private_interfaces)]

use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use ahash::{AHashMap, AHashSet};
use manifold_gpu::{GpuAddressMode, GpuBinding, GpuComputePipeline, GpuSampler, GpuTextureFormat};

use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, NodeRequires,
};
use crate::node_graph::freeze::classify::FusionKind;
use crate::node_graph::freeze::markers::Marker;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{
    ArrayType, ChannelElementType, ChannelSpec, NodeInput, NodeOutput, NodePort, PortKind,
    PortType, ScalarType,
};

pub const TYPE_ID: &str = "node.wgsl_compute";

// Hand-written registration (no `primitive!` macro for this node —
// the macro hardcodes const port arrays, whereas WgslCompute derives
// ports per-instance from the WGSL source).
inventory::submit! {
    crate::node_graph::persistence::PrimitiveFactory {
        type_id: TYPE_ID,
        create: || Box::new(WgslCompute::new()),
        picker: Some(crate::node_graph::palette::PickerInfo {
            label: "WGSL Compute",
            category: crate::node_graph::palette::PaletteCategory::Atom,
        }),
    }
}

/// Minimal valid kernel — solid grey fill. Just enough to keep the
/// pipeline alive when a freshly-created node is dropped into a graph
/// before the user supplies real source.
pub const DEFAULT_WGSL: &str = r#"
struct U { f0: f32, };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y { return; }
    _ = u.f0;
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(0.5, 0.5, 0.5, 1.0));
}
"#;

// ─────────────────────────────────────────────────────────────────────
// Public node
// ─────────────────────────────────────────────────────────────────────

pub struct WgslCompute {
    /// The kernel actually compiled, introspected, and dispatched. For a
    /// full-kernel node this is the author's source verbatim. For a FUSION
    /// FRAGMENT (see [`fragment_source`](Self::fragment_source)) this is the
    /// standalone kernel synthesized from the fragment's body + declared
    /// ports/params via `codegen::generate_standalone` — so compile, dispatch,
    /// port-shadowing and uniform packing are byte-identical to a hand kernel.
    source: String,
    /// `Some(original fragment text)` when this node was authored in FUSION
    /// FRAGMENT form (a `fn body(...)` + `// @fusion:` marker, no bindings/
    /// `cs_main`). `wgsl_source()` returns this so the editor / persistence
    /// round-trips the fragment, not the synthesized kernel. `None` for a
    /// full-kernel node. The freeze compiler reads
    /// [`fusion_kind`](Self::fusion_kind) / [`fusion_body`](Self::fusion_body)
    /// to fold the synthesized atom into a fused region like any built-in.
    fragment_source: Option<String>,
    /// Fusion classification reported to the freeze compiler. `Pointwise` /
    /// `Source` for a fragment-form node, `Boundary` (the opaque-kernel default)
    /// for a full-kernel node. Returned by the [`EffectNode::fusion_kind`] impl.
    fusion_kind: FusionKind,
    /// Leaked fragment body (the verbatim `fn body(...)` fragment) returned by
    /// [`EffectNode::wgsl_body`]. `None` for a full-kernel node. Leaked because
    /// the trait returns `&'static str` (an atom's body is normally an
    /// `include_str!`); bounded by distinct fragment sources in a session.
    fusion_body: Option<&'static str>,

    // Derived from naga on `set_wgsl_source` / `new`:
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    params: Vec<ParamDef>,
    bindings: Vec<BindingSlot>,
    uniform_layout: Option<UniformLayout>,
    workgroup_size: [u32; 3],
    aliased_pairs: Vec<(String, String)>,
    /// Long-lived &str view of `aliased_pairs`, returned by the
    /// `aliased_array_io` trait method. Rebuilt on every parse so
    /// references stay valid for `&self` borrows.
    aliased_view: Vec<(&'static str, &'static str)>,
    /// Output port names that should be sized to canvas dims by the
    /// chain pre-allocator. Currently every `Array<atomic<u32>>`
    /// accumulator output gets this treatment, matching the convention
    /// `node.draw_particles` uses for its `accum` port. Returned
    /// from `canvas_sized_array_outputs()`.
    canvas_sized_outputs: Vec<&'static str>,
    /// JSON-installed per-output-port canvas-relative size as
    /// `(numerator, denominator)`. Read back by
    /// `output_canvas_scale()` so the chain pre-allocator can size
    /// the persistent slot at `canvas × num / denom`. Recovers the
    /// legacy quarter-res render trick for BlackHole's deflection
    /// bake without baking it into the primitive's Rust shape.
    /// Empty by default; populated via `set_output_canvas_scale()`.
    /// Survives `reparse` — the JSON-side scale describes the
    /// PORT, not the WGSL.
    output_canvas_scales: AHashMap<String, (u32, u32)>,
    /// String arena backing the leaked `&'static str`s used by port
    /// declarations and the aliased_view. Each parse leaks fresh
    /// strings; bounded by distinct port names across the process
    /// lifetime (acceptable — port names come from WGSL identifiers
    /// and a session uses a small finite set).
    _leaked_strings: Vec<&'static str>,
    output_formats: AHashMap<String, GpuTextureFormat>,
    /// Which output port determines dispatch geometry. Defaults to the
    /// first storage texture output, falling back to the first array
    /// output. Settable via the `dispatch_port` extra field from JSON.
    dispatch_port: Option<String>,

    /// Address mode for the lazily-created gather sampler. Default
    /// `ClampToEdge`. A FUSED region whose gather
    /// member wraps (a toroidal fluid gradient) carries a
    /// `// @sampler_address_mode: repeat` marker on its `samp` binding, parsed
    /// in [`introspect`] into this field so the sampler is created at the same
    /// mode the unfused atom uses — keeping fused == unfused at the edges.
    sampler_address_mode: GpuAddressMode,
    /// `true` when the source carries a `// @reset_gated` marker: the node
    /// exposes a synthetic optional `reset_trigger` input and re-dispatches only
    /// on that trigger's integer edges (an expensive generator-side kernel — the
    /// editable seed pattern — whose output is consumed only on a clip reset).
    /// Unwired / no marker ⇒ dispatches every frame (no behaviour change).
    reset_gated: bool,
    /// Last observed `reset_trigger` integer, for the `reset_gated` edge check.
    /// `None` (first frame) differs from any value ⇒ the first frame dispatches.
    last_reset_trigger: Option<i32>,
    /// `// @dispatch_count_param: <field>` marker (emitted by the fused buffer
    /// codegen): the named uniform param carries the kernel's LIVE element
    /// count, so the array dispatch grid is min(capacity, that value) instead
    /// of the full buffer capacity — matching the standalone particle
    /// integrators, which dispatch only live particles. The kernel carries the
    /// matching in-bounds guard, so this is purely a perf cap.
    dispatch_count_param: Option<String>,
    /// `// @derived_uniform_member:` markers (D7/P0), resolved against the
    /// uniform layout: one per fused member with non-empty `derived_uniforms`.
    /// `evaluate()` refreshes each frame via `derived_uniform_registry::recompute`
    /// instead of the generic port-shadow pack. Empty for every kernel this
    /// phase doesn't touch (a hand-authored kernel, a fragment, an ordinary
    /// fused region with no derived-uniform member) — behaviour unchanged.
    derived_uniform_members: Vec<DerivedUniformMember>,
    /// `true` when the source carries a `// @pure` marker: the author asserts
    /// the kernel's output depends only on its params + wired inputs (no time,
    /// no frame counter, no carried state), so the executor's memoizer may
    /// hold its output while those are unchanged (BlackHole's param-driven
    /// deflection bake). Author-asserted, like the fusion markers — a kernel
    /// that reads `time` or aliases a persistent array must NOT carry it.
    source_pure: bool,

    // Runtime / GPU caches:
    pipeline: Option<GpuComputePipeline>,
    sampler: Option<GpuSampler>,
    compiled_hash: Option<u64>,
    compile_failed: bool,
    uniform_scratch: Vec<u8>,
    /// Per-uniform-member last-emitted-value cache, used by the
    /// MANIFOLD_WGSL_COMPUTE_TRACE diagnostic. Keyed by member name.
    /// Populated only when the env var is set; otherwise stays empty.
    last_logged_uniforms: AHashMap<String, f32>,

    // ── Static-param specialization (roadmap 4) ──────────────────────────
    /// Uniform member names (`n{i}_<param>`) the freeze install marked
    /// `// @static_param:` — params with no in-graph control wire, eligible to
    /// bake into a `const` variant. Empty for hand-authored / buffer / non-fused
    /// kernels, which makes the whole specialization path inert (zero behaviour
    /// change). Correctness never rests on this set: the runtime value-key compare
    /// below serves the generic kernel whenever a "static" field's live value
    /// doesn't match a baked variant, so a mis-classified (actually-dynamic) field
    /// is always rendered correctly, just via the fallback.
    static_param_fields: Vec<String>,
    /// P7/D8 precision propagation: per-texture-input access parsed from the
    /// install's `// @input_access:` markers, aligned to the texture inputs of
    /// [`Self::inputs`] in declaration order (the `input_access_of` contract).
    /// Empty (the trait default) when the source carries no markers — every
    /// hand-authored kernel keeps its prior conservative-filtering treatment.
    input_access_view: &'static [crate::node_graph::freeze::classify::InputAccess],
    /// Ports named by `// @precision_critical:` markers — the fused kernel
    /// re-declares the D6(a) fp32 request its members would have made unfused.
    precision_critical_view: &'static [&'static str],
    /// Bounded LRU of specialized kernel variants, keyed by the baked value-set.
    /// Front = most-recently-used. `SPEC_CACHE_CAP` deep so a swept slider can't
    /// accumulate pipelines.
    spec_variants: Vec<SpecVariant>,
    /// The static value-key seen last frame + how many consecutive frames it has
    /// held. The stability gate compiles a variant only once a key has held for
    /// `SPEC_STABLE_FRAMES` frames, so a slider being swept (key changes every
    /// frame) never triggers a single one-shot compile — it just rides the
    /// generic kernel until it settles.
    last_spec_key: Option<Vec<String>>,
    spec_stable: u32,
}

/// Kill switch for static-param specialization. `MANIFOLD_WGSL_SPECIALIZE=0`
/// renders every fused kernel through its generic (all-uniform) variant — the
/// permanent fallback — so a show can fall back instantly if a specialized kernel
/// ever misbehaves, and the parity suite can isolate specialization. Mirrors
/// `MANIFOLD_FREEZE` / `MANIFOLD_CHAIN_FUSION`.
fn specialization_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("MANIFOLD_WGSL_SPECIALIZE") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    })
}

/// Cap on cached specialized variants per node (LRU). Small: a settled show uses
/// one value-set per node, a knob sweep settles on one — 4 covers A/B/C/last.
const SPEC_CACHE_CAP: usize = 4;
/// Frames a static value-key must hold before its variant is compiled. 1 means
/// "compile on the 2nd identical frame", so a per-frame-changing sweep never
/// compiles and a settle pays exactly one compile one frame later.
const SPEC_STABLE_FRAMES: u32 = 1;

/// A compiled specialized kernel: the generic kernel with its static params baked
/// to module-scope `const`s for the exact `key` value-set. Valid ONLY for that
/// value-set — the dispatch value-key compare guarantees it's never used for any
/// other values. Carries its own (smaller) uniform layout + reused scratch so the
/// dispatch packs only the surviving dynamic members.
struct SpecVariant {
    /// Baked value-set: the const literal per static field, in
    /// `static_param_fields` order. Two frames with the same key share a variant.
    key: Vec<String>,
    /// Uniform layout of the specialized kernel (dynamic members only + pad).
    layout: Option<UniformLayout>,
    /// Reused uniform scratch sized to `layout.span`. Owned so the hot path packs
    /// without a per-frame allocation.
    scratch: Vec<u8>,
    pipeline: GpuComputePipeline,
}

#[derive(Clone, Debug)]
struct BindingSlot {
    binding: u32,
    kind: BindingKind,
}

#[derive(Clone, Debug)]
enum BindingKind {
    Uniform,
    /// Sampled texture input. `is_3d` distinguishes `texture_3d<f32>`
    /// (a `Texture3D` port, bound from `ctx.inputs.texture_3d`) from the
    /// common `texture_2d<f32>` — the fused buffer kernels sample volumes
    /// (3D force fields) through the same carrier.
    SampledTexture { port: String, is_3d: bool },
    Sampler,
    StorageTexture { port: String, _format: GpuTextureFormat, _write_only: bool },
    /// `var<storage, read>` of `array<T>` — read-only input.
    StorageArrayRead { port: String, _item: ArrayType },
    /// `var<storage, read_write>` of `array<T>` — aliased in/out (the
    /// integrator pattern). Port name is shared by an input port and
    /// an output port of the same name.
    StorageArrayReadWrite { port: String, _item: ArrayType },
    /// `var<storage, read_write>` of `array<atomic<u32>>` — atomic
    /// accumulator output (the scatter pattern).
    StorageAtomicAccumOut { port: String },
    /// `var<storage, read_write>` of `array<T>` carrying a `// @fused_output`
    /// marker — a FRESH OUTPUT-ONLY array, NOT an aliased in/out. WGSL has no
    /// write-only storage access mode, so the buffer-fusion codegen declares the
    /// output read_write and tags it with the marker; this binding makes the node
    /// expose it as an output port only (no input port, no `aliased_pairs`
    /// entry), so the node's read-only inputs stay forward dependencies and the
    /// loader allocates a fresh output buffer. Fixes the buffer-fusion ordering
    /// bug where an aliased output made the producer wire read as a feedback
    /// back-edge. Bound from `ctx.outputs.array(port)`.
    StorageArrayWriteOut { port: String, _item: ArrayType },
}

#[derive(Clone, Debug)]
struct UniformLayout {
    span: u32,
    members: Vec<UniformMember>,
}

#[derive(Clone, Debug)]
struct UniformMember {
    name: String,
    offset: u32,
    ty: UniformMemberType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UniformMemberType {
    F32,
    I32,
    U32,
    Bool,
}

/// A resolved `// @derived_uniform_member:` marker (D7/P0,
/// `docs/CINEMATIC_POST_DESIGN.md`): the contiguous uniform-field block
/// `[offset, offset + words*4)` a fused member's `derived_uniforms()` occupies,
/// and how to refresh it every frame. `evaluate()` (a) SKIPS this byte range in
/// the generic port-shadow pack loop (these fields are never wired/bound — the
/// whole point is they're recomputed, not read from a param/wire) and (b)
/// separately calls `derived_uniform_registry::recompute(type_id, ctx)` and
/// writes the returned values into the range, in order.
#[derive(Clone, Debug)]
struct DerivedUniformMember {
    /// Byte offset of the block's first field in the packed uniform buffer.
    offset: u32,
    /// Consecutive f32 words (== struct fields, a vec3 already expanded to 3
    /// named fields by codegen) the block spans.
    words: u32,
    /// The member's original primitive `type_id` — the recompute registry key.
    type_id: String,
    /// `camera_ext_N` port name to read for this member's recompute, if its
    /// derived uniforms are sourced from a wired Camera external.
    camera_port: Option<String>,
}

impl UniformMemberType {
    fn write_to(self, dst: &mut [u8], value: &ParamValue) {
        // `ParamValue::Int` was collapsed into `Float` in the storage
        // layer (see feedback_eliminate_bug_class_at_storage_layer);
        // integers ride through Float and we cast at the boundary.
        match (self, value) {
            (Self::F32, ParamValue::Float(f)) => dst.copy_from_slice(&f.to_ne_bytes()),
            (Self::I32, ParamValue::Float(f)) => dst.copy_from_slice(&(*f as i32).to_ne_bytes()),
            (Self::U32, ParamValue::Float(f)) => {
                dst.copy_from_slice(&((*f).max(0.0) as u32).to_ne_bytes())
            }
            (Self::Bool, ParamValue::Bool(b)) => {
                let v: u32 = if *b { 1 } else { 0 };
                dst.copy_from_slice(&v.to_ne_bytes());
            }
            (Self::Bool, ParamValue::Float(f)) => {
                let v: u32 = if *f >= 0.5 { 1 } else { 0 };
                dst.copy_from_slice(&v.to_ne_bytes());
            }
            _ => {
                // Mismatch — leave bytes as zeros; primitive ran with
                // a default but the JSON typed the slot wrong. Surfaces
                // visually as wrong shader behaviour, which is
                // preferable to a panic on the hot path.
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Construction
// ─────────────────────────────────────────────────────────────────────

impl Default for WgslCompute {
    fn default() -> Self {
        Self::new()
    }
}

impl WgslCompute {
    pub fn new() -> Self {
        let mut node = Self {
            source: String::new(),
            fragment_source: None,
            fusion_kind: FusionKind::Boundary,
            fusion_body: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            params: Vec::new(),
            bindings: Vec::new(),
            uniform_layout: None,
            workgroup_size: [1, 1, 1],
            aliased_pairs: Vec::new(),
            aliased_view: Vec::new(),
            canvas_sized_outputs: Vec::new(),
            output_canvas_scales: AHashMap::new(),
            _leaked_strings: Vec::new(),
            output_formats: AHashMap::new(),
            dispatch_port: None,
            sampler_address_mode: GpuAddressMode::ClampToEdge,
            reset_gated: false,
            source_pure: false,
            last_reset_trigger: None,
            dispatch_count_param: None,
            derived_uniform_members: Vec::new(),
            pipeline: None,
            sampler: None,
            compiled_hash: None,
            compile_failed: false,
            uniform_scratch: Vec::new(),
            last_logged_uniforms: AHashMap::new(),
            static_param_fields: Vec::new(),
            input_access_view: &[],
            precision_critical_view: &[],
            spec_variants: Vec::new(),
            last_spec_key: None,
            spec_stable: 0,
        };
        node.reparse(DEFAULT_WGSL.to_string());
        node
    }

    fn cached_type_id() -> &'static EffectNodeType {
        static CACHE: OnceLock<EffectNodeType> = OnceLock::new();
        CACHE.get_or_init(|| EffectNodeType::new(TYPE_ID))
    }

    /// Re-derive port shape, uniform layout, binding map, and
    /// workgroup size from a new WGSL source. Invalidates the
    /// pipeline cache. On parse failure: keeps the previous shape,
    /// flips `compile_failed`, logs a warning. The dispatch is
    /// skipped on the failed-compile path.
    fn reparse(&mut self, source: String) {
        self.pipeline = None;
        self.compiled_hash = None;

        // FUSION FRAGMENT contract (design: wgsl_compute fusion contract). A
        // source carrying a `// @fusion:` marker + a `fn body(` is a fusable
        // body fragment, not a full kernel: ports/params come from `// @in:` /
        // `// @param:` markers (a fragment has no bindings to introspect).
        // Synthesize the standalone kernel from those, then run the EXISTING
        // introspection on it — so the compile/dispatch/uniform-packing path is
        // byte-identical to a hand-authored kernel. The fragment text is kept in
        // `fragment_source` (round-trip) and the body is leaked for the freeze
        // compiler's `wgsl_body()`. Fail closed: a fragment that doesn't
        // synthesize/parse leaves the node compile-failed (renders nothing — a
        // clear load error check-presets surfaces), never a broken card.
        let fragment = FusionFragment::parse(&source);
        let kernel = match &fragment {
            Some(frag) => match frag.synthesize() {
                Ok(k) => k,
                Err(msg) => {
                    log::warn!(
                        "[node.wgsl_compute] fusion-fragment synthesis failed: {msg}"
                    );
                    self.compile_failed = true;
                    self.source = source;
                    self.fragment_source = None;
                    self.fusion_kind = FusionKind::Boundary;
                    self.fusion_body = None;
                    return;
                }
            },
            None => source.clone(),
        };
        self.source = kernel;

        let parsed = match introspect(&self.source) {
            Ok(p) => p,
            Err(msg) => {
                log::warn!("[node.wgsl_compute] introspection failed: {msg}");
                self.compile_failed = true;
                self.fragment_source = None;
                self.fusion_kind = FusionKind::Boundary;
                self.fusion_body = None;
                return;
            }
        };
        self.compile_failed = false;

        self.inputs = parsed.inputs;
        self.outputs = parsed.outputs;
        self.params = parsed.params;
        self.bindings = parsed.bindings;
        self.uniform_layout = parsed.uniform_layout;
        self.workgroup_size = parsed.workgroup_size;
        self.aliased_pairs = parsed.aliased_pairs;
        self.output_formats = parsed.output_formats;
        // Always refresh from the newly-derived default — a stale
        // dispatch_port from a prior source would point at a port
        // that no longer exists. JSON-driven override goes through a
        // separate setter (not implemented yet).
        self.dispatch_port = parsed.default_dispatch_port;
        // A new source may change the gather sampler's address mode (a fused
        // region carrying a repeat marker). Drop a cached sampler built at the
        // old mode so it's recreated at the new one on the next dispatch.
        if self.sampler_address_mode != parsed.sampler_address_mode {
            self.sampler = None;
        }
        self.sampler_address_mode = parsed.sampler_address_mode;
        self.reset_gated = parsed.reset_gated;
        self.source_pure = parsed.source_pure;
        self.dispatch_count_param = parsed.dispatch_count_param;
        self.derived_uniform_members = parsed.derived_uniform_members;
        // A new source invalidates every baked variant (different kernel, different
        // value-keys). Drop them and the stability tracker so specialization
        // re-derives from the new kernel.
        self.static_param_fields = parsed.static_param_fields;
        // P7/D8: rebuild the leaked access/precision views from the source's
        // markers, aligned to the texture inputs in declaration order. No
        // markers → empty slices → identical-to-before trait defaults.
        {
            use crate::node_graph::freeze::classify::InputAccess;
            let mut access_map: AHashMap<String, InputAccess> = AHashMap::new();
            let mut pc_ports: Vec<String> = Vec::new();
            for line in strip_block_comments(&self.source).lines() {
                match Marker::parse(line) {
                    Some(Marker::InputAccess { port, token }) => {
                        let access = match token.as_str() {
                            "coincident" => InputAccess::Coincident,
                            "coincident_texel" => InputAccess::CoincidentTexel,
                            "gather" => InputAccess::Gather,
                            "gather_texel" => InputAccess::GatherTexel,
                            _ => continue,
                        };
                        access_map.insert(port, access);
                    }
                    Some(Marker::PrecisionCritical { port }) => {
                        if !pc_ports.contains(&port) {
                            pc_ports.push(port);
                        }
                    }
                    _ => {}
                }
            }
            if access_map.is_empty() && pc_ports.is_empty() {
                self.input_access_view = &[];
                self.precision_critical_view = &[];
            } else {
                let access_vec: Vec<InputAccess> = self
                    .inputs
                    .iter()
                    .filter(|i| {
                        matches!(
                            i.ty,
                            PortType::Texture2D
                                | PortType::Texture2DTyped(_)
                                | PortType::Texture3D
                        )
                    })
                    .map(|i| access_map.get(i.name.as_ref()).copied().unwrap_or_default())
                    .collect();
                self.input_access_view = Box::leak(access_vec.into_boxed_slice());
                let pc_vec: Vec<&'static str> = pc_ports
                    .into_iter()
                    .map(|p| {
                        let leaked: &'static str = Box::leak(p.into_boxed_str());
                        self._leaked_strings.push(leaked);
                        leaked
                    })
                    .collect();
                self.precision_critical_view = Box::leak(pc_vec.into_boxed_slice());
            }
        }
        self.spec_variants.clear();
        self.last_spec_key = None;
        self.spec_stable = 0;

        // Rebuild leaked &'static views.
        self._leaked_strings.clear();
        self.aliased_view = self
            .aliased_pairs
            .iter()
            .map(|(a, b)| {
                let la: &'static str = Box::leak(a.clone().into_boxed_str());
                let lb: &'static str = Box::leak(b.clone().into_boxed_str());
                self._leaked_strings.push(la);
                self._leaked_strings.push(lb);
                (la, lb)
            })
            .collect();
        // Atomic-u32 accumulator outputs default to canvas-sized
        // allocation, matching node.draw_particles' convention. The
        // dynamic node has no way to express custom capacity yet —
        // when that's needed, surface it as a JSON-side hint or per-port
        // metadata. For BlackHole the canvas-sized default is exactly
        // right (polar density grid sized to display canvas).
        self.canvas_sized_outputs = self
            .bindings
            .iter()
            .filter_map(|b| match &b.kind {
                BindingKind::StorageAtomicAccumOut { port } => {
                    let leaked: &'static str = Box::leak(port.clone().into_boxed_str());
                    self._leaked_strings.push(leaked);
                    Some(leaked)
                }
                _ => None,
            })
            .collect();

        if let Some(layout) = &self.uniform_layout {
            self.uniform_scratch.resize(layout.span as usize, 0);
        } else {
            self.uniform_scratch.clear();
        }

        // Fragment form: record the contract for the freeze compiler and restore
        // the author's declared param defaults/ranges. The synthesized kernel's
        // `Params` struct carries zeroed defaults (the marker-declared defaults
        // aren't visible to naga introspection), so `parsed.params` would lose
        // them — overwrite with the fragment's ParamDefs, whose names, order and
        // count match the synthesized uniform members one-to-one (same PARAMS
        // order both directions), so the layout/port-shadow bindings stay valid.
        match &fragment {
            Some(frag) => {
                self.fragment_source = Some(source);
                self.fusion_kind = frag.kind;
                self.fusion_body =
                    Some(Box::leak(frag.body.clone().into_boxed_str()) as &'static str);
                self.params = frag.params.clone();

                // `generate_standalone` names each texture-input binding
                // `tex_<name>` and the single output `dst`. Rename the
                // introspected ports (and their binding port keys) back to the
                // AUTHORED names — `<name>` per `@in`, and `out` for the output —
                // so presets wire to the names the fragment declares, not codegen
                // internals. Scalar param-shadow inputs carry the param name
                // already (no `tex_` prefix) — but BUG-012: a scalar param
                // author-named `tex_<x>` (e.g. `tex_speed`) collides with this
                // convention, so the rename must be filtered to texture-typed
                // ports (mirroring the binding-key rename just below), never
                // applied by name alone.
                for inp in &mut self.inputs {
                    if matches!(inp.ty, PortType::Texture2D | PortType::Texture2DTyped(_))
                        && let Some(stripped) = inp.name.strip_prefix("tex_")
                    {
                        inp.name = Cow::Borrowed(leak_str(stripped));
                    }
                }
                for b in &mut self.bindings {
                    match &mut b.kind {
                        BindingKind::SampledTexture { port, .. } => {
                            if let Some(s) = port.strip_prefix("tex_") {
                                *port = s.to_string();
                            }
                        }
                        BindingKind::StorageTexture { port, .. } if port == FRAGMENT_DST => {
                            *port = FRAGMENT_OUT.to_string();
                        }
                        _ => {}
                    }
                }
                for out in &mut self.outputs {
                    if out.name == FRAGMENT_DST {
                        out.name = Cow::Borrowed(FRAGMENT_OUT);
                    }
                }
                if let Some(fmt) = self.output_formats.remove(FRAGMENT_DST) {
                    self.output_formats.insert(FRAGMENT_OUT.to_string(), fmt);
                }
                if self.dispatch_port.as_deref() == Some(FRAGMENT_DST) {
                    self.dispatch_port = Some(FRAGMENT_OUT.to_string());
                }
            }
            None => {
                self.fragment_source = None;
                self.fusion_kind = FusionKind::Boundary;
                self.fusion_body = None;
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Introspection
// ─────────────────────────────────────────────────────────────────────

struct ParsedShader {
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
    params: Vec<ParamDef>,
    bindings: Vec<BindingSlot>,
    uniform_layout: Option<UniformLayout>,
    workgroup_size: [u32; 3],
    aliased_pairs: Vec<(String, String)>,
    output_formats: AHashMap<String, GpuTextureFormat>,
    default_dispatch_port: Option<String>,
    sampler_address_mode: GpuAddressMode,
    reset_gated: bool,
    source_pure: bool,
    dispatch_count_param: Option<String>,
    /// Uniform member names tagged `// @static_param:` by the freeze install —
    /// the param fields eligible to bake into a specialized `const` variant.
    static_param_fields: Vec<String>,
    /// Resolved `// @derived_uniform_member:` markers (D7/P0). See
    /// [`DerivedUniformMember`].
    derived_uniform_members: Vec<DerivedUniformMember>,
}

/// The synthetic input port a `// @reset_gated` kernel exposes to receive its
/// edge trigger (wired from the same clip-trigger count the gated consumer uses).
const RESET_TRIGGER_PORT: &str = "reset_trigger";

/// `generate_standalone` names the single storage-texture output `dst`; a
/// fragment node renames it to the conventional atom output port `out`.
const FRAGMENT_DST: &str = "dst";
const FRAGMENT_OUT: &str = "out";

/// Pick the compute entry point that `introspect` AND every
/// `create_compute_pipeline` call must agree on. A synthesized or built-in kernel
/// has exactly one compute entry; a fragment-form node can accidentally embed a
/// stray `@compute fn` (a debug leftover, a copy-paste) BEFORE the generated
/// `cs_main`, and naga's `entry_points[0]` would then be that leftover — it would
/// be introspected AND dispatched instead of the real kernel, silently rendering
/// stale/blank output (BUG-010).
///
/// Rule: a single `@compute` entry wins regardless of name (back-compat with
/// hand-authored kernels not named `cs_main`); with more than one, only the entry
/// named `cs_main` (the synthesis convention) is unambiguous — otherwise fail
/// validation, exactly as the module doc promises.
fn select_compute_entry(module: &naga::Module) -> Result<&naga::EntryPoint, String> {
    let mut computes = module
        .entry_points
        .iter()
        .filter(|e| e.stage == naga::ShaderStage::Compute);
    let first = computes.next().ok_or("no @compute entry point")?;
    // Single compute entry — the overwhelming case, byte-identical to the old
    // `entry_points[0]` pick for every well-formed kernel.
    if computes.next().is_none() {
        return Ok(first);
    }
    module
        .entry_points
        .iter()
        .find(|e| e.stage == naga::ShaderStage::Compute && e.name == "cs_main")
        .ok_or_else(|| {
            "multiple @compute entry points and none named `cs_main` — ambiguous, refusing"
                .to_string()
        })
}

fn introspect(source: &str) -> Result<ParsedShader, String> {
    let module = naga::front::wgsl::parse_str(source).map_err(|e| e.emit_to_string(source))?;

    let ep = select_compute_entry(&module)?;
    let workgroup_size = ep.workgroup_size;

    // Extract `// @channel_skip` markers from the source. Naga preserves
    // comments through parse but doesn't surface them, so the marker
    // has to be recovered from the original text and merged into the
    // struct walk below. Pure transform of the source; no I/O.
    let skip_map = extract_channel_skip(source);
    // `// @fused_output` markers: storage `array<T>` globals tagged as a fresh
    // output-only buffer (the buffer-fusion codegen's dst). WGSL has no
    // write-only storage mode, so these are declared `read_write` but must NOT be
    // treated as aliased in/out — see `BindingKind::StorageArrayWriteOut`.
    let fused_outputs = extract_fused_outputs(source);
    // `// @reset_gated`: the kernel re-dispatches only on a `reset_trigger` edge.
    let reset_gated = source_has_reset_gated_marker(source);
    // `// @pure`: author-asserted memoizable kernel (params + inputs only).
    let source_pure = source_has_pure_marker(source);
    // `// @dispatch_count_param: <field>`: the named uniform param caps the
    // array dispatch grid at the live element count (fused buffer codegen).
    let dispatch_count_param = extract_dispatch_count_param(source);
    // D7/P0: `// @camera_external:` / `// @derived_uniform_member:` markers —
    // see `emit_derived_uniform_markers` in `freeze/codegen.rs` for what emits
    // them and the resolution block below for what consumes them.
    let camera_externals = extract_camera_externals(source);
    let raw_derived_markers = extract_derived_uniform_markers(source);

    let mut inputs: Vec<NodeInput> = Vec::new();
    let mut outputs: Vec<NodeOutput> = Vec::new();
    let mut params: Vec<ParamDef> = Vec::new();
    let mut bindings: Vec<BindingSlot> = Vec::new();
    let mut uniform_layout: Option<UniformLayout> = None;
    let mut aliased_pairs: Vec<(String, String)> = Vec::new();
    let mut output_formats: AHashMap<String, GpuTextureFormat> = AHashMap::new();
    let mut default_dispatch_port: Option<String> = None;
    let mut first_array_out: Option<String> = None;

    for (_handle, gv) in module.global_variables.iter() {
        let Some(binding) = gv.binding.as_ref() else {
            continue;
        };
        if binding.group != 0 {
            return Err(format!(
                "binding group {} not supported (only group 0)",
                binding.group
            ));
        }
        let name = gv
            .name
            .clone()
            .ok_or_else(|| format!("global at binding {} has no name", binding.binding))?;
        let ty = &module.types[gv.ty];

        match gv.space {
            naga::AddressSpace::Uniform => {
                let (layout, derived_params, derived_scalar_inputs) =
                    parse_uniform(&module, ty, &name)?;
                if uniform_layout.is_some() {
                    return Err("multiple uniform globals not supported".into());
                }
                uniform_layout = Some(layout);
                params = derived_params;
                // Port-shadow every non-pad uniform member: each becomes
                // an OPTIONAL ScalarF32 input port with the same name as
                // the param. evaluate() uses scalar_or_param(name) to
                // read from the wire when present, falling back to the
                // param value otherwise. Matches the §6.2 authoring
                // rule that "every numeric scalar param ships as a
                // port-shadowed optional input by default."
                inputs.extend(derived_scalar_inputs);
                bindings.push(BindingSlot {
                    binding: binding.binding,
                    kind: BindingKind::Uniform,
                });
            }
            naga::AddressSpace::Handle => match &ty.inner {
                naga::TypeInner::Image { dim, arrayed, class } => {
                    if *arrayed {
                        return Err(format!("texture '{name}' is arrayed (unsupported)"));
                    }
                    // Sampled textures may be 2D or 3D (volume sampling — the
                    // fused buffer kernels read 3D force fields). Storage
                    // textures stay 2D-only: every storage-output code path
                    // (dispatch sizing, output allocation) assumes 2D.
                    let is_3d = matches!(dim, naga::ImageDimension::D3);
                    if !matches!(dim, naga::ImageDimension::D2 | naga::ImageDimension::D3) {
                        return Err(format!(
                            "texture '{name}' has unsupported dimension (2D/3D only)"
                        ));
                    }
                    if is_3d && !matches!(class, naga::ImageClass::Sampled { .. }) {
                        return Err(format!(
                            "texture '{name}' is 3D but not sampled (3D storage unsupported)"
                        ));
                    }
                    match class {
                        naga::ImageClass::Sampled { .. } => {
                            inputs.push(NodePort {
                                name: Cow::Borrowed(leak_str(&name)),
                                ty: if is_3d { PortType::Texture3D } else { PortType::Texture2D },
                                kind: PortKind::Input,
                                required: true,
                            });
                            bindings.push(BindingSlot {
                                binding: binding.binding,
                                kind: BindingKind::SampledTexture { port: name, is_3d },
                            });
                        }
                        naga::ImageClass::Storage { format, access } => {
                            let fmt = storage_format_to_gpu(*format).ok_or_else(|| {
                                format!("unsupported storage texture format on '{name}'")
                            })?;
                            let write_only =
                                access.contains(naga::StorageAccess::STORE)
                                    && !access.contains(naga::StorageAccess::LOAD);
                            let port_name = leak_str(&name);
                            outputs.push(NodePort {
                                name: Cow::Borrowed(port_name),
                                ty: PortType::Texture2D,
                                kind: PortKind::Output,
                                required: false,
                            });
                            output_formats.insert(name.clone(), fmt);
                            if default_dispatch_port.is_none() {
                                default_dispatch_port = Some(name.clone());
                            }
                            bindings.push(BindingSlot {
                                binding: binding.binding,
                                kind: BindingKind::StorageTexture {
                                    port: name,
                                    _format: fmt,
                                    _write_only: write_only,
                                },
                            });
                        }
                        naga::ImageClass::Depth { .. } => {
                            return Err(format!(
                                "depth texture '{name}' not supported"
                            ));
                        }
                        _ => {
                            return Err(format!(
                                "texture '{name}' has unsupported image class"
                            ));
                        }
                    }
                }
                naga::TypeInner::Sampler { .. } => {
                    bindings.push(BindingSlot {
                        binding: binding.binding,
                        kind: BindingKind::Sampler,
                    });
                }
                _ => {
                    return Err(format!(
                        "handle-space binding '{name}' is neither image nor sampler"
                    ));
                }
            },
            naga::AddressSpace::Storage { access } => {
                // Expect a runtime-sized array (`array<T>`).
                let naga::TypeInner::Array { base, size: naga::ArraySize::Dynamic, stride } =
                    ty.inner
                else {
                    return Err(format!(
                        "storage binding '{name}' is not a runtime array<T>"
                    ));
                };
                let element = &module.types[base];
                let is_atomic_u32 = matches!(
                    element.inner,
                    naga::TypeInner::Atomic(naga::Scalar {
                        kind: naga::ScalarKind::Uint,
                        width: 4,
                    })
                );
                let read = access.contains(naga::StorageAccess::LOAD);
                let write = access.contains(naga::StorageAccess::STORE);

                if is_atomic_u32 {
                    // Atomic accumulator — always declared as an
                    // OUTPUT Array(Anonymous, item_size=4) port.
                    // Downstream consumers that need typed u32 buffers
                    // (resolve_accumulator, etc.) wire through
                    // `node.cast_as_u32` to relabel the wire. wgsl_compute
                    // itself is type-agnostic: the WGSL kernel owns the
                    // per-byte interpretation.
                    let port_name = leak_str(&name);
                    // Atomic accumulator: an `array<atomic<u32>>` is
                    // u32-per-slot, so the Channels signature is the
                    // single-channel u32 form that downstream consumers
                    // (resolve_accumulator etc.) declare via Array(u32).
                    // Same `value: U32` shape that u32's KnownItem impl
                    // produces; matches by hash, validates cleanly.
                    outputs.push(NodePort {
                        name: Cow::Borrowed(port_name),
                        ty: PortType::Array(ArrayType {
                            item_size: 4,
                            item_align: 4,
                            specs: &[ChannelSpec {
                                name: crate::node_graph::channel_names::well_known::VALUE,
                                ty: ChannelElementType::U32,
                            }],
                            match_mode: crate::node_graph::ports::MatchMode::Exact,
                        }),
                        kind: PortKind::Output,
                        required: false,
                    });
                    if default_dispatch_port.is_none() && first_array_out.is_none() {
                        first_array_out = Some(name.clone());
                    }
                    bindings.push(BindingSlot {
                        binding: binding.binding,
                        kind: BindingKind::StorageAtomicAccumOut { port: name },
                    });
                } else {
                    // Non-atomic struct array. The naga struct walk in
                    // `struct_members_to_specs` emits a typed Channels
                    // signature derived from the WGSL struct's fields,
                    // honoring any `// @channel_skip` markers in the
                    // source.
                    let item = element_to_array_type(element, stride, &module, &skip_map)?;
                    let port_name = leak_str(&name);
                    if fused_outputs.contains(&name) {
                        // `// @fused_output` — a fresh OUTPUT-ONLY array (the
                        // buffer-fusion dst). Declared read_write (WGSL has no
                        // write-only storage), but exposed as an output port only:
                        // no input port, no aliased pair. The node's read-only
                        // inputs stay forward deps (correct execution order) and
                        // the loader allocates a fresh buffer via
                        // `array_output_capacity`.
                        outputs.push(NodePort {
                            name: Cow::Borrowed(port_name),
                            ty: PortType::Array(item),
                            kind: PortKind::Output,
                            required: false,
                        });
                        if default_dispatch_port.is_none() && first_array_out.is_none() {
                            first_array_out = Some(name.clone());
                        }
                        bindings.push(BindingSlot {
                            binding: binding.binding,
                            kind: BindingKind::StorageArrayWriteOut { port: name, _item: item },
                        });
                    } else if read && write {
                        // Aliased in/out — declare an input AND an
                        // output with the same name, register the
                        // pair in aliased_array_io.
                        inputs.push(NodePort {
                            name: Cow::Borrowed(port_name),
                            ty: PortType::Array(item),
                            kind: PortKind::Input,
                            required: true,
                        });
                        outputs.push(NodePort {
                            name: Cow::Borrowed(port_name),
                            ty: PortType::Array(item),
                            kind: PortKind::Output,
                            required: false,
                        });
                        aliased_pairs.push((name.clone(), name.clone()));
                        if default_dispatch_port.is_none() && first_array_out.is_none() {
                            first_array_out = Some(name.clone());
                        }
                        bindings.push(BindingSlot {
                            binding: binding.binding,
                            kind: BindingKind::StorageArrayReadWrite {
                                port: name,
                                _item: item,
                            },
                        });
                    } else if read && !write {
                        inputs.push(NodePort {
                            name: Cow::Borrowed(port_name),
                            ty: PortType::Array(item),
                            kind: PortKind::Input,
                            required: true,
                        });
                        bindings.push(BindingSlot {
                            binding: binding.binding,
                            kind: BindingKind::StorageArrayRead {
                                port: name,
                                _item: item,
                            },
                        });
                    } else {
                        return Err(format!(
                            "storage array '{name}' has unsupported access {access:?}"
                        ));
                    }
                }
            }
            _ => {
                // Private / WorkGroup / Function / other internal
                // address spaces — not module-level bindings, ignore.
            }
        }
    }

    if default_dispatch_port.is_none() {
        default_dispatch_port = first_array_out;
    }

    // D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): resolve each raw
    // `// @derived_uniform_member:` marker against the uniform layout's field
    // offsets (now known), then EXCLUDE that member's fields from the generic
    // port-shadow/param set built above — these fields are never wired/bound by
    // a user; `node.wgsl_compute` recomputes them itself every frame (see
    // `evaluate()`). A marker naming a field this kernel doesn't actually have
    // is ignored defensively (never panics — codegen is trusted, but a hand-
    // edited kernel with a stray marker degrades to "field stays a normal
    // param" rather than crashing introspection).
    let mut derived_uniform_members: Vec<DerivedUniformMember> = Vec::new();
    if !raw_derived_markers.is_empty()
        && let Some(layout) = &uniform_layout
    {
        let mut excluded: AHashSet<String> = AHashSet::default();
        for m in &raw_derived_markers {
            let Some(start) = layout.members.iter().position(|f| f.name == m.first_field) else {
                continue;
            };
            for f in layout.members.iter().skip(start).take(m.words as usize) {
                excluded.insert(f.name.clone());
            }
            derived_uniform_members.push(DerivedUniformMember {
                offset: layout.members[start].offset,
                words: m.words,
                type_id: m.type_id.clone(),
                camera_port: m.camera_port.clone(),
            });
        }
        if !excluded.is_empty() {
            params.retain(|p| !excluded.contains(p.name.as_ref()));
            inputs.retain(|i| !excluded.contains(i.name.as_ref()));
        }
    }

    // D7/P0: each `// @camera_external:` marker synthesizes a DECLARED,
    // non-introspected Camera-typed input port. Camera has no WGSL
    // representation (unlike every texture/storage/uniform binding above), so
    // this is the only way a CPU-struct external reaches a fused node whose
    // entire persisted shape is its WGSL text.
    for name in &camera_externals {
        if !inputs.iter().any(|p| p.name == name.as_str()) {
            inputs.push(NodePort {
                name: Cow::Owned(name.clone()),
                ty: PortType::Camera,
                kind: PortKind::Input,
                required: true,
            });
        }
    }

    // A `// @reset_gated` kernel gains a synthetic optional `reset_trigger` input
    // (an f32 the caller wires from a clip-trigger count). Skip if the shader
    // already declares a same-named member, so there's never a double port.
    if reset_gated && !inputs.iter().any(|p| p.name == RESET_TRIGGER_PORT) {
        inputs.push(NodePort {
            name: Cow::Borrowed(RESET_TRIGGER_PORT),
            ty: PortType::Scalar(ScalarType::F32),
            kind: PortKind::Input,
            required: false,
        });
    }

    Ok(ParsedShader {
        inputs,
        outputs,
        params,
        bindings,
        uniform_layout,
        workgroup_size,
        aliased_pairs,
        output_formats,
        default_dispatch_port,
        sampler_address_mode: parse_sampler_address_mode(source),
        reset_gated,
        source_pure,
        dispatch_count_param,
        static_param_fields: extract_static_params(source),
        derived_uniform_members,
    })
}

/// Scan for `// @static_param: <field>` markers (emitted by the freeze install on
/// a texture fused kernel's uniform param fields that have no control wire). Each
/// names a `n{i}_<param>` uniform member whose live value may be baked into a
/// module-scope `const` variant. Same comment conventions as the other markers
/// (block comments stripped first, own-line or trailing, exact prefix).
fn extract_static_params(source: &str) -> Vec<String> {
    let stripped = strip_block_comments(source);
    let mut fields = Vec::new();
    for line in stripped.lines() {
        if let Some(Marker::StaticParam { field }) = Marker::parse(line)
            && !fields.iter().any(|f| f == &field)
        {
            fields.push(field);
        }
    }
    fields
}

/// WGSL `const` declaration for a baked static member. The value MUST equal the
/// bytes [`UniformMemberType::write_to`] would pack for the same `f`, so the
/// specialized kernel is bit-identical to the generic one — only now the value is
/// a compile-time constant. F32 uses Rust's shortest round-trippable `Debug`
/// formatting (always has a `.` or exponent, parses back to the same f32); I32 /
/// U32 replicate `write_to`'s saturating `as i32` / `.max(0) as u32` casts.
fn spec_const_decl(name: &str, ty: UniformMemberType, f: f32) -> String {
    match ty {
        UniformMemberType::F32 => format!("const {name}: f32 = {f:?};"),
        UniformMemberType::I32 => format!("const {name}: i32 = {};", f as i32),
        UniformMemberType::U32 => format!("const {name}: u32 = {};", f.max(0.0) as u32),
        // A Bool member never reaches a fused kernel (Bool/Enum params lay out as
        // u32 — freeze codegen `param_wgsl_type`), but stay correct if one does.
        UniformMemberType::Bool => format!("const {name}: bool = {};", f >= 0.5),
    }
}

fn parse_uniform(
    module: &naga::Module,
    ty: &naga::Type,
    binding_name: &str,
) -> Result<(UniformLayout, Vec<ParamDef>, Vec<NodeInput>), String> {
    let naga::TypeInner::Struct { members, span } = &ty.inner else {
        return Err(format!(
            "uniform binding '{binding_name}' is not a struct"
        ));
    };
    let mut layout_members = Vec::new();
    let mut params: Vec<ParamDef> = Vec::new();
    let mut scalar_inputs: Vec<NodeInput> = Vec::new();
    for m in members {
        let Some(name) = m.name.clone() else {
            return Err("uniform struct member with no name".into());
        };
        let inner = &module.types[m.ty].inner;
        let ty = match inner {
            naga::TypeInner::Scalar(scalar) => match (scalar.kind, scalar.width) {
                (naga::ScalarKind::Float, 4) => UniformMemberType::F32,
                (naga::ScalarKind::Sint, 4) => UniformMemberType::I32,
                (naga::ScalarKind::Uint, 4) => UniformMemberType::U32,
                (naga::ScalarKind::Bool, _) => UniformMemberType::Bool,
                _ => {
                    return Err(format!(
                        "uniform member '{name}' has unsupported scalar {scalar:?}"
                    ));
                }
            },
            _ => {
                return Err(format!(
                    "uniform member '{name}' is not a scalar (vec/mat not yet supported)"
                ));
            }
        };
        layout_members.push(UniformMember {
            name: name.clone(),
            offset: m.offset,
            ty,
        });
        if name.starts_with("_pad") {
            continue;
        }
        let pname = leak_str(&name);
        let param_ty = match ty {
            UniformMemberType::F32 => ParamType::Float,
            UniformMemberType::I32 | UniformMemberType::U32 => ParamType::Int,
            UniformMemberType::Bool => ParamType::Bool,
        };
        let default = match ty {
            UniformMemberType::F32 => ParamValue::Float(0.0),
            // Int/U32 ride through Float in the storage layer; the
            // shader-side u/i cast happens in `UniformMemberType::write_to`.
            UniformMemberType::I32 | UniformMemberType::U32 => ParamValue::Float(0.0),
            UniformMemberType::Bool => ParamValue::Bool(false),
        };
        params.push(ParamDef {
            name: Cow::Borrowed(pname),
            label: pname,
            ty: param_ty,
            default,
            range: None,
            enum_values: &[],
        });
        // Port-shadow each non-pad uniform member with an OPTIONAL
        // ScalarF32 input. evaluate() prefers the wired value if
        // present, falls back to the param value otherwise.
        scalar_inputs.push(NodePort {
            name: Cow::Borrowed(pname),
            ty: PortType::Scalar(ScalarType::F32),
            kind: PortKind::Input,
            required: false,
        });
    }
    Ok((
        UniformLayout {
            span: *span,
            members: layout_members,
        },
        params,
        scalar_inputs,
    ))
}

fn element_to_array_type(
    element: &naga::Type,
    _stride: u32,
    module: &naga::Module,
    skip_map: &ChannelSkipMap,
) -> Result<ArrayType, String> {
    // wgsl_compute is the byte-buffer escape hatch. The naga struct
    // walk in `struct_members_to_specs` emits a typed Channels
    // signature describing the storage-array element struct; if the
    // struct contains unsupported types (matrices, runtime arrays),
    // specs falls back to `&[]` and the wire connects only against
    // other empty-specs Array wires of matching size+align via the
    // raw-byte rule in `port_types_compatible`.
    //
    // align=4 not naga's vec3-padded alignment of 16 — matches the
    // Rust-side layout convention every other primitive uses.
    let naga::TypeInner::Struct { span, members } = &element.inner else {
        return Err("storage array element is not a struct".into());
    };
    let specs = struct_members_to_specs(members, module, element.name.as_deref(), skip_map);
    Ok(ArrayType {
        item_size: *span,
        item_align: 4,
        specs,
        match_mode: crate::node_graph::ports::MatchMode::Exact,
    })
}

/// Walk a WGSL storage-array struct's members and emit a Channels
/// signature.
///
/// Per `docs/CHANNEL_TYPE_SYSTEM.md` §8.2 / §14.9:
/// - Fields the author tagged with a preceding `// @channel_skip`
///   marker are SKIPPED. The skip set is the per-struct lookup in
///   `skip_map`, built by [`extract_channel_skip`] from the original
///   source before naga sees it.
/// - The legacy `_pad[0-9]*` name-prefix heuristic was retired with
///   the marker's landing — naming a field `padding` (or anything
///   else) no longer silently drops it from the wire. Authors who
///   want a field excluded write the marker.
/// - Each remaining field maps to a [`ChannelSpec`] with:
///     - `name`: `ChannelName::from_str(field.name)`. The hash collides
///       with the registry's `well_known::*` constants when the WGSL
///       author happens to use a canonical name; otherwise the
///       signature carries the field's raw name (debug lookup falls
///       back to the hex hash).
///     - `ty`: mapped from the field's WGSL type via
///       [`naga_type_to_channel_element_type`].
/// - Fields whose type doesn't map cleanly (matrices, runtime arrays,
///   atomics) cause the entire signature to fall back to `&[]` — the
///   wire connects only against other empty-specs Array wires of
///   matching size+align via the raw-byte rule in
///   `port_types_compatible`.
///
/// The returned slice is `'static` via `Box::leak`. Same justification
/// as `leak_str`: bounded by the distinct field-name + element-type
/// combinations across all loaded wgsl_compute shaders in a session.
fn struct_members_to_specs(
    members: &[naga::StructMember],
    module: &naga::Module,
    struct_name: Option<&str>,
    skip_map: &ChannelSkipMap,
) -> &'static [ChannelSpec] {
    let skip_set = struct_name.and_then(|n| skip_map.get(n));
    let mut specs: Vec<ChannelSpec> = Vec::with_capacity(members.len());
    for m in members {
        let Some(name) = m.name.as_deref() else {
            return &[];
        };
        if let Some(set) = skip_set
            && set.contains(name)
        {
            continue;
        }
        let inner = &module.types[m.ty].inner;
        let Some(ty) = naga_type_to_channel_element_type(inner) else {
            return &[];
        };
        // Leak the field name to a `'static` str so it can both back
        // the `ChannelName` (hash-keyed) and register against the
        // runtime debug-name registry. The registration lets editor
        // tooltips and validator error messages recover "real" /
        // "_pad0" / etc. instead of showing the raw hex hash.
        let leaked = leak_str(name);
        let ch = crate::node_graph::ports::ChannelName::from_str(leaked);
        crate::node_graph::channel_names::register_runtime_name(ch, leaked);
        specs.push(ChannelSpec { name: ch, ty });
    }
    Box::leak(specs.into_boxed_slice())
}

fn naga_type_to_channel_element_type(inner: &naga::TypeInner) -> Option<ChannelElementType> {
    use crate::node_graph::ports::ChannelElementType as CET;
    use naga::ScalarKind as SK;
    use naga::VectorSize as VS;
    match inner {
        naga::TypeInner::Scalar(scalar) => match (scalar.kind, scalar.width) {
            (SK::Float, 4) => Some(CET::F32),
            (SK::Sint, 4) => Some(CET::I32),
            (SK::Uint, 4) => Some(CET::U32),
            _ => None,
        },
        naga::TypeInner::Vector { size, scalar } => match (size, scalar.kind, scalar.width) {
            (VS::Bi, SK::Float, 4) => Some(CET::Vec2F),
            (VS::Tri, SK::Float, 4) => Some(CET::Vec3F),
            (VS::Quad, SK::Float, 4) => Some(CET::Vec4F),
            _ => None,
        },
        _ => None,
    }
}

fn storage_format_to_gpu(f: naga::StorageFormat) -> Option<GpuTextureFormat> {
    use naga::StorageFormat as N;
    Some(match f {
        N::R32Float => GpuTextureFormat::R32Float,
        N::Rg32Float => GpuTextureFormat::Rg32Float,
        N::Rgba8Unorm => GpuTextureFormat::Rgba8Unorm,
        N::Rgba16Float => GpuTextureFormat::Rgba16Float,
        N::Rgba32Float => GpuTextureFormat::Rgba32Float,
        _ => return None,
    })
}

/// Leak a runtime string to `&'static str`. Used for port names whose
/// identity comes from WGSL identifiers. Bounded leak: a session
/// touches only the distinct port-name set across all loaded
/// presets, which is tiny.
fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

// ─────────────────────────────────────────────────────────────────────
// `// @channel_skip` preprocessor
// ─────────────────────────────────────────────────────────────────────

/// Per-struct field skip-set extracted from `// @channel_skip` markers
/// in WGSL source. See [`extract_channel_skip`].
type ChannelSkipMap = AHashMap<String, AHashSet<String>>;

/// Scan WGSL source for `// @channel_skip` markers preceding storage-
/// array struct fields. Returns a map from struct name → set of field
/// names to skip when emitting the Channels signature.
///
/// Per `docs/CHANNEL_TYPE_SYSTEM.md` §8.2 / §14.9 — the explicit marker
/// is the only mechanism for skipping fields (the old `_pad*` name-
/// prefix heuristic was retired alongside this preprocessor's landing).
///
/// Marker semantics:
/// - Must be a line-comment marker: `// @channel_skip`. Surrounding
///   whitespace is fine (`//@channel_skip`, `//   @channel_skip`,
///   trailing whitespace). Trailing text after the marker (e.g.
///   `// @channel_skip — reason`) is NOT accepted; use a separate
///   comment line for prose.
/// - Must appear on its own line — same-line markers
///   (`x: f32, // @channel_skip`) are ignored. The marker precedes
///   the field.
/// - May be separated from the field by blank lines or other comment
///   lines; the marker stays "pending" until a field arrives.
/// - Multiple stacked markers are idempotent — they all apply to the
///   next field, not the next N fields.
/// - Block comments (`/* @channel_skip */`) are NOT honored. Block
///   comments are stripped from the source before marker extraction so
///   they cannot smuggle a `//` sequence either.
/// - Orphan markers (inside a struct, not followed by a field before
///   the struct closes; or outside any struct entirely) emit a
///   `log::warn` but don't fail the parse — the shader still loads,
///   just without the requested skip.
///
/// The function is a pure transformation; no shared state, no I/O.
fn extract_channel_skip(source: &str) -> ChannelSkipMap {
    let stripped = strip_block_comments(source);
    let mut map: ChannelSkipMap = AHashMap::default();
    let mut current_struct: Option<String> = None;
    let mut waiting_struct: Option<String> = None;
    let mut brace_depth: i32 = 0;
    let mut pending_skip = false;

    for (line_idx, raw_line) in stripped.lines().enumerate() {
        let line = raw_line.trim_start();
        let (code, comment) = split_line_comment(line);
        let code = code.trim();

        // 1. Marker-only lines (no code on the line). Only honor the
        //    marker when we're inside a struct body; outside, it's an
        //    orphan and we warn.
        if code.is_empty() {
            if let Some(c) = comment
                && is_channel_skip_marker(c)
            {
                if current_struct.is_some() {
                    pending_skip = true;
                } else {
                    log::warn!(
                        "[node.wgsl_compute] @channel_skip marker at line {} \
                         is outside any struct — ignored",
                        line_idx + 1
                    );
                }
            }
            continue;
        }

        // 2. Detect `struct Name` at top level. waiting_struct remembers
        //    the name until the opening `{` arrives (possibly on a
        //    later line).
        if current_struct.is_none()
            && waiting_struct.is_none()
            && brace_depth == 0
            && let Some(name) = parse_struct_keyword(code)
        {
            waiting_struct = Some(name);
        }

        // 3. Walk braces on this line's code part. Strings / chars don't
        //    exist in WGSL declarations, so a raw scan is safe.
        for ch in code.chars() {
            match ch {
                '{' => {
                    brace_depth += 1;
                    if brace_depth == 1
                        && let Some(name) = waiting_struct.take()
                    {
                        current_struct = Some(name);
                        pending_skip = false;
                    }
                }
                '}' => {
                    brace_depth -= 1;
                    if brace_depth == 0
                        && let Some(name) = current_struct.take()
                    {
                        if pending_skip {
                            log::warn!(
                                "[node.wgsl_compute] @channel_skip marker inside \
                                 `struct {}` was not followed by a field before \
                                 the struct closed — ignored",
                                name
                            );
                        }
                        pending_skip = false;
                    }
                }
                _ => {}
            }
        }

        // 4. If we're now inside a struct body, look for a field decl on
        //    this line. (Lines that just opened the struct — `struct X {`
        //    — typically have no field text after the `{`; parse_field_name
        //    returns None for those.)
        if let Some(ref struct_name) = current_struct
            && let Some(field) = parse_field_name(code)
            && pending_skip
        {
            map.entry(struct_name.clone()).or_default().insert(field);
            pending_skip = false;
        }
    }

    // Orphan marker at EOF (struct never closed cleanly, or trailing
    // marker outside any struct).
    if pending_skip {
        log::warn!(
            "[node.wgsl_compute] @channel_skip marker at end of source was not \
             followed by a field — ignored"
        );
    }

    map
}

/// Replace each `/* ... */` block-comment run with whitespace, preserving
/// newlines so line numbers stay aligned with the original source. WGSL
/// block comments nest (per the WGSL spec); the depth counter handles
/// that. Anything inside a block comment is neutralised before the
/// line-comment / marker scan runs.
fn strip_block_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut depth: u32 = 0;
    while let Some(c) = chars.next() {
        if depth == 0 {
            if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                depth = 1;
                out.push(' ');
                out.push(' ');
            } else {
                out.push(c);
            }
        } else {
            if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                depth += 1;
                out.push(' ');
                out.push(' ');
            } else if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                depth -= 1;
                out.push(' ');
                out.push(' ');
            } else if c == '\n' {
                out.push('\n');
            } else {
                out.push(' ');
            }
        }
    }
    out
}

/// Split a line into `(code_before_double_slash, comment_after)`. WGSL
/// `//` runs to end of line, so the first occurrence wins.
fn split_line_comment(line: &str) -> (&str, Option<&str>) {
    match line.find("//") {
        Some(idx) => (&line[..idx], Some(&line[idx + 2..])),
        None => (line, None),
    }
}

/// Does this comment body (everything after `//`) hold the channel-skip
/// marker exactly?
fn is_channel_skip_marker(comment: &str) -> bool {
    comment.trim() == "@channel_skip"
}

/// Scan for `// @fused_output` markers preceding a `var<storage, ...> NAME:`
/// global, returning the marked storage-array global NAMES. These are fresh
/// OUTPUT-ONLY buffers (the buffer-fusion dst) — declared `read_write` because
/// WGSL has no write-only storage access mode, but exposed as output ports only
/// (see [`BindingKind::StorageArrayWriteOut`]). Same conventions as
/// `// @channel_skip`: block comments stripped first, own-line marker, exact
/// match (trailing text not accepted). The marker applies to the next storage
/// global declaration line.
/// Scan for a `// @sampler_address_mode: <token>` marker (emitted by the freeze
/// compiler on a fused region's `samp` binding when its gather members wrap) and
/// return the address mode to create the gather sampler at. Absent / unknown ⇒
/// `ClampToEdge`, the default for every hand-authored kernel. WGSL carries no
/// address mode in the shader, so this side channel is how a fused toroidal
/// gradient gets a repeat sampler instead of the default clamp. Same comment
/// conventions as `// @fused_output` (block comments stripped first; matches a
/// trailing or own-line `// @sampler_address_mode: <token>`).
fn parse_sampler_address_mode(source: &str) -> GpuAddressMode {
    let stripped = strip_block_comments(source);
    for line in stripped.lines() {
        if let Some(Marker::SamplerAddressMode { mode }) = Marker::parse(line) {
            return match mode.as_str() {
                "repeat" => GpuAddressMode::Repeat,
                "mirror" => GpuAddressMode::MirrorRepeat,
                _ => GpuAddressMode::ClampToEdge,
            };
        }
    }
    GpuAddressMode::ClampToEdge
}

/// Whether the source carries a `// @reset_gated` line-comment marker (own-line
/// or trailing). Same conventions as the other markers: block comments stripped
/// first, exact match. Drives the synthetic `reset_trigger` input + dispatch gate.
/// Scan for a `// @dispatch_count_param: <field>` marker (emitted by the fused
/// buffer codegen): the named uniform param carries the kernel's live element
/// count, capping the array dispatch grid below the buffer capacity.
fn extract_dispatch_count_param(source: &str) -> Option<String> {
    let stripped = strip_block_comments(source);
    stripped.lines().find_map(|line| match Marker::parse(line) {
        Some(Marker::DispatchCountParam { field }) => Some(field),
        _ => None,
    })
}

/// Scan for `// @camera_external: <name>` markers (emitted by the fused-region
/// codegen — `emit_derived_uniform_markers` in `freeze/codegen.rs`, one per
/// distinct Camera external the region routes). Each names a DECLARED,
/// non-introspected Camera-typed input port to synthesize: Camera has no WGSL
/// representation, so unlike every other port kind this one can't be discovered
/// from a binding — the marker is the only channel (D7/P0,
/// `docs/CINEMATIC_POST_DESIGN.md`). Order preserved, deduped.
fn extract_camera_externals(source: &str) -> Vec<String> {
    let stripped = strip_block_comments(source);
    let mut names = Vec::new();
    for line in stripped.lines() {
        if let Some(Marker::CameraExternal { name }) = Marker::parse(line)
            && !names.iter().any(|n| n == &name)
        {
            names.push(name);
        }
    }
    names
}

/// One `// @derived_uniform_member: <first_field> words=<n> <type_id>
/// [<camera_port>]` marker, parsed but not yet resolved against the uniform
/// layout (that happens in [`introspect`], once `parse_uniform` has field
/// offsets). `first_field` is the exact name of the first uniform-struct field
/// in this member's derived-uniform block (`n{i}_dt_scaled`, `n{i}_cam_fwd_x`,
/// …); `words` is how many CONSECUTIVE fields (== f32 values, a vec3 already
/// expanded to 3 named fields by codegen) the block spans.
struct RawDerivedUniformMarker {
    first_field: String,
    words: u32,
    type_id: String,
    camera_port: Option<String>,
}

/// Scan for `// @derived_uniform_member:` markers (emitted by
/// `emit_derived_uniform_markers`, one per fused-region member with non-empty
/// `derived_uniforms`). Format: `<first_field> words=<n> <type_id>`, optionally
/// followed by a `camera_ext_N` port name when the member's recompute needs a
/// Camera external.
fn extract_derived_uniform_markers(source: &str) -> Vec<RawDerivedUniformMarker> {
    let stripped = strip_block_comments(source);
    let mut markers = Vec::new();
    for line in stripped.lines() {
        if let Some(Marker::DerivedUniformMember { first_field, words, type_id, camera_port }) =
            Marker::parse(line)
        {
            markers.push(RawDerivedUniformMarker { first_field, words, type_id, camera_port });
        }
    }
    markers
}

fn source_has_reset_gated_marker(source: &str) -> bool {
    let stripped = strip_block_comments(source);
    stripped.lines().any(|line| matches!(Marker::parse(line), Some(Marker::ResetGated)))
}

/// Whether the source carries a `// @pure` line-comment marker — the author's
/// assertion that the kernel is memoizable (output depends only on params +
/// wired inputs; no time uniform, no frame counter, no persistent/aliased
/// state). Same own-line comment convention as `@reset_gated`.
fn source_has_pure_marker(source: &str) -> bool {
    let stripped = strip_block_comments(source);
    stripped.lines().any(|line| matches!(Marker::parse(line), Some(Marker::Pure)))
}

fn extract_fused_outputs(source: &str) -> std::collections::HashSet<String> {
    let stripped = strip_block_comments(source);
    let mut set = std::collections::HashSet::new();
    let mut pending = false;
    for raw_line in stripped.lines() {
        let line = raw_line.trim_start();
        let (code, _comment) = split_line_comment(line);
        let code = code.trim();
        if code.is_empty() {
            if matches!(Marker::parse(line), Some(Marker::FusedOutput)) {
                pending = true;
            }
            continue;
        }
        if pending {
            if let Some(name) = parse_storage_global_name(code) {
                set.insert(name);
            }
            pending = false;
        }
    }
    set
}

/// Parse `NAME` from a `... var<storage, ...> NAME: array<...>;` declaration, or
/// `None` if the line isn't a storage global.
fn parse_storage_global_name(code: &str) -> Option<String> {
    let idx = code.find("var<storage")?;
    let after = &code[idx..];
    let gt = after.find('>')?; // close of `var<storage, ...>`
    let rest = after[gt + 1..].trim_start(); // `NAME: array<...>;`
    let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

/// If `code` begins with `struct <ident>`, return the identifier. Used
/// to detect the start of a struct declaration so the brace walker can
/// associate the upcoming `{...}` body with this name.
fn parse_struct_keyword(code: &str) -> Option<String> {
    let s = code.trim_start();
    let rest = s.strip_prefix("struct")?;
    let next = rest.chars().next()?;
    if !next.is_whitespace() {
        // e.g., `structure: ...` — not the keyword.
        return None;
    }
    let rest = rest.trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

/// Extract the field name from a WGSL struct-field declaration. Tolerates
/// any leading `@attr(args)` annotations (e.g., `@align(16) position:
/// vec3<f32>,`). Returns None for lines that don't look like field decls.
fn parse_field_name(code: &str) -> Option<String> {
    let mut s = code.trim();
    // Strip `{` left over from a struct-open line like `struct X {`.
    s = s.trim_start_matches('{').trim_start();
    while s.starts_with('@') {
        let after_at = &s[1..];
        let ident_end = after_at
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(after_at.len());
        let after_ident = after_at[ident_end..].trim_start();
        if let Some(rest) = after_ident.strip_prefix('(') {
            let mut depth = 1i32;
            let mut end = None;
            for (i, ch) in rest.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let end = end?;
            s = rest[end..].trim_start();
        } else {
            s = after_ident;
        }
    }
    let colon = s.find(':')?;
    let ident = s[..colon].trim();
    if ident.is_empty() {
        return None;
    }
    if !ident.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some(ident.to_string())
}

// ─────────────────────────────────────────────────────────────────────
// Fusion-fragment contract (`// @fusion:` body fragments)
// ─────────────────────────────────────────────────────────────────────

/// A fusable body fragment authored in `node.wgsl_compute`: a pure
/// `fn body(...)` written to the freeze codegen ABI, with its ports/params
/// declared by markers (a fragment has no bindings to introspect). Detected by
/// a `// @fusion:` marker; synthesized into a standalone kernel via
/// [`generate_standalone`](crate::node_graph::freeze::codegen::generate_standalone)
/// and then introspected exactly like a hand-authored kernel.
///
/// Markers (own-line or trailing `//` comments; block comments stripped first):
///   - `// @fusion: pointwise` — one texture input, one `vec4` output.
///   - `// @fusion: source` — zero inputs (a 0-input generator).
///   - `// @in: <port>` — one texture input, in body-arg order (Pointwise wants 1).
///   - `// @param: <name> = <default> [<min>, <max>]` — a scalar f32 param;
///     the `[min, max]` range tail is optional.
///
/// Body ABI (the existing freeze ABI — see any atom's `*_body.wgsl`, e.g. gain):
///   Pointwise: `fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, <params…>) -> vec4<f32>`
///   Source:    `fn body(uv: vec2<f32>, dims: vec2<f32>, <params…>) -> vec4<f32>`
/// The `@param` declaration order must match the body's trailing arg order.
struct FusionFragment {
    kind: FusionKind,
    inputs: Vec<NodeInput>,
    params: Vec<ParamDef>,
    body: String,
}

impl FusionFragment {
    /// Parse the fragment markers, or `None` when `source` carries no
    /// `// @fusion:` marker or no `fn body(` — i.e. a full kernel (the existing
    /// opaque path). A malformed marker (unknown kind, bad `@param`) also returns
    /// `None`; the caller then treats the source as a full kernel, whose introspect
    /// reports the real error and leaves the node compile-failed (fail closed).
    fn parse(source: &str) -> Option<FusionFragment> {
        let stripped = strip_block_comments(source);
        let mut kind: Option<FusionKind> = None;
        let mut inputs: Vec<NodeInput> = Vec::new();
        let mut params: Vec<ParamDef> = Vec::new();
        for line in stripped.lines() {
            let Some(c) = split_line_comment(line).1 else {
                continue;
            };
            let c = c.trim();
            if let Some(rest) = c.strip_prefix("@fusion:") {
                kind = match Marker::parse(line) {
                    Some(Marker::Fusion { kind }) => match kind.as_str() {
                        "pointwise" => Some(FusionKind::Pointwise),
                        "source" => Some(FusionKind::Source),
                        // Marker::parse only yields Fusion for these two kinds —
                        // unreachable, kept as a fail-closed match arm.
                        _ => return None,
                    },
                    _ => {
                        let other = rest.trim();
                        log::warn!("[node.wgsl_compute] unknown @fusion kind '{other}'");
                        return None;
                    }
                };
            } else if let Some(rest) = c.strip_prefix("@in:") {
                let name = rest.trim();
                if !name.is_empty() {
                    inputs.push(NodePort {
                        name: Cow::Borrowed(leak_str(name)),
                        ty: PortType::Texture2D,
                        kind: PortKind::Input,
                        required: true,
                    });
                }
            } else if let Some(rest) = c.strip_prefix("@param:") {
                match parse_fusion_param(rest) {
                    Some(p) => params.push(p),
                    None => {
                        log::warn!("[node.wgsl_compute] malformed @param marker: '{rest}'");
                        return None;
                    }
                }
            }
        }
        let kind = kind?;
        // A real fragment defines `fn body(`. Without it, fall through to the
        // full-kernel path (which will surface the actual parse error).
        if !source.contains("fn body(") {
            return None;
        }
        Some(FusionFragment {
            kind,
            inputs,
            params,
            body: source.to_string(),
        })
    }

    /// Synthesize the standalone kernel from the fragment — the exact single-
    /// source codegen the freeze compiler also generates for this atom, so the
    /// dispatched kernel and the fused inline are guaranteed consistent. The
    /// single texture output is named `dst` by the codegen (its binding name);
    /// introspection therefore exposes one output port `dst`.
    fn synthesize(&self) -> Result<String, String> {
        const OUT: &[NodeOutput] = &[NodePort {
            name: Cow::Borrowed("out"),
            ty: PortType::Texture2D,
            kind: PortKind::Output,
            required: false,
        }];
        crate::node_graph::freeze::codegen::generate_standalone(
            &crate::node_graph::freeze::codegen::StandaloneKernelSpec {
                fusion_kind: self.kind,
                body: &self.body,
                inputs: &self.inputs,
                params: &self.params,
                input_access: &[],
                derived_uniforms: &[],
                outputs: OUT,
                stencil_fetch: false,
                includes: &[],
            },
        )
        .map_err(|e| format!("{e:?}"))
    }
}

/// Parse a `// @param: <name> = <default> [<min>, <max>]` marker tail (the text
/// after `@param:`) into a scalar f32 [`ParamDef`]. The `[min, max]` range is
/// optional. `None` on any malformed field. v1 rejects a name a WGSL reserved
/// word would force the codegen to mangle (`type` → `p_type`), which would
/// desync the uniform member from the port/param name.
fn parse_fusion_param(rest: &str) -> Option<ParamDef> {
    let (name, after_eq) = rest.trim().split_once('=')?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    if crate::node_graph::freeze::codegen::wgsl_safe_field(name).as_ref() != name {
        return None;
    }
    let after_eq = after_eq.trim();
    let (default_str, range) = match after_eq.split_once('[') {
        Some((d, r)) => {
            let r = r.trim().strip_suffix(']')?;
            let (lo, hi) = r.split_once(',')?;
            let lo: f32 = lo.trim().parse().ok()?;
            let hi: f32 = hi.trim().parse().ok()?;
            (d.trim(), Some((lo, hi)))
        }
        None => (after_eq, None),
    };
    let default: f32 = default_str.trim().parse().ok()?;
    let name_static = leak_str(name);
    Some(ParamDef {
        name: Cow::Borrowed(name_static),
        label: name_static,
        ty: ParamType::Float,
        default: ParamValue::Float(default),
        range,
        enum_values: &[],
    })
}

// ─────────────────────────────────────────────────────────────────────
// EffectNode impl
// ─────────────────────────────────────────────────────────────────────

impl EffectNode for WgslCompute {
    // depth_rule: user-authored arbitrary WGSL kernel — its color/UV/combine
    // semantics aren't known at authoring time, so Terminal is the safest
    // conservative default (never wrongly Inherit/Warp an unknown shader).
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &EffectNodeType {
        Self::cached_type_id()
    }

    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }

    fn outputs(&self) -> &[NodeOutput] {
        &self.outputs
    }

    fn parameters(&self) -> &[ParamDef] {
        &self.params
    }

    fn wgsl_source(&self) -> Option<&str> {
        // Round-trip the AUTHORED text: the original fragment for a fragment-form
        // node (so the editor / persistence never see the synthesized kernel),
        // else the verbatim full-kernel source.
        Some(self.fragment_source.as_deref().unwrap_or(&self.source))
    }

    fn set_wgsl_source(&mut self, source: &str) {
        // Compare against the AUTHORED text on both shapes — for a fragment node
        // `self.source` holds the synthesized kernel, never the incoming fragment.
        if self.fragment_source.as_deref() == Some(source)
            || (self.fragment_source.is_none() && self.source == source)
        {
            return;
        }
        self.reparse(source.to_string());
    }

    fn input_access(&self) -> &'static [crate::node_graph::freeze::classify::InputAccess] {
        self.input_access_view
    }

    fn precision_critical_inputs(&self) -> &'static [&'static str] {
        self.precision_critical_view
    }

    fn fusion_kind(&self) -> FusionKind {
        self.fusion_kind
    }

    /// Mirrors [`Self::fusion_kind`]'s dynamic dispatch: a full-kernel
    /// `node.wgsl_compute` (user-authored source, no fragment parse) is a
    /// Boundary IO bridge; a fragment-form node that parsed to a fusable
    /// kind carries no boundary reason at all — the XOR invariant
    /// (`every_boundary_atom_declares_its_reason`) must hold per-instance,
    /// not just for the type, because this primitive's fusability is
    /// authored content, not a static declaration.
    fn boundary_reason(&self) -> Option<crate::node_graph::freeze::classify::BoundaryReason> {
        if self.fusion_kind == FusionKind::Boundary {
            Some(crate::node_graph::freeze::classify::BoundaryReason::IoBridge)
        } else {
            None
        }
    }

    // `// @pure` marker: author-asserted memoizable kernel. The memoizer's own
    // input/param dirty-checks do the rest — a kernel whose inputs change every
    // frame gains nothing, a param-only kernel (BlackHole's deflection bake)
    // dispatches once per edit.
    fn is_pure(&self) -> bool {
        self.source_pure
    }

    fn wgsl_body(&self) -> Option<&'static str> {
        self.fusion_body
    }

    fn output_format(&self, port: &str) -> Option<GpuTextureFormat> {
        self.output_formats.get(port).copied()
    }

    fn set_output_format(&mut self, _port: &str, _format: GpuTextureFormat) {
        // Format is derived from the WGSL — JSON overrides are
        // ignored. Changing the output format means editing the
        // `texture_storage_2d<F, write>` declaration in the source.
    }

    fn output_canvas_scale(
        &self,
        port: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        self.output_canvas_scales.get(port).copied()
    }

    fn set_output_canvas_scale(&mut self, port: &str, scale: (u32, u32)) {
        // Honored — unlike `set_output_format`, the canvas scale is
        // genuinely a JSON-side property (the WGSL can't express
        // "allocate this output at canvas/4"). Lets the BlackHole
        // preset run its deflection bake at quarter-res without
        // shader changes.
        self.output_canvas_scales
            .insert(port.to_string(), scale);
    }

    fn aliased_array_io(&self) -> &[(&str, &str)] {
        &self.aliased_view
    }

    fn canvas_sized_array_outputs(&self) -> &[&str] {
        &self.canvas_sized_outputs
    }

    fn array_output_capacity(
        &self,
        port_name: &str,
        params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        // A `@fused_output` (write-only) array is coincident with the region's
        // inputs — one element per input element. The fused kernel writes exactly
        // `[0, min(arrayLength of every array external))` (the BUG-008 count guard,
        // matching the unfused atoms' `min(a, b, …)` clamp), so sizing `dst` to the
        // SMALLEST input leaves no never-written tail. `max` over-sized the buffer
        // when inputs differed and downstream (`render_instanced_3d_mesh` derives
        // its instance count from the physical buffer size) drew ghost instances
        // from the uninitialized VRAM tail (BUG-011). Equal-length regions (every
        // shipped buffer preset) are unaffected: `min` of equal lengths is that
        // length.
        let is_fused_out = self.bindings.iter().any(|b| {
            matches!(&b.kind, BindingKind::StorageArrayWriteOut { port, .. } if port == port_name)
        });
        if is_fused_out {
            return input_capacities.iter().map(|(_, c)| *c).min();
        }
        // Otherwise fall back to the trait default (explicit `max_capacity` param).
        let is_array_output = self
            .outputs
            .iter()
            .any(|p| p.name == port_name && matches!(p.ty, PortType::Array(_)));
        if !is_array_output {
            return None;
        }
        params.get("max_capacity").and_then(|v| v.as_u32_clamped(1))
    }

    fn requires(&self) -> NodeRequires {
        NodeRequires {
            gpu_encoder: true,
            ..Default::default()
        }
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if self.compile_failed {
            return;
        }

        // Reset gate (`// @reset_gated` marker → the synthetic `reset_trigger`
        // input): an expensive generator-side kernel — the editable seed pattern —
        // whose output is consumed only on a clip reset. When the trigger is wired,
        // re-dispatch only on its integer edges (+ first frame); between resets the
        // output (a persistent pooled resource) keeps the last pattern, which the
        // equally-gated consumer ignores until the next reset. Unwired ⇒ every frame
        // (no behaviour change). Mirrors the gate on node.spawn_from_image.
        if self.reset_gated
            && let Some(ParamValue::Float(v)) = ctx.inputs.scalar(RESET_TRIGGER_PORT)
        {
            let current = v.round() as i32;
            let edge = self.last_reset_trigger != Some(current);
            self.last_reset_trigger = Some(current);
            if !edge {
                // Skip the dispatch. If this kernel aliases an array in/out (an
                // in-place buffer seed like ParticleText / FluidSim3D), mark the
                // aliased output accessed so the executor's staleness guard accepts
                // the skip: the @reset_gated contract is that the consumer reads the
                // buffer only on reset, so last reset's retained content is exactly
                // what it expects — intentional retention, NOT stale-as-bug. Harmless
                // for a non-aliased kernel (the guard doesn't fire there anyway). The
                // retention is real only if the aliased buffer persists between
                // frames — pair this with `node.spawn_particles seed_mode=OnceOnReset`.
                ctx.mark_gpu_accessed();
                return;
            }
        }

        // Compile (or recompile) the pipeline lazily on source change.
        let source_hash = hash_str(&self.source);
        if self.pipeline.is_none() || self.compiled_hash != Some(source_hash) {
            let gpu = ctx.gpu_encoder();
            // Naga has already validated this source in `reparse` — a successful
            // introspect implies a valid module. Pick the SAME entry `introspect`
            // chose (`select_compute_entry`), never `entry_points[0]`, so a stray
            // leading `@compute fn` can't dispatch in place of `cs_main` (BUG-010).
            let entry = match naga::front::wgsl::parse_str(&self.source)
                .ok()
                .and_then(|m| select_compute_entry(&m).ok().map(|e| e.name.clone()))
            {
                Some(name) => name,
                None => return,
            };
            self.pipeline =
                Some(gpu.device.create_compute_pipeline(&self.source, &entry, TYPE_ID));
            self.compiled_hash = Some(source_hash);
        }

        // Lazy-create sampler if any binding needs one.
        let needs_sampler = self
            .bindings
            .iter()
            .any(|b| matches!(b.kind, BindingKind::Sampler));
        if needs_sampler && self.sampler.is_none() {
            let gpu = ctx.gpu_encoder();
            // Address mode from the `// @sampler_address_mode` marker (default
            // clamp): a fused toroidal gather gets a repeat sampler so it samples
            // its edges exactly like the unfused atom.
            self.sampler = Some(gpu.device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                address_mode_u: self.sampler_address_mode,
                address_mode_v: self.sampler_address_mode,
                address_mode_w: self.sampler_address_mode,
                ..Default::default()
            }));
        }

        // Pack uniforms into the scratch buffer. Each non-pad member
        // reads via scalar_or_param(name, 0.0) — port-shadows-param,
        // so a wired generator_input.aspect / driver / LFO takes
        // precedence over the static param value, which falls back to
        // 0.0 if neither is set. Bool / Int members read the same
        // float and cast at write time (Int storage was collapsed
        // into Float — feedback_eliminate_bug_class_at_storage_layer).
        let trace = std::env::var_os("MANIFOLD_WGSL_COMPUTE_TRACE").is_some();
        let node_id = ctx.node_id;

        // STATIC-PARAM SPECIALIZATION: pick the kernel variant for this frame. The
        // generic kernel (all params as uniforms) is the permanent fallback; a
        // specialized variant bakes the no-control-wire params to module-scope
        // consts for an exact value-set. `select_spec_variant` value-keys against
        // the live param reads and never blocks the render path — a key it hasn't
        // compiled (a tweaked knob, a swept slider) serves generic this frame.
        let spec_idx = self.select_spec_variant(ctx);

        // Pack the active variant's uniform. The generic kernel packs every member;
        // a specialized kernel packs only its surviving dynamic members (the static
        // ones live in the kernel as consts), so its smaller layout/scratch are used.
        match spec_idx {
            None => {
                if let Some(layout) = &self.uniform_layout {
                    for byte in self.uniform_scratch.iter_mut() {
                        *byte = 0;
                    }
                    for m in &layout.members {
                        if m.name.starts_with("_pad") {
                            continue;
                        }
                        // D7/P0: a field inside a `derived_uniform_members` block
                        // is never wired/bound — skip the generic port-shadow
                        // pack here; the dedicated loop below (after this match)
                        // recomputes and writes it instead.
                        if self.derived_uniform_members.iter().any(|d| {
                            m.offset >= d.offset && m.offset < d.offset + d.words * 4
                        }) {
                            continue;
                        }
                        let f = ctx.scalar_or_param(&m.name, 0.0);
                        if trace && self.last_logged_uniforms.get(&m.name).copied() != Some(f) {
                            eprintln!(
                                "[wgsl_compute node={:?}] uniform '{}' = {} (was {:?})",
                                node_id,
                                m.name,
                                f,
                                self.last_logged_uniforms.get(&m.name).copied(),
                            );
                            self.last_logged_uniforms.insert(m.name.clone(), f);
                        }
                        let val = ParamValue::Float(f);
                        let size = 4;
                        let start = m.offset as usize;
                        let end = start + size;
                        if end <= self.uniform_scratch.len() {
                            m.ty.write_to(&mut self.uniform_scratch[start..end], &val);
                        }
                    }
                }
                // D7/P0: refresh every derived-uniform block this frame via the
                // registry (frame context +, when the member needs one, the
                // wired Camera external's live value) — the per-member `run()`
                // equivalent for a fused kernel. `None` (unregistered, or a
                // camera-derived member whose camera is unwired) leaves the
                // block's bytes at the zero-fill above — a visible-but-safe
                // degrade, never a panic; `has_recompute` at fuse-build time
                // (`freeze/install.rs`) means "unregistered" should not occur
                // for a member that made it into a fused kernel at all. Each
                // field is written through its OWN `UniformMemberType::write_to`
                // (looked up from `layout.members`, not assumed f32) — a
                // `frame_count:u32` field needs the same `.max(0.0) as u32` cast
                // `run()`'s hand packing gets, not a raw f32 bit-copy.
                if let Some(layout) = &self.uniform_layout {
                    for d in &self.derived_uniform_members {
                        let Some(start_idx) =
                            layout.members.iter().position(|f| f.offset == d.offset)
                        else {
                            continue;
                        };
                        let camera = d.camera_port.as_deref().and_then(|p| ctx.inputs.camera(p));
                        let dctx =
                            crate::node_graph::freeze::derived_uniform_registry::DerivedUniformContext {
                                frame: &ctx.time,
                                camera: camera.as_ref(),
                            };
                        let Some(values) =
                            crate::node_graph::freeze::derived_uniform_registry::recompute(
                                &d.type_id,
                                &dctx,
                            )
                        else {
                            continue;
                        };
                        for (field, v) in layout
                            .members
                            .iter()
                            .skip(start_idx)
                            .take(d.words as usize)
                            .zip(values.iter())
                        {
                            let start = field.offset as usize;
                            let end = start + 4;
                            if end <= self.uniform_scratch.len() {
                                field.ty.write_to(
                                    &mut self.uniform_scratch[start..end],
                                    &ParamValue::Float(*v),
                                );
                            }
                        }
                    }
                }
            }
            Some(i) => {
                let v = &mut self.spec_variants[i];
                if let Some(layout) = &v.layout {
                    for byte in v.scratch.iter_mut() {
                        *byte = 0;
                    }
                    for m in &layout.members {
                        if m.name.starts_with("_pad") {
                            continue;
                        }
                        let f = ctx.scalar_or_param(&m.name, 0.0);
                        let start = m.offset as usize;
                        let end = start + 4;
                        if end <= v.scratch.len() {
                            m.ty.write_to(&mut v.scratch[start..end], &ParamValue::Float(f));
                        }
                    }
                }
            }
        }

        // Resolve dispatch geometry from the chosen dispatch port.
        let (dx, dy, dz) = match self.compute_dispatch(ctx) {
            Some(d) => d,
            None => {
                log::warn!(
                    "[node.wgsl_compute] no dispatch port resolved; skipping dispatch"
                );
                return;
            }
        };

        // Build the GpuBinding list in WGSL @binding order. We need
        // to materialise resource refs from ctx first, then collect
        // into the binding slice. Texture / buffer refs must outlive
        // the dispatch_compute call.
        let mut tex_refs: Vec<(u32, &manifold_gpu::GpuTexture)> = Vec::with_capacity(8);
        let mut buf_refs: Vec<(u32, &manifold_gpu::GpuBuffer)> = Vec::with_capacity(8);
        let mut sampler_refs: Vec<(u32, &GpuSampler)> = Vec::with_capacity(2);

        for slot in &self.bindings {
            match &slot.kind {
                BindingKind::Uniform => { /* handled below as Bytes */ }
                BindingKind::SampledTexture { port, is_3d } => {
                    let tex = if *is_3d {
                        ctx.inputs.texture_3d(port)
                    } else {
                        ctx.inputs.texture_2d(port)
                    };
                    let Some(tex) = tex else {
                        log::warn!(
                            "[node.wgsl_compute] required input texture '{port}' unwired"
                        );
                        return;
                    };
                    tex_refs.push((slot.binding, tex));
                }
                BindingKind::Sampler => {
                    let Some(s) = self.sampler.as_ref() else {
                        return;
                    };
                    sampler_refs.push((slot.binding, s));
                }
                BindingKind::StorageTexture { port, .. } => {
                    let Some(tex) = ctx.outputs.texture_2d(port) else {
                        log::warn!(
                            "[node.wgsl_compute] output texture '{port}' not allocated"
                        );
                        return;
                    };
                    tex_refs.push((slot.binding, tex));
                }
                BindingKind::StorageArrayRead { port, .. } => {
                    let Some(buf) = ctx.inputs.array(port) else {
                        log::warn!(
                            "[node.wgsl_compute] required input array '{port}' unwired"
                        );
                        return;
                    };
                    buf_refs.push((slot.binding, buf));
                }
                BindingKind::StorageArrayReadWrite { port, .. } => {
                    // For aliased in/out, the chain runtime routes
                    // both port slots to one physical buffer. We
                    // bind from the input side — the alias guarantees
                    // the output side points at the same memory.
                    let Some(buf) = ctx.inputs.array(port) else {
                        log::warn!(
                            "[node.wgsl_compute] aliased array '{port}' unwired"
                        );
                        return;
                    };
                    buf_refs.push((slot.binding, buf));
                }
                BindingKind::StorageAtomicAccumOut { port } => {
                    let Some(buf) = ctx.outputs.array(port) else {
                        log::warn!(
                            "[node.wgsl_compute] accum output '{port}' not allocated"
                        );
                        return;
                    };
                    buf_refs.push((slot.binding, buf));
                }
                BindingKind::StorageArrayWriteOut { port, .. } => {
                    // Fresh output-only array (buffer-fusion dst) — bind the
                    // loader-allocated output buffer; the kernel only writes it.
                    let Some(buf) = ctx.outputs.array(port) else {
                        log::warn!(
                            "[node.wgsl_compute] fused output array '{port}' not allocated"
                        );
                        return;
                    };
                    buf_refs.push((slot.binding, buf));
                }
            }
        }

        // Now assemble the GpuBinding slice. Uniform Bytes points into the active
        // variant's scratch (generic = self.uniform_scratch; specialized = the
        // variant's smaller scratch).
        let active_scratch: &[u8] = match spec_idx {
            None => &self.uniform_scratch,
            Some(i) => &self.spec_variants[i].scratch,
        };
        let mut gpu_bindings: Vec<GpuBinding> = Vec::with_capacity(self.bindings.len());
        for slot in &self.bindings {
            match &slot.kind {
                BindingKind::Uniform => {
                    gpu_bindings.push(GpuBinding::Bytes {
                        binding: slot.binding,
                        data: active_scratch,
                    });
                }
                BindingKind::SampledTexture { .. } | BindingKind::StorageTexture { .. } => {
                    let (_, tex) = tex_refs
                        .iter()
                        .find(|(b, _)| *b == slot.binding)
                        .expect("tex ref present");
                    gpu_bindings.push(GpuBinding::Texture {
                        binding: slot.binding,
                        texture: tex,
                    });
                }
                BindingKind::Sampler => {
                    let (_, samp) = sampler_refs
                        .iter()
                        .find(|(b, _)| *b == slot.binding)
                        .expect("sampler ref present");
                    gpu_bindings.push(GpuBinding::Sampler {
                        binding: slot.binding,
                        sampler: samp,
                    });
                }
                BindingKind::StorageArrayRead { .. }
                | BindingKind::StorageArrayReadWrite { .. }
                | BindingKind::StorageAtomicAccumOut { .. }
                | BindingKind::StorageArrayWriteOut { .. } => {
                    let (_, buf) = buf_refs
                        .iter()
                        .find(|(b, _)| *b == slot.binding)
                        .expect("buf ref present");
                    gpu_bindings.push(GpuBinding::Buffer {
                        binding: slot.binding,
                        buffer: buf,
                        offset: 0,
                    });
                }
            }
        }

        let pipeline = match spec_idx {
            None => self.pipeline.as_ref().expect("pipeline compiled above"),
            Some(i) => &self.spec_variants[i].pipeline,
        };
        let gpu = ctx.gpu_encoder();
        gpu.native_enc
            .dispatch_compute(pipeline, &gpu_bindings, [dx, dy, dz], TYPE_ID);
    }
}

impl WgslCompute {
    /// Choose the kernel variant to dispatch this frame. `None` → use the generic
    /// kernel (the permanent, always-correct fallback). `Some(0)` → dispatch
    /// `spec_variants[0]`, a kernel with the no-control-wire params baked to
    /// module-scope consts for the live value-set.
    ///
    /// Correctness is the value-key compare, never the install-time classification:
    /// the baked variant is used ONLY when every static field's live value matches
    /// the value-set it was baked with. A tweaked knob or a binding written after
    /// the build makes the values diverge → the generic kernel serves the new
    /// values immediately (no stale frame). Never blocks the render path: a key the
    /// node hasn't compiled serves generic, and a new variant is compiled only once
    /// a key has held [`SPEC_STABLE_FRAMES`] frames (so a swept slider compiles
    /// nothing, a settle pays one compile). The device-level pipeline cache makes a
    /// previously-seen value-set's compile near-instant.
    fn select_spec_variant(&mut self, ctx: &mut EffectNodeContext<'_, '_>) -> Option<usize> {
        // D7/P0: a kernel carrying derived-uniform members never specializes.
        // Baking a reduced variant would shift the surviving fields' byte
        // offsets (fewer dynamic fields → different padding), and
        // `derived_uniform_members`'s stored `offset`s are computed once from
        // the GENERIC layout — reusing them against a specialized variant's
        // scratch would write to the wrong bytes. Simple, always-correct:
        // these kernels just always ride the generic (permanent fallback) path.
        if !self.derived_uniform_members.is_empty() {
            return None;
        }
        if self.static_param_fields.is_empty() || !specialization_enabled() {
            return None;
        }
        // Texture kernels only. A buffer / particle kernel's uniform also carries
        // live element counts and derived frame values; keep specialization off it
        // entirely (belt-and-suspenders beyond install's texture-only markers).
        let texture_only = self.bindings.iter().all(|b| {
            matches!(
                b.kind,
                BindingKind::Uniform
                    | BindingKind::SampledTexture { .. }
                    | BindingKind::Sampler
                    | BindingKind::StorageTexture { .. }
            )
        });
        if !texture_only || self.dispatch_count_param.is_some() {
            return None;
        }

        // Live value + member type per static field → the const literal. The
        // literal vec IS the value-key (two frames with identical literals share a
        // variant). A non-finite value can't be a WGSL literal — bail to generic.
        let statics: Vec<(&str, UniformMemberType, f32)> = {
            let layout = self.uniform_layout.as_ref()?;
            let mut v = Vec::with_capacity(self.static_param_fields.len());
            for field in &self.static_param_fields {
                let Some(m) = layout.members.iter().find(|m| &m.name == field) else {
                    return None; // a marker named a field that isn't in the layout
                };
                let f = ctx.scalar_or_param(field, 0.0);
                if !f.is_finite() {
                    return None;
                }
                v.push((field.as_str(), m.ty, f));
            }
            v
        };
        let key: Vec<String> =
            statics.iter().map(|(n, ty, f)| spec_const_decl(n, *ty, *f)).collect();

        // Hit: dispatch the cached variant, move it to the LRU front.
        if let Some(pos) = self.spec_variants.iter().position(|v| v.key == key) {
            if pos != 0 {
                let v = self.spec_variants.remove(pos);
                self.spec_variants.insert(0, v);
            }
            self.last_spec_key = Some(key);
            return Some(0);
        }

        // Miss: stability gate — only compile once the key has held a frame, so a
        // sweep (key changes every frame) never triggers a one-shot compile.
        let stable = self.last_spec_key.as_ref() == Some(&key);
        self.spec_stable = if stable { self.spec_stable + 1 } else { 0 };
        self.last_spec_key = Some(key.clone());
        if self.spec_stable < SPEC_STABLE_FRAMES {
            return None;
        }

        // Compile the variant. Parse-guard first so a transform bug degrades to
        // generic instead of hitting create_compute_pipeline's panic-on-error.
        let spec_src = self.specialize_source(&statics)?;
        let entry = match naga::front::wgsl::parse_str(&spec_src) {
            Ok(m) => select_compute_entry(&m).ok().map(|e| e.name.clone())?,
            Err(e) => {
                log::warn!(
                    "[node.wgsl_compute] specialized kernel failed to parse, using generic: {e:?}"
                );
                return None;
            }
        };
        let spec_layout = introspect(&spec_src).ok()?.uniform_layout;
        let span = spec_layout.as_ref().map(|l| l.span as usize).unwrap_or(0);
        if std::env::var_os("MANIFOLD_WGSL_COMPUTE_TRACE").is_some() {
            eprintln!(
                "[wgsl_compute node={:?}] specialized {} static field(s): {:?}",
                ctx.node_id,
                statics.len(),
                key
            );
        }
        let pipeline =
            ctx.gpu_encoder()
                .device
                .create_compute_pipeline(&spec_src, &entry, TYPE_ID);
        self.spec_variants.insert(
            0,
            SpecVariant {
                key,
                layout: spec_layout,
                scratch: vec![0u8; span],
                pipeline,
            },
        );
        self.spec_variants.truncate(SPEC_CACHE_CAP);
        Some(0)
    }

    /// Build the specialized kernel for one static value-set: lift each
    /// `// @static_param` member out of `struct Params` into a module-scope `const`
    /// carrying its baked value, re-pad the struct, and rewrite `params.<field>` →
    /// `<field>` in the body. A pure textual transform of the generic kernel using
    /// values that equal the packed uniform bytes — so the kernel is arithmetically
    /// identical, only now the optimizer sees the values as constants and its
    /// CCP/DCE can strip the dead mode/quality branches. `statics` = (field, type,
    /// baked value) for every static member. `None` (→ generic) if the generic
    /// source has no `struct Params` to rewrite (never happens for a fused kernel).
    fn specialize_source(&self, statics: &[(&str, UniformMemberType, f32)]) -> Option<String> {
        let layout = self.uniform_layout.as_ref()?;
        let static_set: std::collections::HashSet<&str> =
            statics.iter().map(|(n, ..)| *n).collect();

        // Rebuild struct Params from the layout: keep dynamic (non-static, non-pad)
        // members verbatim, drop static + old pads, re-pad to a multiple of 4 fields
        // (matching the freeze codegen's 16-byte uniform alignment).
        let mut struct_body = String::new();
        let mut dyn_count = 0usize;
        for m in &layout.members {
            if m.name.starts_with("_pad") || static_set.contains(m.name.as_str()) {
                continue;
            }
            let ty = match m.ty {
                UniformMemberType::F32 => "f32",
                UniformMemberType::I32 => "i32",
                UniformMemberType::U32 | UniformMemberType::Bool => "u32",
            };
            struct_body.push_str(&format!("    {}: {ty},\n", m.name));
            dyn_count += 1;
        }
        if dyn_count == 0 {
            struct_body
                .push_str("    _pad0: u32,\n    _pad1: u32,\n    _pad2: u32,\n    _pad3: u32,\n");
        } else {
            for k in 0..((4 - (dyn_count % 4)) % 4) {
                struct_body.push_str(&format!("    _pad{k}: u32,\n"));
            }
        }

        let mut consts = String::new();
        for (name, ty, f) in statics {
            consts.push_str(&spec_const_decl(name, *ty, *f));
            consts.push('\n');
        }

        // Splice: replace the generic `struct Params { ... }` block with the const
        // block + the rebuilt struct. The block has no inner braces (only scalar
        // member lines), so the first `\n}\n` after the opener is its close.
        let src = &self.source;
        let start = src.find("struct Params {")?;
        let close = src[start..].find("\n}\n")? + start;
        let head = &src[..start];
        let tail = &src[close + 3..];
        let mut out = format!("{head}{consts}struct Params {{\n{struct_body}}}\n{tail}");
        for (name, ..) in statics {
            out = crate::node_graph::freeze::codegen::rename_ident(
                &out,
                &format!("params.{name}"),
                name,
            );
        }
        Some(out)
    }

    /// Resolve dispatch geometry from the dispatch port + workgroup
    /// size. For a texture output: dims / workgroup. For an array
    /// output: capacity / workgroup.x along X.
    fn compute_dispatch(&self, ctx: &EffectNodeContext<'_, '_>) -> Option<(u32, u32, u32)> {
        let port = self.dispatch_port.as_deref()?;
        let [wx, wy, wz] = self.workgroup_size;
        // Try texture output first.
        if let Some(tex) = ctx.outputs.texture_2d(port) {
            return Some((tex.width.div_ceil(wx), tex.height.div_ceil(wy.max(1)), 1));
        }
        // Then array output (atomic accum or particle in/out).
        if let Some(buf) = ctx.outputs.array(port) {
            // Look up the declared item_size for this port from our
            // introspected outputs list — the WGSL storage struct's
            // byte span. count = buf_bytes / item_size = number of
            // items the shader needs to process. The shader's
            // `arrayLength(&items)` returns the same value, so the
            // early-return at `i >= arrayLength(...)` lines up with
            // the dispatch geometry: no wasted workgroups, no missed
            // items. Falls back to the 4-byte-stride default if the
            // port type isn't an Array(...) somehow — defensive only.
            //
            // The earlier `buf.size() / 4` formula treated every
            // 4-byte slot as one work item, which dispatched 16×
            // more workgroups than needed for a 64-byte Particle
            // buffer (8M particles → 500K workgroups). That
            // exceeded Apple Silicon's per-dim threadgroup grid
            // limit (~64K-128K depending on family) and silently
            // dropped the dispatch — the FluidSim2D seed_pattern
            // bug manifested as "uniform updates fine, edges fire
            // fine, but the seed buffer never receives the
            // shader's writes."
            let item_size = self
                .outputs
                .iter()
                .find(|p| p.name == port)
                .and_then(|p| match p.ty {
                    PortType::Array(at) => Some(at.item_size),
                    _ => None,
                })
                .unwrap_or(4)
                .max(1);
            let mut count = (buf.size() as u32) / item_size;
            // `// @dispatch_count_param`: cap the grid at the live element
            // count (the fused particle integrators' `active_count`) — the
            // kernel guards the same bound, so threads past it are pure waste.
            // Missing/unwired param falls back to the capacity dispatch.
            if let Some(p) = &self.dispatch_count_param {
                let live = ctx.scalar_or_param(p, f32::MAX);
                if live.is_finite() {
                    // Floor of one group: a zero live count still dispatches one
                    // (immediately-guarded) group rather than a zero-dim grid.
                    count = count.min(live.max(0.0).round() as u32).max(1);
                }
            }
            return Some((count.div_ceil(wx), wy.max(1), wz.max(1)));
        }
        None
    }
}

fn hash_str(s: &str) -> u64 {
    let mut h = ahash::AHasher::default();
    s.hash(&mut h);
    h.finish()
}

// ─────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn input_names(node: &WgslCompute) -> Vec<&str> {
        node.inputs.iter().map(|i| i.name.as_ref()).collect()
    }

    // Built through `Marker::emit` rather than hand-typed, so this fixture stays
    // off the single-sourced marker grammar's negative gate.
    fn fragment_src() -> String {
        format!(
            "{}\n// @in: src\n// @param: scale = 0.75 [0, 2]\nfn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, scale: f32) -> vec4<f32> {{\n    return vec4<f32>(c.rgb * scale, c.a);\n}}\n",
            Marker::Fusion { kind: "pointwise".to_string() }.emit()
        )
    }

    #[test]
    fn fragment_form_introspects_and_reports_fusion_contract() {
        let fragment_src = fragment_src();
        let mut node = WgslCompute::new();
        node.set_wgsl_source(&fragment_src);
        assert!(!node.compile_failed, "fragment must synthesize + introspect");
        assert_eq!(node.fusion_kind(), FusionKind::Pointwise);
        let body = node.wgsl_body().expect("fragment exposes a body");
        assert!(body.contains("fn body"));
        // One texture input (`src`) + the `scale` param's port-shadow.
        assert!(node.inputs.iter().any(|i| i.name == "src" && i.ty == PortType::Texture2D));
        assert!(node.inputs.iter().any(|i| i.name == "scale"));
        // The output is renamed from the codegen's `dst` to the atom `out`.
        assert_eq!(node.outputs.len(), 1);
        assert_eq!(node.outputs[0].name, "out");
        assert_eq!(node.outputs[0].ty, PortType::Texture2D);
        // The author's declared default + range survive introspection.
        assert_eq!(node.params.len(), 1);
        assert_eq!(node.params[0].name, "scale");
        assert_eq!(node.params[0].default, ParamValue::Float(0.75));
        assert_eq!(node.params[0].range, Some((0.0, 2.0)));
        // wgsl_source() round-trips the AUTHORED fragment, not the kernel.
        assert_eq!(node.wgsl_source(), Some(fragment_src.as_str()));
    }

    /// BUG-012: a scalar `@param` author-named `tex_<x>` (a legal but unusual
    /// choice) used to collide with the texture-input rename convention —
    /// the fragment-form rename loop stripped a literal `tex_` prefix off
    /// EVERY input port name with no type filter, so `tex_speed` became
    /// `speed` on the port while the uniform layout + `self.params` stayed
    /// keyed `tex_speed`, and a wired LFO/Ableton control on that param
    /// silently went nowhere. The texture input (`src`) still renames
    /// (`tex_src` → `src` in codegen, but here the AUTHORED name is already
    /// `src`, so this asserts the port-shadow keeps its param name intact).
    #[test]
    fn fragment_scalar_param_named_tex_prefixed_is_not_stripped() {
        let src = format!(
            "{}\n// @in: src\n// @param: tex_speed = 1.0 [0, 4]\nfn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, tex_speed: f32) -> vec4<f32> {{\n    return vec4<f32>(c.rgb * tex_speed, c.a);\n}}\n",
            Marker::Fusion { kind: "pointwise".to_string() }.emit()
        );
        let mut node = WgslCompute::new();
        node.set_wgsl_source(&src);
        assert!(!node.compile_failed, "fragment must synthesize + introspect");
        // The texture input renamed normally (`tex_src` → `src`).
        assert!(node.inputs.iter().any(|i| i.name == "src" && i.ty == PortType::Texture2D));
        // The scalar param-shadow port keeps its full authored name — no
        // `tex_` stripped off a non-texture port.
        assert!(
            node.inputs.iter().any(|i| i.name == "tex_speed"),
            "scalar param-shadow port must not be renamed by the texture convention: {:?}",
            input_names(&node)
        );
        assert!(node.params.iter().any(|p| p.name == "tex_speed"));
    }

    /// BUG-010: a stray leftover `@compute fn` BEFORE the synthesized `cs_main`
    /// used to become `entry_points[0]` and be introspected AND dispatched in
    /// place of the real kernel — silently. `select_compute_entry` must pick
    /// `cs_main` by name; the workgroup size that lands proves which entry won.
    #[test]
    fn multi_entry_kernel_picks_cs_main_not_leading_entry() {
        let src = r#"
struct U { k: f32, };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var out_tex: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(8, 8)
fn debug_pass(@builtin(global_invocation_id) id: vec3<u32>) {
}
@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    _ = u.k;
    textureStore(out_tex, vec2<i32>(id.xy), vec4<f32>(1.0));
}
"#;
        let mut node = WgslCompute::new();
        node.set_wgsl_source(src);
        assert!(!node.compile_failed, "a multi-entry kernel with cs_main must introspect");
        assert_eq!(
            node.workgroup_size,
            [16, 16, 1],
            "cs_main's workgroup must win over the leading debug_pass entry (BUG-010)",
        );
    }

    /// BUG-010: two `@compute` entries with none named `cs_main` is ambiguous —
    /// the old code silently ran `entry_points[0]`; introspection must now fail,
    /// exactly as the module doc promises.
    #[test]
    fn multi_entry_kernel_without_cs_main_fails_validation() {
        let src = r#"
@group(0) @binding(0) var out_tex: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(8, 8)
fn pass_a(@builtin(global_invocation_id) id: vec3<u32>) {
    textureStore(out_tex, vec2<i32>(id.xy), vec4<f32>(0.0));
}
@compute @workgroup_size(16, 16)
fn pass_b(@builtin(global_invocation_id) id: vec3<u32>) {
    textureStore(out_tex, vec2<i32>(id.xy), vec4<f32>(1.0));
}
"#;
        let mut node = WgslCompute::new();
        node.set_wgsl_source(src);
        assert!(
            node.compile_failed,
            "two @compute entries and no cs_main is ambiguous — must fail validation (BUG-010)",
        );
    }

    /// BUG-011: a fused buffer kernel's `@fused_output` `dst` must be sized to the
    /// SMALLEST array input, not the largest. The kernel only writes
    /// `[0, min(arrayLength))` (the BUG-008 count guard), so `max` left an
    /// uninitialized tail that downstream instance renderers drew as ghosts.
    #[test]
    fn fused_output_capacity_is_min_of_inputs_not_max() {
        let src = r#"
struct P { a: vec4<f32>, b: vec4<f32>, };
struct Params { t: f32, };
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> src_0: array<P>;
@group(0) @binding(2) var<storage, read> src_1: array<P>;
// @fused_output
@group(0) @binding(3) var<storage, read_write> dst: array<P>;
@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= min(arrayLength(&src_0), arrayLength(&src_1)) { return; }
    _ = params.t;
    dst[idx] = src_0[idx];
}
"#;
        let mut node = WgslCompute::new();
        node.set_wgsl_source(src);
        assert!(!node.compile_failed, "fused-output kernel must introspect:\n{src}");
        let out_port = node
            .outputs
            .iter()
            .find(|o| matches!(o.ty, PortType::Array(_)))
            .expect("the @fused_output storage array is an Array output port")
            .name
            .clone();
        let cap = node.array_output_capacity(
            &out_port,
            &Default::default(),
            &[("src_0", 10), ("src_1", 4)],
        );
        assert_eq!(
            cap,
            Some(4),
            "dst sized to the smallest input (no never-written tail), not max (BUG-011)",
        );
    }

    #[test]
    fn full_kernel_stays_a_fusion_boundary() {
        let node = WgslCompute::new();
        assert_eq!(node.fusion_kind(), FusionKind::Boundary);
        assert!(node.wgsl_body().is_none());
        assert!(node.fragment_source.is_none());
    }

    #[test]
    fn default_source_introspects_to_one_uniform_one_texture_out() {
        let node = WgslCompute::new();
        assert!(!node.compile_failed, "default WGSL must parse");
        // The one uniform member `f0` port-shadows as an optional
        // ScalarF32 input — wire OR param drives it.
        assert_eq!(input_names(&node), vec!["f0"]);
        assert_eq!(node.inputs[0].ty, PortType::Scalar(ScalarType::F32));
        assert!(!node.inputs[0].required);
        assert_eq!(node.outputs.len(), 1);
        assert_eq!(node.outputs[0].name, "output_tex");
        assert_eq!(node.outputs[0].ty, PortType::Texture2D);
        assert_eq!(node.params.len(), 1);
        assert_eq!(node.params[0].name, "f0");
        assert_eq!(node.workgroup_size, [16, 16, 1]);
        assert_eq!(
            node.output_format("output_tex"),
            Some(GpuTextureFormat::Rgba16Float)
        );
    }

    #[test]
    fn reset_gated_marker_exposes_synthetic_reset_trigger_input() {
        // A `// @reset_gated` source gains an OPTIONAL `reset_trigger` input that
        // edge-gates the dispatch (the seed-pattern pattern: cheap to skip between
        // clip resets). Unmarked sources never get the port. Built through
        // `Marker::emit` so this fixture stays off the negative gate.
        let gated = format!(
            "{}\n\
             struct U {{ pattern: u32, }};\n\
             @group(0) @binding(0) var<uniform> u: U;\n\
             @group(0) @binding(1) var density: texture_storage_2d<r32float, write>;\n\
             @compute @workgroup_size(16, 16)\n\
             fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {{\n\
             _ = u.pattern;\n\
             textureStore(density, vec2<i32>(id.xy), vec4<f32>(1.0));\n\
             }}\n",
            Marker::ResetGated.emit()
        );
        let mut node = WgslCompute::new();
        node.set_wgsl_source(&gated);
        assert!(!node.compile_failed, "gated shader must parse");
        assert!(node.reset_gated, "marker sets reset_gated");
        let rt = node
            .inputs
            .iter()
            .find(|p| p.name == "reset_trigger")
            .expect("synthetic reset_trigger input present");
        assert_eq!(rt.ty, PortType::Scalar(ScalarType::F32));
        assert!(!rt.required, "reset_trigger is optional (unwired ⇒ every frame)");

        // Same shader without the marker: no synthetic port, runs every frame.
        let ungated = gated.replace(&format!("{}\n", Marker::ResetGated.emit()), "");
        let mut plain = WgslCompute::new();
        plain.set_wgsl_source(&ungated);
        assert!(!plain.reset_gated);
        assert!(
            !plain.inputs.iter().any(|p| p.name == "reset_trigger"),
            "no marker ⇒ no reset_trigger port"
        );

        // `@pure` (same own-line marker convention): author-asserted
        // memoizable kernel. Marked ⇒ is_pure; unmarked ⇒ not.
        let pure_src = format!("{}\n{ungated}", Marker::Pure.emit());
        let mut pure_node = WgslCompute::new();
        pure_node.set_wgsl_source(&pure_src);
        assert!(!pure_node.compile_failed, "pure shader must parse");
        assert!(EffectNode::is_pure(&pure_node), "@pure marker sets is_pure");
        assert!(!EffectNode::is_pure(&plain), "no marker ⇒ not pure");
    }

    #[test]
    fn blackhole_deflection_shape() {
        // 0-input → 3 storage texture outputs at quarter-res. The
        // shape the static wgsl_compute family cannot express.
        let src = r#"
struct U { cam_dist: f32, tilt: f32, spin: f32, };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var defl_a: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var defl_b: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var sky_dir: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    _ = u.cam_dist; _ = u.tilt; _ = u.spin;
    textureStore(defl_a, vec2<i32>(id.xy), vec4<f32>(0.0));
    textureStore(defl_b, vec2<i32>(id.xy), vec4<f32>(0.0));
    textureStore(sky_dir, vec2<i32>(id.xy), vec4<f32>(0.0));
}
"#;
        let mut node = WgslCompute::new();
        node.set_wgsl_source(src);
        assert!(!node.compile_failed);
        // Uniform members port-shadow as 3 optional ScalarF32 inputs;
        // no texture inputs.
        assert_eq!(input_names(&node), vec!["cam_dist", "tilt", "spin"]);
        for inp in &node.inputs {
            assert_eq!(inp.ty, PortType::Scalar(ScalarType::F32));
            assert!(!inp.required);
        }
        assert_eq!(node.outputs.len(), 3);
        assert_eq!(node.outputs[0].name, "defl_a");
        assert_eq!(node.outputs[1].name, "defl_b");
        assert_eq!(node.outputs[2].name, "sky_dir");
        assert_eq!(node.params.len(), 3);
        assert!(node.aliased_array_io().is_empty());
        assert_eq!(node.dispatch_port.as_deref(), Some("defl_a"));
    }

    #[test]
    fn particle_integrator_shape_aliases_in_out() {
        let src = r#"
struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};
struct U { dt: f32, };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;
@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= arrayLength(&particles) { return; }
    let p = particles[gid.x];
    particles[gid.x].life = p.life + u.dt;
}
"#;
        let mut node = WgslCompute::new();
        node.set_wgsl_source(src);
        assert!(!node.compile_failed);
        // Inputs = [uniform port-shadow scalar "dt", aliased
        // Array(Anonymous, size=64) "particles" (read_write storage
        // maps to both an input and output port of the same name).
        //
        // Phase 4a: naga walks the Particle struct and emits a typed
        // Channels signature derived from the WGSL Particle struct's
        // fields via naga.
        assert_eq!(input_names(&node), vec!["dt", "particles"]);
        assert_eq!(node.inputs[0].ty, PortType::Scalar(ScalarType::F32));
        match node.inputs[1].ty {
            PortType::Array(at) => {
                assert_eq!(at.item_size, 64);
                assert_eq!(at.item_align, 4);
                assert_eq!(at.match_mode, crate::node_graph::ports::MatchMode::Exact);
                // naga-derived Channels signature: vec3 position +
                // vec3 velocity + f32 life + f32 age + vec4 color.
                assert_eq!(at.specs.len(), 5);
                let names: Vec<&'static str> = at
                    .specs
                    .iter()
                    .map(|s| s.name.debug_name().unwrap_or("<unknown>"))
                    .collect();
                assert_eq!(
                    names,
                    vec!["position", "velocity", "life", "age", "color"]
                );
                use crate::node_graph::ports::ChannelElementType as CET;
                let types: Vec<CET> = at.specs.iter().map(|s| s.ty).collect();
                assert_eq!(types, vec![CET::Vec3F, CET::Vec3F, CET::F32, CET::F32, CET::Vec4F]);
            }
            _ => panic!("expected Array port"),
        }
        assert_eq!(node.outputs.len(), 1);
        assert_eq!(node.outputs[0].name, "particles");
        let aliased = node.aliased_array_io();
        assert_eq!(aliased.len(), 1);
        assert_eq!(aliased[0], ("particles", "particles"));
        assert_eq!(node.workgroup_size, [256, 1, 1]);
    }

    #[test]
    fn polar_splat_shape_array_in_plus_two_atomic_accums() {
        let src = r#"
struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};
struct U { disk_inner: f32, disk_outer: f32, };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read> particles: array<Particle>;
@group(0) @binding(2) var<storage, read_write> accum_top: array<atomic<u32>>;
@group(0) @binding(3) var<storage, read_write> accum_bot: array<atomic<u32>>;
@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= arrayLength(&particles) { return; }
    let p = particles[gid.x];
    if p.position.y >= 0.0 {
        atomicAdd(&accum_top[gid.x], u32(u.disk_inner));
    } else {
        atomicAdd(&accum_bot[gid.x], u32(u.disk_outer));
    }
}
"#;
        let mut node = WgslCompute::new();
        node.set_wgsl_source(src);
        assert!(!node.compile_failed);
        // Inputs: 2 uniform port-shadow scalars (disk_inner, disk_outer)
        // + 1 read-only Array<Particle>.
        assert_eq!(
            input_names(&node),
            vec!["disk_inner", "disk_outer", "particles"]
        );
        assert_eq!(node.outputs.len(), 2);
        assert_eq!(node.outputs[0].name, "accum_top");
        assert_eq!(node.outputs[1].name, "accum_bot");
        // Atomic-u32 outputs carry the single-channel [value: U32]
        // signature matching u32's KnownItem impl, so downstream
        // typed consumers (resolve_accumulator etc.) wire directly
        // without a cast atom in between.
        match node.outputs[0].ty {
            PortType::Array(at) => {
                assert_eq!(at.item_size, 4);
                assert_eq!(at.item_align, 4);
                assert_eq!(at.specs.len(), 1);
                assert_eq!(at.specs[0].name.debug_name(), Some("value"));
                use crate::node_graph::ports::ChannelElementType as CET;
                assert_eq!(at.specs[0].ty, CET::U32);
                assert_eq!(at.match_mode, crate::node_graph::ports::MatchMode::Exact);
            }
            _ => panic!("expected Array port"),
        }
        assert!(node.aliased_array_io().is_empty());
        // No texture output; dispatch port falls back to the first
        // array output.
        assert_eq!(node.dispatch_port.as_deref(), Some("accum_top"));
    }

    // ─────────────────────────────────────────────────────────────────
    // `// @channel_skip` preprocessor unit tests
    // ─────────────────────────────────────────────────────────────────

    fn skip(src: &str) -> AHashMap<String, AHashSet<String>> {
        extract_channel_skip(src)
    }

    fn fields(map: &AHashMap<String, AHashSet<String>>, struct_name: &str) -> Vec<String> {
        let mut v: Vec<String> = map
            .get(struct_name)
            .into_iter()
            .flat_map(|s| s.iter().cloned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn skip_marker_on_preceding_line() {
        let src = r#"
struct Particle {
    position: vec3<f32>,
    // @channel_skip
    padding: f32,
    velocity: vec3<f32>,
};
"#;
        let m = skip(src);
        assert_eq!(fields(&m, "Particle"), vec!["padding"]);
    }

    #[test]
    fn skip_marker_with_leading_whitespace() {
        let src = "
struct X {
\t\t// @channel_skip
\t\tfoo: f32,
};
";
        let m = skip(src);
        assert_eq!(fields(&m, "X"), vec!["foo"]);
    }

    #[test]
    fn skip_marker_with_whitespace_variations() {
        // `//@channel_skip`, `//   @channel_skip`, trailing whitespace.
        let src = "
struct X {
    //@channel_skip
    a: f32,
    //   @channel_skip
    b: f32,
    // @channel_skip
    c: f32,
    d: f32,
};
";
        let m = skip(src);
        assert_eq!(fields(&m, "X"), vec!["a", "b", "c"]);
    }

    #[test]
    fn marker_with_trailing_text_is_rejected() {
        // Strict v1: marker must be the bare `@channel_skip` after trim.
        // Trailing prose like `— reason` disqualifies the marker.
        let src = "
struct X {
    // @channel_skip — reason
    a: f32,
};
";
        let m = skip(src);
        assert!(m.is_empty(), "marker with trailing text should not skip");
    }

    #[test]
    fn multi_line_struct_declaration() {
        // Brace on its own line, plus a field on the same line as the `{`.
        let src = "
struct X
{
    // @channel_skip
    a: f32,
    b: f32,
};
";
        let m = skip(src);
        assert_eq!(fields(&m, "X"), vec!["a"]);
    }

    #[test]
    fn mixed_line_and_block_comments() {
        // Block comments are stripped before the line-comment scan, so
        // `// @channel_skip` inside a `/* ... */` block does NOT count.
        let src = "
struct X {
    /* // @channel_skip */
    a: f32,
    // @channel_skip
    /* explanatory block comment */
    b: f32,
    c: f32,
};
";
        let m = skip(src);
        assert_eq!(fields(&m, "X"), vec!["b"]);
    }

    #[test]
    fn two_structs_dont_share_skip_set() {
        let src = r#"
struct A {
    // @channel_skip
    pad_a: f32,
    real_a: f32,
};
struct B {
    real_b: f32,
    // @channel_skip
    pad_b: f32,
};
"#;
        let m = skip(src);
        assert_eq!(fields(&m, "A"), vec!["pad_a"]);
        assert_eq!(fields(&m, "B"), vec!["pad_b"]);
    }

    #[test]
    fn whitespace_variation_around_marker_token() {
        // `// @channel_skip`, `// @channel_skip\t`, `// @channel_skip   `.
        let src = "
struct X {
    // @channel_skip\t
    a: f32,
};
";
        let m = skip(src);
        assert_eq!(fields(&m, "X"), vec!["a"]);
    }

    #[test]
    fn same_line_marker_is_ignored() {
        let src = "
struct X {
    a: f32, // @channel_skip
    b: f32,
};
";
        let m = skip(src);
        assert!(
            m.is_empty(),
            "same-line markers must NOT apply to the field they trail"
        );
    }

    #[test]
    fn stacked_markers_all_apply_to_next_field_idempotently() {
        // Three stacked markers, then a single field — only that field
        // is skipped. The markers don't queue up to skip the next 3.
        let src = "
struct X {
    // @channel_skip
    // @channel_skip
    // @channel_skip
    only_this: f32,
    not_this: f32,
    not_this_either: f32,
};
";
        let m = skip(src);
        assert_eq!(fields(&m, "X"), vec!["only_this"]);
    }

    #[test]
    fn marker_with_intervening_comment_lines_still_applies() {
        let src = "
struct X {
    // @channel_skip
    // descriptive comment in between
    // another descriptive comment
    target: f32,
    other: f32,
};
";
        let m = skip(src);
        assert_eq!(fields(&m, "X"), vec!["target"]);
    }

    #[test]
    fn marker_outside_any_struct_is_ignored() {
        // No panic; no entry created.
        let src = "
// @channel_skip
const X: f32 = 1.0;

struct Y {
    a: f32,
};
";
        let m = skip(src);
        assert!(m.is_empty());
    }

    #[test]
    fn marker_with_no_following_field_in_struct_is_ignored() {
        let src = "
struct X {
    a: f32,
    // @channel_skip
};
";
        let m = skip(src);
        assert!(m.is_empty());
    }

    #[test]
    fn field_with_attribute_annotation_still_parses() {
        // `@align(16)` attributes before the field name don't confuse
        // the marker→field association.
        let src = "
struct X {
    // @channel_skip
    @align(16) padding: vec3<f32>,
    real: f32,
};
";
        let m = skip(src);
        assert_eq!(fields(&m, "X"), vec!["padding"]);
    }

    #[test]
    fn empty_source_returns_empty_map() {
        let m = skip("");
        assert!(m.is_empty());
    }

    #[test]
    fn no_markers_returns_empty_map() {
        let src = "
struct X {
    a: f32,
    b: f32,
};
";
        let m = skip(src);
        assert!(m.is_empty());
    }

    #[test]
    fn marker_skipped_field_can_have_pad_name() {
        // Author named a non-padding field `padding`; the marker rescues
        // it from the future where the heuristic would have eaten it.
        // (Since the heuristic is gone, this just confirms the marker
        // works on arbitrary names.)
        let src = "
struct X {
    // @channel_skip
    padding: f32,
    real: f32,
};
";
        let m = skip(src);
        assert_eq!(fields(&m, "X"), vec!["padding"]);
    }

    // ─────────────────────────────────────────────────────────────────
    // End-to-end integration of `// @channel_skip` through naga walk
    // ─────────────────────────────────────────────────────────────────

    #[test]
    fn channel_skip_marker_drops_padding_field_from_channels_signature() {
        // A struct with an explicit padding field named `padding` (no
        // `_pad` prefix) — the legacy heuristic would have missed this
        // field, leaving it in the wire. The marker rescues it.
        let src = r#"
struct Particle {
    position: vec3<f32>,
    // @channel_skip
    padding: f32,
    velocity: vec3<f32>,
    life:     f32,
    age:      f32,
    color:    vec4<f32>,
};
struct U { dt: f32, };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;
@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= arrayLength(&particles) { return; }
    particles[gid.x].life = particles[gid.x].life + u.dt;
}
"#;
        let mut node = WgslCompute::new();
        node.set_wgsl_source(src);
        assert!(!node.compile_failed, "shader must parse");
        let array_port = node
            .inputs
            .iter()
            .find(|i| i.name == "particles")
            .expect("particles input port");
        match array_port.ty {
            PortType::Array(at) => {
                let names: Vec<&'static str> = at
                    .specs
                    .iter()
                    .map(|s| s.name.debug_name().unwrap_or("<unknown>"))
                    .collect();
                // `padding` must be absent. Order preserves WGSL order
                // minus the skipped field.
                assert_eq!(
                    names,
                    vec!["position", "velocity", "life", "age", "color"],
                    "channel_skip marker did not drop `padding` from Channels signature"
                );
            }
            _ => panic!("expected Array port"),
        }
    }

    #[test]
    fn pad_prefixed_field_without_marker_is_kept_after_heuristic_drop() {
        // Counterpoint: with the legacy `_pad*` heuristic retired, a
        // field named `_pad0` is now emitted as a channel unless the
        // author marks it. This is the new contract.
        let src = r#"
struct S {
    position: vec3<f32>,
    _pad0:    f32,
};
@group(0) @binding(0) var<storage, read> items: array<S>;
@compute @workgroup_size(64)
fn cs_main() { _ = items[0].position; }
"#;
        let mut node = WgslCompute::new();
        node.set_wgsl_source(src);
        assert!(!node.compile_failed);
        let port = node
            .inputs
            .iter()
            .find(|i| i.name == "items")
            .expect("items input");
        match port.ty {
            PortType::Array(at) => {
                // Both fields show up — the heuristic is gone. `position`
                // resolves through `well_known::POSITION`; `_pad0` is a
                // runtime-introduced name whose `debug_name` falls back
                // to None.
                use crate::node_graph::ports::{ChannelElementType as CET, ChannelName};
                assert_eq!(at.specs.len(), 2);
                assert_eq!(at.specs[0].name, ChannelName::from_str("position"));
                assert_eq!(at.specs[0].ty, CET::Vec3F);
                assert_eq!(at.specs[1].name, ChannelName::from_str("_pad0"));
                assert_eq!(at.specs[1].ty, CET::F32);
            }
            _ => panic!("expected Array port"),
        }
    }

    #[test]
    fn marker_isolation_between_two_storage_structs() {
        // Two storage structs in the same shader; markers in one must
        // not leak to the other.
        let src = r#"
struct A {
    // @channel_skip
    age:      f32,
    life:     f32,
};
struct B {
    age:      f32,
    life:     f32,
};
@group(0) @binding(0) var<storage, read> as_in: array<A>;
@group(0) @binding(1) var<storage, read> bs_in: array<B>;
@compute @workgroup_size(64)
fn cs_main() { _ = as_in[0].life + bs_in[0].life; }
"#;
        let mut node = WgslCompute::new();
        node.set_wgsl_source(src);
        assert!(!node.compile_failed);
        let names_of = |port_name: &str| -> Vec<&'static str> {
            let p = node
                .inputs
                .iter()
                .find(|i| i.name == port_name)
                .expect("input");
            match p.ty {
                PortType::Array(at) => at
                    .specs
                    .iter()
                    .map(|s| s.name.debug_name().unwrap_or("<unknown>"))
                    .collect(),
                _ => panic!("expected Array"),
            }
        };
        // A drops `age`; B keeps it (its skip set is empty).
        assert_eq!(names_of("as_in"), vec!["life"]);
        assert_eq!(names_of("bs_in"), vec!["age", "life"]);
    }

    #[test]
    fn malformed_wgsl_marks_compile_failed_and_keeps_prior_shape() {
        let mut node = WgslCompute::new();
        let prior_outputs = node.outputs.len();
        node.set_wgsl_source("this is not wgsl");
        assert!(node.compile_failed);
        // Previous shape is retained — the chain keeps working on
        // last-known-good ports until valid WGSL lands.
        assert_eq!(node.outputs.len(), prior_outputs);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    //! End-to-end GPU smoke for the dynamic node. Confirms the
    //! introspection-derived port shape actually flows through chain
    //! compile → pre-allocation → executor dispatch → texture
    //! readback. Validation is the simplest possible: the default
    //! kernel writes a solid 50% grey, so every output pixel must
    //! be approximately `(0.5, 0.5, 0.5, 1.0)`.

    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::Slot;
    use crate::node_graph::freeze::markers::Marker;
    use crate::node_graph::{
        FinalOutput, FrameTime, Graph, MetalBackend, Executor, compile,
    };

    use super::WgslCompute;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    #[test]
    fn default_kernel_dispatches_and_writes_grey_to_output() {
        let device = crate::test_device();
        let (w, h) = (32u32, 32u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut g = Graph::new();
        let comp = g.add_node(Box::new(WgslCompute::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((comp, "output_tex"), (out, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let backend = MetalBackend::new(device.arc(), w, h, format);
        let out_slot = Slot(backend.slot_count());
        let mut exec = Executor::new(Box::new(backend));
        let mut native_enc = device.create_encoder("wgsl-compute-smoke");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
        }
        native_enc.commit_and_wait_completed();

        let out_tex = exec
            .backend()
            .texture_2d(out_slot)
            .expect("final output texture retained");
        let bytes_per_row = w * 8;
        let readback = device.create_buffer_shared(u64::from(h * bytes_per_row));
        let mut readback_enc = device.create_encoder("wgsl-compute-readback");
        readback_enc.copy_texture_to_buffer(out_tex, &readback, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback.mapped_ptr().expect("shared buffer pointer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

        let tol = 0.01;
        for i in 0..(w * h) as usize {
            let o = i * 4;
            let r = f16::from_bits(halves[o]).to_f32();
            let g = f16::from_bits(halves[o + 1]).to_f32();
            let b = f16::from_bits(halves[o + 2]).to_f32();
            let a = f16::from_bits(halves[o + 3]).to_f32();
            assert!(
                (r - 0.5).abs() < tol
                    && (g - 0.5).abs() < tol
                    && (b - 0.5).abs() < tol
                    && (a - 1.0).abs() < tol,
                "pixel {i}: expected ~(0.5,0.5,0.5,1.0), got ({r},{g},{b},{a})"
            );
        }
    }

    // ── Static-param specialization: checkpoint (rule 4) ─────────────────
    // A fused-shaped texture kernel rendered through the GENERIC kernel (params as
    // uniforms) vs the SPECIALIZED kernel (static params baked to module consts)
    // must be BIT-exact — not within a tolerance. Baking a uniform read as a const
    // with the identical value cannot change the arithmetic; if any texel differs,
    // the baking is wrong. The kernel below exercises an f32 multiply, an i32
    // mode-branch (the kind of dead branch spirv-opt's CCP/DCE strips once the
    // selector is a const), and a SURVIVING dynamic param so the shrunk uniform
    // path is covered too.
    // Built through `Marker::emit` (the two `@static_param` lines) rather than
    // hand-typed, so this fixture stays off the negative gate.
    fn spec_kernel() -> String {
        format!(
            "{}\n{}\n\
struct Params {{
    n0_k: f32,
    n0_mode: i32,
    n0_gain: f32,
    _pad0: u32,
}}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var src_0: texture_2d<f32>;
@group(0) @binding(2) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {{
    let dims = textureDimensions(dst);
    if id.x >= dims.x || id.y >= dims.y {{ return; }}
    let coord = vec2<i32>(i32(id.x), i32(id.y));
    let c = textureLoad(src_0, coord, 0);
    var r: vec4<f32>;
    if (params.n0_mode == 0) {{
        r = c * params.n0_k;
    }} else {{
        r = vec4<f32>(1.0) - c * params.n0_k;
    }}
    r = r * params.n0_gain;
    textureStore(dst, coord, r);
}}
",
            Marker::StaticParam { field: "n0_k".to_string() }.emit(),
            Marker::StaticParam { field: "n0_mode".to_string() }.emit(),
        )
    }

    fn spec_gradient(device: &manifold_gpu::GpuDevice, w: u32, h: u32) -> manifold_gpu::GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = f16::from_f32(x as f32 / w as f32);
                px[i + 1] = f16::from_f32(y as f32 / h as f32);
                px[i + 2] = f16::from_f32(0.37);
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        let tex = device.create_texture(&manifold_gpu::GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: manifold_gpu::GpuTextureDimension::D2,
            usage: manifold_gpu::GpuTextureUsage::CPU_UPLOAD
                | manifold_gpu::GpuTextureUsage::SHADER_READ
                | manifold_gpu::GpuTextureUsage::COPY_SRC,
            label: "spec-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(
                px.as_ptr().cast::<u8>(),
                std::mem::size_of_val(px.as_slice()),
            )
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// Pack a uniform scratch from a source's introspected layout (named float
    /// values; unspecified members stay zero) — mirrors evaluate()'s packing.
    fn spec_pack(source: &str, values: &[(&str, f32)]) -> Vec<u8> {
        let layout = super::introspect(source).unwrap().uniform_layout.unwrap();
        let mut scratch = vec![0u8; layout.span as usize];
        for m in &layout.members {
            if let Some((_, f)) = values.iter().find(|(n, _)| *n == m.name) {
                let s = m.offset as usize;
                m.ty.write_to(&mut scratch[s..s + 4], &super::ParamValue::Float(*f));
            }
        }
        scratch
    }

    fn spec_dispatch(
        device: &manifold_gpu::GpuDevice,
        wgsl: &str,
        src: &manifold_gpu::GpuTexture,
        uniform: &[u8],
    ) -> Vec<u16> {
        use manifold_gpu::GpuBinding;
        let (w, h) = (src.width, src.height);
        let pipeline = device.create_compute_pipeline(wgsl, "cs_main", "spec-test");
        let out = crate::render_target::RenderTarget::new(
            device,
            w,
            h,
            GpuTextureFormat::Rgba16Float,
            "spec-out",
        );
        let mut enc = device.create_encoder("spec-test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: uniform },
                GpuBinding::Texture { binding: 1, texture: src },
                GpuBinding::Texture { binding: 2, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "spec-test",
        );
        enc.commit_and_wait_completed();
        let bytes_per_row = w * 8;
        let readback = device.create_buffer_shared(u64::from(h * bytes_per_row));
        let mut rb = device.create_encoder("spec-readback");
        rb.copy_texture_to_buffer(&out.texture, &readback, w, h, bytes_per_row);
        rb.commit_and_wait_completed();
        let ptr = readback.mapped_ptr().expect("shared buffer pointer");
        unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) }.to_vec()
    }

    #[test]
    fn specialized_kernel_is_bit_exact_with_generic() {
        let device = crate::test_device();
        let (w, h) = (64u32, 64u32);

        // Parse the kernel through the node so static_param_fields + uniform_layout
        // come from the real introspection path.
        let mut node = WgslCompute::new();
        node.reparse(spec_kernel());
        assert_eq!(
            node.static_param_fields,
            vec!["n0_k".to_string(), "n0_mode".to_string()],
            "markers must parse to the two static fields"
        );

        let (k, mode, gain) = (0.75_f32, 1.0_f32, 1.3_f32);
        let statics: Vec<(&str, super::UniformMemberType, f32)> = vec![
            ("n0_k", super::UniformMemberType::F32, k),
            ("n0_mode", super::UniformMemberType::I32, mode),
        ];
        let spec_src = node.specialize_source(&statics).expect("specialize");
        // The specialized kernel must not carry the static fields in its uniform
        // struct (the `    name: ty,` member form), must rewrite their reads, and
        // must declare them as module consts.
        assert!(!spec_src.contains("    n0_k: f32,"), "n0_k must leave the struct");
        assert!(!spec_src.contains("    n0_mode: i32,"), "n0_mode must leave the struct");
        assert!(!spec_src.contains("params.n0_k"), "params.n0_k must be rewritten");
        assert!(!spec_src.contains("params.n0_mode"), "params.n0_mode must be rewritten");
        assert!(spec_src.contains("    n0_gain: f32,"), "dynamic n0_gain stays in struct");
        assert!(spec_src.contains("params.n0_gain"), "dynamic n0_gain stays a uniform read");
        assert!(spec_src.contains("const n0_k: f32 = 0.75"));
        assert!(spec_src.contains("const n0_mode: i32 = 1"));

        let src = spec_gradient(&device, w, h);
        let generic = spec_dispatch(
            &device,
            &node.source,
            &src,
            &spec_pack(&node.source, &[("n0_k", k), ("n0_mode", mode), ("n0_gain", gain)]),
        );
        let specialized =
            spec_dispatch(&device, &spec_src, &src, &spec_pack(&spec_src, &[("n0_gain", gain)]));

        let differing = generic
            .iter()
            .zip(specialized.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(
            differing, 0,
            "generic vs specialized must be bit-exact, {differing} of {} halves differ",
            generic.len()
        );
    }

    // ── Static-param specialization: checkpoint (rule 6, knob-tweak) ──────
    // A baked param changed mid-run must reflect on the VERY NEXT frame through the
    // generic fallback — no stale frame, no black frame — while the node settles
    // and recompiles a fresh variant. Drives a Source kernel (output = n0_k*n0_gain)
    // through the real chain runtime so the node persists its spec cache across
    // frames, exactly as on stage.
    // Built through `Marker::emit`, same convention as `spec_kernel` above.
    fn spec_source_kernel() -> String {
        format!(
            "{}\n\
struct Params {{
    n0_k: f32,
    n0_gain: f32,
    _pad0: u32,
    _pad1: u32,
}}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {{
    let dims = textureDimensions(dst);
    if id.x >= dims.x || id.y >= dims.y {{ return; }}
    let v = params.n0_k * params.n0_gain;
    textureStore(dst, vec2<i32>(i32(id.x), i32(id.y)), vec4<f32>(v, v, v, 1.0));
}}
",
            Marker::StaticParam { field: "n0_k".to_string() }.emit(),
        )
    }

    #[test]
    fn knob_tweak_reflects_next_frame_via_generic_fallback() {
        use super::ParamValue;
        use crate::node_graph::bindings::Slot;

        let device = crate::test_device();
        let (w, h) = (32u32, 32u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut node = WgslCompute::new();
        node.reparse(spec_source_kernel());
        assert_eq!(node.static_param_fields, vec!["n0_k".to_string()]);

        let mut g = Graph::new();
        let comp = g.add_node(Box::new(node));
        let out = g.add_node(Box::new(FinalOutput::new()));
        g.connect((comp, "dst"), (out, "in")).unwrap();
        g.set_param(comp, "n0_gain", ParamValue::Float(1.0)).unwrap();
        g.set_param(comp, "n0_k", ParamValue::Float(0.5)).unwrap();
        let plan = compile(&g).unwrap();

        let backend = MetalBackend::new(device.arc(), w, h, format);
        let out_slot = Slot(backend.slot_count());
        let mut exec = Executor::new(Box::new(backend));

        let render = |exec: &mut Executor, g: &mut Graph| -> f32 {
            let mut native_enc = device.create_encoder("spec-knob");
            {
                let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
                exec.execute_frame_with_gpu(g, &plan, frame_time(), &mut gpu);
            }
            native_enc.commit_and_wait_completed();
            let tex = exec.backend().texture_2d(out_slot).expect("output retained");
            let bytes_per_row = w * 8;
            let rb = device.create_buffer_shared(u64::from(h * bytes_per_row));
            let mut e = device.create_encoder("spec-knob-rb");
            e.copy_texture_to_buffer(tex, &rb, w, h, bytes_per_row);
            e.commit_and_wait_completed();
            let ptr = rb.mapped_ptr().expect("ptr");
            let center = ((h / 2) * w + w / 2) as usize * 4;
            let halves = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
            f16::from_bits(halves[center]).to_f32()
        };

        // Frames 0..2 settle and specialize on n0_k = 0.5.
        let f0 = render(&mut exec, &mut g);
        let f1 = render(&mut exec, &mut g);
        assert!((f0 - 0.5).abs() < 0.01, "frame 0 should be 0.5, got {f0}");
        assert!((f1 - 0.5).abs() < 0.01, "frame 1 (specialized) should be 0.5, got {f1}");

        // Tweak the baked knob. The VERY NEXT frame must reflect 0.8 (generic
        // serves the live value while the new variant is still uncompiled) — not a
        // stale 0.5, not a black 0.0.
        g.set_param(comp, "n0_k", ParamValue::Float(0.8)).unwrap();
        let f2 = render(&mut exec, &mut g);
        assert!(
            (f2 - 0.8).abs() < 0.01,
            "frame after tweak must reflect 0.8 immediately (no stale/black), got {f2}"
        );

        // It settles again and recompiles a variant for 0.8 — output unchanged.
        let f3 = render(&mut exec, &mut g);
        assert!((f3 - 0.8).abs() < 0.01, "frame 3 (re-specialized) should be 0.8, got {f3}");
    }
}
