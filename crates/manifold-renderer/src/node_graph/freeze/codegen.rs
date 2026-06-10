//! Fusion codegen — emit WGSL kernels from atom `wgsl_body` fragments
//! (design doc §12). This module is the v1 foundation: the **standalone**
//! single-atom kernel generator. It wraps one atom's body fragment in the
//! iteration boilerplate (dims/guard/sample/store) + a merged param uniform,
//! reproducing that atom's hand-written kernel so the hand shader can be
//! deleted (single-source authoring; validated per-atom against the original
//! through the [`TextureDiff`](super::TextureDiff) oracle in build step 1b).
//!
//! The fused MULTI-atom generator (chaining N bodies, namespace+dedup) is
//! build step 3/4; it reuses this module's param-emission + read-path helpers.
//!
//! Determinism (design §12.3): output is byte-identical run-to-run — fields
//! emit in `PARAMS` slice order, the body is verbatim, and there are no
//! float-literal-from-param emissions (all params are live uniform reads in
//! v1, never baked constants). The generated WGSL text is the cross-session
//! pipeline-cache key, so determinism is load-bearing.

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::freeze::classify::{FusionKind, InputAccess};
use crate::node_graph::parameters::{ParamDef, ParamType};
use crate::node_graph::ports::{ChannelElementType, ChannelSpec, NodeInput, NodeOutput, PortType};
use std::fmt::Write as _;

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
        other => Err(CodegenError::UnsupportedParam { name: p.name, ty: other }),
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
/// expands to 3 consecutive f32 fields (`<name>_x/_y/_z`) — matching how the
/// hand atoms already pack a colour as three scalars (e.g. chroma_key's
/// key_r/g/b) — and the body receives it reassembled as a `vec3<f32>`.
pub(crate) fn param_word_count(p: &ParamDef) -> Result<usize, CodegenError> {
    match p.ty {
        ParamType::Float | ParamType::Angle | ParamType::Frequency => Ok(1),
        ParamType::Int | ParamType::Bool | ParamType::Enum => Ok(1),
        ParamType::Vec3 => Ok(3),
        other => Err(CodegenError::UnsupportedParam { name: p.name, ty: other }),
    }
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
    ];
    if RESERVED.contains(&name) {
        std::borrow::Cow::Owned(format!("p_{name}"))
    } else {
        std::borrow::Cow::Borrowed(name)
    }
}

fn is_texture_input(i: &NodeInput) -> bool {
    matches!(
        i.ty,
        PortType::Texture2D | PortType::Texture2DTyped(_) | PortType::Texture3D
    )
}

fn is_texture_port(ty: &PortType) -> bool {
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
enum TexDim {
    D2,
    D3,
}

/// Per-dimension WGSL fragments for the iteration wrapper. (Input texture types
/// are emitted per-input, not here, so a 3D input can feed a 2D-output atom.)
struct DimForms {
    storage_ty: &'static str,
    workgroup: &'static str,
    guard: &'static str,
    uv_expr: &'static str,
    dims_arg: &'static str,
    store_coord: &'static str,
}

fn dim_forms(dim: TexDim) -> DimForms {
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
            workgroup: "4, 4, 4",
            guard: "id.x >= dims.x || id.y >= dims.y || id.z >= dims.z",
            uv_expr: "(vec3<f32>(id) + 0.5) / vec3<f32>(dims)",
            dims_arg: "vec3<f32>(dims)",
            store_coord: "vec3<i32>(id)",
        },
    }
}

/// Generate the standalone kernel for a primitive type — the single-source
/// `run()` path. Reads the body + classification + ports/params off the type's
/// `PrimitiveSpec` consts. Deterministic, so `create_compute_pipeline` caches
/// the result across instances and sessions (the WGSL text is the cache key).
pub fn standalone_for_spec<P: crate::node_graph::primitive::PrimitiveSpec>(
) -> Result<String, CodegenError> {
    let body = P::WGSL_BODY.ok_or(CodegenError::NoBody)?;
    // Buffer atoms (Array output) route directly so they can carry
    // `DERIVED_UNIFORMS` (frame-derived non-param uniform fields). The texture
    // path's public `generate_standalone` signature stays untouched.
    if P::OUTPUTS.iter().any(|o| matches!(o.ty, PortType::Array(_))) {
        return generate_standalone_buffer(
            body,
            P::INPUTS,
            P::PARAMS,
            P::INPUT_ACCESS,
            P::DERIVED_UNIFORMS,
            P::WGSL_INCLUDES,
            P::OUTPUTS,
            P::ATOMIC_OUTPUTS,
        );
    }
    // BUFFER→TEXTURE resolve: an Array input with a (texture) output — the
    // accumulator-to-density bridge. Routes to its own self-contained path so
    // the texture `generate_standalone` never grows an array-input branch.
    if P::INPUTS.iter().any(|i| matches!(i.ty, PortType::Array(_))) {
        return generate_standalone_resolve(body, P::INPUTS, P::PARAMS, P::OUTPUTS);
    }
    generate_standalone(P::FUSION_KIND, body, P::INPUTS, P::PARAMS, P::INPUT_ACCESS, P::OUTPUTS)
}

/// WGSL storage-texture format token for the formats a texture kernel can declare
/// as a write target. `None` for anything else (the standalone path only supports
/// the f16 working default + fp32 opt-in for precision-sensitive feedback loops).
pub fn wgsl_storage_token(fmt: manifold_gpu::GpuTextureFormat) -> Option<&'static str> {
    use manifold_gpu::GpuTextureFormat as F;
    match fmt {
        F::Rgba16Float => Some("rgba16float"),
        F::Rgba32Float => Some("rgba32float"),
        _ => None,
    }
}

/// Like [`standalone_for_spec`] but emits the output storage texture at `fmt`
/// instead of the hardcoded rgba16float. The unfused side of FULL-PRECISION
/// in-loop fusion: a texture atom inside a chaotic feedback loop can declare an
/// fp32 output (via `outputFormats`), and then the editor (unfused) stores its
/// intermediates exactly — matching the fused kernel's f32 registers, so fused ==
/// unfused. A targeted replace of the single dst binding token is safe: input
/// textures are `texture_2d<f32>` (no storage format), so `<rgba16float, write>`
/// appears only on the output. Non-fp32 (incl. the f16 default) returns unchanged.
pub fn standalone_for_spec_fmt<P: crate::node_graph::primitive::PrimitiveSpec>(
    fmt: manifold_gpu::GpuTextureFormat,
) -> Result<String, CodegenError> {
    let wgsl = standalone_for_spec::<P>()?;
    let Some(token) = wgsl_storage_token(fmt) else {
        return Ok(wgsl); // unknown / unsupported → leave the f16 default
    };
    if token == "rgba16float" {
        return Ok(wgsl);
    }
    Ok(wgsl
        .replace(
            "texture_storage_2d<rgba16float, write>",
            &format!("texture_storage_2d<{token}, write>"),
        )
        .replace(
            "texture_storage_3d<rgba16float, write>",
            &format!("texture_storage_3d<{token}, write>"),
        ))
}

/// Generate the standalone `cs_main` kernel for one atom. `body` is the atom's
/// `wgsl_body` fragment (defines `fn body(...)` plus any helpers, verbatim).
///
/// Binding layout matches the hand-written atoms so the result is a drop-in:
/// `@binding(0)` uniform params, `@binding(1..)` each texture input,
/// `@binding(S)` sampler, `@binding(S+1)` output storage texture.
pub fn generate_standalone(
    fusion_kind: FusionKind,
    body: &str,
    inputs: &[NodeInput],
    params: &[ParamDef],
    input_access: &[InputAccess],
    outputs: &[NodeOutput],
) -> Result<String, CodegenError> {
    if body.is_empty() {
        return Err(CodegenError::NoBody);
    }
    // Buffer-domain atoms (Array storage I/O — particle / instance / curve sims)
    // take a separate codegen path: `var<storage>` array bindings, a 1D
    // workgroup keyed on an explicit element count (no `textureDimensions`), and
    // an element struct synthesized from each Array port's Channels signature.
    // Detected by an Array output port — a buffer atom always writes at least one
    // storage array. Texture atoms are unaffected (no Array output).
    if outputs.iter().any(|o| matches!(o.ty, PortType::Array(_))) {
        // Direct callers of generate_standalone (texture-path tests) carry no
        // derived uniforms / atomic outputs; standalone_for_spec routes buffer
        // atoms with their DERIVED_UNIFORMS / ATOMIC_OUTPUTS before reaching here.
        return generate_standalone_buffer(
            body, inputs, params, input_access, &[], &[], outputs, &[],
        );
    }
    let tex_inputs: Vec<&NodeInput> = inputs.iter().filter(|i| is_texture_input(i)).collect();
    // Texture outputs. >1 → the body returns a `BodyOutputs` struct (one vec4 per
    // output port) and the wrapper gates each store on an injected `write_<name>`
    // uniform flag (the executor aliases an unconsumed output slot onto a live
    // one, so an ungated double-write would clobber it). 1 (or 0) keeps the
    // single `dst` path — byte-identical to before for every existing atom.
    let tex_outputs: Vec<&NodeOutput> = outputs.iter().filter(|o| is_texture_port(&o.ty)).collect();
    let multi_output = tex_outputs.len() > 1;
    // The WRAPPER dimensionality follows the OUTPUT (the dispatch grid / store
    // coord / uv): a Texture3D output → 3D wrapper (blur_3d_separable), else 2D.
    // Input texture types are PER-INPUT (a 3D input can feed a 2D-output atom —
    // sample_volume_2d gathers a Texture3D at a body-computed coord and writes 2D).
    let dim = if tex_outputs.iter().any(|o| o.ty == PortType::Texture3D) {
        TexDim::D3
    } else {
        TexDim::D2
    };
    let forms = dim_forms(dim);
    match fusion_kind {
        FusionKind::Pointwise if tex_inputs.len() == 1 => {}
        FusionKind::MultiInputCoincident if tex_inputs.len() >= 2 => {}
        // Generator: no texture input; the body produces from uv/dims/params.
        FusionKind::Source if tex_inputs.is_empty() => {}
        FusionKind::Boundary => return Err(CodegenError::NotFusable(fusion_kind)),
        _ => {
            return Err(CodegenError::WrongTextureArity {
                kind: fusion_kind,
                found: tex_inputs.len(),
            });
        }
    }
    // Per-(texture-)input read-semantics, aligned to `tex_inputs` order; index
    // past the end defaults to Coincident (the resolution-robust sampler read).
    let access_of = |i: usize| input_access.get(i).copied().unwrap_or_default();
    // A sampler is bound when any input is sampler-read (Coincident) OR gathered
    // (a Gather body samples the texture itself, so it needs the sampler too).
    // A CoincidentTexel-only atom (dither) binds no sampler.
    let needs_sampler =
        (0..tex_inputs.len()).any(|i| matches!(access_of(i), InputAccess::Coincident | InputAccess::Gather));
    let any_texel = (0..tex_inputs.len()).any(|i| access_of(i) == InputAccess::CoincidentTexel);
    // Optional texture inputs get a derived `use_<name>: u32` flag injected into
    // the uniform (run() packs `input.is_some()`); the body falls back to a default
    // when the flag is 0. An unwired optional input binds a dummy texture (run()'s
    // job), so the unconditional pre-read is harmless — the body just ignores it.
    // Mirrors the multi-output write-flag injection. (Only pack_channels uses this
    // today; no converted atom has an optional texture input.)
    let optional_tex_inputs: Vec<&NodeInput> =
        tex_inputs.iter().filter(|i| !i.required).copied().collect();
    // A paramless atom (e.g. abs_texture) binds NO uniform and NO Params struct,
    // so its textures start at binding 0 — matching the hand shader, which has no
    // uniform either. The body simply takes no param args (the param loop below
    // is empty). A uniform is also needed when there are injected flags (multi-
    // output write flags or optional-input use flags) even if there are no params.
    let has_uniform =
        !params.is_empty() || multi_output || !optional_tex_inputs.is_empty();

    let mut out = String::new();

    // --- param uniform struct (scalar fields in PARAMS order, padded to a
    // 16-byte multiple to match the setBytes buffer size). Omitted entirely when
    // the atom has no params. A Table param (gradient_ramp's `stops`) expands to a
    // `<name>_count: u32` header word plus a fixed `array<vec4<f32>, TABLE_LEN>`
    // appended after the 16-byte-aligned header; the body receives it as
    // `<name>_count: u32, <name>: array<vec4<f32>, TABLE_LEN>`. param_wgsl_type
    // still rejects Table, so the fused region-grower keeps treating a table atom
    // as a boundary — only this standalone path lays one out. ---
    const TABLE_LEN: usize = 16;
    let table_params: Vec<&ParamDef> =
        params.iter().filter(|p| p.ty == ParamType::Table).collect();
    if has_uniform {
        out.push_str("struct Params {\n");
        let mut header_words = 0usize;
        for p in params {
            if p.ty == ParamType::Table {
                continue; // emitted as count (here) + array (below)
            }
            let f = wgsl_safe_field(p.name);
            if p.ty == ParamType::Vec3 {
                // A vec3 param expands to three consecutive f32 fields.
                writeln!(out, "    {f}_x: f32,").unwrap();
                writeln!(out, "    {f}_y: f32,").unwrap();
                writeln!(out, "    {f}_z: f32,").unwrap();
            } else {
                let ty = param_wgsl_type(p)?;
                writeln!(out, "    {f}: {ty},").unwrap();
            }
            header_words += param_word_count(p)?;
        }
        for t in &table_params {
            writeln!(out, "    {}_count: u32,", t.name).unwrap();
            header_words += 1;
        }
        // Multi-output: one write-gate flag per output (in output order). For
        // voronoi_2d this reproduces the hand uniform's write_out/write_cell_id
        // tail exactly.
        if multi_output {
            for o in &tex_outputs {
                writeln!(out, "    write_{}: u32,", o.name).unwrap();
                header_words += 1;
            }
        }
        for inp in &optional_tex_inputs {
            writeln!(out, "    use_{}: u32,", inp.name).unwrap();
            header_words += 1;
        }
        let pad_words = (4 - (header_words % 4)) % 4;
        for i in 0..pad_words {
            writeln!(out, "    _pad{i}: u32,").unwrap();
        }
        for t in &table_params {
            writeln!(out, "    {}: array<vec4<f32>, {TABLE_LEN}>,", t.name).unwrap();
        }
        out.push_str("}\n\n");
    }

    // --- bindings: [uniform(0)], texture(..), [sampler], output. The uniform is
    // emitted only when the atom has params; the sampler only when at least one
    // input is sampler-read (Coincident). So an all-texel paramless atom binds
    // just its textures + dst — matching what its run() binds. ---
    let mut next_binding = 0u32;
    if has_uniform {
        out.push_str("@group(0) @binding(0) var<uniform> params: Params;\n");
        next_binding = 1;
    }
    for inp in tex_inputs.iter() {
        // Per-input texture dimensionality (a 3D input may feed a 2D-output atom).
        let inp_ty = if inp.ty == PortType::Texture3D {
            "texture_3d<f32>"
        } else {
            "texture_2d<f32>"
        };
        writeln!(
            out,
            "@group(0) @binding({next_binding}) var tex_{}: {};",
            inp.name, inp_ty
        )
        .unwrap();
        next_binding += 1;
    }
    if needs_sampler {
        writeln!(out, "@group(0) @binding({next_binding}) var samp: sampler;").unwrap();
        next_binding += 1;
    }
    // Output storage texture(s). Single output keeps the name `dst` (so existing
    // atoms' WGSL is unchanged); multi-output names each `dst_<port>`.
    if multi_output {
        for o in &tex_outputs {
            writeln!(
                out,
                "@group(0) @binding({next_binding}) var dst_{}: {};",
                o.name, forms.storage_ty
            )
            .unwrap();
            next_binding += 1;
        }
    } else {
        writeln!(
            out,
            "@group(0) @binding({next_binding}) var dst: {};",
            forms.storage_ty
        )
        .unwrap();
    }
    out.push('\n');

    // Multi-output body returns a struct with one vec4 field per output port. The
    // codegen declares it so the body can `return BodyOutputs(out_value, …)`.
    if multi_output {
        out.push_str("struct BodyOutputs {\n");
        for o in &tex_outputs {
            writeln!(out, "    {}: vec4<f32>,", o.name).unwrap();
        }
        out.push_str("}\n\n");
    }

    // --- the atom's body fragment, verbatim ---
    out.push_str(body.trim_end());
    out.push_str("\n\n");

    // --- iteration wrapper: dims/guard, center-UV sample each input, call
    // body in (texture-inputs-then-params) order, store. ---
    writeln!(out, "@compute @workgroup_size({})", forms.workgroup).unwrap();
    out.push_str("fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {\n");
    if multi_output {
        // Any output binding gives the canvas size (run() binds the live slot to
        // every binding, so they share dimensions); use the first.
        writeln!(out, "    let dims = textureDimensions(dst_{});", tex_outputs[0].name).unwrap();
    } else {
        out.push_str("    let dims = textureDimensions(dst);\n");
    }
    writeln!(out, "    if {} {{\n        return;\n    }}", forms.guard).unwrap();
    writeln!(out, "    let uv = {};", forms.uv_expr).unwrap();
    if any_texel {
        writeln!(out, "    let coord = {};", forms.store_coord).unwrap();
    }
    // Read each input by its access kind: Coincident → sampler at the fragment
    // UV (resolution-robust); CoincidentTexel → exact integer-texel load (no
    // filter — required when each texel is a distinct value, e.g. a dither
    // threshold). Both read the fragment's own coordinate. A Gather input is NOT
    // pre-read here — the body computes its own read coord, so it receives the
    // texture + sampler as args (below) and samples them itself.
    for (i, inp) in tex_inputs.iter().enumerate() {
        match access_of(i) {
            InputAccess::Coincident => writeln!(
                out,
                "    let c_{} = textureSampleLevel(tex_{}, samp, uv, 0.0);",
                inp.name, inp.name
            )
            .unwrap(),
            InputAccess::CoincidentTexel => writeln!(
                out,
                "    let c_{} = textureLoad(tex_{}, coord, 0);",
                inp.name, inp.name
            )
            .unwrap(),
            // Gather / GatherTexel: no pre-read; passed as a texture handle (the
            // body computes its own read coord and samples/loads it itself).
            // BufferGather only tags Array inputs (buffer atoms branch away above)
            // — unreachable on a texture input, no pre-read either way.
            InputAccess::Gather | InputAccess::GatherTexel | InputAccess::BufferGather => {}
        }
    }
    // body(<per-input args>, uv, dims, params.<p0>, ...). Each input contributes:
    // a Coincident/CoincidentTexel input → its pre-read colour register `c_<name>`;
    // a Gather input → the texture handle `tex_<name>` + the shared `samp`, which
    // the body samples at a coord it computes. `uv` (normalized center-of-texel)
    // and `dims` (float canvas size) are the ambient fragment context every body
    // receives after its inputs (design §slot / line 60, extended with dims so
    // positional atoms recover aspect = dims.x/dims.y and pixel = uv*dims). Atoms
    // that ignore an arg simply don't read it — spirv-opt's DCE drops it.
    let mut args: Vec<String> = Vec::new();
    for (i, inp) in tex_inputs.iter().enumerate() {
        match access_of(i) {
            // Sampler-gather: the body owns the filter, so it gets the texture +
            // the shared sampler.
            InputAccess::Gather => {
                args.push(format!("tex_{}", inp.name));
                args.push("samp".to_string());
            }
            // Integer-load gather: no sampler — the body does textureLoad itself.
            InputAccess::GatherTexel => {
                args.push(format!("tex_{}", inp.name));
            }
            InputAccess::Coincident | InputAccess::CoincidentTexel => {
                args.push(format!("c_{}", inp.name));
            }
            // Buffer-domain only — the buffer codegen path handles array inputs;
            // a texture input is never tagged BufferGather.
            InputAccess::BufferGather => {
                unreachable!("BufferGather is buffer-domain only — never a texture input")
            }
        }
    }
    args.push("uv".to_string());
    args.push(forms.dims_arg.to_string());
    // Scalar params first (PARAMS order; Vec3 reassembled), then each table as
    // (count, array) — matching the body signature `…, <t>_count, <t>` per table.
    for p in params {
        if p.ty == ParamType::Table {
            continue;
        }
        let f = wgsl_safe_field(p.name);
        if p.ty == ParamType::Vec3 {
            args.push(format!("vec3<f32>(params.{f}_x, params.{f}_y, params.{f}_z)"));
        } else {
            args.push(format!("params.{f}"));
        }
    }
    for t in &table_params {
        args.push(format!("params.{}_count", t.name));
        args.push(format!("params.{}", t.name));
    }
    // Optional-input use-flags last (after the params), matching the body sig.
    for inp in &optional_tex_inputs {
        args.push(format!("params.use_{}", inp.name));
    }
    writeln!(out, "    let result = body({});", args.join(", ")).unwrap();
    if multi_output {
        // Each store gated on its write flag — an unconsumed output's slot is
        // aliased onto a live one, so writing it ungated would clobber the live
        // texture.
        for o in &tex_outputs {
            writeln!(
                out,
                "    if params.write_{} != 0u {{ textureStore(dst_{}, {}, result.{}); }}",
                o.name, o.name, forms.store_coord, o.name
            )
            .unwrap();
        }
    } else {
        writeln!(out, "    textureStore(dst, {}, result);", forms.store_coord).unwrap();
    }
    out.push_str("}\n");

    Ok(out)
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
fn channel_wgsl_ty(t: ChannelElementType) -> &'static str {
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
fn buffer_element_type(
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

/// Emit a WGSL struct definition for a multi-channel element. Field names come
/// from each channel's debug name (the well-known registry), falling back to
/// `c{i}` for runtime-introduced names. std430 layout is implicit in the field
/// types — no explicit pad fields (matching how the `#[repr(C)]` element relies
/// on WGSL alignment to reproduce its stride).
fn emit_buffer_struct(specs: &[ChannelSpec], name: &str) -> String {
    let mut s = format!("struct {name} {{\n");
    for (i, sp) in specs.iter().enumerate() {
        let field = sp
            .name
            .debug_name()
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("c{i}"));
        writeln!(s, "    {field}: {},", channel_wgsl_ty(sp.ty)).unwrap();
    }
    s.push_str("}\n");
    s
}

/// Generate the standalone `cs_main` kernel for a buffer-domain atom. The body
/// fragment defines `fn body(idx: u32, count: u32, <params…>) -> <out elem>` and
/// references the input array global(s) `buf_<port>` (gather form). The wrapper
/// emits: element structs, the merged param uniform (scalar params + an injected
/// `dispatch_count` element count + 16-byte pad), `var<storage>` array bindings
/// (`read` inputs, `read_write` outputs), the body verbatim, and a 1D dispatch
/// guarded on `dispatch_count`.
///
/// Binding layout: `@binding(0)` uniform, then each Array input `read`, then each
/// Array output `read_write`. Deterministic (PARAMS / port order), so the
/// generated text is a stable pipeline-cache key.
#[allow(clippy::too_many_arguments)]
fn generate_standalone_buffer(
    body: &str,
    inputs: &[NodeInput],
    params: &[ParamDef],
    input_access: &[InputAccess],
    derived_uniforms: &[&str],
    includes: &[&str],
    outputs: &[NodeOutput],
    atomic_outputs: &[&str],
) -> Result<String, CodegenError> {
    // Per-array-input access (aligned to array inputs in declaration order):
    //   - BufferGather → the body indexes the input array global `buf_<port>`
    //     itself (grid neighbours, random access). No pre-read, no element arg.
    //   - coincident (the default) → the wrapper pre-reads element `[idx]` into
    //     `e_<port>` and passes it; the body operates on the value (the
    //     pointwise / per-element integrators, jitters, transforms).
    let is_gather = |i: usize| matches!(input_access.get(i), Some(InputAccess::BufferGather));

    let array_inputs: Vec<&NodeInput> = inputs
        .iter()
        .filter(|i| matches!(i.ty, PortType::Array(_)))
        .collect();
    // Texture inputs sampled at a body-computed coord (the particle position) —
    // the buffer analogue of the texture-domain Gather. The body receives each
    // `tex_<name>` handle + the shared `samp` and samples them itself (the
    // `*_at_particles` force-sampler family, anti_clump's modulator).
    let texture_inputs: Vec<&NodeInput> = inputs.iter().filter(|i| is_texture_input(i)).collect();
    // Optional texture inputs get an injected `use_<name>: u32` flag (run() packs
    // `is_some()`); the body falls back to a default when 0. An unwired optional
    // texture binds a dummy 1×1 so the binding is always present (run()'s job).
    let optional_textures: Vec<&NodeInput> =
        texture_inputs.iter().filter(|i| !i.required).copied().collect();
    let array_outputs: Vec<&NodeOutput> = outputs
        .iter()
        .filter(|o| matches!(o.ty, PortType::Array(_)))
        .collect();
    if array_outputs.is_empty() {
        // A buffer atom writes at least one storage array.
        return Err(CodegenError::NotFusable(FusionKind::Boundary));
    }
    // ≥2 array outputs → the body returns a `BufferOutputs` struct the wrapper
    // unpacks (the buffer analogue of the texture multi-output BodyOutputs path);
    // 1 keeps the direct `buf_out[idx] = body(...)` write — byte-identical for
    // every existing single-output atom.
    let multi_output = array_outputs.len() > 1;

    // Resolve element type names (inputs then outputs) so struct naming is stable
    // and a same-typed in/out pair dedups to one struct.
    let mut structs: Vec<(&'static [ChannelSpec], String)> = Vec::new();
    let specs_of = |ty: &PortType| -> &'static [ChannelSpec] {
        match ty {
            PortType::Array(at) => at.specs,
            _ => unreachable!("filtered to Array ports"),
        }
    };
    let in_tys: Vec<String> = array_inputs
        .iter()
        .map(|i| buffer_element_type(specs_of(&i.ty), &mut structs))
        .collect();
    // Per-output (port name, element type), in declaration order.
    let out_infos: Vec<(&str, String)> = array_outputs
        .iter()
        .map(|o| (o.name, buffer_element_type(specs_of(&o.ty), &mut structs)))
        .collect();
    // Output array global name. Normally `buf_<port>`, but if the output port
    // shares a name with an input port (e.g. instance_position_jitter's
    // `instances` in AND out — NOT aliased, separate buffers), disambiguate to
    // `buf_out_<port>` so the two storage globals don't collide. Bodies only ever
    // reference INPUT globals, so this never affects a body.
    let out_global = |name: &str| -> String {
        if array_inputs.iter().any(|i| i.name == name) {
            format!("buf_out_{name}")
        } else {
            format!("buf_{name}")
        }
    };
    // Atomic-accumulator output (scatter, single-output only): emitted as
    // `array<atomic<u32>>` and written by the body via `atomicAdd` on the global
    // itself, not the wrapper's `[idx] = body(...)`. WGSL atomics are integer-only.
    let out_is_atomic = !multi_output && atomic_outputs.contains(&array_outputs[0].name);
    if out_is_atomic && out_infos[0].1 != "u32" && out_infos[0].1 != "i32" {
        return Err(CodegenError::AtomicNonInteger { ty: out_infos[0].1.clone() });
    }
    // Multi-output atomic isn't a shape any atom needs yet.
    if multi_output && array_outputs.iter().any(|o| atomic_outputs.contains(&o.name)) {
        return Err(CodegenError::NotFusable(FusionKind::Boundary));
    }

    let mut out = String::new();

    // --- element structs (deduped, first-appearance order) ---
    for (specs, name) in &structs {
        out.push_str(&emit_buffer_struct(specs, name));
        out.push('\n');
    }

    // --- param uniform: scalar params (PARAMS order) + injected element count +
    // 16-byte pad. The count drives the dispatch guard (buffers have no
    // `textureDimensions`) and is passed to the body as `count`. ---
    out.push_str("struct Params {\n");
    let mut words = 0usize;
    for p in params {
        let ty = param_wgsl_type(p)?; // rejects vec/table/string buffer params
        let f = wgsl_safe_field(p.name);
        writeln!(out, "    {f}: {ty},").unwrap();
        words += param_word_count(p)?; // scalar params are 1 word each
    }
    // Injected non-param derived fields (frame-derived values like dt_scaled),
    // after the params. Each entry is `"name"` (f32) or `"name:ty"` for an
    // explicit scalar type — `"frame_count:u32"` so a frame counter stays an
    // exact integer rather than losing precision as an f32 past ~16M frames.
    // run() packs the resolved value each frame.
    for d in derived_uniforms {
        let (dname, dty) = d.split_once(':').unwrap_or((d, "f32"));
        if dty == "vec3" {
            // A vec3 derived field (a camera basis vector) expands to three
            // consecutive f32 fields, mirroring the texture path's vec3 PARAM
            // packing — the body receives it reassembled as `vec3<f32>`. Packing
            // as 3 scalars (not a `vec3<f32>` field) keeps the 4-byte stride the
            // run()-side `#[repr(C)]` uniform uses, dodging the uniform vec3's
            // 16-byte alignment.
            writeln!(out, "    {dname}_x: f32,").unwrap();
            writeln!(out, "    {dname}_y: f32,").unwrap();
            writeln!(out, "    {dname}_z: f32,").unwrap();
            words += 3;
        } else {
            writeln!(out, "    {dname}: {dty},").unwrap();
            words += 1; // every supported derived scalar is one 4-byte word
        }
    }
    // Optional-texture use-flags (run() packs `is_some()`), after the derived
    // fields. The body multiplies by / branches on these to fall back when an
    // optional texture is unwired.
    for tex in &optional_textures {
        writeln!(out, "    use_{}: u32,", tex.name).unwrap();
        words += 1;
    }
    out.push_str("    dispatch_count: u32,\n");
    words += 1;
    let pad_words = (4 - (words % 4)) % 4;
    for i in 0..pad_words {
        writeln!(out, "    _pad{i}: u32,").unwrap();
    }
    out.push_str("}\n\n");

    // --- bindings: uniform(0), inputs (read), outputs (read_write) ---
    out.push_str("@group(0) @binding(0) var<uniform> params: Params;\n");
    let mut binding = 1u32;
    for (i, inp) in array_inputs.iter().enumerate() {
        writeln!(
            out,
            "@group(0) @binding({binding}) var<storage, read> buf_{}: array<{}>;",
            inp.name, in_tys[i]
        )
        .unwrap();
        binding += 1;
    }
    // Texture inputs, then ONE shared sampler (after the array inputs, before the
    // output array) — matching the hand `*_at_particles` binding order.
    for inp in &texture_inputs {
        let tex_ty = if inp.ty == PortType::Texture3D {
            "texture_3d<f32>"
        } else {
            "texture_2d<f32>"
        };
        writeln!(out, "@group(0) @binding({binding}) var tex_{}: {tex_ty};", inp.name).unwrap();
        binding += 1;
    }
    if !texture_inputs.is_empty() {
        writeln!(out, "@group(0) @binding({binding}) var samp: sampler;").unwrap();
        binding += 1;
    }
    for (name, ety) in &out_infos {
        let storage_ty = if out_is_atomic {
            format!("atomic<{ety}>")
        } else {
            ety.clone()
        };
        writeln!(
            out,
            "@group(0) @binding({binding}) var<storage, read_write> {}: array<{storage_ty}>;",
            out_global(name)
        )
        .unwrap();
        binding += 1;
    }
    out.push('\n');

    // Multi-output body returns a struct with one field per output array (in
    // declaration order); the wrapper writes each `buf_<port>[idx] = result.<port>`.
    if multi_output {
        out.push_str("struct BufferOutputs {\n");
        for (name, ety) in &out_infos {
            writeln!(out, "    {name}: {ety},").unwrap();
        }
        out.push_str("}\n\n");
    }

    // --- shared WGSL library includes (e.g. noise_common's simplex3d), prepended
    // so the body's helper calls resolve — mirrors run()'s format!("{lib}\n{body}").
    for inc in includes {
        out.push_str(inc.trim_end());
        out.push_str("\n\n");
    }

    // --- the atom's body fragment, verbatim (references buf_<port> + structs) ---
    out.push_str(body.trim_end());
    out.push_str("\n\n");

    // --- 1D iteration wrapper: guard on the element count, write one element ---
    out.push_str("@compute @workgroup_size(256)\n");
    out.push_str("fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {\n");
    out.push_str("    let idx = gid.x;\n");
    out.push_str("    if idx >= params.dispatch_count {\n        return;\n    }\n");
    // Pre-read each COINCIDENT input's own element `[idx]`; gather inputs are
    // read by the body itself through the bound global.
    for (i, inp) in array_inputs.iter().enumerate() {
        if !is_gather(i) {
            writeln!(out, "    let e_{} = buf_{}[idx];", inp.name, inp.name).unwrap();
        }
    }
    // body(idx, count, <coincident element args, in array-input order>,
    // <params…>). A gather input contributes NO arg (the body indexes its global
    // itself); idx + count are always passed (a coincident body that ignores
    // them lets DCE drop them). Single output element written directly.
    let mut args: Vec<String> = vec!["idx".to_string(), "params.dispatch_count".to_string()];
    for (i, inp) in array_inputs.iter().enumerate() {
        if !is_gather(i) {
            args.push(format!("e_{}", inp.name));
        }
    }
    // Each texture input contributes its handle + the shared sampler (the body
    // samples it at a coord it computes from the particle position).
    for inp in &texture_inputs {
        args.push(format!("tex_{}", inp.name));
        args.push("samp".to_string());
    }
    for p in params {
        let f = wgsl_safe_field(p.name);
        args.push(format!("params.{f}"));
    }
    for d in derived_uniforms {
        let (dname, dty) = d.split_once(':').unwrap_or((d, "f32"));
        if dty == "vec3" {
            args.push(format!(
                "vec3<f32>(params.{dname}_x, params.{dname}_y, params.{dname}_z)"
            ));
        } else {
            args.push(format!("params.{dname}"));
        }
    }
    // Optional-texture use-flags, last (matching the body signature).
    for tex in &optional_textures {
        args.push(format!("params.use_{}", tex.name));
    }
    if out_is_atomic {
        // Scatter: the body computes its own target cell and `atomicAdd`s into
        // the accumulator global — no coincident single-element write.
        writeln!(out, "    body({});", args.join(", ")).unwrap();
    } else if multi_output {
        // The body returns one element per output array; unpack into each.
        writeln!(out, "    let result = body({});", args.join(", ")).unwrap();
        for (name, _) in &out_infos {
            writeln!(out, "    {}[idx] = result.{name};", out_global(name)).unwrap();
        }
    } else {
        writeln!(
            out,
            "    {}[idx] = body({});",
            out_global(out_infos[0].0),
            args.join(", ")
        )
        .unwrap();
    }
    out.push_str("}\n");

    Ok(out)
}

/// Generate the standalone kernel for a BUFFER→TEXTURE *resolve* atom — the
/// bridge that lifts a `u32` fixed-point accumulator (an `Array(u32)` input,
/// bound `read_write` as `array<atomic<u32>>` because it self-clears) into a
/// float `Texture2D/3D` density. The dispatch grid is the OUTPUT texture's dims
/// (read via `textureDimensions(dst)`, like a Source generator); the wrapper
/// computes the linear cell `idx` from `id` + `dims` and the body does its own
/// `atomicLoad` + `atomicStore(0)` self-clear, returning the `vec4` density the
/// wrapper stores. This is the inverse of the atomic SCATTER (texture-grid
/// dispatch reading one buffer cell, vs particle-grid dispatch writing one).
///
/// ABI: `body(idx: u32, <params…>) -> vec4<f32>`, referencing the `buf_<accum>`
/// global. Self-contained — separate from the texture `generate_standalone` so
/// the ~40 converted texture atoms emit byte-identical WGSL (no array-input
/// branch threaded through their path).
fn generate_standalone_resolve(
    body: &str,
    inputs: &[NodeInput],
    params: &[ParamDef],
    outputs: &[NodeOutput],
) -> Result<String, CodegenError> {
    let array_inputs: Vec<&NodeInput> = inputs
        .iter()
        .filter(|i| matches!(i.ty, PortType::Array(_)))
        .collect();
    let tex_outputs: Vec<&NodeOutput> = outputs.iter().filter(|o| is_texture_port(&o.ty)).collect();
    // v1 resolve: exactly one accumulator in, one density texture out.
    if array_inputs.len() != 1 || tex_outputs.len() != 1 {
        return Err(CodegenError::NotFusable(FusionKind::Boundary));
    }
    let accum = array_inputs[0];
    let out_tex = tex_outputs[0];
    let accum_specs = match &accum.ty {
        PortType::Array(at) => at.specs,
        _ => unreachable!("filtered to Array ports"),
    };
    let mut structs: Vec<(&'static [ChannelSpec], String)> = Vec::new();
    let accum_ty = buffer_element_type(accum_specs, &mut structs);
    // The accumulator is an atomic integer grid (scatter wrote it via atomicAdd;
    // resolve reads + zeros it). WGSL atomics are integer-only.
    if accum_ty != "u32" && accum_ty != "i32" {
        return Err(CodegenError::AtomicNonInteger { ty: accum_ty });
    }
    let dim = if out_tex.ty == PortType::Texture3D {
        TexDim::D3
    } else {
        TexDim::D2
    };
    let forms = dim_forms(dim);

    let mut out = String::new();

    // --- param uniform (scalar params in PARAMS order + 16-byte pad). No
    // injected count: the dispatch grid is the texture, guarded on its dims. ---
    let has_uniform = !params.is_empty();
    if has_uniform {
        out.push_str("struct Params {\n");
        let mut words = 0usize;
        for p in params {
            let ty = param_wgsl_type(p)?;
            let f = wgsl_safe_field(p.name);
            writeln!(out, "    {f}: {ty},").unwrap();
            words += param_word_count(p)?;
        }
        let pad_words = (4 - (words % 4)) % 4;
        for i in 0..pad_words {
            writeln!(out, "    _pad{i}: u32,").unwrap();
        }
        out.push_str("}\n\n");
    }

    // --- bindings: [uniform(0)], accumulator (atomic read_write), output dst. ---
    let mut binding = 0u32;
    if has_uniform {
        out.push_str("@group(0) @binding(0) var<uniform> params: Params;\n");
        binding = 1;
    }
    writeln!(
        out,
        "@group(0) @binding({binding}) var<storage, read_write> buf_{}: array<atomic<{accum_ty}>>;",
        accum.name
    )
    .unwrap();
    binding += 1;
    writeln!(
        out,
        "@group(0) @binding({binding}) var dst: {};",
        forms.storage_ty
    )
    .unwrap();
    out.push('\n');

    // --- body verbatim (references buf_<accum>, returns the density vec4) ---
    out.push_str(body.trim_end());
    out.push_str("\n\n");

    // --- iteration wrapper: dispatch over the output texture's dims; compute the
    // linear cell index; body reads/zeros the accumulator and returns the vec4. ---
    let idx_expr = match dim {
        TexDim::D2 => "id.y * dims.x + id.x",
        TexDim::D3 => "id.z * dims.x * dims.y + id.y * dims.x + id.x",
    };
    writeln!(out, "@compute @workgroup_size({})", forms.workgroup).unwrap();
    out.push_str("fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {\n");
    out.push_str("    let dims = textureDimensions(dst);\n");
    writeln!(out, "    if {} {{\n        return;\n    }}", forms.guard).unwrap();
    writeln!(out, "    let idx = {idx_expr};").unwrap();
    let mut args: Vec<String> = vec!["idx".to_string()];
    for p in params {
        let f = wgsl_safe_field(p.name);
        args.push(format!("params.{f}"));
    }
    writeln!(out, "    let result = body({});", args.join(", ")).unwrap();
    writeln!(out, "    textureStore(dst, {}, result);", forms.store_coord).unwrap();
    out.push_str("}\n");

    Ok(out)
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
    /// An OPTIONAL texture input with no wire (pack_channels' unwired b/a). The
    /// body receives a zero vector and its injected `use_<name>` flag is the
    /// literal `0u`, so the value is never read — the same contract the atom's
    /// `run()` fulfils by binding a dummy texture and clearing the flag. Wiring
    /// is static in the def, so the flag folds at codegen time instead of
    /// becoming a uniform field.
    Unwired,
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
    /// (`dt_scaled`, `frame_count:u32`, …), in body-arg order after the params.
    /// The standalone path computes these in run() and packs them; the fused
    /// buffer path emits them as `n{i}_<name>` Params fields + body args, and the
    /// install pass wires each to the matching `system.generator_input` output so
    /// the fused kernel reads the same frame value the unfused atom would. Empty
    /// for atoms with no frame-derived uniforms (and the texture path).
    pub derived_uniforms: &'static [&'static str],
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
    /// non-empty; each id must name a node in [`Self::nodes`].
    pub outputs: Vec<NodeInstanceId>,
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
}

/// Result of fusing a region: the kernel + the ordered uniform field list
/// (node + param) so the caller can pack the merged uniform / gather live
/// values (DD-A5 per-source descriptor; step 4 gathers from inst.params).
#[derive(Debug, Clone)]
pub struct GeneratedFusion {
    pub wgsl: String,
    pub param_order: Vec<(NodeInstanceId, &'static str)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FnBlock {
    name: String,
    text: String,
}

/// Split a body fragment into its top-level declaration *prelude* (any `const` /
/// `struct` / `alias` / `override` the body declares before its first function)
/// and its `fn` blocks (helpers + `fn body`). The standalone path emits a body
/// verbatim, so it keeps a leading `const NV_EPS = …`; the fused path splits into
/// fns, so without carrying the prelude it would silently drop those decls and
/// the kernel would fail to compile (`no definition in scope`). WGSL has no
/// nested fns, so a column-0 `fn ` reliably starts each definition; a block runs
/// until the next column-0 `fn `. Leading comments / blank lines are dropped
/// (only real declarations are carried).
fn split_fns(fragment: &str) -> (Vec<String>, Vec<FnBlock>) {
    let mut prelude: Vec<String> = Vec::new();
    let mut blocks: Vec<FnBlock> = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    // >0 while inside a MULTI-LINE top-level declaration (a `const` lookup
    // table spanning lines — simplex gradient arrays): the count of unclosed
    // `(`/`[`/`{` carried over, so continuation lines join the prelude instead
    // of being dropped (which would truncate the declaration mid-expression).
    let mut open_brackets = 0i32;
    for line in fragment.lines() {
        if let Some(rest) = line.strip_prefix("fn ") {
            if let Some((name, lines)) = current.take() {
                blocks.push(FnBlock { name, text: lines.join("\n") });
            }
            let name = rest.split('(').next().unwrap_or("").trim().to_string();
            current = Some((name, vec![line.to_string()]));
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line.to_string());
        } else if open_brackets > 0 || is_top_level_decl(line) {
            // One prelude ENTRY per declaration (continuation lines append to
            // the open entry) so the fused paths' dedup-by-entry compares whole
            // declarations — line-wise entries would dedup a shared `);` line
            // across two different lookup tables and truncate the second.
            if open_brackets > 0 {
                let entry = prelude.last_mut().expect("open declaration has an entry");
                entry.push('\n');
                entry.push_str(line);
            } else {
                prelude.push(line.to_string());
            }
            open_brackets += line
                .chars()
                .map(|c| match c {
                    '(' | '[' | '{' => 1,
                    ')' | ']' | '}' => -1,
                    _ => 0,
                })
                .sum::<i32>();
        }
    }
    if let Some((name, lines)) = current.take() {
        blocks.push(FnBlock { name, text: lines.join("\n") });
    }
    (prelude, blocks)
}

/// Replace every whole-identifier occurrence of `from` with `to`. A match
/// counts only when neither neighbour is an identifier character, so renaming
/// `Element` never corrupts `Element2`. Used by the fused buffer codegen to map
/// each body's standalone element-struct names onto the region's global naming.
fn rename_ident(text: &str, from: &str, to: &str) -> String {
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(pos) = text[i..].find(from) {
        let start = i + pos;
        let end = start + from.len();
        out.push_str(&text[i..start]);
        let bounded = (start == 0 || !is_ident(bytes[start - 1]))
            && (end == bytes.len() || !is_ident(bytes[end]));
        out.push_str(if bounded { to } else { from });
        i = end;
    }
    out.push_str(&text[i..]);
    out
}

/// Whether a fragment line is a WGSL top-level declaration the fused prelude must
/// carry (vs a comment / blank line, which it drops).
fn is_top_level_decl(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("const ")
        || t.starts_with("struct ")
        || t.starts_with("alias ")
        || t.starts_with("override ")
}

/// Buffer-domain multi-atom fusion: chain a region of per-element (particle /
/// instance / curve-point) atom bodies into ONE `var<storage>` kernel. The
/// buffer analogue of [`generate_fused`]: pre-read each external array element
/// `[idx]` once, thread each body's output element as a register to the next,
/// write the region output array once. A 1D dispatch over the output array's
/// `arrayLength` (the convention `node.wgsl_compute` keys its buffer dispatch on
/// — NO `dispatch_count` uniform, unlike the standalone buffer path).
///
/// v1 scope — anything outside it returns `Err` so the card renders unfused
/// (always correct; the install pass also naga-parses the result as a final
/// guard): every member is a coincident per-element atom (no `BufferGather` —
/// those stay boundaries), each writes exactly ONE Array output, and its scalar
/// params are port-shadow uniforms. TEXTURE inputs fuse as gathered externals:
/// the kernel binds each as `src_<e>: texture_2d<f32>` plus one shared `samp`,
/// and the consuming body samples it at an element-computed coord — the same
/// `tex + samp` ABI the standalone buffer kernel passes, so the sample is
/// bit-identical (the `*_at_particles` force samplers, anti_clump's modulator).
fn generate_fused_buffer(region: &FusionRegion<'_>) -> Result<GeneratedFusion, CodegenError> {
    let index_of = |id: NodeInstanceId| region.nodes.iter().position(|n| n.node_id == id);
    let specs_of = |ty: &PortType| -> Option<&'static [ChannelSpec]> {
        match ty {
            PortType::Array(at) => Some(at.specs),
            _ => None,
        }
    };

    // Per-member: validate the v1 shape and capture each member's Array input
    // element specs (in node.inputs order) + its single Array output specs.
    struct MemberIo {
        in_specs: Vec<&'static [ChannelSpec]>,
        out_specs: &'static [ChannelSpec],
    }
    let mut member_io: Vec<MemberIo> = Vec::with_capacity(region.nodes.len());
    let mut includes: Vec<&'static str> = Vec::new(); // deduped shared WGSL libs (noise_common, …)
    for node in region.nodes.iter() {
        if !node.fusion_kind.is_fusable() {
            return Err(CodegenError::NotFusable(node.fusion_kind));
        }
        if node.body.is_empty() {
            return Err(CodegenError::NoBody);
        }
        for inc in node.node_includes {
            if !includes.contains(inc) {
                includes.push(inc);
            }
        }
        // Input shape: the finder resolves a buffer member's ARRAY inputs first
        // (coincident element registers), then appends its TEXTURE inputs as
        // gathered externals (the body samples each bound texture at an element-
        // computed coord — the buffer analogue of the texture path's sampler-
        // Gather; same `tex + samp` body-arg ABI the standalone buffer kernel
        // uses). A `BufferGather` array input (neighbor_smooth indexes its global
        // itself) still can't thread a register — boundary.
        let arr_in: Vec<&NodeInput> =
            node.node_inputs.iter().filter(|p| matches!(p.ty, PortType::Array(_))).collect();
        let tex_in: Vec<&NodeInput> =
            node.node_inputs.iter().filter(|p| is_texture_input(p)).collect();
        // node.inputs (the finder's resolved sources) must align 1:1 with the
        // member's Array inputs then its texture inputs — else an input is
        // unwired / mis-resolved.
        if node.inputs.len() != arr_in.len() + tex_in.len() {
            return Err(CodegenError::BadInput);
        }
        for (k, access) in node.input_access.iter().enumerate() {
            let is_texture_entry = k >= arr_in.len();
            // Array entries thread registers (never gather); texture entries are
            // always sampler-gathered externals.
            if is_texture_entry != access.is_gather() {
                return Err(CodegenError::BadInput);
            }
        }
        // Only plain 2D sampled textures: `node.wgsl_compute` rejects sampled 3D
        // textures at introspection, so emitting one would fail the whole build.
        if tex_in.iter().any(|p| p.ty != PortType::Texture2D) {
            return Err(CodegenError::BadInput);
        }
        let arr_out: Vec<&NodeOutput> =
            node.node_outputs.iter().filter(|p| matches!(p.ty, PortType::Array(_))).collect();
        if arr_out.len() != 1 || node.node_outputs.iter().any(|o| is_texture_port(&o.ty)) {
            return Err(CodegenError::BadInput); // v1: single Array output per member
        }
        let in_specs: Vec<&'static [ChannelSpec]> =
            arr_in.iter().map(|p| specs_of(&p.ty)).collect::<Option<_>>().ok_or(CodegenError::BadInput)?;
        let out_specs = specs_of(&arr_out[0].ty).ok_or(CodegenError::BadInput)?;
        member_io.push(MemberIo { in_specs, out_specs });
    }
    // v1: single output region (fan-out buffer regions are a follow-on).
    if region.outputs.len() != 1 {
        return Err(CodegenError::BadInput);
    }

    // Per-slot external kind: an ARRAY slot (read as a coincident element — its
    // element type comes from the consumer's array-input specs) or a TEXTURE
    // slot (bound as `src_<e>: texture_2d<f32>` + the shared `samp`, sampled by
    // the consuming bodies). Every external is read by ≥1 member (the finder
    // built the slot because a member reads it), so each resolves; one producer
    // port has one type, so a both-ways slot is a finder bug — fail closed.
    #[derive(Clone, Copy, PartialEq)]
    enum ExtKind {
        Array(&'static [ChannelSpec]),
        Texture,
    }
    let mut ext_kinds: Vec<Option<ExtKind>> = vec![None; region.num_external_inputs];
    for (mi, node) in region.nodes.iter().enumerate() {
        let arr_count = member_io[mi].in_specs.len();
        for (k, src) in node.inputs.iter().enumerate() {
            if let InputSource::External(e) = src {
                if *e >= region.num_external_inputs {
                    return Err(CodegenError::BadInput);
                }
                let kind = if k < arr_count {
                    ExtKind::Array(member_io[mi].in_specs[k])
                } else {
                    ExtKind::Texture
                };
                match ext_kinds[*e] {
                    Some(existing) if existing != kind => return Err(CodegenError::BadInput),
                    _ => ext_kinds[*e] = Some(kind),
                }
            }
        }
    }
    let ext_kinds: Vec<ExtKind> =
        ext_kinds.into_iter().collect::<Option<_>>().ok_or(CodegenError::BadInput)?;

    // --- element structs (deduped, first-appearance order across all I/O). The
    // fused output is a FRESH write-only `dst` array (not aliased onto an input):
    // its element type is the output member's Array output type. ---
    let mut structs: Vec<(&'static [ChannelSpec], String)> = Vec::new();
    // Element type per ARRAY slot (`None` for texture slots — no element struct).
    let ext_tys: Vec<Option<String>> = ext_kinds
        .iter()
        .map(|k| match k {
            ExtKind::Array(specs) => Some(buffer_element_type(specs, &mut structs)),
            ExtKind::Texture => None,
        })
        .collect();
    let out_member = index_of(region.outputs[0]).ok_or(CodegenError::BadInput)?;
    let out_ty = buffer_element_type(member_io[out_member].out_specs, &mut structs);

    // --- bodies: split into prelude / helpers / the n{i}_body fn (same as the
    // texture path), reconciling element STRUCT NAMES first. Each body's
    // `Element*` references use its STANDALONE naming — first-appearance order
    // over that atom's own array inputs then output — while the region has one
    // GLOBAL naming (external slots, then the output, then intermediates). Two
    // members can even permute the same names (a vec2-input atom calls vec2
    // `Element` and Particle `Element2`; a Particle-input neighbour the
    // reverse), so each member's body is rewritten local → global through
    // placeholders. This walk also registers every intermediate register type
    // (a member output that is neither an external's nor the region output's
    // type) so its struct definition is emitted. ---
    let mut prelude: Vec<String> = Vec::new();
    let mut helpers: Vec<FnBlock> = Vec::new();
    let mut bodies: Vec<String> = Vec::new();
    for (i, node) in region.nodes.iter().enumerate() {
        let mut local: Vec<(&'static [ChannelSpec], String)> = Vec::new();
        let mut renames: Vec<(String, String)> = Vec::new();
        let io = &member_io[i];
        for specs in io.in_specs.iter().copied().chain(std::iter::once(io.out_specs)) {
            if specs.len() < 2 {
                continue; // bare scalar/vector element — no struct, no name
            }
            let l = buffer_element_type(specs, &mut local);
            let g = buffer_element_type(specs, &mut structs);
            if l != g && !renames.iter().any(|(from, _)| *from == l) {
                renames.push((l, g));
            }
        }
        let mut text = node.body.to_string();
        for (k, (from, _)) in renames.iter().enumerate() {
            text = rename_ident(&text, from, &format!("__FUSED_EL{k}__"));
        }
        for (k, (_, to)) in renames.iter().enumerate() {
            text = rename_ident(&text, &format!("__FUSED_EL{k}__"), to);
        }

        let (pre, blocks) = split_fns(&text);
        for line in pre {
            if !prelude.contains(&line) {
                prelude.push(line);
            }
        }
        let mut found_body = false;
        for fb in blocks {
            if fb.name == "body" {
                bodies.push(fb.text.replacen("fn body(", &format!("fn n{i}_body("), 1));
                found_body = true;
            } else {
                match helpers.iter().find(|h| h.name == fb.name) {
                    Some(existing) if existing.text == fb.text => {}
                    Some(_) => return Err(CodegenError::HelperCollision(fb.name)),
                    None => helpers.push(fb),
                }
            }
        }
        if !found_body {
            return Err(CodegenError::NoBody);
        }
    }

    // --- merged param uniform (node-namespaced scalar fields, padded to 16). ---
    let mut param_order: Vec<(NodeInstanceId, &'static str)> = Vec::new();
    let mut struct_body = String::new();
    let mut field_count = 0usize;
    for (i, node) in region.nodes.iter().enumerate() {
        for p in node.params {
            let ty = param_wgsl_type(p)?;
            writeln!(struct_body, "    n{i}_{}: {ty},", p.name).unwrap();
            param_order.push((node.node_id, p.name));
            field_count += 1;
        }
    }
    // Frame-derived uniform fields (`dt_scaled`, `frame_count:u32`, …) per member,
    // after the params — mirrors the standalone path's emission, namespaced
    // `n{i}_<name>`. NOT added to `param_order`: their per-frame value comes from a
    // wire the install pass adds (system.generator_input → this field's port-
    // shadow), not from inst.params. wgsl_compute port-shadows every uniform field,
    // so each becomes an optional ScalarF32 input the installer wires.
    for (i, node) in region.nodes.iter().enumerate() {
        for d in node.derived_uniforms {
            let (dname, dty) = d.split_once(':').unwrap_or((d, "f32"));
            if dty == "vec3" {
                // A vec3 derived field expands to three f32 fields (matches the
                // standalone packing). No buffer atom that fuses needs one yet
                // (camera-basis atoms are gather/atomic boundaries), but keep the
                // shape consistent so a future one codegens correctly.
                writeln!(struct_body, "    n{i}_{dname}_x: f32,").unwrap();
                writeln!(struct_body, "    n{i}_{dname}_y: f32,").unwrap();
                writeln!(struct_body, "    n{i}_{dname}_z: f32,").unwrap();
                field_count += 3;
            } else {
                writeln!(struct_body, "    n{i}_{dname}: {dty},").unwrap();
                field_count += 1;
            }
        }
    }
    let pad_words = (4 - (field_count % 4)) % 4;
    for k in 0..pad_words {
        writeln!(struct_body, "    _pad{k}: u32,").unwrap();
    }
    if field_count == 0 {
        struct_body.push_str("    _pad0: u32,\n    _pad1: u32,\n    _pad2: u32,\n    _pad3: u32,\n");
    }

    let mut out = String::new();
    // element structs first (a struct may be referenced by a binding below).
    for (specs, name) in &structs {
        out.push_str(&emit_buffer_struct(specs, name));
        out.push('\n');
    }
    out.push_str("struct Params {\n");
    out.push_str(&struct_body);
    out.push_str("}\n\n");

    // --- bindings: uniform(0), then the external arrays. Two output models:
    //
    // FRESH (in_place_alias = None, the default): every external binds READ-ONLY
    // (forward deps, correctly ordered after their producers) + a FRESH write-only
    // `dst` tagged `// @fused_output`. WGSL has no write-only storage mode, so dst
    // is read_write but the marker makes node.wgsl_compute treat it as output-only
    // (not aliased) and the loader allocates it fresh. This avoids the aliased-
    // output ordering bug — correct for any FORWARD-produced region (DigitalPlants).
    //
    // IN-PLACE (in_place_alias = Some(k)): the region writes back to external
    // `src_k` — the loop buffer of an `array_feedback` in-place feedback loop. That
    // input binds READ_WRITE (no @fused_output), so node.wgsl_compute sees it
    // read+written → an aliased in/out pair → the loader keeps `in==out` ONE buffer,
    // preserving the loop's in-place contract (without the ordering bug: a loop
    // buffer has no forward producer to mis-order against). The install pass only
    // sets this when the output genuinely aliases a feedback-loop input.
    let in_place = region.in_place_alias;
    if let Some(k) = in_place {
        // Validity: the aliased input must exist, be an ARRAY slot, and carry the
        // output's element type (it IS the output buffer). Install guarantees
        // this; guard anyway.
        if ext_tys.get(k).and_then(|t| t.as_deref()) != Some(out_ty.as_str()) {
            return Err(CodegenError::BadInput);
        }
    }
    out.push_str("@group(0) @binding(0) var<uniform> params: Params;\n");
    let mut binding = 1u32;
    for (e, ty) in ext_tys.iter().enumerate() {
        match ty {
            Some(ty) => {
                let access = if in_place == Some(e) { "read_write" } else { "read" };
                writeln!(
                    out,
                    "@group(0) @binding({binding}) var<storage, {access}> src_{e}: array<{ty}>;"
                )
                .unwrap();
            }
            // A gathered texture external: bound + sampled by the consuming
            // bodies at element-computed coords, via the shared `samp` below.
            None => {
                writeln!(out, "@group(0) @binding({binding}) var src_{e}: texture_2d<f32>;")
                    .unwrap();
            }
        }
        binding += 1;
    }
    // The shared gather sampler, when any texture external exists. The default
    // "clamp" emits no marker — byte-identical to the standalone buffer atoms'
    // default sampler; a non-default mode rides the same side-channel marker the
    // texture path uses (`node.wgsl_compute` reads it at sampler creation).
    if ext_kinds.contains(&ExtKind::Texture) {
        if region.sampler_address_mode == "clamp" {
            writeln!(out, "@group(0) @binding({binding}) var samp: sampler;").unwrap();
        } else {
            writeln!(
                out,
                "@group(0) @binding({binding}) var samp: sampler; // @sampler_address_mode: {}",
                region.sampler_address_mode
            )
            .unwrap();
        }
        binding += 1;
    }
    if in_place.is_none() {
        out.push_str("// @fused_output\n");
        writeln!(
            out,
            "@group(0) @binding({binding}) var<storage, read_write> dst: array<{out_ty}>;"
        )
        .unwrap();
    }
    // `// @dispatch_count_param: <field>` — node.wgsl_compute reads this marker
    // and sizes the dispatch grid from the named uniform field's live value
    // (min'd with capacity) instead of the array length. cs_main carries the
    // matching in-kernel guard.
    if let Some((mi, pname)) = region.dispatch_count_field {
        writeln!(out, "// @dispatch_count_param: n{mi}_{pname}").unwrap();
    }
    out.push('\n');

    // --- shared library includes (noise_common, …), prepended so the bodies'
    // helper calls resolve — the deduped union across the region's members. ---
    for inc in &includes {
        out.push_str(inc.trim_end());
        out.push_str("\n\n");
    }

    // --- shared prelude, helpers, namespaced bodies ---
    for line in &prelude {
        out.push_str(line);
        out.push('\n');
    }
    if !prelude.is_empty() {
        out.push('\n');
    }
    for h in &helpers {
        out.push_str(h.text.trim_end());
        out.push_str("\n\n");
    }
    for b in &bodies {
        out.push_str(b.trim_end());
        out.push_str("\n\n");
    }

    // --- cs_main: 1D element dispatch. Count from an INPUT array (src_0) — it's
    // coincident with the output (one output element per input element), and src_0
    // is a plain read input so it never re-introduces a read on dst. Pre-read each
    // external element [idx] once, thread each body's output element register,
    // write the result to the fresh dst once. ---
    out.push_str("@compute @workgroup_size(256)\n");
    out.push_str("fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {\n");
    out.push_str("    let idx = gid.x;\n");
    // Count anchor: the first ARRAY external (slot 0 in every all-array region,
    // keeping prior fused WGSL — a pipeline-cache key — byte-identical). It's
    // coincident with the output (one output element per input element); a
    // texture slot has no arrayLength, so anchor past any leading texture slot.
    let count_anchor = ext_tys
        .iter()
        .position(|t| t.is_some())
        .ok_or(CodegenError::BadInput)?;
    writeln!(out, "    let count = arrayLength(&src_{count_anchor});").unwrap();
    out.push_str("    if idx >= count {\n        return;\n    }\n");
    // Live-count cap (in-place loop regions): early-return past the members'
    // shared `active_count`, leaving the pool tail untouched exactly like the
    // standalone integrators. The marker (emitted with the bindings above —
    // see `dispatch_count_field`) also shrinks the GRID, so the guard here is
    // the correctness half and the marker is the perf half.
    if let Some((mi, pname)) = region.dispatch_count_field {
        let node = region.nodes.get(mi).ok_or(CodegenError::BadInput)?;
        let p = node
            .params
            .iter()
            .find(|p| p.name == pname)
            .ok_or(CodegenError::BadInput)?;
        let zero = match param_wgsl_type(p)? {
            "f32" => "0.0",
            "i32" => "0",
            _ => return Err(CodegenError::BadInput),
        };
        writeln!(
            out,
            "    if idx >= u32(max(params.n{mi}_{pname}, {zero})) {{\n        return;\n    }}"
        )
        .unwrap();
    }
    // Pre-read each ARRAY external's element `[idx]` once; texture externals are
    // sampled by the bodies themselves (never pre-read — a register is one
    // element, not a whole texture).
    for (e, ty) in ext_tys.iter().enumerate() {
        if ty.is_some() {
            writeln!(out, "    let e_{e} = src_{e}[idx];").unwrap();
        }
    }
    for (i, node) in region.nodes.iter().enumerate() {
        let arr_count = member_io[i].in_specs.len();
        let mut args: Vec<String> = vec!["idx".to_string(), "count".to_string()];
        for (k, src) in node.inputs.iter().enumerate() {
            if k >= arr_count {
                // A gathered texture input: the body receives the bound texture
                // + the shared sampler and samples it at an element-computed
                // coord (same ABI as the standalone buffer kernel). Always an
                // external — members never produce textures, and the finder
                // never admits an unwired one.
                let InputSource::External(e) = src else {
                    return Err(CodegenError::BadInput);
                };
                args.push(format!("src_{e}"));
                args.push("samp".to_string());
                continue;
            }
            match src {
                InputSource::External(e) => args.push(format!("e_{e}")),
                InputSource::Node(id) => {
                    let Some(j) = index_of(*id) else {
                        return Err(CodegenError::BadInput);
                    };
                    if j >= i {
                        return Err(CodegenError::BadInput); // not earlier in topo order
                    }
                    args.push(format!("r{j}"));
                }
                // Optional-unwired is a texture-domain contract (use-flag bodies);
                // no buffer ARRAY input fuses unwired, so reaching here is a
                // finder bug.
                InputSource::Unwired => return Err(CodegenError::BadInput),
            }
        }
        for p in node.params {
            args.push(format!("params.n{i}_{}", p.name));
        }
        // Frame-derived uniforms trail the params (same body-arg order the
        // standalone path uses); a vec3 is reassembled from its three f32 fields.
        for d in node.derived_uniforms {
            let (dname, dty) = d.split_once(':').unwrap_or((d, "f32"));
            if dty == "vec3" {
                args.push(format!(
                    "vec3<f32>(params.n{i}_{dname}_x, params.n{i}_{dname}_y, params.n{i}_{dname}_z)"
                ));
            } else {
                args.push(format!("params.n{i}_{dname}"));
            }
        }
        // Optional-texture use flags, last (matching the standalone body ABI).
        // Wiring is static in the def and the finder only admits WIRED textures,
        // so each flag folds to the literal `1u` instead of a uniform field.
        for _ in node.node_inputs.iter().filter(|p| is_texture_input(p) && !p.required) {
            args.push("1u".to_string());
        }
        writeln!(out, "    let r{i} = n{i}_body({});", args.join(", ")).unwrap();
    }
    // Write the region result. IN-PLACE: back into the aliased loop buffer
    // `src_k` (read+write makes it an aliased in/out pair). FRESH: the separate
    // `dst` array.
    match in_place {
        Some(k) => writeln!(out, "    src_{k}[idx] = r{out_member};").unwrap(),
        None => writeln!(out, "    dst[idx] = r{out_member};").unwrap(),
    }
    out.push_str("}\n");

    Ok(GeneratedFusion { wgsl: out, param_order })
}

/// Generate one fused kernel for a region. Errors if a node isn't fusable, a
/// body lacks `fn body`, an input references an unknown/later node, or two
/// helpers share a name with different bodies (un-dedupable collision).
pub fn generate_fused(region: &FusionRegion<'_>) -> Result<GeneratedFusion, CodegenError> {
    // BUFFER-domain region (the members' output is an Array<T>, not a texture):
    // route to the buffer multi-atom path — `var<storage>` bindings, a 1D
    // particle/element dispatch, element structs threaded as registers. A region
    // is homogeneous (texture and Array wires never connect — different port
    // types), so the output member's output kind decides the whole region.
    if region
        .nodes
        .iter()
        .any(|n| n.node_outputs.iter().any(|o| matches!(o.ty, PortType::Array(_))))
    {
        return generate_fused_buffer(region);
    }
    // node_id -> region index (for resolving InputSource::Node to a register).
    let index_of = |id: NodeInstanceId| region.nodes.iter().position(|n| n.node_id == id);

    // Per-node: split body into a top-level prelude (consts/structs), helpers,
    // and the `body` fn (renamed n{i}_body).
    let mut prelude: Vec<String> = Vec::new(); // deduped top-level decls, emitted once
    let mut helpers: Vec<FnBlock> = Vec::new(); // deduped, emitted once
    let mut bodies: Vec<String> = Vec::new(); // namespaced body fns
    for (i, node) in region.nodes.iter().enumerate() {
        if !node.fusion_kind.is_fusable() {
            return Err(CodegenError::NotFusable(node.fusion_kind));
        }
        if node.body.is_empty() {
            return Err(CodegenError::NoBody);
        }
        let (pre, blocks) = split_fns(node.body);
        for line in pre {
            // Dedup identical declarations (two atoms declaring the same const
            // collapse to one). A same-name / different-value clash would surface
            // as a naga redefinition error, caught by the oracle — never silent.
            if !prelude.contains(&line) {
                prelude.push(line);
            }
        }
        let mut found_body = false;
        for fb in blocks {
            if fb.name == "body" {
                // rename the single definition `fn body(` -> `fn n{i}_body(`
                bodies.push(fb.text.replacen("fn body(", &format!("fn n{i}_body("), 1));
                found_body = true;
            } else {
                // dedup helper by name; identical content collapses, divergent
                // content is an un-fusable collision.
                match helpers.iter().find(|h| h.name == fb.name) {
                    Some(existing) if existing.text == fb.text => {}
                    Some(_) => return Err(CodegenError::HelperCollision(fb.name)),
                    None => helpers.push(fb),
                }
            }
        }
        if !found_body {
            return Err(CodegenError::NoBody);
        }
    }

    // --- merged param uniform (node-namespaced scalar fields, padded to 16). ---
    let mut param_order: Vec<(NodeInstanceId, &'static str)> = Vec::new();
    let mut struct_body = String::new();
    let mut field_count = 0usize;
    for (i, node) in region.nodes.iter().enumerate() {
        for p in node.params {
            let ty = param_wgsl_type(p)?;
            writeln!(struct_body, "    n{i}_{}: {ty},", p.name).unwrap();
            param_order.push((node.node_id, p.name));
            field_count += 1;
        }
    }
    let pad_words = (4 - (field_count % 4)) % 4;
    for k in 0..pad_words {
        writeln!(struct_body, "    _pad{k}: u32,").unwrap();
    }
    if field_count == 0 {
        struct_body.push_str("    _pad0: u32,\n    _pad1: u32,\n    _pad2: u32,\n    _pad3: u32,\n");
    }

    let mut out = String::new();
    out.push_str("struct Params {\n");
    out.push_str(&struct_body);
    out.push_str("}\n\n");

    // --- which inputs are gathered (the body samples a bound texture itself) vs
    // coincident (a register threaded in). A sampler is bound iff some input is a
    // sampler-`Gather`; an external is pre-read into a register only if some node
    // reads it coincidentally — a gather-only external is never load-into-register
    // (the body samples it at a coord it computes). ---
    let mut needs_sampler = false;
    let mut coincident_ext: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for node in &region.nodes {
        for (idx, src) in node.inputs.iter().enumerate() {
            let access = node.input_access.get(idx).copied().unwrap_or(InputAccess::Coincident);
            if access == InputAccess::Gather {
                needs_sampler = true;
            }
            if !access.is_gather()
                && let InputSource::External(e) = src
            {
                coincident_ext.insert(*e);
            }
        }
    }

    // --- bindings: uniform(0), external input textures(1..), [sampler], output. ---
    out.push_str("@group(0) @binding(0) var<uniform> params: Params;\n");
    for e in 0..region.num_external_inputs {
        writeln!(out, "@group(0) @binding({}) var src_{e}: texture_2d<f32>;", e + 1).unwrap();
    }
    let mut next_binding = region.num_external_inputs + 1;
    if needs_sampler {
        // The shared gather sampler. A non-default address mode (a toroidal
        // Repeat gradient) is carried as a marker `node.wgsl_compute` reads to
        // create the sampler at that mode — WGSL has no address mode in the
        // shader, so the runtime sampler descriptor needs this side channel. The
        // default `"clamp"` emits no marker, keeping all-clamp regions (every
        // shipped fusion to date) byte-identical to the prior codegen.
        if region.sampler_address_mode == "clamp" {
            writeln!(out, "@group(0) @binding({next_binding}) var samp: sampler;").unwrap();
        } else {
            writeln!(
                out,
                "@group(0) @binding({next_binding}) var samp: sampler; // @sampler_address_mode: {}",
                region.sampler_address_mode
            )
            .unwrap();
        }
        next_binding += 1;
    }
    // Output storage texture(s). A single-output region keeps the binding named
    // `dst` so its generated WGSL — and the cross-session pipeline-cache key — is
    // byte-identical to the v1 codegen. A FAN-OUT region (an interior member feeds
    // two distinct downstream boundaries) names each `dst_<k>` in `outputs` order;
    // the install pass wires each to that escaping member's live consumers, so no
    // store ever lands on an unallocated output (which would early-return the whole
    // `WgslCompute` dispatch). All outputs are coincident (same canvas), so any one
    // gives the dispatch dims.
    // Each output texture is declared at ITS member's storage format (the
    // `outputFormats` fp32 override else rgba16float) so a fused region honours an
    // fp32 output exactly as the unfused chain would — full-precision in-loop
    // fusion. Falls back to rgba16float if the output id isn't a member (defensive).
    let out_storage = |out_id: NodeInstanceId| -> &'static str {
        index_of(out_id)
            .map(|j| region.nodes[j].output_storage)
            .unwrap_or("rgba16float")
    };
    let multi_output = region.outputs.len() > 1;
    if multi_output {
        for (k, &out_id) in region.outputs.iter().enumerate() {
            writeln!(
                out,
                "@group(0) @binding({}) var dst_{k}: texture_storage_2d<{}, write>;",
                next_binding + k,
                out_storage(out_id)
            )
            .unwrap();
        }
    } else {
        writeln!(
            out,
            "@group(0) @binding({next_binding}) var dst: texture_storage_2d<{}, write>;",
            out_storage(region.outputs[0])
        )
        .unwrap();
    }
    out.push('\n');

    // --- shared prelude (deduped top-level consts/structs the bodies declare),
    // then deduped helpers, then namespaced bodies ---
    for line in &prelude {
        out.push_str(line);
        out.push('\n');
    }
    if !prelude.is_empty() {
        out.push('\n');
    }
    // f16-faithful rounding helper (stencil tier A), emitted once when any
    // member needs it. pack2x16float rounds each f32 to IEEE half (RTNE) —
    // exactly what an rgba16float textureStore would do — and unpack restores
    // the rounded value, so an in-loop member's fused register matches the
    // unfused chain's store+load bit-for-bit.
    if region.nodes.iter().any(|n| n.quantize_f16) {
        out.push_str(
            "fn q16(v: vec4<f32>) -> vec4<f32> {\n    \
             return vec4<f32>(unpack2x16float(pack2x16float(v.xy)), \
             unpack2x16float(pack2x16float(v.zw)));\n}\n\n",
        );
    }
    for h in &helpers {
        out.push_str(h.text.trim_end());
        out.push_str("\n\n");
    }
    for b in &bodies {
        out.push_str(b.trim_end());
        out.push_str("\n\n");
    }

    // --- cs_main: read external inputs once, thread registers, store output ---
    out.push_str("@compute @workgroup_size(16, 16)\n");
    out.push_str("fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {\n");
    let dims_tex = if multi_output { "dst_0" } else { "dst" };
    writeln!(out, "    let dims = textureDimensions({dims_tex});").unwrap();
    out.push_str("    if id.x >= dims.x || id.y >= dims.y {\n        return;\n    }\n");
    out.push_str("    let coord = vec2<i32>(i32(id.x), i32(id.y));\n");
    // Ambient fragment context, computed once and threaded to every body after
    // its inputs (matches the standalone wrapper). `dims` is already bound above.
    out.push_str("    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);\n");
    for &e in &coincident_ext {
        writeln!(out, "    let ext_{e} = textureLoad(src_{e}, coord, 0);").unwrap();
    }
    for (i, node) in region.nodes.iter().enumerate() {
        let mut args: Vec<String> = Vec::new();
        for (idx, src) in node.inputs.iter().enumerate() {
            let access = node.input_access.get(idx).copied().unwrap_or(InputAccess::Coincident);
            match (access, src) {
                // A gathered input: the body samples the bound texture itself at a
                // coord it computes, so it gets the texture handle (+ the shared
                // sampler for the sampler-Gather flavour). It MUST read an external
                // — a register is one texel, not a whole texture — which the finder
                // guarantees by never unioning across a gather-consumed wire.
                (InputAccess::Gather, InputSource::External(e)) => {
                    if *e >= region.num_external_inputs {
                        return Err(CodegenError::BadInput);
                    }
                    args.push(format!("src_{e}"));
                    args.push("samp".to_string());
                }
                (InputAccess::GatherTexel, InputSource::External(e)) => {
                    if *e >= region.num_external_inputs {
                        return Err(CodegenError::BadInput);
                    }
                    args.push(format!("src_{e}"));
                }
                // A gather reading a region register can't be expressed, and a
                // gather needs a real texture to sample — unwired can't fuse
                // (the finder already keeps such a member out of regions).
                (
                    InputAccess::Gather | InputAccess::GatherTexel,
                    InputSource::Node(_) | InputSource::Unwired,
                ) => {
                    return Err(CodegenError::BadInput);
                }
                // Optional input with no wire: the body's injected use flag (the
                // literal `0u`, pushed after params below) gates the read off, so
                // any well-typed value works. Mirrors run()'s dummy-texture bind.
                (_, InputSource::Unwired) => {
                    args.push("vec4<f32>(0.0)".to_string());
                }
                // Coincident / texel: the pre-read external register, or an earlier
                // node's threaded register.
                (_, InputSource::External(e)) => {
                    if *e >= region.num_external_inputs {
                        return Err(CodegenError::BadInput);
                    }
                    args.push(format!("ext_{e}"));
                }
                (_, InputSource::Node(id)) => {
                    let Some(j) = index_of(*id) else {
                        return Err(CodegenError::BadInput);
                    };
                    if j >= i {
                        return Err(CodegenError::BadInput); // not earlier in topo order
                    }
                    args.push(format!("r{j}"));
                }
            }
        }
        args.push("uv".to_string());
        args.push("vec2<f32>(dims)".to_string());
        for p in node.params {
            args.push(format!("params.n{i}_{}", p.name));
        }
        // Optional-input use flags trail the params — the same body-arg order the
        // standalone wrapper uses (`params.use_<name>` there). Wiring is static in
        // the def, so each flag folds to a literal here. `node_inputs` describes
        // the texture ports when install built the region; synthetic test regions
        // leave it empty → no flags pushed, generated WGSL byte-identical.
        let tex_specs: Vec<&NodeInput> =
            node.node_inputs.iter().filter(|p| is_texture_input(p)).collect();
        if tex_specs.len() == node.inputs.len() {
            for (idx, spec) in tex_specs.iter().enumerate() {
                if !spec.required {
                    let wired = !matches!(node.inputs[idx], InputSource::Unwired);
                    args.push(if wired { "1u" } else { "0u" }.to_string());
                }
            }
        } else if node.inputs.iter().any(|s| matches!(s, InputSource::Unwired)) {
            // An unwired optional without port specs to read optionality from
            // can't satisfy the body's use-flag signature — don't miscompile.
            return Err(CodegenError::BadInput);
        }
        if node.quantize_f16 {
            // In-loop f16 member: reproduce the unfused chain's rgba16float
            // store rounding so the loop can't drift fused-vs-unfused.
            writeln!(out, "    let r{i} = q16(n{i}_body({}));", args.join(", ")).unwrap();
        } else {
            writeln!(out, "    let r{i} = n{i}_body({});", args.join(", ")).unwrap();
        }
    }
    if multi_output {
        // Fan-out: each escaping member's register stores to its own `dst_<k>`,
        // in `outputs` order (every one wired to a live consumer by install).
        for (k, out_id) in region.outputs.iter().enumerate() {
            let Some(idx) = index_of(*out_id) else {
                return Err(CodegenError::BadInput);
            };
            writeln!(out, "    textureStore(dst_{k}, coord, r{idx});").unwrap();
        }
    } else {
        let Some(out_idx) = region.outputs.first().and_then(|id| index_of(*id)) else {
            return Err(CodegenError::BadInput);
        };
        writeln!(out, "    textureStore(dst, coord, r{out_idx});").unwrap();
    }
    out.push_str("}\n");

    Ok(GeneratedFusion { wgsl: out, param_order })
}

#[cfg(test)]
mod gpu_tests {
    use super::*;
    use crate::node_graph::effect_node::EffectNode;
    use crate::node_graph::freeze::TextureDiff;
    use crate::node_graph::primitives::Gain;
    use crate::render_target::RenderTarget;
    use half::f16;
    use manifold_gpu::{
        GpuBinding, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
        GpuTextureFormat, GpuTextureUsage,
    };

    const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

    fn gradient(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = f16::from_f32(x as f32 / w as f32);
                px[i + 1] = f16::from_f32(y as f32 / h as f32);
                px[i + 2] = f16::from_f32(0.5);
                px[i + 3] = f16::from_f32(1.0);
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: FMT,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "codegen-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// Dispatch a coincident two-input kernel: uniform(0), a(1), b(2),
    /// sampler(3), dst(4). `param_bytes` is the 16-byte uniform payload.
    fn dispatch_coincident(
        device: &GpuDevice,
        wgsl: &str,
        a: &GpuTexture,
        b: &GpuTexture,
        param_bytes: &[u8],
    ) -> RenderTarget {
        let (w, h) = (a.width, a.height);
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-test-mix");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let out = RenderTarget::new(device, w, h, FMT, "codegen-out-mix");
        let mut enc = device.create_encoder("codegen-test-mix");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: param_bytes },
                GpuBinding::Texture { binding: 1, texture: a },
                GpuBinding::Texture { binding: 2, texture: b },
                GpuBinding::Sampler { binding: 3, sampler: &sampler },
                GpuBinding::Texture { binding: 4, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "codegen-test-mix",
        );
        enc.commit_and_wait_completed();
        out
    }

    /// A second gradient with a different layout, so a + b differ per texel
    /// (so the blend + crossfade is actually exercised).
    fn gradient_b(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                px[i] = f16::from_f32(0.8 - 0.6 * (x as f32 / w as f32));
                px[i + 1] = f16::from_f32(0.2);
                px[i + 2] = f16::from_f32(y as f32 / h as f32);
                px[i + 3] = f16::from_f32(0.5);
            }
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: FMT,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "codegen-input-b",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// Dispatch a standard pointwise kernel: uniform(0), src(1), sampler(2),
    /// dst(3). `param_bytes` is the 16-byte uniform payload.
    fn dispatch_pointwise(
        device: &GpuDevice,
        wgsl: &str,
        input: &GpuTexture,
        param_bytes: &[u8],
    ) -> RenderTarget {
        let (w, h) = (input.width, input.height);
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-test");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let out = RenderTarget::new(device, w, h, FMT, "codegen-out");
        let mut enc = device.create_encoder("codegen-test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: param_bytes },
                GpuBinding::Texture { binding: 1, texture: input },
                GpuBinding::Sampler { binding: 2, sampler: &sampler },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "codegen-test",
        );
        enc.commit_and_wait_completed();
        out
    }

    /// Determinism (design §12.3): the generator emits byte-identical WGSL
    /// across calls — the cross-session pipeline-cache key depends on it.
    #[test]
    fn generated_wgsl_is_deterministic() {
        let g = Gain::new();
        let body = g.wgsl_body().unwrap();
        let a = generate_standalone(g.fusion_kind(), body, g.inputs(), g.parameters(), g.input_access(), g.outputs()).unwrap();
        let b = generate_standalone(g.fusion_kind(), body, g.inputs(), g.parameters(), g.input_access(), g.outputs()).unwrap();
        assert_eq!(a, b, "codegen must be deterministic");
        assert!(a.contains("fn cs_main"), "must emit the cs_main entry");
        assert!(!a.contains("cs_main_"), "no symbol may have cs_main as a prefix");
    }

    /// Regression for the NV_EPS-class bug: a body declaring a top-level `const`
    /// before its `fn body` must carry that const into the fused kernel's shared
    /// prelude. The standalone path keeps it verbatim; the fused path splits into
    /// fns and would otherwise drop it (`no definition in scope`). Two atoms
    /// sharing the const emit it exactly once (deduped).
    #[test]
    fn fused_prelude_carries_and_dedups_top_level_consts() {
        use crate::node_graph::freeze::classify::FusionKind;
        let body = "const K: f32 = 0.25;\n\nfn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>) -> vec4<f32> {\n    return c * K;\n}\n";
        let id = NodeInstanceId;
        let region = FusionRegion {
            nodes: vec![
                RegionNode {
                    node_id: id(0),
                    fusion_kind: FusionKind::Pointwise,
                    body,
                    params: &[],
                    inputs: vec![InputSource::External(0)],
                    input_access: vec![],
                    node_inputs: &[],
                    node_outputs: &[],
                    node_includes: &[],
                    derived_uniforms: &[],
                    output_storage: "rgba16float",
                    quantize_f16: false,
                },
                RegionNode {
                    node_id: id(1),
                    fusion_kind: FusionKind::Pointwise,
                    body,
                    params: &[],
                    inputs: vec![InputSource::Node(id(0))],
                    input_access: vec![],
                    node_inputs: &[],
                    node_outputs: &[],
                    node_includes: &[],
                    derived_uniforms: &[],
                    output_storage: "rgba16float",
                    quantize_f16: false,
                },
            ],
            num_external_inputs: 1,
            outputs: vec![id(1)],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
        };
        let g = generate_fused(&region).expect("a region whose body declares a const fuses");
        assert_eq!(
            g.wgsl.matches("const K: f32 = 0.25;").count(),
            1,
            "the top-level const is carried into the fused kernel exactly once (deduped)"
        );
        assert!(g.wgsl.contains("fn n0_body"), "first body namespaced");
        assert!(g.wgsl.contains("fn n1_body"), "second body namespaced");
    }

    /// Buffer-domain multi-atom fusion: a chain of two per-element instance atoms
    /// fuses into one `var<storage>` kernel. The element struct is synthesized,
    /// every input and the output bind as storage arrays, the dispatch is a 1D
    /// `arrayLength`-guarded loop (the `node.wgsl_compute` buffer convention, with
    /// no `dispatch_count` uniform), the first body's element register threads
    /// into the second, and the shared `noise_common` include is prepended once
    /// (so parse resolves the helper calls; were the include dropped, naga parse
    /// would fail here). The buffer analogue of
    /// `fused_gather_binds_sampler_and_passes_texture`. End-to-end numerical
    /// parity rides the render-parity oracle once the finder emits buffer regions
    /// on the live path.
    #[test]
    fn fused_buffer_region_threads_element_registers() {
        use crate::node_graph::primitive::PrimitiveSpec;
        use crate::node_graph::primitives::InstanceRotationJitter as J;
        let id = NodeInstanceId;
        let mk = |i: u32, src: InputSource| RegionNode {
            node_id: id(i),
            fusion_kind: J::FUSION_KIND,
            body: J::WGSL_BODY.unwrap(),
            params: J::PARAMS,
            inputs: vec![src],
            input_access: J::INPUT_ACCESS.to_vec(),
            node_inputs: J::INPUTS,
            node_outputs: J::OUTPUTS,
            node_includes: J::WGSL_INCLUDES,
            derived_uniforms: J::DERIVED_UNIFORMS,
            output_storage: "rgba16float",
            quantize_f16: false,
        };
        let region = FusionRegion {
            nodes: vec![mk(0, InputSource::External(0)), mk(1, InputSource::Node(id(0)))],
            num_external_inputs: 1,
            outputs: vec![id(1)],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
        };
        let g = generate_fused(&region).expect("buffer region fuses");
        assert!(
            naga::front::wgsl::parse_str(&g.wgsl).is_ok(),
            "fused buffer kernel parses through naga (validates the body ABI + includes):\n{}",
            g.wgsl
        );
        // Inputs are read-only (forward deps); the output is a FRESH write-only
        // `dst` tagged `// @fused_output` (not aliased). This is what keeps the
        // node ordered after its producers.
        assert!(
            g.wgsl.contains("var<storage, read> src_0"),
            "external input bound read-only:\n{}",
            g.wgsl
        );
        assert!(g.wgsl.contains("// @fused_output"), "fresh output tagged @fused_output");
        assert!(
            g.wgsl.contains("var<storage, read_write> dst:"),
            "fresh dst output array declared:\n{}",
            g.wgsl
        );
        assert!(g.wgsl.contains("arrayLength(&src_0)"), "1D dispatch keyed on an input array length");
        assert!(g.wgsl.contains("let e_0 = src_0[idx];"), "external element pre-read once");
        assert!(g.wgsl.contains("let r0 = n0_body"), "first member's element register");
        assert!(g.wgsl.contains("let r1 = n1_body"), "second member threads r0");
        assert!(g.wgsl.contains("dst[idx] = r1;"), "region result written to the fresh output");
    }

    /// Tier 3 — a gather input binds a sampler and is passed to the body as a
    /// texture handle (the body samples it itself at a coord it computes), and is
    /// NOT pre-read into a register. sharpen (a Gather) → invert (Coincident): the
    /// kernel binds `samp`, calls `n0_body(src_0, samp, …)`, never emits
    /// `let ext_0`, and threads sharpen's register into invert.
    #[test]
    fn fused_gather_binds_sampler_and_passes_texture() {
        use crate::node_graph::freeze::classify::InputAccess;
        use crate::node_graph::primitive::PrimitiveSpec;
        use crate::node_graph::primitives::{Invert, Sharpen};
        let id = NodeInstanceId;
        let region = FusionRegion {
            nodes: vec![
                RegionNode {
                    node_id: id(0),
                    fusion_kind: Sharpen::FUSION_KIND,
                    body: Sharpen::WGSL_BODY.unwrap(),
                    params: Sharpen::PARAMS,
                    inputs: vec![InputSource::External(0)],
                    input_access: vec![InputAccess::Gather],
                    node_inputs: &[],
                    node_outputs: &[],
                    node_includes: &[],
                    derived_uniforms: &[],
                    output_storage: "rgba16float",
                    quantize_f16: false,
                },
                RegionNode {
                    node_id: id(1),
                    fusion_kind: Invert::FUSION_KIND,
                    body: Invert::WGSL_BODY.unwrap(),
                    params: Invert::PARAMS,
                    inputs: vec![InputSource::Node(id(0))],
                    input_access: vec![InputAccess::Coincident],
                    node_inputs: &[],
                    node_outputs: &[],
                    node_includes: &[],
                    derived_uniforms: &[],
                    output_storage: "rgba16float",
                    quantize_f16: false,
                },
            ],
            num_external_inputs: 1,
            outputs: vec![id(1)],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
        };
        let g = generate_fused(&region).expect("gather region fuses");
        assert!(g.wgsl.contains("var samp: sampler"), "a sampler is bound for the gather");
        assert!(
            g.wgsl.contains("n0_body(src_0, samp,"),
            "sharpen receives the texture + shared sampler and samples it itself"
        );
        assert!(
            !g.wgsl.contains("let ext_0 ="),
            "a gather-only external is never pre-read into a register"
        );
        assert!(g.wgsl.contains("fn n1_body"), "invert namespaced + threads sharpen's register");
    }

    /// Fan-out — a region with two escaping members emits two `dst_<k>` storage
    /// bindings and two `textureStore`s (one per output register), and takes its
    /// dispatch dims from `dst_0`. gain forks into invert (output 0) and contrast
    /// (output 1); both thread gain's register. The single-output path is
    /// unchanged (every other test asserts the byte-identical `var dst`).
    #[test]
    fn fused_fanout_emits_two_dst_bindings() {
        use crate::node_graph::primitive::PrimitiveSpec;
        use crate::node_graph::primitives::{Contrast, Gain, Invert};
        let id = NodeInstanceId;
        let region = FusionRegion {
            nodes: vec![
                RegionNode {
                    node_id: id(0),
                    fusion_kind: Gain::FUSION_KIND,
                    body: Gain::WGSL_BODY.unwrap(),
                    params: Gain::PARAMS,
                    inputs: vec![InputSource::External(0)],
                    input_access: vec![],
                    node_inputs: &[],
                    node_outputs: &[],
                    node_includes: &[],
                    derived_uniforms: &[],
                    output_storage: "rgba16float",
                    quantize_f16: false,
                },
                RegionNode {
                    node_id: id(1),
                    fusion_kind: Invert::FUSION_KIND,
                    body: Invert::WGSL_BODY.unwrap(),
                    params: Invert::PARAMS,
                    inputs: vec![InputSource::Node(id(0))],
                    input_access: vec![],
                    node_inputs: &[],
                    node_outputs: &[],
                    node_includes: &[],
                    derived_uniforms: &[],
                    output_storage: "rgba16float",
                    quantize_f16: false,
                },
                RegionNode {
                    node_id: id(2),
                    fusion_kind: Contrast::FUSION_KIND,
                    body: Contrast::WGSL_BODY.unwrap(),
                    params: Contrast::PARAMS,
                    inputs: vec![InputSource::Node(id(0))],
                    input_access: vec![],
                    node_inputs: &[],
                    node_outputs: &[],
                    node_includes: &[],
                    derived_uniforms: &[],
                    output_storage: "rgba16float",
                    quantize_f16: false,
                },
            ],
            num_external_inputs: 1,
            outputs: vec![id(1), id(2)],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
        };
        let g = generate_fused(&region).expect("fan-out region fuses");
        assert!(g.wgsl.contains("var dst_0:"), "first output binding");
        assert!(g.wgsl.contains("var dst_1:"), "second output binding");
        assert!(!g.wgsl.contains("var dst:"), "no single-output `dst` in a fan-out kernel");
        assert!(
            g.wgsl.contains("textureDimensions(dst_0)"),
            "dims come from the first output (all outputs are coincident)"
        );
        // invert = output 0 (register r1), contrast = output 1 (register r2).
        assert!(g.wgsl.contains("textureStore(dst_0, coord, r1)"), "invert → dst_0");
        assert!(g.wgsl.contains("textureStore(dst_1, coord, r2)"), "contrast → dst_1");
    }

    /// The generated standalone gain kernel reproduces the hand-written
    /// gain.wgsl — same math, same center-UV sampling, same f16 store — so it
    /// is a drop-in (single-source cutover, build step 1b). Both are single
    /// kernels reading the same input: diff directly via the oracle.
    #[test]
    fn generated_gain_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let input = gradient(&device, w, h);

        let g = Gain::new();
        let generated = generate_standalone(
            g.fusion_kind(),
            g.wgsl_body().unwrap(),
            g.inputs(),
            g.parameters(),
            g.input_access(),
            g.outputs(),
        )
        .expect("gain generates");
        let original = include_str!("../primitives/shaders/gain.wgsl");

        // uniform payload: gain = 1.7, then padding (matches both structs).
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&1.7f32.to_le_bytes());

        let from_original = dispatch_pointwise(&device, original, &input, &bytes);
        let from_generated = dispatch_pointwise(&device, &generated, &input, &bytes);

        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &from_original.texture, &from_generated.texture, 1e-5, 1e-5);
        assert_eq!(
            r.over_count, 0,
            "generated gain must reproduce gain.wgsl (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
        assert!(
            r.max_abs < 1e-5,
            "same math + sampling should be ~bit-identical, got max_abs={}",
            r.max_abs
        );
    }

    /// Pack f32 params into a 16-byte-multiple uniform payload.
    fn pack_f32(params: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for p in params {
            bytes.extend_from_slice(&p.to_le_bytes());
        }
        while bytes.len() % 16 != 0 {
            bytes.push(0);
        }
        bytes
    }

    /// 1b safety gate: every remaining pointwise ColorGrade atom's GENERATED
    /// standalone kernel reproduces its hand-written shader bit-for-bit (same
    /// math, same center-UV sampling). Once green, deleting the hand shaders
    /// (the single-source cutover) cannot change rendering. Originals read from
    /// disk so this test self-documents which shaders the cutover will retire.
    #[test]
    fn generated_pointwise_atoms_match_originals() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let input = gradient(&device, w, h);
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let shaders_dir =
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");

        // (type_id, original shader file, representative non-identity params).
        let cases: &[(&str, &str, &[f32])] = &[
            ("node.saturation", "saturation.wgsl", &[1.4]),
            ("node.hue_saturation", "hue_saturation.wgsl", &[30.0, 1.3, 0.9]),
            ("node.contrast", "contrast.wgsl", &[1.5]),
            ("node.colorize", "colorize.wgsl", &[0.5, 200.0, 0.7, 0.6]),
            ("node.clamp_texture", "clamp_texture.wgsl", &[0.1, 0.8]),
            // Vocabulary widening (design §12.3): pure-pointwise color/tone atoms
            // converted to single-source bodies. Partial invert exercises the
            // mix; levels uses the MetallicGlass height shape; posterize at 6.
            ("node.invert", "invert.wgsl", &[0.5]),
            ("node.levels", "levels.wgsl", &[1.26, 0.29, 0.0, 1.0, 0.8]),
            ("node.posterize", "posterize.wgsl", &[6.0]),
            // Positional atom: pixel = uv*dims is identical in both kernels on
            // the square test input, so the per-pixel hash matches bit-for-bit.
            ("node.film_grain", "film_grain.wgsl", &[0.3]),
            // Math/convert pointwise atoms (overnight vocabulary sweep).
            ("node.fract_texture", "fract_texture.wgsl", &[3.0]),
            ("node.power_texture", "power_texture.wgsl", &[2.5]),
            ("node.scale_offset_texture", "scale_offset_texture.wgsl", &[1.5, -0.25]),
            ("node.smoothstep_texture", "smoothstep_texture.wgsl", &[0.2, 0.8]),
            ("node.field_combine", "field_combine.wgsl", &[1.5, -0.5, 0.25]),
        ];
        let differ = TextureDiff::new(&device);
        for (type_id, shader_file, params) in cases {
            let node = registry.construct(type_id).unwrap();
            let generated = generate_standalone(
                node.fusion_kind(),
                node.wgsl_body().unwrap(),
                node.inputs(),
                node.parameters(),
                node.input_access(),
                node.outputs(),
            )
            .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
            let original = std::fs::read_to_string(format!("{shaders_dir}/{shader_file}"))
                .unwrap_or_else(|e| panic!("read {shader_file}: {e}"));
            let bytes = pack_f32(params);

            let from_original = dispatch_pointwise(&device, &original, &input, &bytes);
            let from_generated = dispatch_pointwise(&device, &generated, &input, &bytes);
            let r = differ.compare(
                &device,
                &from_original.texture,
                &from_generated.texture,
                1e-5,
                1e-5,
            );
            assert_eq!(
                r.over_count, 0,
                "{type_id}: generated kernel must reproduce {shader_file} \
                 (max_abs={}, max_rel={})",
                r.max_abs, r.max_rel
            );
        }
    }

    /// Dispatch a fused kernel: uniform(0), single external src(1), output at
    /// `dst_binding`. No sampler (fused kernels textureLoad — read once).
    fn dispatch_fused_kernel(
        device: &GpuDevice,
        wgsl: &str,
        input: &GpuTexture,
        param_bytes: &[u8],
        dst_binding: u32,
    ) -> RenderTarget {
        let (w, h) = (input.width, input.height);
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "fused-test");
        let out = RenderTarget::new(device, w, h, FMT, "fused-out");
        let mut enc = device.create_encoder("fused-test");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: param_bytes },
                GpuBinding::Texture { binding: 1, texture: input },
                GpuBinding::Texture { binding: dst_binding, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "fused-test",
        );
        enc.commit_and_wait_completed();
        out
    }

    /// THE step-3 headline: the multi-atom generator chains all 7 ColorGrade
    /// bodies into ONE kernel (register threading + the source->{chain, mix.a}
    /// fork + helper dedup + namespacing + merged uniform), and its output
    /// matches the hand-fused colorgrade_fused.wgsl bit-for-bit through the
    /// oracle. This is the auto-generated 7.4× ColorGrade.
    #[test]
    fn fused_colorgrade_generated_matches_hand_kernel() {
        use crate::node_graph::primitive::PrimitiveSpec;
        use crate::node_graph::primitives::{
            ClampTexture, Colorize, Contrast, Gain, HueSaturation, Mix, Saturation,
        };

        let device = crate::test_device();
        let (w, h) = (256u32, 256u32);
        let input = gradient(&device, w, h);
        let id = NodeInstanceId;

        // ColorGrade region: gain -> saturation -> hue -> contrast -> colorize,
        // then mix(a=source fork, b=colorize) -> clamp. Bodies/params from the
        // atom types' consts.
        let region = FusionRegion {
            nodes: vec![
                RegionNode { node_id: id(0), fusion_kind: Gain::FUSION_KIND, body: Gain::WGSL_BODY.unwrap(), params: Gain::PARAMS, inputs: vec![InputSource::External(0)], input_access: vec![], node_inputs: &[], node_outputs: &[], node_includes: &[], derived_uniforms: &[], output_storage: "rgba16float", quantize_f16: false },
                RegionNode { node_id: id(1), fusion_kind: Saturation::FUSION_KIND, body: Saturation::WGSL_BODY.unwrap(), params: Saturation::PARAMS, inputs: vec![InputSource::Node(id(0))], input_access: vec![], node_inputs: &[], node_outputs: &[], node_includes: &[], derived_uniforms: &[], output_storage: "rgba16float", quantize_f16: false },
                RegionNode { node_id: id(2), fusion_kind: HueSaturation::FUSION_KIND, body: HueSaturation::WGSL_BODY.unwrap(), params: HueSaturation::PARAMS, inputs: vec![InputSource::Node(id(1))], input_access: vec![], node_inputs: &[], node_outputs: &[], node_includes: &[], derived_uniforms: &[], output_storage: "rgba16float", quantize_f16: false },
                RegionNode { node_id: id(3), fusion_kind: Contrast::FUSION_KIND, body: Contrast::WGSL_BODY.unwrap(), params: Contrast::PARAMS, inputs: vec![InputSource::Node(id(2))], input_access: vec![], node_inputs: &[], node_outputs: &[], node_includes: &[], derived_uniforms: &[], output_storage: "rgba16float", quantize_f16: false },
                RegionNode { node_id: id(4), fusion_kind: Colorize::FUSION_KIND, body: Colorize::WGSL_BODY.unwrap(), params: Colorize::PARAMS, inputs: vec![InputSource::Node(id(3))], input_access: vec![], node_inputs: &[], node_outputs: &[], node_includes: &[], derived_uniforms: &[], output_storage: "rgba16float", quantize_f16: false },
                RegionNode { node_id: id(5), fusion_kind: Mix::FUSION_KIND, body: Mix::WGSL_BODY.unwrap(), params: Mix::PARAMS, inputs: vec![InputSource::External(0), InputSource::Node(id(4))], input_access: vec![], node_inputs: &[], node_outputs: &[], node_includes: &[], derived_uniforms: &[], output_storage: "rgba16float", quantize_f16: false },
                RegionNode { node_id: id(6), fusion_kind: ClampTexture::FUSION_KIND, body: ClampTexture::WGSL_BODY.unwrap(), params: ClampTexture::PARAMS, inputs: vec![InputSource::Node(id(5))], input_access: vec![], node_inputs: &[], node_outputs: &[], node_includes: &[], derived_uniforms: &[], output_storage: "rgba16float", quantize_f16: false },
            ],
            num_external_inputs: 1,
            outputs: vec![id(6)],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
        };
        let fused = generate_fused(&region).expect("fuse ColorGrade region");

        // Structural: shared helpers deduped (hue_saturation + colorize both
        // carry rgb2hsv/hsv2rgb), every body namespaced, one entry.
        assert_eq!(
            fused.wgsl.matches("fn rgb2hsv").count(),
            1,
            "rgb2hsv must be deduped to one copy"
        );
        assert_eq!(fused.wgsl.matches("fn hsv2rgb").count(), 1, "hsv2rgb deduped");
        assert!(!fused.wgsl.contains("fn body("), "every body must be namespaced");
        assert!(fused.wgsl.contains("fn n5_body"), "mix body namespaced as n5_body");
        assert_eq!(fused.wgsl.matches("fn cs_main").count(), 1, "exactly one entry");

        // Pack the generated uniform per its param_order. Same logical values
        // as the hand kernel below; mix.mode is the one u32.
        let slot_bytes = |nid: u32, name: &str| -> [u8; 4] {
            if (nid, name) == (5, "mode") {
                return 0u32.to_le_bytes();
            }
            let v: f32 = match (nid, name) {
                (0, "gain") => 1.15,
                (1, "saturation") => 1.3,
                (2, "hue") => 25.0,
                (2, "saturation") => 1.2,
                (2, "value") => 1.0,
                (3, "contrast") => 1.2,
                (4, "amount") => 0.4,
                (4, "hue") => 210.0,
                (4, "saturation") => 0.8,
                (4, "focus") => 0.6,
                (5, "amount") => 1.0,
                (6, "min") => 0.0,
                (6, "max") => 65000.0,
                _ => panic!("unexpected param {nid}.{name}"),
            };
            v.to_le_bytes()
        };
        let mut bytes = Vec::new();
        for (nid, name) in &fused.param_order {
            bytes.extend_from_slice(&slot_bytes(nid.0, name));
        }
        while bytes.len() % 16 != 0 {
            bytes.push(0);
        }
        let from_generated = dispatch_fused_kernel(&device, &fused.wgsl, &input, &bytes, 2);

        // Hand kernel (colorgrade_fused.wgsl) via the reference module, same values.
        let hand_params = crate::node_graph::freeze::reference::ColorGradeParams {
            gain: 1.15,
            sat_s: 1.3,
            hue_deg: 25.0,
            sat_h: 1.2,
            val_h: 1.0,
            contrast: 1.2,
            col_amount: 0.4,
            col_hue: 210.0,
            col_sat: 0.8,
            col_focus: 0.6,
            mix_amount: 1.0,
            mix_mode: 0,
            clamp_min: 0.0,
            clamp_max: 65000.0,
            _pad0: 0.0,
            _pad1: 0.0,
        };
        let pipeline = crate::node_graph::freeze::reference::colorgrade_pipeline(&device);
        let hand_out = RenderTarget::new(&device, w, h, FMT, "hand-cg");
        {
            let mut enc = device.create_encoder("hand-cg");
            crate::node_graph::freeze::reference::dispatch_fused_colorgrade(
                &mut enc,
                &pipeline,
                &input,
                &hand_out.texture,
                &hand_params,
            );
            enc.commit_and_wait_completed();
        }

        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &hand_out.texture, &from_generated.texture, 1e-4, 1e-4);
        assert_eq!(
            r.over_count, 0,
            "auto-generated fused ColorGrade must match the hand kernel \
             (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    /// The coincident two-input path: the generated standalone mix kernel
    /// reproduces mix.wgsl (two textures, blend mode + alpha lerp). Exercises
    /// the generator's MultiInputCoincident branch before the 1b cutover.
    #[test]
    fn generated_mix_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let a = gradient(&device, w, h);
        let b = gradient_b(&device, w, h);

        let m = crate::node_graph::primitives::Mix::new();
        let node: &dyn EffectNode = &m;
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("mix generates");
        let original = include_str!("../primitives/shaders/mix.wgsl");

        // uniform payload: amount = 0.6 (f32), mode = 4 (Multiply, u32), pad.
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&0.6f32.to_le_bytes());
        bytes[4..8].copy_from_slice(&4u32.to_le_bytes());

        let from_original = dispatch_coincident(&device, original, &a, &b, &bytes);
        let from_generated = dispatch_coincident(&device, &generated, &a, &b, &bytes);

        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &from_original.texture, &from_generated.texture, 1e-5, 1e-5);
        assert_eq!(
            r.over_count, 0,
            "generated mix must reproduce mix.wgsl (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
        assert!(r.max_abs < 1e-5, "coincident path should be ~bit-identical, got {}", r.max_abs);
    }

    /// Positional-atom parity (design §12.3 vocabulary widening). vignette is the
    /// first atom that reads its pixel POSITION via the ambient `uv`/`dims` args.
    /// The generated standalone kernel derives `aspect = dims.x/dims.y` itself and
    /// must reproduce the hand vignette.wgsl (which takes `aspect` as a uniform)
    /// bit-for-bit — so the two uniform payloads differ (hand carries aspect,
    /// generated doesn't). Verified on a NON-SQUARE canvas so the aspect-correct
    /// Circle is exercised, plus the uv-only Rectangle.
    #[test]
    fn generated_vignette_matches_original() {
        let device = crate::test_device();
        let (w, h) = (160u32, 128u32); // aspect 1.25, deliberately non-square
        let input = gradient(&device, w, h);
        let aspect = w as f32 / h as f32;

        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let node = registry.construct("node.vignette").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("vignette generates");
        let original = include_str!("../primitives/shaders/vignette.wgsl");
        let differ = TextureDiff::new(&device);

        // (shape, size, softness, strength): Circle (aspect-sensitive) + Rectangle.
        for (shape, size, softness, strength) in
            [(0u32, 0.6f32, 0.4f32, 1.0f32), (2u32, 0.95, 0.06, 1.0)]
        {
            // Hand uniform: shape, size, softness, strength, aspect + pad → 32 B.
            let mut hand_bytes = Vec::new();
            hand_bytes.extend_from_slice(&shape.to_le_bytes());
            hand_bytes.extend_from_slice(&size.to_le_bytes());
            hand_bytes.extend_from_slice(&softness.to_le_bytes());
            hand_bytes.extend_from_slice(&strength.to_le_bytes());
            hand_bytes.extend_from_slice(&aspect.to_le_bytes());
            while hand_bytes.len() % 16 != 0 {
                hand_bytes.push(0);
            }
            // Generated uniform: shape, size, softness, strength → 16 B (aspect
            // is recovered from dims inside the body, not plumbed through).
            let mut gen_bytes = Vec::new();
            gen_bytes.extend_from_slice(&shape.to_le_bytes());
            gen_bytes.extend_from_slice(&size.to_le_bytes());
            gen_bytes.extend_from_slice(&softness.to_le_bytes());
            gen_bytes.extend_from_slice(&strength.to_le_bytes());

            let from_original = dispatch_pointwise(&device, original, &input, &hand_bytes);
            let from_generated = dispatch_pointwise(&device, &generated, &input, &gen_bytes);
            let r = differ.compare(
                &device,
                &from_original.texture,
                &from_generated.texture,
                1e-4,
                1e-4,
            );
            assert_eq!(
                r.over_count, 0,
                "vignette shape {shape}: generated must reproduce vignette.wgsl \
                 (max_abs={}, max_rel={})",
                r.max_abs, r.max_rel
            );
        }
    }

    /// Dispatch a two-input EXACT-TEXEL kernel: uniform(0), a(1), b(2), dst(3) —
    /// NO sampler (both inputs are textureLoad'd). Mirrors dither's binding set.
    fn dispatch_two_texel(
        device: &GpuDevice,
        wgsl: &str,
        a: &GpuTexture,
        b: &GpuTexture,
        param_bytes: &[u8],
    ) -> RenderTarget {
        let (w, h) = (a.width, a.height);
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-test-dither");
        let out = RenderTarget::new(device, w, h, FMT, "codegen-out-dither");
        let mut enc = device.create_encoder("codegen-test-dither");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: param_bytes },
                GpuBinding::Texture { binding: 1, texture: a },
                GpuBinding::Texture { binding: 2, texture: b },
                GpuBinding::Texture { binding: 3, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "codegen-test-dither",
        );
        enc.commit_and_wait_completed();
        out
    }

    /// CoincidentTexel parity (design §12.3 read-semantics generalization).
    /// dither is the first atom with exact-texel inputs and NO sampler — both
    /// `in` and `pattern` are textureLoad'd at the fragment texel (sampling the
    /// threshold map would blend neighbouring thresholds and smear the dither).
    /// The generated standalone kernel must reproduce hand dither.wgsl
    /// bit-for-bit AND emit the sampler-free binding set (uniform(0), in(1),
    /// pattern(2), dst(3)) so it's a drop-in for dither's run().
    #[test]
    fn generated_dither_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let source = gradient(&device, w, h);
        let pattern = gradient_b(&device, w, h); // R channel = the threshold map

        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let node = registry.construct("node.dither").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("dither generates");

        // Structural: the all-texel atom binds NO sampler and reads both inputs
        // via textureLoad (the new CoincidentTexel read-path).
        assert!(
            !generated.contains("var samp: sampler"),
            "an all-CoincidentTexel atom must bind no sampler:\n{generated}"
        );
        assert_eq!(
            generated.matches("textureLoad(").count(),
            2,
            "both dither inputs must be textureLoad'd:\n{generated}"
        );

        let original = include_str!("../primitives/shaders/dither.wgsl");
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&0.5f32.to_le_bytes()); // amount

        let from_original = dispatch_two_texel(&device, original, &source, &pattern, &bytes);
        let from_generated = dispatch_two_texel(&device, &generated, &source, &pattern, &bytes);
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "generated dither must reproduce dither.wgsl (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    /// Dispatch an N-input coincident kernel: uniform(0), inputs(1..=N),
    /// sampler(N+1), dst(N+2) — the generated MultiInputCoincident layout for any
    /// arity. Generalizes `dispatch_coincident` (which is fixed at 2 inputs).
    fn dispatch_coincident_n(
        device: &GpuDevice,
        wgsl: &str,
        inputs: &[&GpuTexture],
        param_bytes: &[u8],
    ) -> RenderTarget {
        let (w, h) = (inputs[0].width, inputs[0].height);
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-coincident-n");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let out = RenderTarget::new(device, w, h, FMT, "codegen-out-coincident-n");
        let mut bindings: Vec<GpuBinding> =
            vec![GpuBinding::Bytes { binding: 0, data: param_bytes }];
        for (i, t) in inputs.iter().enumerate() {
            bindings.push(GpuBinding::Texture { binding: (i + 1) as u32, texture: t });
        }
        bindings.push(GpuBinding::Sampler {
            binding: (inputs.len() + 1) as u32,
            sampler: &sampler,
        });
        bindings.push(GpuBinding::Texture {
            binding: (inputs.len() + 2) as u32,
            texture: &out.texture,
        });
        let mut enc = device.create_encoder("codegen-coincident-n");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [w.div_ceil(16), h.div_ceil(16), 1],
            "codegen-coincident-n",
        );
        enc.commit_and_wait_completed();
        out
    }

    /// Coincident multi-input parity (overnight vocabulary sweep): each blend
    /// atom's generated kernel reproduces its hand shader bit-for-bit. Inputs
    /// alternate the two gradients — parity is generated-vs-hand on identical
    /// inputs, so the specific textures don't matter, only that both kernels see
    /// the same set. Covers arities 2, 3, and 5.
    #[test]
    fn generated_coincident_atoms_match_originals() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let ga = gradient(&device, w, h);
        let gb = gradient_b(&device, w, h);
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let shaders_dir =
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
        let differ = TextureDiff::new(&device);

        // (type_id, hand shader, #texture inputs, f32 params in PARAMS order).
        let cases: &[(&str, &str, usize, &[f32])] = &[
            ("node.wet_dry", "wet_dry_mix.wgsl", 2, &[0.6]),
            ("node.hdr_retention_mix", "hdr_retention_mix.wgsl", 2, &[0.7]),
            ("node.masked_mix", "masked_mix.wgsl", 3, &[0.8]),
            ("node.texture_sum_5", "texture_sum_5.wgsl", 5, &[5.0]),
        ];
        for (type_id, shader_file, n_inputs, params) in cases {
            let node = registry.construct(type_id).unwrap();
            let generated = generate_standalone(
                node.fusion_kind(),
                node.wgsl_body().unwrap(),
                node.inputs(),
                node.parameters(),
                node.input_access(),
                node.outputs(),
            )
            .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
            let original = std::fs::read_to_string(format!("{shaders_dir}/{shader_file}"))
                .unwrap_or_else(|e| panic!("read {shader_file}: {e}"));
            let texs: Vec<&GpuTexture> =
                (0..*n_inputs).map(|i| if i % 2 == 0 { &ga } else { &gb }).collect();
            let bytes = pack_f32(params);
            let from_original = dispatch_coincident_n(&device, &original, &texs, &bytes);
            let from_generated = dispatch_coincident_n(&device, &generated, &texs, &bytes);
            let r = differ.compare(
                &device,
                &from_original.texture,
                &from_generated.texture,
                1e-5,
                1e-5,
            );
            assert_eq!(
                r.over_count, 0,
                "{type_id}: generated must reproduce {shader_file} (max_abs={}, max_rel={})",
                r.max_abs, r.max_rel
            );
        }
    }

    /// Enum/int pointwise parity (overnight sweep): atoms whose uniform mixes f32
    /// and u32 (Enum -> u32) fields, so the payload is packed by hand rather than
    /// via pack_f32. flash branches on `mode`; reinhard on `curve`. Standard
    /// pointwise layout (uniform(0), tex(1), sampler(2), dst(3)).
    #[test]
    fn generated_enum_pointwise_atoms_match_originals() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let input = gradient(&device, w, h);
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let shaders_dir =
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
        let differ = TextureDiff::new(&device);

        // flash: amount=0.7 (f32), mode=2 Gain (u32), pad to 16.
        let mut flash_bytes = Vec::new();
        flash_bytes.extend_from_slice(&0.7f32.to_le_bytes());
        flash_bytes.extend_from_slice(&2u32.to_le_bytes());
        while flash_bytes.len() < 16 {
            flash_bytes.push(0);
        }
        // reinhard: intensity=1.5, contrast=1.2 (f32), curve=1 Simple (u32), pad.
        let mut reinhard_bytes = Vec::new();
        reinhard_bytes.extend_from_slice(&1.5f32.to_le_bytes());
        reinhard_bytes.extend_from_slice(&1.2f32.to_le_bytes());
        reinhard_bytes.extend_from_slice(&1u32.to_le_bytes());
        while reinhard_bytes.len() < 16 {
            reinhard_bytes.push(0);
        }

        // chroma_key: key_color Vec3 (3 f32) + tolerance, softness (f32) + mode
        // (Enum -> u32) + pad → 32 B. The Vec3 param expands to 3 uniform floats,
        // matching the hand shader's key_r/g/b layout.
        let mut chroma_bytes = Vec::new();
        chroma_bytes.extend_from_slice(&0.0f32.to_le_bytes()); // key R (greenscreen)
        chroma_bytes.extend_from_slice(&1.0f32.to_le_bytes()); // key G
        chroma_bytes.extend_from_slice(&0.0f32.to_le_bytes()); // key B
        chroma_bytes.extend_from_slice(&0.4f32.to_le_bytes()); // tolerance
        chroma_bytes.extend_from_slice(&0.1f32.to_le_bytes()); // softness
        chroma_bytes.extend_from_slice(&1u32.to_le_bytes()); // mode = Reject
        while chroma_bytes.len() < 32 {
            chroma_bytes.push(0);
        }
        let cases: &[(&str, &str, &[u8])] = &[
            ("node.flash", "flash.wgsl", flash_bytes.as_slice()),
            ("node.reinhard_tone_map", "reinhard_tone_map.wgsl", reinhard_bytes.as_slice()),
            ("node.chroma_key", "chroma_key.wgsl", chroma_bytes.as_slice()),
        ];
        for (type_id, shader_file, bytes) in cases {
            let node = registry.construct(type_id).unwrap();
            let generated = generate_standalone(
                node.fusion_kind(),
                node.wgsl_body().unwrap(),
                node.inputs(),
                node.parameters(),
                node.input_access(),
                node.outputs(),
            )
            .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
            let original = std::fs::read_to_string(format!("{shaders_dir}/{shader_file}"))
                .unwrap_or_else(|e| panic!("read {shader_file}: {e}"));
            let from_original = dispatch_pointwise(&device, &original, &input, bytes);
            let from_generated = dispatch_pointwise(&device, &generated, &input, bytes);
            let r = differ.compare(
                &device,
                &from_original.texture,
                &from_generated.texture,
                1e-5,
                1e-5,
            );
            assert_eq!(
                r.over_count, 0,
                "{type_id}: generated must reproduce {shader_file} (max_abs={}, max_rel={})",
                r.max_abs, r.max_rel
            );
        }
    }

    /// Dispatch a PARAMLESS pointwise kernel: tex(0), sampler(1), dst(2) — no
    /// uniform binding (a paramless atom's generated kernel binds none).
    fn dispatch_paramless_pointwise(
        device: &GpuDevice,
        wgsl: &str,
        input: &GpuTexture,
    ) -> RenderTarget {
        let (w, h) = (input.width, input.height);
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-paramless");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let out = RenderTarget::new(device, w, h, FMT, "codegen-out-paramless");
        let mut enc = device.create_encoder("codegen-paramless");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Texture { binding: 0, texture: input },
                GpuBinding::Sampler { binding: 1, sampler: &sampler },
                GpuBinding::Texture { binding: 2, texture: &out.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "codegen-paramless",
        );
        enc.commit_and_wait_completed();
        out
    }

    /// Paramless parity (overnight sweep): abs_texture has zero params, so the
    /// generated kernel emits NO uniform and starts its textures at binding 0 —
    /// a drop-in for the hand abs_texture.wgsl, which also has no uniform. Proves
    /// the paramless codegen path matches bit-for-bit.
    #[test]
    fn generated_paramless_atom_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let input = gradient(&device, w, h);
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let node = registry.construct("node.abs_texture").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("abs_texture generates");

        // Structural: no uniform, textures start at binding 0.
        assert!(
            !generated.contains("var<uniform>"),
            "a paramless atom must bind no uniform:\n{generated}"
        );
        assert!(
            generated.contains("@group(0) @binding(0) var tex_in"),
            "paramless tex must start at binding 0:\n{generated}"
        );

        let original = include_str!("../primitives/shaders/abs_texture.wgsl");
        let from_original = dispatch_paramless_pointwise(&device, original, &input);
        let from_generated = dispatch_paramless_pointwise(&device, &generated, &input);
        let differ = TextureDiff::new(&device);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "generated abs_texture must reproduce abs_texture.wgsl (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    /// Gather parity (design §11.B): remap is the first GATHER atom — `source` is
    /// sampled at a coord the body COMPUTES, so the codegen passes it as a
    /// texture+sampler arg (not a pre-read register), while `uv_field` is
    /// coincident. The hand remap.wgsl interleaves the sampler between its two
    /// textures (uniform0/src1/samp2/field3/out4); the generated kernel binds the
    /// textures consecutively then the sampler (uniform0/src1/field2/samp3/dst4),
    /// so each is dispatched with its own layout. wrap=Mirror exercises the
    /// wrap_coord helper.
    #[test]
    fn generated_remap_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let source = gradient(&device, w, h);
        let field = gradient_b(&device, w, h); // .rg carry the target UVs
        let sampler = device.create_sampler(&GpuSamplerDesc::default());

        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let node = registry.construct("node.remap").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("remap generates");

        // Structural: gather `source` is NOT pre-read; textures then sampler.
        assert!(
            generated.contains("@group(0) @binding(1) var tex_source"),
            "source at binding 1:\n{generated}"
        );
        assert!(
            generated.contains("@group(0) @binding(2) var tex_uv_field"),
            "uv_field at binding 2:\n{generated}"
        );
        assert!(
            generated.contains("@group(0) @binding(3) var samp"),
            "sampler after the textures:\n{generated}"
        );
        assert!(
            !generated.contains("let c_source"),
            "a Gather input must not be pre-sampled into a register:\n{generated}"
        );

        let original = include_str!("../primitives/shaders/remap.wgsl");
        // wrap=2 (Mirror), mode=0 (Absolute).
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&2u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&0u32.to_le_bytes());

        // Hand layout: uniform(0), source(1), sampler(2), uv_field(3), out(4).
        let hand_out = RenderTarget::new(&device, w, h, FMT, "remap-hand");
        {
            let pipeline = device.create_compute_pipeline(original, ENTRY, "remap-hand");
            let mut enc = device.create_encoder("remap-hand");
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: &bytes },
                    GpuBinding::Texture { binding: 1, texture: &source },
                    GpuBinding::Sampler { binding: 2, sampler: &sampler },
                    GpuBinding::Texture { binding: 3, texture: &field },
                    GpuBinding::Texture { binding: 4, texture: &hand_out.texture },
                ],
                [w.div_ceil(16), h.div_ceil(16), 1],
                "remap-hand",
            );
            enc.commit_and_wait_completed();
        }
        // Generated layout: uniform(0), source(1), uv_field(2), sampler(3), dst(4).
        let gen_out = RenderTarget::new(&device, w, h, FMT, "remap-gen");
        {
            let pipeline = device.create_compute_pipeline(&generated, ENTRY, "remap-gen");
            let mut enc = device.create_encoder("remap-gen");
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: &bytes },
                    GpuBinding::Texture { binding: 1, texture: &source },
                    GpuBinding::Texture { binding: 2, texture: &field },
                    GpuBinding::Sampler { binding: 3, sampler: &sampler },
                    GpuBinding::Texture { binding: 4, texture: &gen_out.texture },
                ],
                [w.div_ceil(16), h.div_ceil(16), 1],
                "remap-gen",
            );
            enc.commit_and_wait_completed();
        }

        let differ = TextureDiff::new(&device);
        let r = differ.compare(&device, &hand_out.texture, &gen_out.texture, 1e-5, 1e-5);
        assert_eq!(
            r.over_count, 0,
            "generated remap must reproduce remap.wgsl (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    /// More gather atoms, now the Gather codegen exists. chromatic_displace
    /// (3-tap RGB split) and uv_displace_by_flow both bind
    /// uniform0/tex1/tex2/samp3/dst4 for BOTH the hand shader and the generated
    /// kernel, so dispatch_coincident covers them directly. The first texture is
    /// the gathered `in`, the second the coincident field.
    #[test]
    fn generated_gather_atoms_match_originals() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let ga = gradient(&device, w, h);
        let gb = gradient_b(&device, w, h);
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let shaders_dir =
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
        let differ = TextureDiff::new(&device);

        // color_lut: `in` coincident (a = the centre sample) + `lut` gathered
        // (b, sampled at the luminance-indexed coord). amount=0.5 exercises the
        // crossfade; the LUT texture is just gradient_b sampled at y=0.5.
        let cases: &[(&str, &str, &[f32])] = &[
            ("node.chromatic_displace", "chromatic_displace.wgsl", &[2.0]),
            ("node.uv_displace_by_flow", "uv_displace_by_flow.wgsl", &[0.05, 0.5]),
            ("node.color_lut", "lut1d.wgsl", &[0.5, 1.5]),
            // slope_displace: base (a) + image (b) both gathered; strength/step/weight.
            ("node.slope_displace", "slope_displace.wgsl", &[5.0, 5.0, 0.001]),
            // texture_advect: in (a) gathered at adv_uv + velocity (b) coincident;
            // dt + boundary(0=Repeat, body ignores it — the test sampler is Clamp,
            // but adv samples land in-bounds for this velocity so wrap is moot).
            ("node.texture_advect", "texture_advect.wgsl", &[2.0, 0.0]),
        ];
        for (type_id, shader_file, params) in cases {
            let node = registry.construct(type_id).unwrap();
            let generated = generate_standalone(
                node.fusion_kind(),
                node.wgsl_body().unwrap(),
                node.inputs(),
                node.parameters(),
                node.input_access(),
                node.outputs(),
            )
            .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
            let original = std::fs::read_to_string(format!("{shaders_dir}/{shader_file}"))
                .unwrap_or_else(|e| panic!("read {shader_file}: {e}"));
            let bytes = pack_f32(params);
            let from_original = dispatch_coincident(&device, &original, &ga, &gb, &bytes);
            let from_generated = dispatch_coincident(&device, &generated, &ga, &gb, &bytes);
            let r = differ.compare(
                &device,
                &from_original.texture,
                &from_generated.texture,
                1e-5,
                1e-5,
            );
            assert_eq!(
                r.over_count, 0,
                "{type_id}: generated must reproduce {shader_file} (max_abs={}, max_rel={})",
                r.max_abs, r.max_rel
            );
        }
    }

    /// Single-input GATHER parity: the neighbourhood-filter family (sharpen,
    /// edge_detect) reads `in` at offsets the body computes, so it binds the
    /// 1-input layout uniform(0)/tex(1)/samp(2)/dst(3) — identical to a pointwise
    /// atom — and the body samples `in` itself. Both recover the texel step from
    /// the ambient `dims` (= output size), so the generated kernel ignores any
    /// texel_size_* fields the hand uniform carries; the parity payload still
    /// packs those fields (= 1/dims at the test size) so the hand shader reads the
    /// matching step. `dispatch_pointwise` covers the shared 1-input layout.
    #[test]
    fn generated_single_input_gather_atoms_match_originals() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let input = gradient(&device, w, h);
        let texel = 1.0f32 / 128.0;
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let shaders_dir =
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
        let differ = TextureDiff::new(&device);

        // sharpen PARAMS: [amount]. edge_detect PARAMS: [amount, threshold]; its
        // hand uniform additionally carries [texel_x, texel_y] = 1/dims.
        let sharpen_bytes = pack_f32(&[1.5]);
        let edge_bytes = pack_f32(&[0.7, 0.2, texel, texel]);
        // gradient_central_diff PARAMS: [channel, scale_mode, wrap_mode] (all
        // Enum->u32). channel=G, scale_mode=UV (exercises the dims*0.5 branch);
        // wrap_mode is host-side sampler-only so the body ignores it. The hand
        // uniform is {channel, scale_mode, _pad, _pad}; the generated Params is
        // {channel, scale_mode, wrap_mode, _pad}. Both read channel/scale_mode
        // from the same offsets, so one 16-byte payload drives both.
        let mut grad_bytes = vec![0u8; 16];
        grad_bytes[0..4].copy_from_slice(&1u32.to_le_bytes()); // channel = G
        grad_bytes[4..8].copy_from_slice(&1u32.to_le_bytes()); // scale_mode = UV
        // convolution_2d_9tap PARAMS: [k0..k8, bias, normalise (Bool->u32)] —
        // identical layout to the hand ConvUniforms. A normalising box blur
        // exercises the sum-normalise divide + the centre-alpha passthrough.
        let mut conv_bytes = vec![0u8; 48];
        for i in 0..9 {
            conv_bytes[i * 4..i * 4 + 4].copy_from_slice(&1.0f32.to_le_bytes());
        }
        conv_bytes[36..40].copy_from_slice(&0.0f32.to_le_bytes()); // bias
        conv_bytes[40..44].copy_from_slice(&1u32.to_le_bytes()); // normalise = true
        // mirror_axis PARAMS: [angle] — Gather sampled at the mirrored UV; the body
        // computes cos/sin from angle on the GPU (matching the hand), bit-exact.
        let mirror_bytes = pack_f32(&[std::f32::consts::FRAC_PI_4]);
        // heightmap_to_normal PARAMS: [z_scale, aspect, coord_space]; coord_space=0
        // (TangentZ) packs as f32 0.0 = u32 0.
        let heightmap_bytes = pack_f32(&[0.5, 1.0, 0.0]);
        let cases: &[(&str, &str, &[u8])] = &[
            ("node.sharpen", "sharpen.wgsl", sharpen_bytes.as_slice()),
            ("node.edge_detect", "edge_detect.wgsl", edge_bytes.as_slice()),
            ("node.gradient_central_diff", "gradient_central_diff.wgsl", grad_bytes.as_slice()),
            ("node.convolution_2d_9tap", "convolution_2d_9tap.wgsl", conv_bytes.as_slice()),
            ("node.mirror_axis", "mirror_axis.wgsl", mirror_bytes.as_slice()),
            ("node.heightmap_to_normal", "heightmap_to_normal.wgsl", heightmap_bytes.as_slice()),
        ];
        for (type_id, shader_file, bytes) in cases {
            let node = registry.construct(type_id).unwrap();
            let generated = generate_standalone(
                node.fusion_kind(),
                node.wgsl_body().unwrap(),
                node.inputs(),
                node.parameters(),
                node.input_access(),
                node.outputs(),
            )
            .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
            // Structural: the gather input is NOT pre-sampled into a register.
            assert!(
                !generated.contains("let c_in"),
                "{type_id}: a Gather input must not be pre-sampled:\n{generated}"
            );
            let original = std::fs::read_to_string(format!("{shaders_dir}/{shader_file}"))
                .unwrap_or_else(|e| panic!("read {shader_file}: {e}"));
            let from_original = dispatch_pointwise(&device, &original, &input, bytes);
            let from_generated = dispatch_pointwise(&device, &generated, &input, bytes);
            let r = differ.compare(
                &device,
                &from_original.texture,
                &from_generated.texture,
                1e-5,
                1e-5,
            );
            assert_eq!(
                r.over_count, 0,
                "{type_id}: generated must reproduce {shader_file} (max_abs={}, max_rel={})",
                r.max_abs, r.max_rel
            );
        }
    }

    /// Dual-packed GATHER parity: node.gaussian_blur is a single-input gather
    /// whose hand uniform interleaves computed texel_x/texel_y fields the body no
    /// longer reads (it recovers the step from `dims`), and whose generated Params
    /// instead carries the address_mode param (host-side sampler only). So the two
    /// kernels take DIFFERENT 32-byte uniform layouts for the same logical params:
    /// the hand layout {kernel_size, axis, step, texel_x, texel_y, radius_mode,
    /// radius, _pad} and the generated layout {kernel_size, axis, step,
    /// radius_mode, radius, address_mode, _pad, _pad}. Pack each, dispatch via the
    /// shared 1-input layout, diff. Covers Fixed (9/17-tap) and Dynamic modes on
    /// both axes; the default Clamp sampler matches address_mode=0.
    #[test]
    fn generated_separable_gaussian_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let input = gradient(&device, w, h);
        let texel = 1.0f32 / 128.0;
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let differ = TextureDiff::new(&device);

        let node = registry.construct("node.gaussian_blur").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("gaussian_blur generates");
        assert!(
            !generated.contains("let c_in"),
            "a Gather input must not be pre-sampled:\n{generated}"
        );
        let original = include_str!("../primitives/shaders/separable_gaussian.wgsl");

        let pack_hand = |ks: u32, axis: u32, step: f32, rmode: u32, radius: f32| -> Vec<u8> {
            let mut b = vec![0u8; 32];
            b[0..4].copy_from_slice(&ks.to_le_bytes());
            b[4..8].copy_from_slice(&axis.to_le_bytes());
            b[8..12].copy_from_slice(&step.to_le_bytes());
            b[12..16].copy_from_slice(&texel.to_le_bytes()); // texel_x
            b[16..20].copy_from_slice(&texel.to_le_bytes()); // texel_y
            b[20..24].copy_from_slice(&rmode.to_le_bytes());
            b[24..28].copy_from_slice(&radius.to_le_bytes());
            b
        };
        let pack_gen = |ks: u32, axis: u32, step: f32, rmode: u32, radius: f32| -> Vec<u8> {
            let mut b = vec![0u8; 32];
            b[0..4].copy_from_slice(&ks.to_le_bytes());
            b[4..8].copy_from_slice(&axis.to_le_bytes());
            b[8..12].copy_from_slice(&step.to_le_bytes());
            b[12..16].copy_from_slice(&rmode.to_le_bytes());
            b[16..20].copy_from_slice(&radius.to_le_bytes());
            // address_mode = 0 (Clamp) at [20..24], pads at [24..32].
            b
        };

        // (kernel_size, axis, step, radius_mode, radius).
        let sets: &[(u32, u32, f32, u32, f32)] = &[
            (1, 0, 2.0, 0, 0.0),  // Fixed 17-tap, horizontal, step 2
            (0, 1, 1.0, 0, 0.0),  // Fixed 9-tap, vertical
            (2, 0, 1.0, 0, 0.0),  // Fixed 25-tap, horizontal
            (1, 0, 1.0, 1, 10.0), // Dynamic, horizontal, radius 10
            (1, 1, 1.0, 1, 5.0),  // Dynamic, vertical, radius 5
        ];
        for &(ks, axis, step, rmode, radius) in sets {
            let hand_bytes = pack_hand(ks, axis, step, rmode, radius);
            let gen_bytes = pack_gen(ks, axis, step, rmode, radius);
            let from_original = dispatch_pointwise(&device, original, &input, &hand_bytes);
            let from_generated = dispatch_pointwise(&device, &generated, &input, &gen_bytes);
            let r = differ.compare(
                &device,
                &from_original.texture,
                &from_generated.texture,
                1e-5,
                1e-5,
            );
            assert_eq!(
                r.over_count, 0,
                "gaussian_blur set (ks={ks}, axis={axis}, step={step}, rmode={rmode}, \
                 radius={radius}): generated must reproduce separable_gaussian.wgsl \
                 (max_abs={}, max_rel={})",
                r.max_abs, r.max_rel
            );
        }
    }

    /// Dual-packed SOURCE parity: node.basic_shape is a generator whose run()
    /// used to preprocess its params before packing (uv_scale = 1/scale, shape
    /// index as f32, wireframe thresholded) into a reordered hand uniform. The
    /// body now does that preprocessing, so the generated Params carry the RAW
    /// params in declaration order. The two kernels take DIFFERENT 32-byte
    /// layouts for the same logical inputs — pack each, dispatch as a Source, and
    /// diff across all three shapes (solid + wireframe + rotated).
    #[test]
    fn generated_basic_shape_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let differ = TextureDiff::new(&device);

        let node = registry.construct("node.basic_shape").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("basic_shape generates");
        let original = include_str!("../primitives/shaders/basic_shape.wgsl");

        // Hand layout: {aspect, line, uv_scale=1/scale, shape_idx(f32), is_wireframe
        // (thresholded 0/1), rotation, _pad, _pad}.
        let pack_hand = |shape: u32, aspect: f32, scale: f32, line: f32, rot: f32, wf: f32| -> Vec<u8> {
            let uv_scale = if scale > 0.0 { 1.0 / scale } else { 1.0 };
            let wf_flag = if wf > 0.5 { 1.0f32 } else { 0.0 };
            let mut b = vec![0u8; 32];
            b[0..4].copy_from_slice(&aspect.to_le_bytes());
            b[4..8].copy_from_slice(&line.to_le_bytes());
            b[8..12].copy_from_slice(&uv_scale.to_le_bytes());
            b[12..16].copy_from_slice(&(shape as f32).to_le_bytes());
            b[16..20].copy_from_slice(&wf_flag.to_le_bytes());
            b[20..24].copy_from_slice(&rot.to_le_bytes());
            b
        };
        // Generated layout: {shape(u32), aspect, scale, line, rotation, is_wireframe
        // (raw), _pad, _pad}.
        let pack_gen = |shape: u32, aspect: f32, scale: f32, line: f32, rot: f32, wf: f32| -> Vec<u8> {
            let mut b = vec![0u8; 32];
            b[0..4].copy_from_slice(&shape.to_le_bytes());
            b[4..8].copy_from_slice(&aspect.to_le_bytes());
            b[8..12].copy_from_slice(&scale.to_le_bytes());
            b[12..16].copy_from_slice(&line.to_le_bytes());
            b[16..20].copy_from_slice(&rot.to_le_bytes());
            b[20..24].copy_from_slice(&wf.to_le_bytes());
            b
        };

        // (shape, aspect, scale, line, rotation, is_wireframe).
        let sets: &[(u32, f32, f32, f32, f32, f32)] = &[
            (0, 1.0, 1.0, 0.015, 0.0, 0.0),  // Square, solid
            (1, 1.5, 0.8, 0.02, 0.5, 1.0),   // Diamond, wireframe, rotated, aspect
            (2, 1.0, 1.2, 0.01, -0.3, 0.0),  // Octagon, solid, rotated, scaled
        ];
        for &(shape, aspect, scale, line, rot, wf) in sets {
            let hand_bytes = pack_hand(shape, aspect, scale, line, rot, wf);
            let gen_bytes = pack_gen(shape, aspect, scale, line, rot, wf);
            let from_original = dispatch_source(&device, original, Some(&hand_bytes), w, h);
            let from_generated = dispatch_source(&device, &generated, Some(&gen_bytes), w, h);
            let r = differ.compare(
                &device,
                &from_original.texture,
                &from_generated.texture,
                1e-5,
                1e-5,
            );
            assert_eq!(
                r.over_count, 0,
                "basic_shape (shape={shape}, scale={scale}, wf={wf}): generated must \
                 reproduce basic_shape.wgsl (max_abs={}, max_rel={})",
                r.max_abs, r.max_rel
            );
        }
    }

    /// TABLE-param SOURCE parity: node.gradient_ramp's `stops` Table param expands
    /// in the generated uniform to a `stops_count` header word + a fixed
    /// `array<vec4<f32>, 16>` after the aligned header, and the body receives
    /// (stops_count, stops). The hand uniform is {count, domain, _pad, _pad, stops}
    /// (count first); the generated layout is {domain, count, _pad, _pad, stops}
    /// (scalar params before table counts) — the array sits at the same offset 16
    /// in both, only the two header scalars swap. Pack each, dispatch as a Source,
    /// diff. domain=2 exercises the past-last-stop extrapolation tail.
    #[test]
    fn generated_gradient_ramp_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let differ = TextureDiff::new(&device);

        let node = registry.construct("node.gradient_ramp").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("gradient_ramp generates");
        // Structural: the Table param expands to a count word + a vec4 array.
        assert!(
            generated.contains("stops_count: u32"),
            "table count word missing:\n{generated}"
        );
        assert!(
            generated.contains("stops: array<vec4<f32>, 16>"),
            "table array missing:\n{generated}"
        );
        let original = include_str!("../primitives/shaders/gradient_ramp.wgsl");

        let count: u32 = 3;
        let domain: f32 = 2.0;
        let stops: [[f32; 4]; 3] = [
            [0.0, 0.0, 0.0, 0.0], // black at t=0
            [0.5, 1.0, 0.0, 0.0], // red at t=0.5
            [1.0, 1.0, 1.0, 0.0], // yellow at t=1.0 (then extrapolated to t=2)
        ];
        // 16 vec4 array = 256 bytes, first 3 stops filled.
        let mut stops_bytes = vec![0u8; 256];
        for (i, s) in stops.iter().enumerate() {
            for (j, v) in s.iter().enumerate() {
                let off = i * 16 + j * 4;
                stops_bytes[off..off + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        // Hand header: {count, domain, _pad, _pad}.
        let mut hand = vec![0u8; 16];
        hand[0..4].copy_from_slice(&count.to_le_bytes());
        hand[4..8].copy_from_slice(&domain.to_le_bytes());
        hand.extend_from_slice(&stops_bytes);
        // Generated header: {domain, count, _pad, _pad}.
        let mut gen_bytes = vec![0u8; 16];
        gen_bytes[0..4].copy_from_slice(&domain.to_le_bytes());
        gen_bytes[4..8].copy_from_slice(&count.to_le_bytes());
        gen_bytes.extend_from_slice(&stops_bytes);

        let from_original = dispatch_source(&device, original, Some(&hand), w, h);
        let from_generated = dispatch_source(&device, &generated, Some(&gen_bytes), w, h);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "generated gradient_ramp must reproduce gradient_ramp.wgsl \
             (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    /// RESAMPLE GATHER parity: node.downsample's output is SMALLER than its input
    /// (a box filter), so it can't reuse dispatch_pointwise (which sizes output ==
    /// input). The body is a single-input Gather that reads `in` via textureLoad at
    /// input-pixel coords, recovering its output pixel id from uv and the box
    /// factor from in_dims/out_dims. Dispatch a 128→64 (factor 2) reduction for
    /// both the hand and generated kernels and diff. The uniform `factor` is
    /// diagnostic (the shader uses the dim ratio), so one 16-byte payload drives
    /// both.
    #[test]
    fn generated_downsample_matches_original() {
        let device = crate::test_device();
        let input = gradient(&device, 128, 128);
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let differ = TextureDiff::new(&device);

        let node = registry.construct("node.downsample").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("downsample generates");
        assert!(
            !generated.contains("let c_in"),
            "a Gather input must not be pre-sampled:\n{generated}"
        );
        let original = include_str!("../primitives/shaders/downsample.wgsl");

        let dispatch = |wgsl: &str| -> RenderTarget {
            let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-downsample");
            let sampler = device.create_sampler(&GpuSamplerDesc::default());
            let out = RenderTarget::new(&device, 64, 64, FMT, "codegen-out-downsample");
            let mut bytes = [0u8; 16];
            bytes[0..4].copy_from_slice(&4u32.to_le_bytes()); // diagnostic factor
            let mut enc = device.create_encoder("codegen-downsample");
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: &bytes },
                    GpuBinding::Texture { binding: 1, texture: &input },
                    GpuBinding::Sampler { binding: 2, sampler: &sampler },
                    GpuBinding::Texture { binding: 3, texture: &out.texture },
                ],
                [64u32.div_ceil(16), 64u32.div_ceil(16), 1],
                "codegen-downsample",
            );
            enc.commit_and_wait_completed();
            out
        };

        let from_original = dispatch(original);
        let from_generated = dispatch(&generated);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "generated downsample must reproduce downsample.wgsl (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    /// SPECIALIZATION + 2-input GATHER parity: node.gaussian_blur_variable_width
    /// gathers `in` + `width` along one axis and selects its tap count / weighting
    /// via the QUALITY_LEVEL / WEIGHTING_MODE specialization tokens (run() compiles
    /// the GENERATED WGSL through create_specialized_compute_pipeline, same as the
    /// hand kernel). Both kernels take the identical binding layout (uniform0/in1/
    /// width2/samp3/dst4) and the body reads only direction+max_radius from the
    /// uniform, so one 16-byte payload drives both; specialize each with the SAME
    /// (quality, weighting) and diff across three combos.
    #[test]
    fn generated_gaussian_blur_variable_width_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let src = gradient(&device, w, h);
        let width = gradient_b(&device, w, h); // R channel varies → CoC varies
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let differ = TextureDiff::new(&device);

        let node = registry.construct("node.gaussian_blur_variable_width").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("variable-width blur generates");
        assert!(
            !generated.contains("let c_in") && !generated.contains("let c_width"),
            "both gather inputs must avoid pre-sampling:\n{generated}"
        );
        let original =
            include_str!("../primitives/shaders/gaussian_blur_variable_width.wgsl");

        // {direction (0=H), max_radius, _pad, _pad}; the body reads only these two.
        let mut bytes = [0u8; 16];
        bytes[4..8].copy_from_slice(&12.0f32.to_le_bytes());

        let dispatch = |wgsl: &str, q: &str, wt: &str| -> RenderTarget {
            let pipeline = device.create_specialized_compute_pipeline(
                wgsl,
                ENTRY,
                &[("QUALITY_LEVEL", q), ("WEIGHTING_MODE", wt)],
                "vbw-test",
            );
            let sampler = device.create_sampler(&GpuSamplerDesc::default());
            let out = RenderTarget::new(&device, w, h, FMT, "vbw-out");
            let mut enc = device.create_encoder("vbw-test");
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: &bytes },
                    GpuBinding::Texture { binding: 1, texture: &src },
                    GpuBinding::Texture { binding: 2, texture: &width },
                    GpuBinding::Sampler { binding: 3, sampler: &sampler },
                    GpuBinding::Texture { binding: 4, texture: &out.texture },
                ],
                [w.div_ceil(16), h.div_ceil(16), 1],
                "vbw-test",
            );
            enc.commit_and_wait_completed();
            out
        };

        for (q, wt) in [("1u", "0u"), ("2u", "1u"), ("0u", "0u")] {
            let h_out = dispatch(original, q, wt);
            let g_out = dispatch(&generated, q, wt);
            let r = differ.compare(&device, &h_out.texture, &g_out.texture, 1e-5, 1e-5);
            assert_eq!(
                r.over_count, 0,
                "variable-width blur (Q={q}, W={wt}): generated must reproduce the hand \
                 kernel (max_abs={}, max_rel={})",
                r.max_abs, r.max_rel
            );
        }
    }

    /// Allocate an n×n×n 3D texture with the given usage.
    fn make_3d_texture(device: &GpuDevice, n: u32, usage: GpuTextureUsage, label: &'static str) -> GpuTexture {
        device.create_texture(&GpuTextureDesc {
            width: n,
            height: n,
            depth: n,
            format: FMT,
            dimension: GpuTextureDimension::D3,
            usage,
            label,
            mip_levels: 1,
        })
    }

    /// 3D-VOLUME GATHER parity: node.blur_3d_separable blurs a Texture3D along one
    /// axis. The hand fluid_blur_3d.wgsl has two entry points (blur_scalar /
    /// blur_vector); the generated kernel merges them behind a runtime `mode`
    /// branch and runs through the dim-aware (texture_storage_3d, @workgroup_size
    /// (4,4,4), vec3 id/uv) wrapper. The input is filled on-GPU with a 3D gradient;
    /// both kernels read it and their output volumes are read back (full depth via
    /// copy_texture_3d_to_buffer) and compared per voxel. Dual-packed: the hand
    /// uniform is {vol_res, axis, radius, _pad}, the generated is {mode, axis,
    /// vol_res, radius}.
    #[test]
    fn generated_blur_3d_separable_matches_original() {
        let device = crate::test_device();
        let n = 32u32;
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();

        let node = registry.construct("node.blur_3d_separable").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("blur_3d generates");
        // Structural: 3D texture types + 3D dispatch.
        assert!(
            generated.contains("texture_storage_3d<rgba16float, write>"),
            "3D storage output missing:\n{generated}"
        );
        assert!(
            generated.contains("var tex_in: texture_3d<f32>"),
            "3D sampled input missing:\n{generated}"
        );
        assert!(
            generated.contains("@compute @workgroup_size(4, 4, 4)"),
            "3D workgroup missing:\n{generated}"
        );
        let original =
            include_str!("../../generators/shaders/fluid_blur_3d.wgsl");

        // Fill the input volume with a 3D gradient (varies along every axis).
        let input = make_3d_texture(
            &device,
            n,
            GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
            "blur3d-in",
        );
        let fill_wgsl = "\
@group(0) @binding(0) var vol: texture_storage_3d<rgba16float, write>;\n\
@compute @workgroup_size(4, 4, 4)\n\
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {\n\
    let d = textureDimensions(vol);\n\
    if id.x >= d.x || id.y >= d.y || id.z >= d.z { return; }\n\
    let f = vec3<f32>(id) / vec3<f32>(d);\n\
    textureStore(vol, vec3<i32>(id), vec4<f32>(f.x, f.y, f.z, 0.5 + 0.5 * f.x));\n\
}\n";
        {
            let pipeline = device.create_compute_pipeline(fill_wgsl, ENTRY, "blur3d-fill");
            let mut enc = device.create_encoder("blur3d-fill");
            enc.dispatch_compute(
                &pipeline,
                &[GpuBinding::Texture { binding: 0, texture: &input }],
                [n.div_ceil(4), n.div_ceil(4), n.div_ceil(4)],
                "blur3d-fill",
            );
            enc.commit_and_wait_completed();
        }

        let run = |wgsl: &str, entry: &str, param_bytes: &[u8]| -> Vec<u16> {
            let pipeline = device.create_compute_pipeline(wgsl, entry, "blur3d");
            let sampler = device.create_sampler(&GpuSamplerDesc::default());
            let out = make_3d_texture(
                &device,
                n,
                GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
                "blur3d-out",
            );
            let mut enc = device.create_encoder("blur3d");
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: param_bytes },
                    GpuBinding::Texture { binding: 1, texture: &input },
                    GpuBinding::Sampler { binding: 2, sampler: &sampler },
                    GpuBinding::Texture { binding: 3, texture: &out },
                ],
                [n.div_ceil(4), n.div_ceil(4), n.div_ceil(4)],
                "blur3d",
            );
            enc.commit_and_wait_completed();
            let bytes_per_row = n * 8; // rgba16float
            let total = u64::from(bytes_per_row) * u64::from(n) * u64::from(n);
            let buf = device.create_buffer_shared(total);
            let mut renc = device.create_encoder("blur3d-readback");
            renc.copy_texture_3d_to_buffer(&out, &buf, n, n, n, bytes_per_row);
            renc.commit_and_wait_completed();
            let ptr = buf.mapped_ptr().expect("shared buffer pointer");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (n * n * n * 4) as usize) };
            halves.to_vec()
        };

        // axis=0, radius=4. Hand layout {vol_res, axis, radius, _pad}.
        let mut hand = [0u8; 16];
        hand[0..4].copy_from_slice(&n.to_le_bytes()); // vol_res
        hand[8..12].copy_from_slice(&4.0f32.to_le_bytes()); // radius
        // Generated layout {mode, axis, vol_res, radius}.
        let gen_bytes = |mode: u32| -> [u8; 16] {
            let mut b = [0u8; 16];
            b[0..4].copy_from_slice(&mode.to_le_bytes());
            b[8..12].copy_from_slice(&(n as i32).to_le_bytes()); // vol_res
            b[12..16].copy_from_slice(&4.0f32.to_le_bytes()); // radius
            b
        };

        for (mode, entry) in [(0u32, "blur_scalar"), (1u32, "blur_vector")] {
            let hand_vol = run(original, entry, &hand);
            let gen_vol = run(&generated, ENTRY, &gen_bytes(mode));
            assert_eq!(hand_vol.len(), gen_vol.len());
            let mut max_abs = 0.0f32;
            for (a, b) in hand_vol.iter().zip(gen_vol.iter()) {
                let fa = half::f16::from_bits(*a).to_f32();
                let fb = half::f16::from_bits(*b).to_f32();
                max_abs = max_abs.max((fa - fb).abs());
            }
            assert!(
                max_abs < 1e-3,
                "blur_3d mode={mode} ({entry}): generated must reproduce the hand kernel \
                 (max_abs={max_abs})"
            );
        }
    }

    /// Fill an n³ density volume on-GPU with a 3D gradient (varies along x/y/z).
    fn fill_volume_gradient(device: &GpuDevice, vol: &GpuTexture, n: u32) {
        let fill_wgsl = "\
@group(0) @binding(0) var vol: texture_storage_3d<rgba16float, write>;\n\
@compute @workgroup_size(4, 4, 4)\n\
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {\n\
    let d = textureDimensions(vol);\n\
    if id.x >= d.x || id.y >= d.y || id.z >= d.z { return; }\n\
    let f = vec3<f32>(id) / vec3<f32>(d);\n\
    textureStore(vol, vec3<i32>(id), vec4<f32>(f.x, f.y, f.z, 0.5 + 0.5 * f.x));\n\
}\n";
        let pipeline = device.create_compute_pipeline(fill_wgsl, ENTRY, "vol-fill");
        let mut enc = device.create_encoder("vol-fill");
        enc.dispatch_compute(
            &pipeline,
            &[GpuBinding::Texture { binding: 0, texture: vol }],
            [n.div_ceil(4), n.div_ceil(4), n.div_ceil(4)],
            "vol-fill",
        );
        enc.commit_and_wait_completed();
    }

    /// Read back a full n³ volume as f16 bits.
    fn readback_volume(device: &GpuDevice, vol: &GpuTexture, n: u32) -> Vec<u16> {
        let bytes_per_row = n * 8; // rgba16float
        let total = u64::from(bytes_per_row) * u64::from(n) * u64::from(n);
        let buf = device.create_buffer_shared(total);
        let mut renc = device.create_encoder("vol-readback");
        renc.copy_texture_3d_to_buffer(vol, &buf, n, n, n, bytes_per_row);
        renc.commit_and_wait_completed();
        let ptr = buf.mapped_ptr().expect("shared buffer pointer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (n * n * n * 4) as usize) };
        halves.to_vec()
    }

    /// 3D GatherTexel parity: node.gradient_central_diff_3d reads its density
    /// volume via integer textureLoad (6-tap central difference, toroidal wrap, NO
    /// sampler). The generated kernel binds uniform(0)/tex(1)/dst(2) — identical to
    /// the hand layout (GatherTexel emits no sampler) — so one uniform drives both.
    /// The hand entry is `main`; the generated is `cs_main`.
    #[test]
    fn generated_gradient_central_diff_3d_matches_original() {
        let device = crate::test_device();
        let n = 32u32;
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();

        let node = registry.construct("node.gradient_central_diff_3d").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("gradient_central_diff_3d generates");
        assert!(
            !generated.contains("var samp: sampler"),
            "a GatherTexel input must bind no sampler:\n{generated}"
        );
        assert!(
            generated.contains("var tex_density: texture_3d<f32>"),
            "3D sampled input missing:\n{generated}"
        );
        let original = include_str!("../primitives/shaders/gradient_central_diff_3d.wgsl");

        let density = make_3d_texture(
            &device,
            n,
            GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
            "grad3d-in",
        );
        fill_volume_gradient(&device, &density, n);

        // {vol_res, vol_depth, _pad, _pad} — same bits for hand (u32) + generated (i32).
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&n.to_le_bytes());
        bytes[4..8].copy_from_slice(&n.to_le_bytes());

        let run = |wgsl: &str, entry: &str| -> Vec<u16> {
            let pipeline = device.create_compute_pipeline(wgsl, entry, "grad3d");
            let out = make_3d_texture(
                &device,
                n,
                GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
                "grad3d-out",
            );
            let mut enc = device.create_encoder("grad3d");
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: &bytes },
                    GpuBinding::Texture { binding: 1, texture: &density },
                    GpuBinding::Texture { binding: 2, texture: &out },
                ],
                [n.div_ceil(4), n.div_ceil(4), n.div_ceil(4)],
                "grad3d",
            );
            enc.commit_and_wait_completed();
            readback_volume(&device, &out, n)
        };

        let hand_vol = run(original, "main");
        let gen_vol = run(&generated, ENTRY);
        let mut max_abs = 0.0f32;
        for (a, b) in hand_vol.iter().zip(gen_vol.iter()) {
            let fa = half::f16::from_bits(*a).to_f32();
            let fb = half::f16::from_bits(*b).to_f32();
            max_abs = max_abs.max((fa - fb).abs());
        }
        assert!(
            max_abs < 1e-3,
            "gradient_central_diff_3d: generated must reproduce the hand kernel (max_abs={max_abs})"
        );
    }

    /// 3D CoincidentTexel parity (dual-packed): node.curl_slope_force_3d reads its
    /// gradient volume at the OWN voxel (integer textureLoad, no sampler) and
    /// combines curl + slope. ref_axis is CPU-normalized in run(), so the body
    /// uses it directly (no GPU rsqrt — bit-exact). The hand uniform pads
    /// vol_res/vol_depth to 16 (48 bytes); the generated Params are contiguous (32
    /// bytes) — pack each from the same logical values.
    #[test]
    fn generated_curl_slope_force_3d_matches_original() {
        let device = crate::test_device();
        let n = 32u32;
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();

        let node = registry.construct("node.curl_slope_force_3d").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("curl_slope_force_3d generates");
        assert!(
            !generated.contains("var samp: sampler"),
            "a CoincidentTexel input binds no sampler:\n{generated}"
        );
        let original = include_str!("../primitives/shaders/curl_slope_force_3d.wgsl");

        let gradient = make_3d_texture(
            &device,
            n,
            GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
            "curl3d-in",
        );
        fill_volume_gradient(&device, &gradient, n);

        // Pre-normalized ref_axis (CPU), curl=2, slope=-1.
        let raw = [0.3f32, 0.8, 0.5];
        let inv = (raw[0] * raw[0] + raw[1] * raw[1] + raw[2] * raw[2]).sqrt().recip();
        let ax = [raw[0] * inv, raw[1] * inv, raw[2] * inv];
        let (curl, slope) = (2.0f32, -1.0f32);

        // Hand layout: {vol_res, vol_depth, _pad, _pad, curl, slope, ax, ay, az, _pad×3} = 48B.
        let mut hand = vec![0u8; 48];
        hand[0..4].copy_from_slice(&n.to_le_bytes());
        hand[4..8].copy_from_slice(&n.to_le_bytes());
        hand[16..20].copy_from_slice(&curl.to_le_bytes());
        hand[20..24].copy_from_slice(&slope.to_le_bytes());
        hand[24..28].copy_from_slice(&ax[0].to_le_bytes());
        hand[28..32].copy_from_slice(&ax[1].to_le_bytes());
        hand[32..36].copy_from_slice(&ax[2].to_le_bytes());
        // Generated layout: {vol_res, vol_depth, curl, slope, ax, ay, az, _pad} = 32B.
        let mut gen_bytes = vec![0u8; 32];
        gen_bytes[0..4].copy_from_slice(&n.to_le_bytes());
        gen_bytes[4..8].copy_from_slice(&n.to_le_bytes());
        gen_bytes[8..12].copy_from_slice(&curl.to_le_bytes());
        gen_bytes[12..16].copy_from_slice(&slope.to_le_bytes());
        gen_bytes[16..20].copy_from_slice(&ax[0].to_le_bytes());
        gen_bytes[20..24].copy_from_slice(&ax[1].to_le_bytes());
        gen_bytes[24..28].copy_from_slice(&ax[2].to_le_bytes());

        let run = |wgsl: &str, entry: &str, bytes: &[u8]| -> Vec<u16> {
            let pipeline = device.create_compute_pipeline(wgsl, entry, "curl3d");
            let out = make_3d_texture(
                &device,
                n,
                GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
                "curl3d-out",
            );
            let mut enc = device.create_encoder("curl3d");
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: bytes },
                    GpuBinding::Texture { binding: 1, texture: &gradient },
                    GpuBinding::Texture { binding: 2, texture: &out },
                ],
                [n.div_ceil(4), n.div_ceil(4), n.div_ceil(4)],
                "curl3d",
            );
            enc.commit_and_wait_completed();
            readback_volume(&device, &out, n)
        };

        let hand_vol = run(original, "main", &hand);
        let gen_vol = run(&generated, ENTRY, &gen_bytes);
        let mut max_abs = 0.0f32;
        for (a, b) in hand_vol.iter().zip(gen_vol.iter()) {
            let fa = half::f16::from_bits(*a).to_f32();
            let fb = half::f16::from_bits(*b).to_f32();
            max_abs = max_abs.max((fa - fb).abs());
        }
        assert!(
            max_abs < 1e-3,
            "curl_slope_force_3d: generated must reproduce the hand kernel (max_abs={max_abs})"
        );
    }

    /// Vector-op parity: length_vec2 + normalize_vec2 are paramless Pointwise
    /// (tex0/samp1/dst2); rotate_vec2_by_angle is Pointwise — the hand shader still
    /// reads CPU-precomputed cos_a/sin_a while the generated body computes them from
    /// `angle`, so they're dual-packed and compared at f16 precision (the output is
    /// f16, so the sub-f16 GPU-vs-CPU trig difference is below the store).
    #[test]
    fn generated_vector_op_atoms_match_originals() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let input = gradient(&device, w, h); // .rg = (x/w, y/h)
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let shaders_dir =
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
        let differ = TextureDiff::new(&device);

        // Paramless vector ops.
        for (type_id, shader) in [
            ("node.length_vec2", "length_vec2.wgsl"),
            ("node.normalize_vec2", "normalize_vec2.wgsl"),
        ] {
            let node = registry.construct(type_id).unwrap();
            let generated = generate_standalone(
                node.fusion_kind(),
                node.wgsl_body().unwrap(),
                node.inputs(),
                node.parameters(),
                node.input_access(),
                node.outputs(),
            )
            .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
            let original = std::fs::read_to_string(format!("{shaders_dir}/{shader}"))
                .unwrap_or_else(|e| panic!("read {shader}: {e}"));
            let from_original = dispatch_paramless_pointwise(&device, &original, &input);
            let from_generated = dispatch_paramless_pointwise(&device, &generated, &input);
            let r = differ.compare(
                &device,
                &from_original.texture,
                &from_generated.texture,
                1e-5,
                1e-5,
            );
            assert_eq!(
                r.over_count, 0,
                "{type_id}: generated must reproduce {shader} (max_abs={}, max_rel={})",
                r.max_abs, r.max_rel
            );
        }

        // rotate_vec2_by_angle: dual-packed (hand cos/sin vs generated angle).
        let node = registry.construct("node.rotate_vec2_by_angle").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("rotate_vec2 generates");
        let original = std::fs::read_to_string(format!("{shaders_dir}/rotate_vec2_by_angle.wgsl"))
            .expect("read rotate_vec2_by_angle.wgsl");
        let angle = 0.7f32;
        let hand_bytes = pack_f32(&[angle.cos(), angle.sin()]); // hand reads cos_a/sin_a
        let gen_bytes = pack_f32(&[angle]); // generated reads angle
        let from_original = dispatch_pointwise(&device, &original, &input, &hand_bytes);
        let from_generated = dispatch_pointwise(&device, &generated, &input, &gen_bytes);
        // f16-level tolerance: the GPU-vs-CPU trig difference is sub-f16.
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            3e-3,
            3e-3,
        );
        assert_eq!(
            r.over_count, 0,
            "rotate_vec2_by_angle: generated must reproduce the hand kernel at f16 \
             (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    /// Single-input CoincidentTexel parity: node.hash_field_by_seed reads `field`
    /// at the OWN texel via integer textureLoad (no sampler) and hashes it with a
    /// seed. Binding layout uniform(0)/tex(1)/dst(2) — no sampler — for both the
    /// hand and generated kernel, so one payload drives both. hash2/hash1 use GPU
    /// sin (matching the hand), so it's bit-exact.
    #[test]
    fn generated_hash_field_by_seed_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let input = gradient(&device, w, h); // .rg = a value field
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let differ = TextureDiff::new(&device);

        let node = registry.construct("node.hash_field_by_seed").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("hash_field_by_seed generates");
        assert!(
            !generated.contains("var samp: sampler"),
            "a CoincidentTexel input binds no sampler:\n{generated}"
        );
        let original = include_str!("../primitives/shaders/hash_field_by_seed.wgsl");

        // {seed, seed_x, seed_y, mode} = 16B; mode=0 (Hash2).
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&3.0f32.to_le_bytes());
        bytes[4..8].copy_from_slice(&1.73f32.to_le_bytes());
        bytes[8..12].copy_from_slice(&2.91f32.to_le_bytes());

        let run = |wgsl: &str| -> RenderTarget {
            let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "hashseed");
            let out = RenderTarget::new(&device, w, h, FMT, "hashseed-out");
            let mut enc = device.create_encoder("hashseed");
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: &bytes },
                    GpuBinding::Texture { binding: 1, texture: &input },
                    GpuBinding::Texture { binding: 2, texture: &out.texture },
                ],
                [w.div_ceil(16), h.div_ceil(16), 1],
                "hashseed",
            );
            enc.commit_and_wait_completed();
            out
        };

        let from_original = run(original);
        let from_generated = run(&generated);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "generated hash_field_by_seed must reproduce the hand kernel (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    /// OPTIONAL-INPUT use-flag parity: node.pack_channels combines 4 optional
    /// coincident inputs (r/g/b/a) into RGBA, falling back to default_* when an
    /// input is unwired (use_*==0). The codegen injects a use_<name> flag per
    /// optional input. Dual-packed: the hand uniform is {use_r..use_a, defaults[4]}
    /// (use first), the generated is {default_r..a, use_r..a} (params then injected
    /// flags). use=[1,0,1,1] exercises both the wired-read and default-fallback
    /// paths. Binding layout uniform(0)/r(1)/g(2)/b(3)/a(4)/samp(5)/dst(6) for both.
    #[test]
    fn generated_pack_channels_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let ga = gradient(&device, w, h);
        let gb = gradient_b(&device, w, h);
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let differ = TextureDiff::new(&device);

        let node = registry.construct("node.pack_channels").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("pack_channels generates");
        assert!(
            generated.contains("use_r: u32"),
            "optional-input use flag missing:\n{generated}"
        );
        let original = include_str!("../primitives/shaders/pack_channels.wgsl");

        let use_flags = [1u32, 0, 1, 1]; // g unwired → falls back to default_g
        let defaults = [0.1f32, 0.5, 0.2, 1.0];
        // Hand: {use_r..use_a, defaults[4]}.
        let mut hand = Vec::new();
        for u in use_flags {
            hand.extend_from_slice(&u.to_le_bytes());
        }
        for d in defaults {
            hand.extend_from_slice(&d.to_le_bytes());
        }
        // Generated: {default_r..a, use_r..a}.
        let mut gen_bytes = Vec::new();
        for d in defaults {
            gen_bytes.extend_from_slice(&d.to_le_bytes());
        }
        for u in use_flags {
            gen_bytes.extend_from_slice(&u.to_le_bytes());
        }

        let inputs = [&ga, &gb, &ga, &gb];
        let from_original = dispatch_coincident_n(&device, original, &inputs, &hand);
        let from_generated = dispatch_coincident_n(&device, &generated, &inputs, &gen_bytes);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "generated pack_channels must reproduce the hand kernel (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    /// trig_texture parity: 3 coincident inputs (in + optional freq_tex/phase_tex)
    /// with injected use-flags. The uniform layout already matches (params then
    /// flags), but the HAND shader binds its output at 3 (before the optional
    /// textures) while the generated kernel is regular (textures/sampler/output),
    /// so the hand needs a custom dispatch and the generated uses
    /// dispatch_coincident_n. use_freq_tex=1 (per-pixel freq) + use_phase_tex=0
    /// (scalar phase) exercises both paths; GPU sin matches bit-exact.
    #[test]
    fn generated_trig_texture_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let in_tex = gradient(&device, w, h);
        let freq_t = gradient_b(&device, w, h);
        let phase_t = gradient(&device, w, h); // unused (use_phase_tex=0)
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let differ = TextureDiff::new(&device);

        let node = registry.construct("node.trig_texture").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("trig_texture generates");
        assert!(
            generated.contains("use_freq_tex: u32") && generated.contains("use_phase_tex: u32"),
            "optional-input use flags missing:\n{generated}"
        );
        let original = include_str!("../primitives/shaders/trig_texture.wgsl");

        // {freq, phase, mode, use_freq_tex, use_phase_tex, _pad×3} = 32B.
        let mut bytes = vec![0u8; 32];
        bytes[0..4].copy_from_slice(&2.0f32.to_le_bytes()); // freq
        bytes[4..8].copy_from_slice(&0.5f32.to_le_bytes()); // phase
        // mode = 0 (Sin)
        bytes[12..16].copy_from_slice(&1u32.to_le_bytes()); // use_freq_tex = per-pixel
        // use_phase_tex = 0 (scalar)

        let from_generated =
            dispatch_coincident_n(&device, &generated, &[&in_tex, &freq_t, &phase_t], &bytes);
        // Hand layout: uniform(0), in(1), sampler(2), out(3), freq_tex(4), phase_tex(5).
        let hand_out = {
            let pipeline = device.create_compute_pipeline(original, ENTRY, "trig-hand");
            let sampler = device.create_sampler(&GpuSamplerDesc::default());
            let out = RenderTarget::new(&device, w, h, FMT, "trig-hand-out");
            let mut enc = device.create_encoder("trig-hand");
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: &bytes },
                    GpuBinding::Texture { binding: 1, texture: &in_tex },
                    GpuBinding::Sampler { binding: 2, sampler: &sampler },
                    GpuBinding::Texture { binding: 3, texture: &out.texture },
                    GpuBinding::Texture { binding: 4, texture: &freq_t },
                    GpuBinding::Texture { binding: 5, texture: &phase_t },
                ],
                [w.div_ceil(16), h.div_ceil(16), 1],
                "trig-hand",
            );
            enc.commit_and_wait_completed();
            out
        };
        let r = differ.compare(
            &device,
            &hand_out.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "generated trig_texture must reproduce the hand kernel (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    /// TIME-PARAM + MULTI-OUTPUT SOURCE parity: node.block_displace_field emits
    /// `offset` + `hash` from a per-block hash animated by `time` (now a backing
    /// param so the generated body reads it from the uniform). Dual-packed: the
    /// hand uniform is {amount, block_size, speed, time} (16B), the generated adds
    /// the multi-output write flags (32B). Both bind uniform(0)/out(1)/out(2); diff
    /// each output. bdf_hash2 uses GPU sin, so it's bit-exact.
    #[test]
    fn generated_block_displace_field_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let differ = TextureDiff::new(&device);

        let node = registry.construct("node.block_displace_field").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("block_displace_field generates");
        assert!(
            generated.contains("struct BodyOutputs") && generated.contains("write_offset: u32"),
            "multi-output struct/flags missing:\n{generated}"
        );
        let original = include_str!("../primitives/shaders/block_displace_field.wgsl");

        let (amount, block_size, speed, time) = (0.8f32, 16.0f32, 2.0f32, 1.0f32);
        let mut hand = vec![0u8; 16];
        hand[0..4].copy_from_slice(&amount.to_le_bytes());
        hand[4..8].copy_from_slice(&block_size.to_le_bytes());
        hand[8..12].copy_from_slice(&speed.to_le_bytes());
        hand[12..16].copy_from_slice(&time.to_le_bytes());
        let mut gen_bytes = vec![0u8; 32];
        gen_bytes[0..16].copy_from_slice(&hand);
        gen_bytes[16..20].copy_from_slice(&1u32.to_le_bytes()); // write_offset
        gen_bytes[20..24].copy_from_slice(&1u32.to_le_bytes()); // write_hash

        let (h_off, h_hash) = dispatch_two_output_source(&device, original, &hand, w, h);
        let (g_off, g_hash) = dispatch_two_output_source(&device, &generated, &gen_bytes, w, h);
        let r_off = differ.compare(&device, &h_off.texture, &g_off.texture, 1e-5, 1e-5);
        assert_eq!(
            r_off.over_count, 0,
            "block_displace `offset`: generated must reproduce the hand kernel (max_abs={})",
            r_off.max_abs
        );
        let r_hash = differ.compare(&device, &h_hash.texture, &g_hash.texture, 1e-5, 1e-5);
        assert_eq!(
            r_hash.over_count, 0,
            "block_displace `hash`: generated must reproduce the hand kernel (max_abs={})",
            r_hash.max_abs
        );
    }

    /// lic_integrate parity: 2-input gather (source + velocity both walked along
    /// the streamline). `steps` is an Int param (i32), so it can't go through
    /// pack_f32 (f32 bits would mis-read as int) — hand-pack steps=16 (i32) + dt=2
    /// (f32). Both kernels read the same bits; dispatch_coincident binds
    /// uniform(0)/source(1)/velocity(2)/samp(3)/dst(4) for both.
    #[test]
    fn generated_lic_integrate_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let ga = gradient(&device, w, h); // source
        let gb = gradient_b(&device, w, h); // velocity (.rg)
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let differ = TextureDiff::new(&device);

        let node = registry.construct("node.lic_integrate").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("lic_integrate generates");
        let original = include_str!("../primitives/shaders/lic_integrate.wgsl");

        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&16i32.to_le_bytes()); // steps
        bytes[4..8].copy_from_slice(&2.0f32.to_le_bytes()); // dt

        let from_original = dispatch_coincident(&device, original, &ga, &gb, &bytes);
        let from_generated = dispatch_coincident(&device, &generated, &ga, &gb, &bytes);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "generated lic_integrate must reproduce the hand kernel (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    /// MIXED-DIM parity: node.sample_volume_2d gathers a Texture3D `in` at a slice
    /// coord and writes a Texture2D `out`. The generated kernel must bind tex_in as
    /// texture_3d (per-input dim) while the wrapper stays 2D (output dim). Fill a
    /// 32^3 volume, sample it into a 2D output; both kernels share uniform(0)/
    /// volume(1)/samp(2)/dst(3) and the same payload.
    #[test]
    fn generated_sample_volume_2d_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let n = 32u32;
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let differ = TextureDiff::new(&device);

        let node = registry.construct("node.sample_volume_2d").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("sample_volume_2d generates");
        assert!(
            generated.contains("var tex_in: texture_3d<f32>"),
            "3D input binding missing:\n{generated}"
        );
        assert!(
            generated.contains("var dst: texture_storage_2d<rgba16float, write>"),
            "2D output binding missing:\n{generated}"
        );
        let original = include_str!("../primitives/shaders/sample_volume_2d.wgsl");

        let volume = make_3d_texture(
            &device,
            n,
            GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
            "svol-in",
        );
        fill_volume_gradient(&device, &volume, n);

        // {slice_z, uv_scale, center_x, center_y}.
        let mut bytes = [0u8; 16];
        bytes[0..4].copy_from_slice(&0.5f32.to_le_bytes()); // slice_z
        bytes[4..8].copy_from_slice(&1.0f32.to_le_bytes()); // uv_scale

        let run = |wgsl: &str| -> RenderTarget {
            let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "svol");
            let sampler = device.create_sampler(&GpuSamplerDesc::default());
            let out = RenderTarget::new(&device, w, h, FMT, "svol-out");
            let mut enc = device.create_encoder("svol");
            enc.dispatch_compute(
                &pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: &bytes },
                    GpuBinding::Texture { binding: 1, texture: &volume },
                    GpuBinding::Sampler { binding: 2, sampler: &sampler },
                    GpuBinding::Texture { binding: 3, texture: &out.texture },
                ],
                [w.div_ceil(16), h.div_ceil(16), 1],
                "svol",
            );
            enc.commit_and_wait_completed();
            out
        };

        let from_original = run(original);
        let from_generated = run(&generated);
        let r = differ.compare(
            &device,
            &from_original.texture,
            &from_generated.texture,
            1e-5,
            1e-5,
        );
        assert_eq!(
            r.over_count, 0,
            "generated sample_volume_2d must reproduce the hand kernel (max_abs={}, max_rel={})",
            r.max_abs, r.max_rel
        );
    }

    /// Dispatch a two-output SOURCE kernel: uniform(0), dst_a(1), dst_b(2). Both
    /// outputs get their own texture (no aliasing) so each can be diffed.
    fn dispatch_two_output_source(
        device: &GpuDevice,
        wgsl: &str,
        param_bytes: &[u8],
        w: u32,
        h: u32,
    ) -> (RenderTarget, RenderTarget) {
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-multi-out");
        let a = RenderTarget::new(device, w, h, FMT, "codegen-out-a");
        let b = RenderTarget::new(device, w, h, FMT, "codegen-out-b");
        let mut enc = device.create_encoder("codegen-multi-out");
        enc.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: param_bytes },
                GpuBinding::Texture { binding: 1, texture: &a.texture },
                GpuBinding::Texture { binding: 2, texture: &b.texture },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "codegen-multi-out",
        );
        enc.commit_and_wait_completed();
        (a, b)
    }

    /// MULTI-OUTPUT SOURCE parity: node.voronoi_2d writes two storage textures
    /// (`out` = F1/F2/edge/cell_hash, `cell_id` = the F1-winning cell coordinate).
    /// The generated kernel declares both as dst_<port>, the body returns a
    /// BodyOutputs struct, and each store is gated on an injected write_<port>
    /// flag. Those flags land at the same offsets as the hand uniform's
    /// write_out/write_cell_id, so the generated Params layout equals
    /// VoronoiUniforms exactly — one payload drives both kernels. Diff each output
    /// independently (both write flags on, distinct textures, no aliasing).
    #[test]
    fn generated_voronoi_2d_matches_original() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let differ = TextureDiff::new(&device);

        let node = registry.construct("node.voronoi_2d").unwrap();
        let generated = generate_standalone(
            node.fusion_kind(),
            node.wgsl_body().unwrap(),
            node.inputs(),
            node.parameters(),
            node.input_access(),
            node.outputs(),
        )
        .expect("voronoi_2d generates");
        // Structural: two storage outputs, a struct return, and per-output gates.
        assert!(
            generated.contains("var dst_out: texture_storage_2d<rgba16float, write>"),
            "dst_out binding missing:\n{generated}"
        );
        assert!(
            generated.contains("var dst_cell_id: texture_storage_2d<rgba16float, write>"),
            "dst_cell_id binding missing:\n{generated}"
        );
        assert!(generated.contains("struct BodyOutputs"), "struct missing:\n{generated}");
        assert!(generated.contains("write_out: u32"), "write_out flag missing:\n{generated}");
        assert!(
            generated.contains("write_cell_id: u32"),
            "write_cell_id flag missing:\n{generated}"
        );
        let original = include_str!("../primitives/shaders/voronoi_2d.wgsl");

        // {scale, offset_x, offset_y, jitter, out_scale, write_out, write_cell_id, _pad}.
        let mut bytes = vec![0u8; 32];
        bytes[0..4].copy_from_slice(&8.0f32.to_le_bytes()); // scale
        bytes[12..16].copy_from_slice(&1.0f32.to_le_bytes()); // jitter (full random)
        bytes[16..20].copy_from_slice(&1.0f32.to_le_bytes()); // out_scale
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes()); // write_out
        bytes[24..28].copy_from_slice(&1u32.to_le_bytes()); // write_cell_id

        let (h_out, h_cell) = dispatch_two_output_source(&device, original, &bytes, w, h);
        let (g_out, g_cell) = dispatch_two_output_source(&device, &generated, &bytes, w, h);
        let r_out = differ.compare(&device, &h_out.texture, &g_out.texture, 1e-5, 1e-5);
        assert_eq!(
            r_out.over_count, 0,
            "voronoi `out`: generated must reproduce voronoi_2d.wgsl (max_abs={}, max_rel={})",
            r_out.max_abs, r_out.max_rel
        );
        let r_cell = differ.compare(&device, &h_cell.texture, &g_cell.texture, 1e-5, 1e-5);
        assert_eq!(
            r_cell.over_count, 0,
            "voronoi `cell_id`: generated must reproduce voronoi_2d.wgsl (max_abs={}, max_rel={})",
            r_cell.max_abs, r_cell.max_rel
        );
    }

    /// Dispatch a SOURCE (generator) kernel: [uniform(0)], output. No texture
    /// inputs, no sampler — a paramless source binds only its output at binding 0.
    fn dispatch_source(
        device: &GpuDevice,
        wgsl: &str,
        param_bytes: Option<&[u8]>,
        w: u32,
        h: u32,
    ) -> RenderTarget {
        let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-source");
        let out = RenderTarget::new(device, w, h, FMT, "codegen-out-source");
        let mut bindings: Vec<GpuBinding> = Vec::new();
        let mut next = 0u32;
        if let Some(bytes) = param_bytes {
            bindings.push(GpuBinding::Bytes { binding: 0, data: bytes });
            next = 1;
        }
        bindings.push(GpuBinding::Texture { binding: next, texture: &out.texture });
        let mut enc = device.create_encoder("codegen-source");
        enc.dispatch_compute(
            &pipeline,
            &bindings,
            [w.div_ceil(16), h.div_ceil(16), 1],
            "codegen-source",
        );
        enc.commit_and_wait_completed();
        out
    }

    /// Source (generator) parity (overnight sweep): a 0-input atom produces from
    /// uv/dims/params, no colour input. checkerboard (params → uniform0/out1) and
    /// the paramless uv_field (out0 only — exercises the no-uniform Source path)
    /// both reproduce their hand shaders bit-for-bit.
    #[test]
    fn generated_source_atoms_match_originals() {
        let device = crate::test_device();
        let (w, h) = (128u32, 128u32);
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let shaders_dir =
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/node_graph/primitives/shaders");
        let differ = TextureDiff::new(&device);

        // checkerboard PARAMS: [scale, offset_x, offset_y].
        let checker_bytes = pack_f32(&[8.0, 0.0, 0.0]);
        // centered_uv PARAMS: [cx, cy, scale_x, scale_y] (16-byte uniform).
        let centered_bytes = pack_f32(&[0.5, 0.5, 2.0, 2.0]);
        // linear_gradient PARAMS: [cx, cy, rotation, softness]; its hand uniform
        // is padded to 32 bytes, so pack 32 (satisfies the 32-byte hand decl AND
        // the 16-byte generated decl — a larger buffer binds fine to a smaller
        // uniform).
        let lg_bytes = pack_f32(&[0.5, 0.5, 0.785, 0.3, 0.0, 0.0, 0.0, 0.0]);
        // distance_to_point [cx,cy,scale,scale_x,scale_y] (32B hand uniform).
        let dist_bytes = pack_f32(&[0.3, 0.7, 1.5, 2.0, 1.0, 0.0, 0.0, 0.0]);
        // polar_field [cx,cy] (16B).
        let polar_bytes = pack_f32(&[0.3, 0.7]);
        // box_mask [cx,cy,half_width,half_height,rotation,softness] (32B).
        let box_bytes = pack_f32(&[0.5, 0.5, 0.25, 0.25, 0.785, 0.1, 0.0, 0.0]);
        // mirror_fold_uv [mode] (Enum -> u32), packed by hand to 16B.
        let mut mirror_bytes = vec![0u8; 16];
        mirror_bytes[0..4].copy_from_slice(&8u32.to_le_bytes()); // FoldBoth
        // radial_fold_uv [segments, cx, cy] (16B).
        let radial_bytes = pack_f32(&[6.0, 0.5, 0.5]);
        // ellipse_mask [cx,cy,radius_x,radius_y,rotation,softness] (32B).
        let ellipse_bytes = pack_f32(&[0.5, 0.5, 0.3, 0.2, 0.785, 0.1, 0.0, 0.0]);
        // dither_pattern [algorithm] (Enum -> u32), 16B; 0 = Bayer (the LUT path).
        let mut dither_pat_bytes = vec![0u8; 16];
        dither_pat_bytes[0..4].copy_from_slice(&0u32.to_le_bytes());
        // simplex_field_2d [scale_x, scale_y, offset_x, offset_y, z, output_channel
        // (u32)], 32B — packed by hand for the mid-struct u32.
        let mut simplex_bytes = vec![0u8; 32];
        simplex_bytes[0..4].copy_from_slice(&3.0f32.to_le_bytes());
        simplex_bytes[4..8].copy_from_slice(&3.0f32.to_le_bytes());
        simplex_bytes[16..20].copy_from_slice(&0.5f32.to_le_bytes()); // z
        // offset_x/y = 0, output_channel = 0 (R) — already zeroed.
        // node.noise [type(u32), scale, offset_x, offset_y, octaves(i32),
        // lacunarity, persistence], 32B — one case per branch to exercise every
        // helper (Perlin fBM / Simplex snoise / Random hash).
        let noise_case = |ty: i32, scale: f32, octaves: i32| {
            let mut b = vec![0u8; 32];
            b[0..4].copy_from_slice(&ty.to_le_bytes());
            b[4..8].copy_from_slice(&scale.to_le_bytes());
            b[16..20].copy_from_slice(&octaves.to_le_bytes());
            b[20..24].copy_from_slice(&2.0f32.to_le_bytes()); // lacunarity
            b[24..28].copy_from_slice(&0.5f32.to_le_bytes()); // persistence
            b
        };
        let noise_perlin = noise_case(0, 4.0, 3); // Perlin + fBM (3 octaves)
        let noise_simplex = noise_case(1, 4.0, 1); // Simplex
        let noise_random = noise_case(2, 8.0, 1); // Random hash
        // radial_offset_field [mode (u32), angle, falloff], 16B; mode=0 (Radial).
        let mut radial_offset_bytes = vec![0u8; 16];
        radial_offset_bytes[0..4].copy_from_slice(&0u32.to_le_bytes()); // mode = Radial
        radial_offset_bytes[8..12].copy_from_slice(&0.5f32.to_le_bytes()); // falloff
        // uv_strip_clamp [width, mode (u32)], 16B; mode=2 (Both).
        let mut strip_bytes = vec![0u8; 16];
        strip_bytes[0..4].copy_from_slice(&0.5f32.to_le_bytes()); // width
        strip_bytes[4..8].copy_from_slice(&2u32.to_le_bytes()); // mode = Both
        // scanline_jitter_field [amount, scanline, speed, time], 16B. GPU sin →
        // bit-exact; time is a backing param packed by run().
        let scanline_bytes = pack_f32(&[0.8, 0.3, 2.0, 1.0]);
        // flow_field_noise [time, z_scale, warp_scale, resolution], 16B. warp=0.5
        // exercises the domain warp; resolution slot ignored by the body.
        let flow_bytes = pack_f32(&[1.0, 0.01, 0.5, 0.0]);
        let cases: &[(&str, &str, Option<&[u8]>)] = &[
            ("node.checkerboard", "checkerboard.wgsl", Some(checker_bytes.as_slice())),
            ("node.uv_field", "uv_field.wgsl", None),
            ("node.centered_uv", "centered_uv.wgsl", Some(centered_bytes.as_slice())),
            ("node.linear_gradient", "linear_gradient.wgsl", Some(lg_bytes.as_slice())),
            ("node.distance_to_point", "distance_to_point.wgsl", Some(dist_bytes.as_slice())),
            ("node.polar_field", "polar_field.wgsl", Some(polar_bytes.as_slice())),
            ("node.box_mask", "box_mask.wgsl", Some(box_bytes.as_slice())),
            ("node.mirror_fold_uv", "mirror_fold_uv.wgsl", Some(mirror_bytes.as_slice())),
            ("node.radial_fold_uv", "radial_fold_uv.wgsl", Some(radial_bytes.as_slice())),
            ("node.ellipse_mask", "ellipse_mask.wgsl", Some(ellipse_bytes.as_slice())),
            ("node.dither_pattern", "dither_pattern.wgsl", Some(dither_pat_bytes.as_slice())),
            ("node.simplex_field_2d", "simplex_field_2d.wgsl", Some(simplex_bytes.as_slice())),
            ("node.noise", "noise.wgsl", Some(noise_perlin.as_slice())),
            ("node.noise", "noise.wgsl", Some(noise_simplex.as_slice())),
            ("node.noise", "noise.wgsl", Some(noise_random.as_slice())),
            ("node.radial_offset_field", "radial_offset_field.wgsl", Some(radial_offset_bytes.as_slice())),
            ("node.uv_strip_clamp", "uv_strip_clamp.wgsl", Some(strip_bytes.as_slice())),
            ("node.scanline_jitter_field", "scanline_jitter_field.wgsl", Some(scanline_bytes.as_slice())),
            ("node.flow_field_noise", "flow_field_noise.wgsl", Some(flow_bytes.as_slice())),
        ];
        for (type_id, shader_file, bytes) in cases {
            let node = registry.construct(type_id).unwrap();
            let generated = generate_standalone(
                node.fusion_kind(),
                node.wgsl_body().unwrap(),
                node.inputs(),
                node.parameters(),
                node.input_access(),
                node.outputs(),
            )
            .unwrap_or_else(|e| panic!("{type_id} generate: {e:?}"));
            let original = std::fs::read_to_string(format!("{shaders_dir}/{shader_file}"))
                .unwrap_or_else(|e| panic!("read {shader_file}: {e}"));
            let from_original = dispatch_source(&device, &original, *bytes, w, h);
            let from_generated = dispatch_source(&device, &generated, *bytes, w, h);
            let r = differ.compare(
                &device,
                &from_original.texture,
                &from_generated.texture,
                1e-5,
                1e-5,
            );
            assert_eq!(
                r.over_count, 0,
                "{type_id}: generated must reproduce {shader_file} (max_abs={}, max_rel={})",
                r.max_abs, r.max_rel
            );
        }
    }
}
