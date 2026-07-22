use std::fmt::Write as _;

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::freeze::classify::InputAccess;
use crate::node_graph::freeze::markers::Marker;
use crate::node_graph::parameters::ParamType;
use crate::node_graph::ports::{ChannelSpec, NodeInput, NodeOutput, PortType};

use super::types::{
    buffer_element_type, is_texture_input, is_texture_port, param_wgsl_type, param_word_count,
    CodegenError, FusionRegion, GeneratedFusion, InputSource, RegionNode,
};
use super::uniforms::{emit_buffer_struct, emit_derived_uniform_markers};


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
pub(crate) fn rename_ident(text: &str, from: &str, to: &str) -> String {
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

pub(super) fn generate_fused_buffer(region: &FusionRegion<'_>) -> Result<GeneratedFusion, CodegenError> {
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
        /// Per texture input (in port order, after the array entries):
        /// whether it is a `Texture3D` — drives the fused kernel's
        /// `texture_3d<f32>` vs `texture_2d<f32>` external declaration.
        tex_3d: Vec<bool>,
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
        // Sampled 2D / 3D textures only (`node.wgsl_compute` introspects both;
        // 3D is the volume force-field read the integrator chains need).
        if tex_in.iter().any(|p| !matches!(p.ty, PortType::Texture2D | PortType::Texture3D)) {
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
        let tex_3d: Vec<bool> = tex_in.iter().map(|p| p.ty == PortType::Texture3D).collect();
        member_io.push(MemberIo { in_specs, out_specs, tex_3d });
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
        Texture { is_3d: bool },
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
                    // Texture entries follow the array entries in node.inputs
                    // order, so `k - arr_count` indexes the member's texture
                    // ports. Dimensionality must agree across consumers of one
                    // external (one producer port, one type) — the PartialEq
                    // mismatch check below fails closed if it doesn't.
                    let is_3d =
                        member_io[mi].tex_3d.get(k - arr_count).copied().ok_or(CodegenError::BadInput)?;
                    ExtKind::Texture { is_3d }
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
            ExtKind::Texture { .. } => None,
        })
        .collect();
    let out_member = index_of(region.outputs[0].0).ok_or(CodegenError::BadInput)?;
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
            if p.ty == ParamType::Vec3 {
                // Vec3 param → three consecutive namespaced f32 fields
                // (wgsl-vec3-alignment convention; matches the standalone
                // path's `<name>_x/_y/_z` packing, P5/D4).
                writeln!(struct_body, "    n{i}_{}_x: f32,", p.name).unwrap();
                writeln!(struct_body, "    n{i}_{}_y: f32,", p.name).unwrap();
                writeln!(struct_body, "    n{i}_{}_z: f32,", p.name).unwrap();
            } else if matches!(p.ty, ParamType::Vec4 | ParamType::Color) {
                // Vec4/Color param → four consecutive namespaced f32 fields,
                // already word-aligned (no padding needed) — P5/D4 scope
                // expansion, same mechanism the standalone path's "P3 wave 2"
                // reassembly already proved.
                writeln!(struct_body, "    n{i}_{}_x: f32,", p.name).unwrap();
                writeln!(struct_body, "    n{i}_{}_y: f32,", p.name).unwrap();
                writeln!(struct_body, "    n{i}_{}_z: f32,", p.name).unwrap();
                writeln!(struct_body, "    n{i}_{}_w: f32,", p.name).unwrap();
            } else {
                let ty = param_wgsl_type(p)?;
                writeln!(struct_body, "    n{i}_{}: {ty},", p.name).unwrap();
            }
            param_order.push((node.node_id, crate::node_graph::intern_name(&p.name)));
            field_count += param_word_count(p)?;
        }
    }
    // Frame-derived uniform fields (`dt_scaled`, `frame_count:u32`, a camera's
    // `cam_fwd_x`/`_y`/`_z`, …) per member, after the params — mirrors the
    // standalone path's emission, namespaced `n{i}_<name>`. NOT added to
    // `param_order`: these fields are never sourced from a wire OR inst.params —
    // `node.wgsl_compute` recomputes their VALUES itself every frame via
    // `derived_uniform_registry::recompute(member.type_id, ...)`, keyed off the
    // `// @derived_uniform_member:` marker `emit_derived_uniform_markers` writes
    // below (D7/P0, `docs/CINEMATIC_POST_DESIGN.md`; superseded the old
    // install-time control-wire whitelist).
    for (i, node) in region.nodes.iter().enumerate() {
        for d in node.derived_uniforms {
            let (dname, dty) = d.split_once(':').unwrap_or((d, "f32"));
            if dty == "vec3" {
                // A vec3 derived field expands to three f32 fields, matching the
                // standalone packing (a camera-basis atom, e.g. a future
                // `coc_from_depth`-style member).
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
    emit_derived_uniform_markers(&mut out, region);

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
            // 3D externals (volume force fields) declare texture_3d — the
            // body's own signature already expects that handle type.
            None => {
                let tex_ty = match ext_kinds[e] {
                    ExtKind::Texture { is_3d: true } => "texture_3d<f32>",
                    _ => "texture_2d<f32>",
                };
                writeln!(out, "@group(0) @binding({binding}) var src_{e}: {tex_ty};").unwrap();
            }
        }
        binding += 1;
    }
    // The shared gather sampler, when any texture external exists. The default
    // "clamp" emits no marker — byte-identical to the standalone buffer atoms'
    // default sampler; a non-default mode rides the same side-channel marker the
    // texture path uses (`node.wgsl_compute` reads it at sampler creation).
    if ext_kinds.iter().any(|k| matches!(k, ExtKind::Texture { .. })) {
        if region.sampler_address_mode == "clamp" {
            writeln!(out, "@group(0) @binding({binding}) var samp: sampler;").unwrap();
        } else {
            let marker =
                Marker::SamplerAddressMode { mode: region.sampler_address_mode.to_string() };
            writeln!(out, "@group(0) @binding({binding}) var samp: sampler; {}", marker.emit())
                .unwrap();
        }
        binding += 1;
    }
    if in_place.is_none() {
        writeln!(out, "{}", Marker::FusedOutput.emit()).unwrap();
        writeln!(
            out,
            "@group(0) @binding({binding}) var<storage, read_write> dst: array<{out_ty}>;"
        )
        .unwrap();
    }
    // `@dispatch_count_param` — node.wgsl_compute reads this marker and sizes
    // the dispatch grid from the named uniform field's live value (min'd with
    // capacity) instead of the array length. cs_main carries the matching
    // in-kernel guard.
    if let Some((mi, pname)) = region.dispatch_count_field {
        let marker = Marker::DispatchCountParam { field: format!("n{mi}_{pname}") };
        writeln!(out, "{}", marker.emit()).unwrap();
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
    // Count anchor: the ARRAY externals (slot 0 first in every all-array region,
    // keeping prior fused WGSL — a pipeline-cache key — byte-identical). Each is
    // coincident with the output (one output element per input element) and every
    // one is pre-read at `[idx]` below; a texture slot has no arrayLength, so skip
    // it. With a SINGLE array external the count is that external's length exactly
    // (unchanged text). With MORE THAN ONE, bound by the SHORTEST so a shorter
    // input can't be read out of bounds (BUG-008) — the unfused atoms clamp to
    // `min(a, b, …)` for the same reason. Equal-length regions (every shipped
    // buffer preset) are unaffected: `min` of equal lengths is that length.
    let array_ext: Vec<usize> = ext_tys
        .iter()
        .enumerate()
        .filter_map(|(e, t)| t.is_some().then_some(e))
        .collect();
    let count_anchor = *array_ext.first().ok_or(CodegenError::BadInput)?;
    if array_ext.len() == 1 {
        writeln!(out, "    let count = arrayLength(&src_{count_anchor});").unwrap();
    } else {
        let mut expr = format!("arrayLength(&src_{count_anchor})");
        for &e in &array_ext[1..] {
            expr = format!("min({expr}, arrayLength(&src_{e}))");
        }
        writeln!(out, "    let count = {expr};").unwrap();
    }
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
                // no buffer ARRAY input fuses unwired, and virtual sources are
                // texture-domain only — reaching either here is a finder bug.
                // A multi-output NodeOutput source is texture-domain only too
                // (D4/P6: buffer atoms with texture outputs are boundaries, so
                // no ARRAY register ever comes from one) — reaching it here is
                // the same class of finder bug.
                InputSource::Unwired | InputSource::Virtual(_) | InputSource::NodeOutput(..) => {
                    return Err(CodegenError::BadInput);
                }
            }
        }
        for p in node.params {
            if p.ty == ParamType::Vec3 {
                args.push(format!(
                    "vec3<f32>(params.n{i}_{}_x, params.n{i}_{}_y, params.n{i}_{}_z)",
                    p.name, p.name, p.name
                ));
            } else if matches!(p.ty, ParamType::Vec4 | ParamType::Color) {
                args.push(format!(
                    "vec4<f32>(params.n{i}_{}_x, params.n{i}_{}_y, params.n{i}_{}_z, params.n{i}_{}_w)",
                    p.name, p.name, p.name, p.name
                ));
            } else {
                args.push(format!("params.n{i}_{}", p.name));
            }
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

    // D3 (BUG-114): per-external-slot kind. A TEXTURE region's external is
    // normally a texture producer, but a member may now tag an Array input
    // `BufferIndex` (the `draw_*` family) — that producer is an ARRAY, bound
    // as `var<storage, read> src_<e>: array<ExtK>` instead of a texture. Each
    // member's `node.inputs`/`input_access` pack [texture entries] ++ [array
    // entries] (region.rs's `build_region` D3 append), so an input index
    // `idx >= tex_count` for that member names one of ITS array ports, in
    // declared order — the texture-domain analogue of `generate_fused_buffer`'s
    // `ExtKind` resolution.
    #[derive(Clone, Copy)]
    enum ExtKind {
        Texture,
        Array(&'static [ChannelSpec]),
    }
    let mut ext_kinds: Vec<ExtKind> = vec![ExtKind::Texture; region.num_external_inputs];
    for node in region.nodes.iter() {
        let tex_count = node.node_inputs.iter().filter(|p| is_texture_input(p)).count();
        let array_ports: Vec<&NodeInput> =
            node.node_inputs.iter().filter(|p| matches!(p.ty, PortType::Array(_))).collect();
        for (idx, src) in node.inputs.iter().enumerate() {
            if node.input_access.get(idx).copied().unwrap_or_default() != InputAccess::BufferIndex {
                continue;
            }
            if let InputSource::External(e) = src {
                if *e >= region.num_external_inputs {
                    return Err(CodegenError::BadInput);
                }
                let specs = match array_ports.get(idx - tex_count) {
                    Some(NodeInput { ty: PortType::Array(at), .. }) => at.specs,
                    _ => return Err(CodegenError::BadInput),
                };
                ext_kinds[*e] = ExtKind::Array(specs);
            }
        }
    }
    // Element struct(s) for any Array-kind external, synthesized from the
    // port's Channels signature by the same helpers the buffer codegen path
    // uses — generalizes `ExtK` to every `draw_*` atom's own signature.
    let mut ext_structs: Vec<(&'static [ChannelSpec], String)> = Vec::new();
    let ext_array_tys: Vec<Option<String>> = ext_kinds
        .iter()
        .map(|k| match k {
            ExtKind::Array(specs) => Some(buffer_element_type(specs, &mut ext_structs)),
            ExtKind::Texture => None,
        })
        .collect();

    // Per-node: split body into a top-level prelude (consts/structs), helpers,
    // and the `body` fn (renamed n{i}_body).
    let mut prelude: Vec<String> = Vec::new(); // deduped top-level decls, emitted once
    let mut helpers: Vec<FnBlock> = Vec::new(); // deduped, emitted once
    let mut bodies: Vec<String> = Vec::new(); // namespaced body fns
    let mut includes: Vec<&'static str> = Vec::new(); // deduped shared WGSL libs (noise_common, depth_common, …) — BUG-135
    for (i, node) in region.nodes.iter().enumerate() {
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
        // D3 (BUG-114): rewrite this member's BufferIndex-tagged array-input
        // token `buf_<port>` (the standalone body's local storage-global name)
        // to the region's resolved external slot `src_<e>` — the array
        // analogue of the stencil-fetch renaming below, riding the same
        // whole-fragment namespacing this codegen already does for structs.
        let tex_count = node.node_inputs.iter().filter(|p| is_texture_input(p)).count();
        let array_ports: Vec<&NodeInput> =
            node.node_inputs.iter().filter(|p| matches!(p.ty, PortType::Array(_))).collect();
        let mut member_body = node.body.to_string();
        for (arr_idx, port) in array_ports.iter().enumerate() {
            let slot_idx = tex_count + arr_idx;
            if node.input_access.get(slot_idx).copied().unwrap_or_default() != InputAccess::BufferIndex
            {
                continue;
            }
            match node.inputs.get(slot_idx) {
                Some(InputSource::External(e)) => {
                    member_body =
                        rename_ident(&member_body, &format!("buf_{}", port.name), &format!("src_{e}"));
                }
                _ => return Err(CodegenError::BadInput),
            }
        }
        // D4/P6: a MULTI-output member (struct-return body, ≥2 texture
        // outputs — voronoi_2d's `out`/`cell_id`) declares its own
        // `N{i}BodyOutputs` struct (namespaced so two multi-output members in
        // one region, or a name collision with another member's helper,
        // can't clash) and its body's `-> BodyOutputs` / `return
        // BodyOutputs(...)` are rewritten to match — the fused analogue of
        // the standalone wrapper's single global `BodyOutputs` (which is safe
        // there because a standalone kernel has exactly one member). Every
        // member with exactly one (or zero) texture outputs is untouched —
        // byte-identical for every existing region.
        let member_tex_outputs: Vec<&NodeOutput> =
            node.node_outputs.iter().filter(|o| is_texture_port(&o.ty)).collect();
        if member_tex_outputs.len() > 1 {
            let struct_name = format!("N{i}BodyOutputs");
            member_body = rename_ident(&member_body, "BodyOutputs", &struct_name);
            let mut decl = format!("struct {struct_name} {{\n");
            for o in &member_tex_outputs {
                writeln!(decl, "    {}: vec4<f32>,", o.name).unwrap();
            }
            decl.push_str("}\n");
            if !prelude.contains(&decl) {
                prelude.push(decl);
            }
        }
        let (pre, blocks) = split_fns(&member_body);
        for line in pre {
            // Dedup identical declarations (two atoms declaring the same const
            // collapse to one). A same-name / different-value clash would surface
            // as a naga redefinition error, caught by the oracle — never silent.
            if !prelude.contains(&line) {
                prelude.push(line);
            }
        }
        if node.stencil_fetch {
            // STENCIL member: its helpers call the free `fetch_<port>` fns, so
            // they can't dedup across members — namespace the member's ENTIRE
            // fragment (every fn it defines + every fetch reference) with the
            // `n{i}_` prefix and emit it whole. Module-scope fn order is free
            // in WGSL, so the fetch definitions (emitted below the helpers)
            // resolve regardless of position.
            if !blocks.iter().any(|b| b.name == "body") {
                return Err(CodegenError::NoBody);
            }
            let fn_names: Vec<String> = blocks.iter().map(|b| b.name.clone()).collect();
            for fb in blocks {
                let mut text = fb.text;
                for name in &fn_names {
                    text = rename_ident(&text, name, &format!("n{i}_{name}"));
                }
                for port in node.node_inputs.iter().filter(|p| is_texture_input(p)) {
                    let fetch = format!("fetch_{}", port.name);
                    text = rename_ident(&text, &fetch, &format!("n{i}_{fetch}"));
                }
                bodies.push(text);
            }
            continue;
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
            if p.ty == ParamType::Vec3 {
                // Vec3 param → three consecutive namespaced f32 fields
                // (wgsl-vec3-alignment convention; matches the standalone
                // path's `<name>_x/_y/_z` packing, P5/D4).
                writeln!(struct_body, "    n{i}_{}_x: f32,", p.name).unwrap();
                writeln!(struct_body, "    n{i}_{}_y: f32,", p.name).unwrap();
                writeln!(struct_body, "    n{i}_{}_z: f32,", p.name).unwrap();
            } else if matches!(p.ty, ParamType::Vec4 | ParamType::Color) {
                // Vec4/Color param → four consecutive namespaced f32 fields,
                // already word-aligned (no padding needed) — P5/D4 scope
                // expansion, same mechanism the standalone path's "P3 wave 2"
                // reassembly already proved.
                writeln!(struct_body, "    n{i}_{}_x: f32,", p.name).unwrap();
                writeln!(struct_body, "    n{i}_{}_y: f32,", p.name).unwrap();
                writeln!(struct_body, "    n{i}_{}_z: f32,", p.name).unwrap();
                writeln!(struct_body, "    n{i}_{}_w: f32,", p.name).unwrap();
            } else {
                let ty = param_wgsl_type(p)?;
                writeln!(struct_body, "    n{i}_{}: {ty},", p.name).unwrap();
            }
            param_order.push((node.node_id, crate::node_graph::intern_name(&p.name)));
            field_count += param_word_count(p)?;
        }
    }
    // Frame-derived uniform fields (D7/P0) — see the identical block in
    // `generate_fused_buffer` for the full rationale. Texture-fused regions
    // never carried these before this phase (only the standalone texture path
    // did, and only as of the P0 standalone-layer commit); this is the fusion
    // half, letting a camera-derived pointwise atom (D1's `coc_from_depth`
    // shape) fuse with a pointwise neighbour instead of forcing a boundary.
    for (i, node) in region.nodes.iter().enumerate() {
        for d in node.derived_uniforms {
            let (dname, dty) = d.split_once(':').unwrap_or((d, "f32"));
            if dty == "vec3" {
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
    // D3 (BUG-114): element struct(s) for any Array-kind external, emitted
    // before Params (mirroring `generate_fused_buffer`'s struct-then-Params
    // order). Empty for every region without a BufferIndex member — no output
    // change to any existing fused texture kernel.
    for (specs, name) in &ext_structs {
        out.push_str(&emit_buffer_struct(specs, name));
        out.push('\n');
    }
    out.push_str("struct Params {\n");
    out.push_str(&struct_body);
    out.push_str("}\n\n");
    emit_derived_uniform_markers(&mut out, region);

    // --- which region-node indices are VIRTUAL (chain members recomputed inside
    // a stencil fetch). cs_main never evaluates them and never pre-reads their
    // externals at its own coord — the fetch textureLoads those per corner. ---
    let virtual_nodes: std::collections::BTreeSet<usize> = region
        .virtual_chains
        .iter()
        .flat_map(|c| c.members.iter().copied())
        .collect();

    // --- which inputs are gathered (the body samples a bound texture itself) vs
    // coincident (a register threaded in). A sampler is bound iff some input is a
    // sampler-`Gather` over a REAL external (a virtual-source fetch recomputes
    // instead of sampling); an external is pre-read into a register only if some
    // cs_main-evaluated node reads it coincidentally — a gather-only external is
    // never load-into-register (the body samples it at a coord it computes). ---
    let mut needs_sampler = false;
    let mut coincident_ext: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for (i, node) in region.nodes.iter().enumerate() {
        for (idx, src) in node.inputs.iter().enumerate() {
            let access = node.input_access.get(idx).copied().unwrap_or(InputAccess::Coincident);
            if access == InputAccess::Gather && matches!(src, InputSource::External(_)) {
                needs_sampler = true;
            }
            if virtual_nodes.contains(&i) {
                // EVERY chain external read goes through the shared sampler
                // (sampled at the corner uv, or by the gather body itself).
                if matches!(src, InputSource::External(_)) {
                    needs_sampler = true;
                }
                continue; // never pre-read at cs_main's own coord
            }
            if !access.is_gather()
                && let InputSource::External(e) = src
            {
                coincident_ext.insert(*e);
            }
        }
    }
    // CROSS-RESOLUTION externals (workstream 4): a space-mismatched coincident
    // external is pre-read through the shared sampler at uv (the resolution-robust
    // standalone read), so the region needs a sampler even with no gather member.
    if !region.sampled_externals.is_empty() {
        needs_sampler = true;
    }

    // --- bindings: uniform(0), external inputs(1..) [texture or, D3, storage
    // array], [sampler], output. ---
    out.push_str("@group(0) @binding(0) var<uniform> params: Params;\n");
    for (e, ty) in ext_array_tys.iter().enumerate() {
        match ty {
            Some(ty) => writeln!(
                out,
                "@group(0) @binding({}) var<storage, read> src_{e}: array<{ty}>;",
                e + 1
            )
            .unwrap(),
            None => writeln!(out, "@group(0) @binding({}) var src_{e}: texture_2d<f32>;", e + 1)
                .unwrap(),
        }
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
            let marker =
                Marker::SamplerAddressMode { mode: region.sampler_address_mode.to_string() };
            writeln!(
                out,
                "@group(0) @binding({next_binding}) var samp: sampler; {}",
                marker.emit()
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
        for (k, (out_id, _)) in region.outputs.iter().enumerate() {
            writeln!(
                out,
                "@group(0) @binding({}) var dst_{k}: texture_storage_2d<{}, write>;",
                next_binding + k,
                out_storage(*out_id)
            )
            .unwrap();
        }
    } else {
        writeln!(
            out,
            "@group(0) @binding({next_binding}) var dst: texture_storage_2d<{}, write>;",
            out_storage(region.outputs[0].0)
        )
        .unwrap();
    }
    out.push('\n');

    // --- shared WGSL library includes (e.g. depth_common's linearize_depth),
    // prepended so the bodies' helper calls resolve — mirrors
    // generate_fused_buffer's identical block. ---
    for inc in &includes {
        out.push_str(inc.trim_end());
        out.push_str("\n\n");
    }

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
    // unfused chain's store+load bit-for-bit. A virtual chain's OUTPUT gets the
    // same rounding (the unfused chain stored f16 for the consumer to sample)
    // unless its storage is fp32 (exact store — no rounding to reproduce).
    let chain_q16 = region.virtual_chains.iter().any(|c| {
        region
            .nodes
            .get(c.output)
            .map(|n| n.output_storage != "rgba32float")
            .unwrap_or(false)
    });
    if region.nodes.iter().any(|n| n.quantize_f16) || chain_q16 {
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
    // STENCIL members: one `n{i}_fetch_<port>` per sampler-Gather input.
    //   - REAL external: the literal textureSampleLevel over the bound src —
    //     exactly the read the standalone wrapper's fetch does, so a fused
    //     stencil member samples bit-identically to its unfused dispatch.
    //   - VIRTUAL source: re-evaluate the absorbed chain at the tap's four
    //     bilinear corner texels (`n{i}_vsrc_<port>`: hardware-faithful
    //     per-corner address wrap, exact textureLoads of the chain's externals,
    //     q16 on the chain output to reproduce its unfused f16 store) and lerp
    //     in f32. Corner values are bit-identical to the unfused chain's stored
    //     texels; the manual lerp vs the hardware filter unit is the documented
    //     stencil-tier gap, measured by the stencil parity proof.
    let dims_tex = if multi_output { "dst_0" } else { "dst" };
    for (i, node) in region.nodes.iter().enumerate() {
        if !node.stencil_fetch {
            continue;
        }
        let tex_specs: Vec<&NodeInput> =
            node.node_inputs.iter().filter(|p| is_texture_input(p)).collect();
        if tex_specs.len() != node.inputs.len() {
            return Err(CodegenError::BadInput); // need port names for fetch fns
        }
        for (idx, src) in node.inputs.iter().enumerate() {
            let access = node.input_access.get(idx).copied().unwrap_or(InputAccess::Coincident);
            if access != InputAccess::Gather {
                continue;
            }
            let port = tex_specs[idx].name.clone();
            match src {
                InputSource::External(e) => {
                    if *e >= region.num_external_inputs {
                        return Err(CodegenError::BadInput);
                    }
                    writeln!(
                        out,
                        "fn n{i}_fetch_{port}(c: vec2<f32>) -> vec4<f32> {{\n    \
                         return textureSampleLevel(src_{e}, samp, c, 0.0);\n}}\n",
                    )
                    .unwrap();
                }
                InputSource::Virtual(ci) => {
                    let chain = region
                        .virtual_chains
                        .get(*ci)
                        .filter(|c| c.consumer == i && c.input_index == idx)
                        .ok_or(CodegenError::BadInput)?;
                    // Per-corner texel index under the consumer's sampler
                    // address mode — each corner wraps independently, exactly
                    // how the hardware filter footprint addresses.
                    let wrap = match region.sampler_address_mode {
                        "repeat" => "    let ti = ((t % di) + di) % di;\n",
                        "mirror" => {
                            "    let p = ((t % (di * 2)) + di * 2) % (di * 2);\n    \
                             let ti = select(p, di * 2 - vec2<i32>(1) - p, p >= di);\n"
                        }
                        _ => "    let ti = clamp(t, vec2<i32>(0), di - vec2<i32>(1));\n",
                    };
                    let mut vsrc = format!(
                        "fn n{i}_vsrc_{port}(t: vec2<i32>, vd: vec2<f32>) -> vec4<f32> {{\n    \
                         let di = vec2<i32>(vd);\n{wrap}    \
                         let vuv = (vec2<f32>(ti) + vec2<f32>(0.5)) / vd;\n"
                    );
                    for &j in &chain.members {
                        let cnode = region.nodes.get(j).ok_or(CodegenError::BadInput)?;
                        let args =
                            chain_member_args(j, cnode, region, &index_of, &chain.members)?;
                        writeln!(vsrc, "    let v{j} = n{j}_body({});", args.join(", ")).unwrap();
                    }
                    let out_node =
                        region.nodes.get(chain.output).ok_or(CodegenError::BadInput)?;
                    if out_node.output_storage == "rgba32float" {
                        writeln!(vsrc, "    return v{};\n}}", chain.output).unwrap();
                    } else {
                        writeln!(vsrc, "    return q16(v{});\n}}", chain.output).unwrap();
                    }
                    out.push_str(&vsrc);
                    writeln!(
                        out,
                        "\nfn n{i}_fetch_{port}(c: vec2<f32>) -> vec4<f32> {{\n    \
                         let vd = vec2<f32>(textureDimensions({dims_tex}));\n    \
                         let x = c * vd - vec2<f32>(0.5);\n    \
                         let f = fract(x);\n    \
                         let i0 = vec2<i32>(floor(x));\n    \
                         let c00 = n{i}_vsrc_{port}(i0, vd);\n    \
                         let c10 = n{i}_vsrc_{port}(i0 + vec2<i32>(1, 0), vd);\n    \
                         let c01 = n{i}_vsrc_{port}(i0 + vec2<i32>(0, 1), vd);\n    \
                         let c11 = n{i}_vsrc_{port}(i0 + vec2<i32>(1, 1), vd);\n    \
                         return mix(mix(c00, c10, f.x), mix(c01, c11, f.x), f.y);\n}}\n",
                    )
                    .unwrap();
                }
                _ => return Err(CodegenError::BadInput),
            }
        }
    }
    for b in &bodies {
        out.push_str(b.trim_end());
        out.push_str("\n\n");
    }

    // --- cs_main: read external inputs once, thread registers, store output ---
    out.push_str("@compute @workgroup_size(16, 16)\n");
    out.push_str("fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {\n");
    writeln!(out, "    let dims = textureDimensions({dims_tex});").unwrap();
    out.push_str("    if id.x >= dims.x || id.y >= dims.y {\n        return;\n    }\n");
    out.push_str("    let coord = vec2<i32>(i32(id.x), i32(id.y));\n");
    // Ambient fragment context, computed once and threaded to every body after
    // its inputs (matches the standalone wrapper). `dims` is already bound above.
    out.push_str("    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);\n");
    for &e in &coincident_ext {
        // A same-space external is read texel-exact at the kernel's own coord
        // (byte-identical to v1). A CROSS-RESOLUTION external (its producer lives
        // at a different element space — e.g. Watercolor's half-res mask field)
        // is sampled through the shared sampler at the fragment UV, exactly the
        // resolution-robust read the unfused atom makes; a textureLoad here would
        // misread the wrong texel. The body receives `ext_{e}` either way.
        if region.sampled_externals.contains(&e) {
            writeln!(out, "    let ext_{e} = textureSampleLevel(src_{e}, samp, uv, 0.0);").unwrap();
        } else {
            writeln!(out, "    let ext_{e} = textureLoad(src_{e}, coord, 0);").unwrap();
        }
    }
    for (i, node) in region.nodes.iter().enumerate() {
        if virtual_nodes.contains(&i) {
            continue; // chain member — evaluated per corner inside its fetch
        }
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
                    // A stencil member reads through its `n{i}_fetch_<port>` fn
                    // (emitted above) — no body args. The (tex, samp)-args form
                    // gets the texture handle + the shared sampler.
                    if !node.stencil_fetch {
                        args.push(format!("src_{e}"));
                        args.push("samp".to_string());
                    }
                }
                // A virtual source: only a stencil member's sampler-Gather reads
                // one, through its fetch — no body args.
                (InputAccess::Gather, InputSource::Virtual(ci)) => {
                    if !node.stencil_fetch || *ci >= region.virtual_chains.len() {
                        return Err(CodegenError::BadInput);
                    }
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
                    InputSource::Node(_) | InputSource::NodeOutput(..) | InputSource::Unwired,
                ) => {
                    return Err(CodegenError::BadInput);
                }
                (InputAccess::GatherTexel, InputSource::Virtual(_)) => {
                    return Err(CodegenError::BadInput); // texel-load virtual is a follow-on
                }
                // D3 (BUG-114): a BufferIndex array input takes NO body arg — the
                // body references the storage global directly by name, already
                // rewritten to `src_<e>` above (`buf_<port>` → `src_<e>`), exactly
                // BufferGather's ABI. Must resolve to a real external (the finder
                // never unions a BufferIndex-consumed wire, so a Node/Unwired/
                // Virtual source here would be a codegen-shape bug).
                (InputAccess::BufferIndex, InputSource::External(e)) => {
                    if *e >= region.num_external_inputs {
                        return Err(CodegenError::BadInput);
                    }
                }
                (InputAccess::BufferIndex, _) => return Err(CodegenError::BadInput),
                (_, InputSource::Virtual(_)) => {
                    return Err(CodegenError::BadInput); // virtual backs gather reads only
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
                // D4/P6: the producer is MULTI-output — its register `r{j}` is
                // a `N{j}BodyOutputs` struct, not the value itself, so pick
                // the named field this wire actually threads.
                (_, InputSource::NodeOutput(id, port)) => {
                    let Some(j) = index_of(*id) else {
                        return Err(CodegenError::BadInput);
                    };
                    if j >= i {
                        return Err(CodegenError::BadInput); // not earlier in topo order
                    }
                    args.push(format!("r{j}.{port}"));
                }
            }
        }
        args.push("uv".to_string());
        args.push("vec2<f32>(dims)".to_string());
        for p in node.params {
            if p.ty == ParamType::Vec3 {
                args.push(format!(
                    "vec3<f32>(params.n{i}_{}_x, params.n{i}_{}_y, params.n{i}_{}_z)",
                    p.name, p.name, p.name
                ));
            } else if matches!(p.ty, ParamType::Vec4 | ParamType::Color) {
                args.push(format!(
                    "vec4<f32>(params.n{i}_{}_x, params.n{i}_{}_y, params.n{i}_{}_z, params.n{i}_{}_w)",
                    p.name, p.name, p.name, p.name
                ));
            } else {
                args.push(format!("params.n{i}_{}", p.name));
            }
        }
        // Frame-derived uniforms trail the params (D7/P0) — same body-arg order
        // the standalone path uses; a vec3 is reassembled from its three packed
        // f32 fields. `node.wgsl_compute` (not this codegen) is responsible for
        // keeping `params.n{i}_<name>` refreshed every frame.
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
            // store rounding so the loop can't drift fused-vs-unfused. A
            // MULTI-output member's register is a struct, not a vec4 — q16
            // only ever takes a vec4, so this combination is a codegen-shape
            // bug (structurally unreachable: quantize_f16 requires the member
            // sit on a feedback cycle, and every multi-output atom today is a
            // 0-texture-input Source, which can't read its own output back).
            let member_tex_outputs =
                node.node_outputs.iter().filter(|o| is_texture_port(&o.ty)).count();
            if member_tex_outputs > 1 {
                return Err(CodegenError::BadInput);
            }
            writeln!(out, "    let r{i} = q16(n{i}_body({}));", args.join(", ")).unwrap();
        } else {
            writeln!(out, "    let r{i} = n{i}_body({});", args.join(", ")).unwrap();
        }
    }
    // D4/P6: a MULTI-output member's register (`r{idx}`) is a `N{idx}BodyOutputs`
    // struct, not the value — the escaping port picks the field. Single-output
    // members keep the bare register, byte-identical to before.
    let store_expr = |idx: usize, port: &str| -> String {
        let is_multi_output_member =
            region.nodes[idx].node_outputs.iter().filter(|o| is_texture_port(&o.ty)).count() > 1;
        if is_multi_output_member {
            format!("r{idx}.{port}")
        } else {
            format!("r{idx}")
        }
    };
    if multi_output {
        // Fan-out: each escaping member's register stores to its own `dst_<k>`,
        // in `outputs` order (every one wired to a live consumer by install).
        for (k, (out_id, out_port)) in region.outputs.iter().enumerate() {
            let Some(idx) = index_of(*out_id) else {
                return Err(CodegenError::BadInput);
            };
            let expr = store_expr(idx, out_port);
            writeln!(out, "    textureStore(dst_{k}, coord, {expr});").unwrap();
        }
    } else {
        let Some((out_id, out_port)) = region.outputs.first() else {
            return Err(CodegenError::BadInput);
        };
        let Some(out_idx) = index_of(*out_id) else {
            return Err(CodegenError::BadInput);
        };
        let expr = store_expr(out_idx, out_port);
        writeln!(out, "    textureStore(dst, coord, {expr});").unwrap();
    }
    out.push_str("}\n");

    Ok(GeneratedFusion { wgsl: out, param_order })
}

/// Body-arg marshalling for one VIRTUAL chain member, evaluated inside a
/// stencil fetch's `n{i}_vsrc_<port>` at corner texel `ti` / uv `vuv` / dims
/// `vd`: a coincident external reads via `textureSampleLevel(src_e, samp,
/// vuv, 0.0)` — the same resolution-robust sampled read the unfused
/// standalone atom makes (exact texel at same-res, bilinear for a half-res
/// flow field); a sampler-Gather external passes `(src_e, samp)` so the body
/// samples at its own computed coords; an earlier chain member threads its
/// per-corner register `v{j}`; params/uv/dims/use-flags follow the cs_main
/// marshaller. The finder gates chains to Coincident/Gather members agreeing
/// on the region sampler mode — anything else here is a codegen bug.
fn chain_member_args(
    j: usize,
    node: &RegionNode<'_>,
    region: &FusionRegion<'_>,
    index_of: &dyn Fn(NodeInstanceId) -> Option<usize>,
    chain_members: &[usize],
) -> Result<Vec<String>, CodegenError> {
    let mut args: Vec<String> = Vec::new();
    for (idx, src) in node.inputs.iter().enumerate() {
        let access = node.input_access.get(idx).copied().unwrap_or(InputAccess::Coincident);
        if !matches!(access, InputAccess::Coincident | InputAccess::Gather) {
            return Err(CodegenError::BadInput);
        }
        match src {
            InputSource::External(e) => {
                if *e >= region.num_external_inputs {
                    return Err(CodegenError::BadInput);
                }
                if access == InputAccess::Gather {
                    args.push(format!("src_{e}"));
                    args.push("samp".to_string());
                } else {
                    args.push(format!("textureSampleLevel(src_{e}, samp, vuv, 0.0)"));
                }
            }
            InputSource::Node(id) => {
                if access == InputAccess::Gather {
                    return Err(CodegenError::BadInput); // a register isn't a texture
                }
                let Some(k) = index_of(*id) else {
                    return Err(CodegenError::BadInput);
                };
                // Must be an EARLIER member of this same chain.
                let pos = chain_members.iter().position(|&m| m == k);
                let own = chain_members.iter().position(|&m| m == j);
                match (pos, own) {
                    (Some(p), Some(o)) if p < o => args.push(format!("v{k}")),
                    _ => return Err(CodegenError::BadInput),
                }
            }
            // D4/P6: a multi-output producer never gets absorbed into a
            // stencil chain (region.rs bails defensively when building one) —
            // reaching this is a finder bug, fail closed rather than guess.
            InputSource::NodeOutput(..) => return Err(CodegenError::BadInput),
            InputSource::Unwired => args.push("vec4<f32>(0.0)".to_string()),
            InputSource::Virtual(_) => return Err(CodegenError::BadInput),
        }
    }
    args.push("vuv".to_string());
    args.push("vd".to_string());
    for p in node.params {
        if p.ty == ParamType::Vec3 {
            args.push(format!(
                "vec3<f32>(params.n{j}_{}_x, params.n{j}_{}_y, params.n{j}_{}_z)",
                p.name, p.name, p.name
            ));
        } else if matches!(p.ty, ParamType::Vec4 | ParamType::Color) {
            args.push(format!(
                "vec4<f32>(params.n{j}_{}_x, params.n{j}_{}_y, params.n{j}_{}_z, params.n{j}_{}_w)",
                p.name, p.name, p.name, p.name
            ));
        } else {
            args.push(format!("params.n{j}_{}", p.name));
        }
    }
    // Frame-derived uniforms trail the params (D7/P0) — same convention as the
    // non-virtual cs_main marshaller above; a stencil-absorbed chain member with
    // derived uniforms (e.g. a camera-derived producer feeding a blur's fetch)
    // reads the SAME `n{j}_<name>` fields `node.wgsl_compute` refreshes every
    // frame, whether or not this member ends up inside a virtual chain.
    for d in node.derived_uniforms {
        let (dname, dty) = d.split_once(':').unwrap_or((d, "f32"));
        if dty == "vec3" {
            args.push(format!(
                "vec3<f32>(params.n{j}_{dname}_x, params.n{j}_{dname}_y, params.n{j}_{dname}_z)"
            ));
        } else {
            args.push(format!("params.n{j}_{dname}"));
        }
    }
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
        return Err(CodegenError::BadInput);
    }
    Ok(args)
}
