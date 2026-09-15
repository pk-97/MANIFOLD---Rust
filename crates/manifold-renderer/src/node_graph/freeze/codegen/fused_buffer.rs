//! The BUFFER-domain fused-kernel path (`var<storage>` bindings, a 1D
//! element dispatch, element structs threaded as registers) — split out of
//! `fused.rs` when the output-capacity-expression work (BUG-orm4
//! (scene-mirror-blocked-output-multiplier-capacity)) pushed that file past
//! the god-file ceiling. The texture path, shared body helpers, and the
//! `generate_fused` dispatcher stay in `fused.rs`.

use std::fmt::Write as _;

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::freeze::classify::InputAccess;
use crate::node_graph::freeze::markers::Marker;
use crate::node_graph::parameters::ParamType;
use crate::node_graph::ports::{ChannelSpec, NodeInput, NodeOutput, PortType};

use super::fused::{FnBlock, is_param_derived, rename_ident, split_fns};
use super::types::{
    buffer_element_type, is_texture_input, is_texture_port, param_wgsl_type, param_word_count,
    CodegenError, FusionRegion, GeneratedFusion, InputSource,
};
use super::uniforms::{emit_buffer_struct, emit_derived_uniform_markers};

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
        // (coincident element registers, or a `BufferGather` input kept as a
        // whole-array external), then appends its TEXTURE inputs as gathered
        // externals (the body samples each bound texture at an element-
        // computed coord — the buffer analogue of the texture path's sampler-
        // Gather; same `tex + samp` body-arg ABI the standalone buffer kernel
        // uses). A `BufferGather` array input (neighbor_smooth indexes the
        // array global itself) stays whole-array: bound as a read-only
        // `src_<slot>` the body references directly (`buf_<port>` renamed to
        // `src_<slot>`), never pre-read into a per-element register — a
        // pre-read would run off the end of an input shorter than the output.
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
            // Texture entries are always gathered externals. Array entries are
            // Coincident (threaded register) or BufferGather (whole-array
            // external) — the texture gather flavours never tag an Array port.
            let ok = if is_texture_entry {
                access.is_gather()
            } else {
                matches!(access, InputAccess::Coincident | InputAccess::BufferGather)
            };
            if !ok {
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
    // element type comes from the consumer's array-input specs — or, when a
    // `BufferGather` consumer indexes it whole, bound but never pre-read) or a
    // TEXTURE slot (bound as `src_<e>: texture_2d<f32>` + the shared `samp`,
    // sampled by the consuming bodies). Every external is read by ≥1 member
    // (the finder built the slot because a member reads it), so each resolves;
    // one producer port has one type, so a both-ways slot is a finder bug —
    // fail closed.
    #[derive(Clone, Copy, PartialEq)]
    enum ExtKind {
        Array(&'static [ChannelSpec]),
        Texture { is_3d: bool },
    }
    let mut ext_kinds: Vec<Option<ExtKind>> = vec![None; region.num_external_inputs];
    // Per-slot read shapes across its consuming members: `ext_gathered` — some
    // consumer reads it through `BufferGather` (whole-array, at body-computed
    // indices); `ext_coincident` — some consumer threads it as a per-element
    // register (needs the `src_<e>[idx]` pre-read + an `e_<e>` body arg). A
    // slot can be BOTH (the finder dedupes one producer port read both ways by
    // two members into one external): then the binding serves both read
    // shapes. A gathered-only slot is excluded from the pre-read — a
    // coincident `src_<e>[idx]` read would run off the end of an input shorter
    // than the dispatch count (reflect_array reads `idx % input_cap`).
    let mut ext_gathered: Vec<bool> = vec![false; region.num_external_inputs];
    let mut ext_coincident: Vec<bool> = vec![false; region.num_external_inputs];
    for (mi, node) in region.nodes.iter().enumerate() {
        let arr_count = member_io[mi].in_specs.len();
        for (k, src) in node.inputs.iter().enumerate() {
            if let InputSource::External(e) = src {
                if *e >= region.num_external_inputs {
                    return Err(CodegenError::BadInput);
                }
                let kind = if k < arr_count {
                    if node.input_access.get(k).copied().unwrap_or_default()
                        == InputAccess::BufferGather
                    {
                        ext_gathered[*e] = true;
                    } else {
                        ext_coincident[*e] = true;
                    }
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
        // BufferGather (BUG-x72p): rewrite this member's gathered array-input
        // global `buf_<port>` (the standalone body's local storage-global name)
        // to the region's resolved external slot `src_<e>` — no body arg, the
        // same ABI the texture path's BufferIndex uses. Coincident array
        // inputs keep the `e_<e>` register arg (cs_main below).
        let arr_ports: Vec<&NodeInput> =
            node.node_inputs.iter().filter(|p| matches!(p.ty, PortType::Array(_))).collect();
        for (arr_idx, port) in arr_ports.iter().enumerate() {
            if node.input_access.get(arr_idx).copied().unwrap_or_default()
                != InputAccess::BufferGather
            {
                continue;
            }
            match node.inputs.get(arr_idx) {
                Some(InputSource::External(e)) => {
                    text = rename_ident(&text, &format!("buf_{}", port.name), &format!("src_{e}"));
                }
                _ => return Err(CodegenError::BadInput),
            }
        }
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
            // A frame-time input that is ALSO a declared param is emitted in the
            // param block at its original position; emitting it again here would
            // duplicate the struct field. The derived-uniform marker still covers
            // it so the generic pack skips it and the recompute refreshes it.
            if node.params.iter().any(|p| p.name.as_ref() == dname) {
                continue;
            }
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
    // BUG-orm4: a widened output capacity (a MultipleOf member in the region)
    // can only write a FRESH dst sized to the widened count. Writing back
    // IN PLACE over the shorter aliased loop buffer would run off its end —
    // refuse (the region renders unfused, always correct).
    if in_place.is_some() && region.output_capacity.is_some() {
        return Err(CodegenError::BadInput);
    }
    if let Some(k) = in_place {
        // Validity: the aliased input must exist, be an ARRAY slot, and carry the
        // output's element type (it IS the output buffer). Install guarantees
        // this; guard anyway.
        if ext_tys.get(k).and_then(|t| t.as_deref()) != Some(out_ty.as_str()) {
            return Err(CodegenError::BadInput);
        }
        // A GATHERED aliased input would be read at body-computed neighbour
        // indices while other invocations write it in the same dispatch — a
        // cross-thread race the unfused multi-dispatch chain never has. Refuse
        // (the region renders unfused, always correct).
        if ext_gathered.get(k).copied().unwrap_or(false) {
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
        // BUG-orm4: a widened output capacity rides a marker so
        // `node.wgsl_compute` sizes this fresh dst to the SAME expression
        // (the default min-over-inputs would cut the multiplied range —
        // mirror half, echo stride — out of the buffer). Identity regions
        // emit no marker: byte-identical WGSL, and the default sizing is
        // exactly the legacy min anchor.
        if let Some(expr) = &region.output_capacity {
            writeln!(out, "{}", Marker::FusedOutputCapacity { expr: expr.clone() }.emit()).unwrap();
        }
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
    // keeping prior fused WGSL — a pipeline-cache key — byte-identical). Each
    // is coincident with the output (one output element per input element) and
    // every one is pre-read at `[idx]` below; a texture slot has no arrayLength,
    // so skip it. A GATHERED array slot also joins the min — never pre-read, but
    // its length still bounds the dispatch, exactly matching how
    // `node.wgsl_compute` sizes the fresh `dst` (min over every wired array
    // input) so the kernel never writes past its own output. With a SINGLE
    // array external the count is that external's length exactly (unchanged
    // text). With MORE THAN ONE, bound by the SHORTEST so a shorter input can't
    // be read out of bounds (BUG-008) — the unfused atoms clamp to
    // `min(a, b, …)` for the same reason. Equal-length regions (every shipped
    // buffer preset) are unaffected: `min` of equal lengths is that length.
    // BUG-orm4: a region whose output capacity composes to a widened
    // expression (a MultipleOf member — reflect_array's 2x mirror,
    // analytic_echo_instances' 8x stride) anchors on THAT instead: the
    // expression's WGSL rendering (e.g. `2u * arrayLength(&src_0)`), composed
    // and verified at `build_region`, so the multiplied range is dispatched
    // and written; the matching `// @fused_output_capacity:` marker (emitted
    // with the bindings above) sizes the fresh dst to the same count.
    let array_ext: Vec<usize> = ext_tys
        .iter()
        .enumerate()
        .filter_map(|(e, t)| t.is_some().then_some(e))
        .collect();
    let count_anchor = *array_ext.first().ok_or(CodegenError::BadInput)?;
    let anchor_expr = match &region.output_capacity {
        Some(expr) => expr.to_wgsl(),
        None if array_ext.len() == 1 => format!("arrayLength(&src_{count_anchor})"),
        None => {
            let mut expr = format!("arrayLength(&src_{count_anchor})");
            for &e in &array_ext[1..] {
                expr = format!("min({expr}, arrayLength(&src_{e}))");
            }
            expr
        }
    };
    writeln!(out, "    let count = {anchor_expr};").unwrap();
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
    // Pre-read each COINCIDENT-read array external's element `[idx]` once (its
    // `e_<e>` register arg below); gathered array externals are indexed by the
    // bodies themselves at computed indices — never pre-read. Texture
    // externals are sampled by the bodies themselves (never pre-read — a
    // register is one element, not a whole texture).
    for (e, ty) in ext_tys.iter().enumerate() {
        if ty.is_some() && ext_coincident[e] {
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
            if node.input_access.get(k).copied().unwrap_or_default() == InputAccess::BufferGather
            {
                // The body reads the bound `src_<e>` array global directly
                // (renamed from `buf_<port>` above) — no element arg. Must be
                // an external: the finder never unions a gather-consumed wire,
                // so a member/unwired source here is a finder bug.
                match src {
                    InputSource::External(e) if *e < region.num_external_inputs => {}
                    _ => return Err(CodegenError::BadInput),
                }
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
            // A frame-time input that is ALSO a declared param is passed at its
            // param position above; pushing it again here would duplicate the
            // body arg (the derived marker still refreshes the shared field).
            if is_param_derived(node, dname) {
                continue;
            }
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
