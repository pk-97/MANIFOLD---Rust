//! Standalone `struct Params { … }` emission: the three standalone codegen
//! paths (texture, buffer, resolve) share one Params shape — scalar params in
//! PARAMS order, then derived fields, then each site's injected fields, padded
//! to a 16-byte multiple — differing only in which extras they carry (table
//! expansion, write/use flags, dispatch_count). Emitted text must stay
//! byte-identical across sites because it is the pipeline-cache key.

use std::fmt::Write as _;

use super::types::{
    param_wgsl_type, param_word_count, wgsl_safe_field, CodegenError,
};
use crate::node_graph::parameters::{ParamDef, ParamType};

/// Per-site knobs for [`emit_params_struct`].
pub(super) struct ParamsStructOpts<'a> {
    /// `Some(table_len)`: a Table param expands to a `<name>_count: u32`
    /// header word plus a fixed `array<vec4<f32>, table_len>` appended after
    /// the 16-byte-aligned header (texture path only — the only standalone
    /// path that lays a table out; `param_wgsl_type` still rejects Table, so
    /// the fused region-grower keeps treating a table atom as a boundary).
    /// `None`: a Table param is rejected by `param_wgsl_type` like any other
    /// non-scalar param (buffer/resolve paths).
    pub table_len: Option<usize>,
    /// The Table params (count header + trailing array); empty when
    /// `table_len` is `None`.
    pub table_params: &'a [&'a ParamDef],
    /// Multi-output write-gate flag names, one `write_<name>: u32` per entry
    /// in output order (texture path only; for voronoi_2d this reproduces the
    /// hand uniform's write_out/write_cell_id tail exactly).
    pub write_flags: &'a [&'a str],
    /// Optional-input use-flag port names, one `use_<name>: u32` per entry
    /// (texture and buffer paths; resolve has no optional inputs).
    pub use_flags: &'a [&'a str],
    /// Expand Vec3/Vec4/Color params to per-component f32 fields (texture path
    /// only; `false` lets `param_wgsl_type` reject them, so a buffer/resolve
    /// atom with a vector param still fails codegen exactly as before).
    pub expand_vectors: bool,
    /// Append the injected `dispatch_count: u32` element count (standalone
    /// buffer path only — it drives the 1D dispatch guard and is passed to
    /// the body as `count`).
    pub dispatch_count: bool,
}

/// Emit `struct Params { … }\n\n` for one standalone kernel: the atom's scalar
/// params in PARAMS order (padded to a 16-byte multiple to match the setBytes
/// buffer size), then the derived fields, then the site's injected fields in
/// their fixed order (table counts, write gates, use flags, dispatch_count),
/// then the pad words, then the trailing table arrays.
pub(super) fn emit_params_struct(
    out: &mut String,
    params: &[ParamDef],
    derived_uniforms: &[&str],
    opts: &ParamsStructOpts<'_>,
) -> Result<(), CodegenError> {
    out.push_str("struct Params {\n");
    let mut words = 0usize;
    for p in params {
        if p.ty == ParamType::Table && opts.table_len.is_some() {
            continue; // emitted as count (below) + array (after the pad)
        }
        let f = wgsl_safe_field(p.name.as_ref());
        if opts.expand_vectors && p.ty == ParamType::Vec3 {
            // A vec3 param expands to three consecutive f32 fields
            // (wgsl-vec3-alignment convention).
            writeln!(out, "    {f}_x: f32,").unwrap();
            writeln!(out, "    {f}_y: f32,").unwrap();
            writeln!(out, "    {f}_z: f32,").unwrap();
        } else if opts.expand_vectors && matches!(p.ty, ParamType::Vec4 | ParamType::Color) {
            // A vec4/color param expands to four consecutive f32 fields,
            // reassembled as a vec4<f32> at the body call site.
            writeln!(out, "    {f}_x: f32,").unwrap();
            writeln!(out, "    {f}_y: f32,").unwrap();
            writeln!(out, "    {f}_z: f32,").unwrap();
            writeln!(out, "    {f}_w: f32,").unwrap();
        } else {
            let ty = param_wgsl_type(p)?;
            writeln!(out, "    {f}: {ty},").unwrap();
        }
        words += param_word_count(p)?;
    }
    words += emit_derived_fields(out, derived_uniforms, "");
    for t in opts.table_params {
        writeln!(out, "    {}_count: u32,", t.name).unwrap();
        words += 1;
    }
    for name in opts.write_flags {
        writeln!(out, "    write_{name}: u32,").unwrap();
        words += 1;
    }
    for name in opts.use_flags {
        writeln!(out, "    use_{name}: u32,").unwrap();
        words += 1;
    }
    if opts.dispatch_count {
        out.push_str("    dispatch_count: u32,\n");
        words += 1;
    }
    let pad_words = (4 - (words % 4)) % 4;
    for i in 0..pad_words {
        writeln!(out, "    _pad{i}: u32,").unwrap();
    }
    if let Some(table_len) = opts.table_len {
        for t in opts.table_params {
            writeln!(out, "    {}: array<vec4<f32>, {table_len}>,", t.name).unwrap();
        }
    }
    out.push_str("}\n\n");
    Ok(())
}

/// Emit the injected non-param derived uniform fields — frame-derived values
/// recomputed by the atom's run() each frame from a CPU-struct input (a
/// Camera's basis vectors, `dt_scaled`, …), placed right after the scalar
/// params in every standalone Params layout. Each entry is `"name"` (f32) or
/// `"name:ty"` for an explicit scalar type (`"frame_count:u32"` so a frame
/// counter stays an exact integer rather than losing precision as an f32 past
/// ~16M frames); `"name:vec3"` expands to three consecutive f32 fields.
/// Packing a vec3 as 3 scalars (not a `vec3<f32>` field) keeps the 4-byte
/// stride the run()-side `#[repr(C)]` uniform uses, dodging the uniform vec3's
/// 16-byte alignment. `prefix` namespaces the field names (the fused paths
/// use `n{i}_`; standalone passes `""`). Returns the 4-byte word count added.
pub(super) fn emit_derived_fields(
    out: &mut String,
    derived_uniforms: &[&str],
    prefix: &str,
) -> usize {
    let mut words = 0usize;
    for d in derived_uniforms {
        let (dname, dty) = d.split_once(':').unwrap_or((d, "f32"));
        if dty == "vec3" {
            writeln!(out, "    {prefix}{dname}_x: f32,").unwrap();
            writeln!(out, "    {prefix}{dname}_y: f32,").unwrap();
            writeln!(out, "    {prefix}{dname}_z: f32,").unwrap();
            words += 3;
        } else {
            writeln!(out, "    {prefix}{dname}: {dty},").unwrap();
            words += 1; // every supported derived scalar is one 4-byte word
        }
    }
    words
}

/// Push the derived-uniform body-call args in declaration order — trailing
/// args after the params at the `body(...)` call site, a vec3 reassembled
/// from its three packed f32 words. Mirrors [`emit_derived_fields`]'s struct
/// layout. `prefix` namespaces the `params.` access (the fused paths use
/// `n{i}_`; standalone passes `""`).
pub(super) fn emit_derived_args(
    args: &mut Vec<String>,
    derived_uniforms: &[&str],
    prefix: &str,
) {
    for d in derived_uniforms {
        let (dname, dty) = d.split_once(':').unwrap_or((d, "f32"));
        if dty == "vec3" {
            args.push(format!(
                "vec3<f32>(params.{prefix}{dname}_x, params.{prefix}{dname}_y, params.{prefix}{dname}_z)"
            ));
        } else {
            args.push(format!("params.{prefix}{dname}"));
        }
    }
}
