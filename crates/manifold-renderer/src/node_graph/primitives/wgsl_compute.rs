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
//! atomics, multiple entry points, group != 0) fail validation with a
//! warning and leave the pipeline empty.

#![allow(private_interfaces)]

use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

use ahash::AHashMap;
use manifold_gpu::{GpuBinding, GpuComputePipeline, GpuSampler, GpuTextureFormat};

use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, NodeRequires,
};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::ports::{
    ArrayType, ChannelElementType, ChannelSpec, ItemKind, NodeInput, NodeOutput, NodePort,
    PortKind, PortType, ScalarType,
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
    source: String,

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
    /// `node.scatter_particles` uses for its `accum` port. Returned
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
}

#[derive(Clone, Debug)]
struct BindingSlot {
    binding: u32,
    kind: BindingKind,
}

#[derive(Clone, Debug)]
enum BindingKind {
    Uniform,
    SampledTexture { port: String },
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
            pipeline: None,
            sampler: None,
            compiled_hash: None,
            compile_failed: false,
            uniform_scratch: Vec::new(),
            last_logged_uniforms: AHashMap::new(),
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
        self.source = source;
        self.pipeline = None;
        self.compiled_hash = None;

        let parsed = match introspect(&self.source) {
            Ok(p) => p,
            Err(msg) => {
                log::warn!("[node.wgsl_compute] introspection failed: {msg}");
                self.compile_failed = true;
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
        // allocation, matching node.scatter_particles' convention. The
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
}

fn introspect(source: &str) -> Result<ParsedShader, String> {
    let module = naga::front::wgsl::parse_str(source).map_err(|e| e.emit_to_string(source))?;

    if module.entry_points.is_empty() {
        return Err("no entry points".into());
    }
    let ep = &module.entry_points[0];
    if ep.stage != naga::ShaderStage::Compute {
        return Err(format!("entry point '{}' is not @compute", ep.name));
    }
    let workgroup_size = ep.workgroup_size;

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
                    if !matches!(dim, naga::ImageDimension::D2) {
                        return Err(format!(
                            "texture '{name}' is not 2D (only Texture2D supported)"
                        ));
                    }
                    match class {
                        naga::ImageClass::Sampled { .. } => {
                            inputs.push(NodePort {
                                name: leak_str(&name),
                                ty: PortType::Texture2D,
                                kind: PortKind::Input,
                                required: true,
                            });
                            bindings.push(BindingSlot {
                                binding: binding.binding,
                                kind: BindingKind::SampledTexture { port: name },
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
                                name: port_name,
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
                    outputs.push(NodePort {
                        name: port_name,
                        ty: PortType::Array(ArrayType {
                            item_size: 4,
                            item_align: 4,
                            item_kind: ItemKind::Anonymous,
                            specs: &[],
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
                    // Non-atomic struct array. Always Array(Anonymous)
                    // with the struct's byte span + align — the wgsl_compute
                    // primitive is the generic byte-buffer escape hatch, and
                    // typed downstream consumers wire through `node.cast_as_*`
                    // atoms to relabel the wire (cast_as_particle for the
                    // 64-byte integrator pattern, cast_as_mesh_vertex for
                    // 32-byte vertex producers, etc.). The Particle special
                    // case here was removed 2026-05-26 — type discipline
                    // lives at the cast-atom boundary, not at the introspection.
                    let item = element_to_array_type(element, stride, &module)?;
                    let port_name = leak_str(&name);
                    if read && write {
                        // Aliased in/out — declare an input AND an
                        // output with the same name, register the
                        // pair in aliased_array_io.
                        inputs.push(NodePort {
                            name: port_name,
                            ty: PortType::Array(item),
                            kind: PortKind::Input,
                            required: true,
                        });
                        outputs.push(NodePort {
                            name: port_name,
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
                            name: port_name,
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
    })
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
            name: pname,
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
            name: pname,
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
) -> Result<ArrayType, String> {
    // wgsl_compute is the byte-buffer escape hatch. The legacy
    // ItemKind::Anonymous tag + (item_size, item_align) bridge survives
    // so existing cast atoms (cast_as_particle, cast_as_u32, ...) keep
    // working through Phase 4a. Phase 4b deletes the cast atoms and
    // ItemKind entirely once preset JSONs migrate.
    //
    // Phase 4a addition: walk the WGSL struct's fields via naga and emit
    // a typed Channels signature alongside the legacy bridge fields.
    // Downstream typed consumers that already declare matching specs
    // (post-Phase-3 KnownItem migration) wire through the new validator
    // path directly when reached without a cast atom in between; Phase
    // 4b JSON-prunes the cast atoms once preset surgery is reviewed.
    //
    // align=4 not naga's vec3-padded alignment of 16 — matches the
    // Rust-side layout convention every other primitive uses.
    let naga::TypeInner::Struct { span, members } = &element.inner else {
        return Err("storage array element is not a struct".into());
    };
    let specs = struct_members_to_specs(members, module);
    Ok(ArrayType {
        item_size: *span,
        item_align: 4,
        item_kind: ItemKind::Anonymous,
        specs,
        match_mode: crate::node_graph::ports::MatchMode::Exact,
    })
}

/// Walk a WGSL storage-array struct's members and emit a Channels
/// signature.
///
/// Per `docs/CHANNEL_TYPE_SYSTEM.md` §8.2:
/// - Fields whose name starts with `_pad` are SKIPPED (existing
///   convention used throughout the codebase, including the uniform-
///   member walker above; the doc's "explicit marker" preference is
///   parked for a follow-up that introduces a preprocessor).
/// - Each remaining field maps to a [`ChannelSpec`] with:
///     - `name`: `ChannelName::from_str(field.name)`. The hash collides
///       with the registry's `well_known::*` constants when the WGSL
///       author happens to use a canonical name; otherwise the
///       signature carries the field's raw name (debug lookup falls
///       back to the hex hash).
///     - `ty`: mapped from the field's WGSL type via
///       [`naga_type_to_channel_element_type`].
/// - Fields whose type doesn't map cleanly (matrices, runtime arrays,
///   atomics) cause the entire signature to fall back to `&[]` — Phase
///   4a is best-effort: when the walk can produce a typed signature,
///   downstream consumers benefit; when it can't, the wire continues
///   to behave exactly like pre-Phase-4 (Anonymous bridge to a cast
///   atom).
///
/// The returned slice is `'static` via `Box::leak`. Same justification
/// as `leak_str`: bounded by the distinct field-name + element-type
/// combinations across all loaded wgsl_compute shaders in a session.
fn struct_members_to_specs(
    members: &[naga::StructMember],
    module: &naga::Module,
) -> &'static [ChannelSpec] {
    let mut specs: Vec<ChannelSpec> = Vec::with_capacity(members.len());
    for m in members {
        let Some(name) = m.name.as_deref() else {
            return &[];
        };
        if name.starts_with("_pad") {
            continue;
        }
        let inner = &module.types[m.ty].inner;
        let Some(ty) = naga_type_to_channel_element_type(inner) else {
            return &[];
        };
        specs.push(ChannelSpec {
            name: crate::node_graph::ports::ChannelName::from_str(leak_str(name)),
            ty,
        });
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
// EffectNode impl
// ─────────────────────────────────────────────────────────────────────

impl EffectNode for WgslCompute {
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
        Some(&self.source)
    }

    fn set_wgsl_source(&mut self, source: &str) {
        if self.source == source {
            return;
        }
        self.reparse(source.to_string());
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

        // Compile (or recompile) the pipeline lazily on source change.
        let source_hash = hash_str(&self.source);
        if self.pipeline.is_none() || self.compiled_hash != Some(source_hash) {
            let gpu = ctx.gpu_encoder();
            // Naga has already validated this source in `reparse` — a
            // successful introspect implies a valid module. We pick
            // the first entry-point name (`introspect` requires
            // exactly one Compute entry).
            let entry = match naga::front::wgsl::parse_str(&self.source)
                .ok()
                .and_then(|m| m.entry_points.into_iter().next().map(|e| e.name))
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
            self.sampler =
                Some(gpu.device.create_sampler(&manifold_gpu::GpuSamplerDesc::default()));
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
        if let Some(layout) = &self.uniform_layout {
            for byte in self.uniform_scratch.iter_mut() {
                *byte = 0;
            }
            for m in &layout.members {
                if m.name.starts_with("_pad") {
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
                BindingKind::SampledTexture { port } => {
                    let Some(tex) = ctx.inputs.texture_2d(port) else {
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
            }
        }

        // Now assemble the GpuBinding slice. Uniform Bytes points
        // into uniform_scratch.
        let mut gpu_bindings: Vec<GpuBinding> = Vec::with_capacity(self.bindings.len());
        for slot in &self.bindings {
            match &slot.kind {
                BindingKind::Uniform => {
                    gpu_bindings.push(GpuBinding::Bytes {
                        binding: slot.binding,
                        data: &self.uniform_scratch,
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
                | BindingKind::StorageAtomicAccumOut { .. } => {
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

        let pipeline = self.pipeline.as_ref().expect("pipeline compiled above");
        let gpu = ctx.gpu_encoder();
        gpu.native_enc
            .dispatch_compute(pipeline, &gpu_bindings, [dx, dy, dz], TYPE_ID);
    }
}

impl WgslCompute {
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
            let count = (buf.size() as u32) / item_size;
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
        node.inputs.iter().map(|i| i.name).collect()
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
        // Channels signature alongside the legacy Anonymous bridge
        // fields. ItemKind stays Anonymous so existing cast_as_*
        // wires keep working through Phase 4b's deletion pass.
        assert_eq!(input_names(&node), vec!["dt", "particles"]);
        assert_eq!(node.inputs[0].ty, PortType::Scalar(ScalarType::F32));
        match node.inputs[1].ty {
            PortType::Array(at) => {
                assert_eq!(at.item_size, 64);
                assert_eq!(at.item_align, 4);
                assert_eq!(at.item_kind, ItemKind::Anonymous);
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
        // Atomic-u32 outputs are Anonymous-4 since the u32 special
        // case was removed 2026-05-26. Downstream consumers wire
        // through `node.cast_as_u32` for typed `Array<u32>` access.
        assert_eq!(
            node.outputs[0].ty,
            PortType::Array(ArrayType {
                item_size: 4,
                item_align: 4,
                item_kind: ItemKind::Anonymous,
                specs: &[],
                match_mode: crate::node_graph::ports::MatchMode::Exact,
            })
        );
        assert!(node.aliased_array_io().is_empty());
        // No texture output; dispatch port falls back to the first
        // array output.
        assert_eq!(node.dispatch_port.as_deref(), Some("accum_top"));
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

#[cfg(test)]
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

        let backend = MetalBackend::new(&device, w, h, format);
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
}
