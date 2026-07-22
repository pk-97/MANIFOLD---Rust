use std::fmt::Write as _;

use crate::node_graph::freeze::classify::{FusionKind, InputAccess};
use crate::node_graph::parameters::{ParamDef, ParamType};
use crate::node_graph::ports::{ChannelSpec, NodeInput, NodeOutput, PortType};

use super::types::{
    buffer_element_type, dim_forms, is_texture_input, is_texture_port, param_wgsl_type,
    param_word_count, wgsl_safe_field, CodegenError, TexDim,
};
use super::uniforms::emit_buffer_struct;


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
    derived_uniforms: &[&str],
    outputs: &[NodeOutput],
) -> Result<String, CodegenError> {
    generate_standalone_ext(
        fusion_kind,
        body,
        inputs,
        params,
        input_access,
        derived_uniforms,
        outputs,
        false,
        &[],
    )
}

/// [`generate_standalone`] with the STENCIL-FETCH body ABI flag and shared
/// WGSL library includes. `includes` mirrors the buffer path's handling
/// exactly: deduped-by-caller, prepended verbatim before the body so its
/// helper calls resolve (same shape as `generate_standalone_buffer`'s
/// identical block). When
/// `stencil_fetch` is true, each sampler-`Gather` input is read by the body
/// through a free `fetch_<port>(uv) -> vec4<f32>` function this wrapper
/// DEFINES as the real `textureSampleLevel` over the bound texture — instead
/// of passing `(texture, sampler)` body args. Compiles to identical code
/// (the fn inlines); the indirection is what lets the FUSED codegen swap a
/// recomputed virtual source in for the texture. `false` keeps every
/// existing atom's generated WGSL byte-identical.
#[allow(clippy::too_many_arguments)]
pub fn generate_standalone_ext(
    fusion_kind: FusionKind,
    body: &str,
    inputs: &[NodeInput],
    params: &[ParamDef],
    input_access: &[InputAccess],
    derived_uniforms: &[&str],
    outputs: &[NodeOutput],
    stencil_fetch: bool,
    includes: &[&str],
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
        // atomic outputs; standalone_for_spec routes buffer atoms with their
        // ATOMIC_OUTPUTS before reaching here. `derived_uniforms` DOES thread
        // through — a buffer atom reached via this generic entry point (e.g. the
        // classify final-gate parse-check in region.rs) still needs its derived
        // fields to generate the same Params layout its real dispatch will use.
        return generate_standalone_buffer(
            body, inputs, params, input_access, derived_uniforms, includes, outputs, &[],
        );
    }
    let tex_inputs: Vec<&NodeInput> = inputs.iter().filter(|i| is_texture_input(i)).collect();
    // D3 (BUG-114): an Array input on an otherwise texture-domain atom (the
    // `draw_*` family — a detections/marks array read while writing a pixel).
    // `INPUT_ACCESS` packs [texture accesses] ++ [array accesses] for such an
    // atom (see `input_port_access`'s D3 comment in region.rs); every entry
    // here MUST be `BufferIndex` — anything else is a codegen-shape error (an
    // Array input the region-grower can't yet express any other way on the
    // texture path).
    let array_inputs: Vec<&NodeInput> =
        inputs.iter().filter(|i| matches!(i.ty, PortType::Array(_))).collect();
    for (ai, _) in array_inputs.iter().enumerate() {
        let access = input_access.get(tex_inputs.len() + ai).copied().unwrap_or_default();
        if access != InputAccess::BufferIndex {
            return Err(CodegenError::BadInput);
        }
    }
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
    // output write flags or optional-input use flags) or derived fields (a
    // camera-consuming atom with no user params still needs the uniform for its
    // frame-derived values) even if there are no params.
    let has_uniform = !params.is_empty()
        || multi_output
        || !optional_tex_inputs.is_empty()
        || !derived_uniforms.is_empty();

    let mut out = String::new();

    // --- D3 (BUG-114): element struct(s) for any Array/BufferIndex input,
    // synthesized from the port's Channels signature by the SAME helpers the
    // buffer codegen path uses (`buffer_element_type`/`emit_buffer_struct`) —
    // this is what generalizes `ExtK` synthesis to every `draw_*` atom's own
    // detections/marks signature, not just `draw_dots`' `Detection`. Emitted
    // before Params (mirroring the buffer path's struct-then-Params order). ---
    let mut structs: Vec<(&'static [ChannelSpec], String)> = Vec::new();
    let array_elem_tys: Vec<String> = array_inputs
        .iter()
        .map(|i| match &i.ty {
            PortType::Array(at) => buffer_element_type(at.specs, &mut structs),
            _ => unreachable!("filtered to Array ports"),
        })
        .collect();
    for (specs, name) in &structs {
        out.push_str(&emit_buffer_struct(specs, name));
        out.push('\n');
    }

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
            let f = wgsl_safe_field(p.name.as_ref());
            if p.ty == ParamType::Vec3 {
                // A vec3 param expands to three consecutive f32 fields.
                writeln!(out, "    {f}_x: f32,").unwrap();
                writeln!(out, "    {f}_y: f32,").unwrap();
                writeln!(out, "    {f}_z: f32,").unwrap();
            } else if matches!(p.ty, ParamType::Vec4 | ParamType::Color) {
                // A vec4/color param expands to four consecutive f32 fields,
                // reassembled as a vec4<f32> at the body call site below.
                writeln!(out, "    {f}_x: f32,").unwrap();
                writeln!(out, "    {f}_y: f32,").unwrap();
                writeln!(out, "    {f}_z: f32,").unwrap();
                writeln!(out, "    {f}_w: f32,").unwrap();
            } else {
                let ty = param_wgsl_type(p)?;
                writeln!(out, "    {f}: {ty},").unwrap();
            }
            header_words += param_word_count(p)?;
        }
        // Injected non-param derived fields (frame-derived values recomputed by
        // the atom's run() each frame from a CPU-struct input, e.g. a Camera's
        // basis vectors) — placed right after the scalar params, mirroring the
        // buffer path's layout (generate_standalone_buffer). Same naming/packing
        // convention: "name" defaults to f32, "name:ty" is an explicit scalar
        // type, "name:vec3" expands to three consecutive f32 fields.
        for d in derived_uniforms {
            let (dname, dty) = d.split_once(':').unwrap_or((d, "f32"));
            if dty == "vec3" {
                writeln!(out, "    {dname}_x: f32,").unwrap();
                writeln!(out, "    {dname}_y: f32,").unwrap();
                writeln!(out, "    {dname}_z: f32,").unwrap();
                header_words += 3;
            } else {
                writeln!(out, "    {dname}: {dty},").unwrap();
                header_words += 1;
            }
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
    // D3 (BUG-114): each Array/BufferIndex input binds its own storage global,
    // named `buf_<port>` (the standalone kernel is single-atom, so no cross-
    // member name collision — the fused path namespaces this to `src_<slot>`
    // instead, see `generate_fused`). The body references it directly by name
    // (no pre-read, no arg — exactly `BufferGather`'s ABI) and guards with
    // `arrayLength` itself, same as the hand `draw_*` shaders already do.
    for (ai, inp) in array_inputs.iter().enumerate() {
        writeln!(
            out,
            "@group(0) @binding({next_binding}) var<storage, read> buf_{}: array<{}>;",
            inp.name, array_elem_tys[ai]
        )
        .unwrap();
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

    // --- stencil-fetch ABI: define `fetch_<port>` over each sampler-Gather
    // input as the real textureSampleLevel, so the body's free fetch calls
    // resolve. Inlines to the exact code the (tex, samp)-args form produced. ---
    if stencil_fetch {
        for (i, inp) in tex_inputs.iter().enumerate() {
            if access_of(i) == InputAccess::Gather {
                writeln!(
                    out,
                    "fn fetch_{}(c: vec2<f32>) -> vec4<f32> {{\n    \
                     return textureSampleLevel(tex_{}, samp, c, 0.0);\n}}\n",
                    inp.name, inp.name
                )
                .unwrap();
            }
        }
    }

    // --- shared WGSL library includes (e.g. depth_common's linearize_depth),
    // prepended so the body's helper calls resolve — mirrors
    // generate_standalone_buffer's identical block and run()'s
    // format!("{lib}\n{body}") convention. ---
    for inc in includes {
        out.push_str(inc.trim_end());
        out.push_str("\n\n");
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
            // BufferGather/BufferIndex only tag Array inputs (buffer atoms branch
            // away above; a texture-domain atom's Array inputs are handled in
            // their own loop, never through `tex_inputs`) — unreachable on a
            // texture input, no pre-read either way.
            InputAccess::Gather
            | InputAccess::GatherTexel
            | InputAccess::BufferGather
            | InputAccess::BufferIndex => {}
        }
    }
    // body(<per-input args>, uv, dims, params.<p0>, ..., <derived fields>,
    // <table count/array pairs>, <optional-input use-flags>). Each input
    // contributes: a Coincident/CoincidentTexel input → its pre-read colour
    // register `c_<name>`; a Gather input → the texture handle `tex_<name>` +
    // the shared `samp`, which the body samples at a coord it computes. `uv`
    // (normalized center-of-texel) and `dims` (float canvas size) are the
    // ambient fragment context every body receives after its inputs (design
    // §slot / line 60, extended with dims so positional atoms recover aspect =
    // dims.x/dims.y and pixel = uv*dims). Derived fields (frame-recomputed
    // CPU-struct data, e.g. a Camera's basis vectors) follow the user params,
    // mirroring the buffer path's layout. Atoms that ignore an arg simply
    // don't read it — spirv-opt's DCE drops it.
    let mut args: Vec<String> = Vec::new();
    for (i, inp) in tex_inputs.iter().enumerate() {
        match access_of(i) {
            // Sampler-gather: the body owns the filter. Stencil-fetch bodies
            // read through the `fetch_<port>` fn defined above (no args);
            // (tex, samp)-args bodies get the texture + the shared sampler.
            InputAccess::Gather => {
                if !stencil_fetch {
                    args.push(format!("tex_{}", inp.name));
                    args.push("samp".to_string());
                }
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
            // A texture-domain atom's Array input is handled in its own loop
            // below (the global `buf_<port>` binding, no body arg) — this
            // `tex_inputs`-indexed loop never reaches a BufferIndex entry.
            InputAccess::BufferIndex => {
                unreachable!("BufferIndex only tags Array inputs — never a texture input")
            }
        }
    }
    args.push("uv".to_string());
    args.push(forms.dims_arg.to_string());
    // Scalar params first (PARAMS order; Vec3 reassembled), then the derived
    // fields (matching the struct layout above), then each table as (count,
    // array) — matching the body signature `…, <t>_count, <t>` per table.
    for p in params {
        if p.ty == ParamType::Table {
            continue;
        }
        let f = wgsl_safe_field(p.name.as_ref());
        if p.ty == ParamType::Vec3 {
            args.push(format!("vec3<f32>(params.{f}_x, params.{f}_y, params.{f}_z)"));
        } else if matches!(p.ty, ParamType::Vec4 | ParamType::Color) {
            args.push(format!(
                "vec4<f32>(params.{f}_x, params.{f}_y, params.{f}_z, params.{f}_w)"
            ));
        } else {
            args.push(format!("params.{f}"));
        }
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
pub(super) fn generate_standalone_buffer(
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
        .map(|o| (o.name.as_ref(), buffer_element_type(specs_of(&o.ty), &mut structs)))
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
    let out_is_atomic = !multi_output && atomic_outputs.contains(&array_outputs[0].name.as_ref());
    if out_is_atomic && out_infos[0].1 != "u32" && out_infos[0].1 != "i32" {
        return Err(CodegenError::AtomicNonInteger { ty: out_infos[0].1.clone() });
    }
    // Multi-output atomic isn't a shape any atom needs yet.
    if multi_output && array_outputs.iter().any(|o| atomic_outputs.contains(&o.name.as_ref())) {
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
        let f = wgsl_safe_field(p.name.as_ref());
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
        let f = wgsl_safe_field(p.name.as_ref());
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
pub(super) fn generate_standalone_resolve(
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
            let f = wgsl_safe_field(p.name.as_ref());
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
        let f = wgsl_safe_field(p.name.as_ref());
        args.push(format!("params.{f}"));
    }
    writeln!(out, "    let result = body({});", args.join(", ")).unwrap();
    writeln!(out, "    textureStore(dst, {}, result);", forms.store_coord).unwrap();
    out.push_str("}\n");

    Ok(out)
}
