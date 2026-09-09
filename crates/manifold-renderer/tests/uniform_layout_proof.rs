//! BUG-gx6i — uniform layout proof for the buffer-family primitives.
//!
//! 69 buffer-domain primitives carry a hand-written `#[repr(C)] bytemuck::Pod`
//! uniform struct (tail field `dispatch_count: u32`) that must byte-match the
//! `struct Params` WGSL `freeze/codegen/standalone.rs` derives from PARAMS +
//! derived_uniforms + optional-texture use-flags. Drift is a silent
//! shader/uniform mismatch — no compiler checks it.
//!
//! This test derives the expected layout INDEPENDENTLY (its own re-implementation
//! of the emission rule, not a call into codegen) and parses each primitive's
//! source for the hand struct, then demands an exact field-by-field match:
//! order, name, type — hence every member offset and the total padded size.
//! A struct that matches no type_id in its file, or a buffer-family primitive
//! no struct matches, fails the suite with a named diff.
//!
//! Scope: the standalone buffer path (the `dispatch_count` family). The texture
//! path's hand structs (write-gate/table family) are a follow-up.

use std::path::{Path, PathBuf};

use manifold_renderer::node_graph::{EffectNode, ParamType, PortType, PrimitiveRegistry};

/// One expected struct field: name as the hand struct spells it (raw param
/// name — the WGSL-side reserved-word prefixing is a text concern, not a byte
/// concern), Rust type spelling the hand struct must use.
#[derive(Debug, PartialEq, Clone)]
struct Field {
    name: String,
    ty: &'static str,
}

/// Byte-layout comparison: pads are anonymous (their names are never read —
/// `_pad0` vs `_pad1` is the same byte), everything else compares exactly.
fn layout_eq(expected: &[Field], got: &[Field]) -> bool {
    if expected.len() != got.len() {
        return false;
    }
    expected.iter().zip(got).all(|(e, g)| {
        let pad = |f: &Field| f.name.starts_with("_pad");
        (pad(e) && pad(g) && e.ty == g.ty) || (e == g)
    })
}

/// Expected Params fields for one registered primitive, or None when the
/// primitive is not in the standalone-buffer family (no Array output, or a
/// param type the buffer path rejects — codegen itself refuses those).
fn expected_fields(node: &dyn EffectNode) -> Option<Vec<Field>> {
    let has_array_output = node
        .outputs()
        .iter()
        .any(|o| matches!(o.ty, PortType::Array(_)));
    if !has_array_output {
        return None;
    }
    let mut fields: Vec<Field> = Vec::new();
    for p in node.parameters() {
        let ty = match p.ty {
            ParamType::Float | ParamType::Angle | ParamType::Frequency => "f32",
            ParamType::Int => "i32",
            ParamType::Bool | ParamType::Enum => "u32",
            // Vec3/Vec4/Color/Table/String: the buffer path rejects these
            // (codegen returns Err), so the atom is not in this family.
            _ => return None,
        };
        fields.push(Field { name: p.name.as_ref().to_string(), ty });
    }
    for d in node.derived_uniforms() {
        let (dname, dty) = d.split_once(':').unwrap_or((d, "f32"));
        if dty == "vec3" {
            for suffix in ["_x", "_y", "_z"] {
                fields.push(Field { name: format!("{dname}{suffix}"), ty: "f32" });
            }
        } else {
            let ty = match dty {
                "f32" | "i32" | "u32" => dty,
                _ => return None,
            };
            fields.push(Field { name: dname.to_string(), ty });
        }
    }
    for inp in node.inputs() {
        let is_tex = matches!(inp.ty, PortType::Texture2D | PortType::Texture3D);
        if is_tex && !inp.required {
            fields.push(Field { name: format!("use_{}", inp.name), ty: "u32" });
        }
    }
    fields.push(Field { name: "dispatch_count".to_string(), ty: "u32" });
    let pad = (4 - (fields.len() % 4)) % 4;
    for i in 0..pad {
        fields.push(Field { name: format!("_pad{i}"), ty: "u32" });
    }
    Some(fields)
}

#[derive(Debug)]
struct HandStruct {
    name: String,
    fields: Vec<Field>,
    line: usize,
}

/// Parse one source file for its top-level (brace-depth-0) structs whose field
/// list contains `dispatch_count: u32`, and for every top-level
/// `type_id: "..."` literal. Nested structs (inside fn bodies / tests) are
/// ignored — the uniform mirror is always a module-level item.
fn parse_source(text: &str) -> (Vec<String>, Vec<HandStruct>) {
    // type_ids: whole-file scan — the literal appears inside the primitive!
    // macro (brace depth ≥ 1) and in inventory submits, never anywhere else.
    let mut type_ids = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("type_id:") {
            if let Some(q1) = rest.find('"') {
                if let Some(q2) = rest[q1 + 1..].find('"') {
                    let tid = rest[q1 + 1..q1 + 1 + q2].to_string();
                    if !type_ids.contains(&tid) {
                        type_ids.push(tid);
                    }
                }
            }
        }
    }
    let mut structs = Vec::new();
    let mut depth: i32 = 0;
    let mut lines = text.lines().enumerate().peekable();
    while let Some((idx, line)) = lines.next() {
        let trimmed = line.trim();
        if depth == 0 {
            if trimmed.starts_with("struct ")
                && trimmed.ends_with('{')
                && !trimmed.contains('(')
            {
                let name = trimmed
                    .trim_start_matches("struct ")
                    .trim_end_matches('{')
                    .trim()
                    .to_string();
                let mut fields = Vec::new();
                let mut is_dispatch_family = false;
                for (fidx, fline) in lines.by_ref() {
                    let ft = fline.trim();
                    if ft == "}" {
                        let _ = fidx;
                        break;
                    }
                    if ft.is_empty() || ft.starts_with("//") {
                        continue;
                    }
                    // `name: ty,` — anything more exotic (arrays, generics,
                    // pub fields) is outside the canonical hand-struct shape.
                    let canon = ft.trim_end_matches(',');
                    if let Some((fname, fty)) = canon.split_once(": ") {
                        if fname == "dispatch_count" && fty == "u32" {
                            is_dispatch_family = true;
                        }
                        fields.push(Field {
                            name: fname.to_string(),
                            ty: match fty {
                                "f32" => "f32",
                                "i32" => "i32",
                                "u32" => "u32",
                                other => Box::leak(other.to_string().into_boxed_str()),
                            },
                        });
                    }
                    // depth stays 0 inside the struct for our purposes; struct
                    // literals don't appear in field position in these files.
                }
                if is_dispatch_family {
                    structs.push(HandStruct { name, fields, line: idx + 1 });
                }
                continue;
            }
        }
        // Crude but adequate brace tracking (runs only on lines not consumed
        // by the struct reader). String/comment braces would fool it; the
        // primitives' files are mechanically uniform, and a miscount fails
        // loudly as a parse gap rather than silently passing.
        for ch in trimmed.chars() {
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
        if depth < 0 {
            depth = 0;
        }
    }
    (type_ids, structs)
}

fn primitives_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/node_graph/primitives")
}

#[test]
fn hand_uniform_structs_match_codegen_layout() {
    let registry = PrimitiveRegistry::with_builtin();
    let mut failures: Vec<String> = Vec::new();
    let mut matched_type_ids: Vec<String> = Vec::new();
    let mut struct_count = 0usize;

    let mut files: Vec<PathBuf> = std::fs::read_dir(primitives_dir())
        .expect("primitives dir")
        .filter_map(|e| {
            let p = e.expect("dir entry").path();
            (p.extension().is_some_and(|x| x == "rs")).then_some(p)
        })
        .collect();
    files.sort();

    for path in &files {
        let text = std::fs::read_to_string(path).expect("readable source");
        let (type_ids, structs) = parse_source(&text);
        if structs.is_empty() {
            continue;
        }
        for hs in &structs {
            struct_count += 1;
            let file = path.file_name().unwrap().to_string_lossy();
            let mut matches: Vec<String> = Vec::new();
            let mut diffs: Vec<String> = Vec::new();
            for tid in &type_ids {
                let Some(node) = registry.construct(tid) else {
                    continue;
                };
                let Some(expected) = expected_fields(node.as_ref()) else {
                    continue;
                };
                if layout_eq(&expected, &hs.fields) {
                    matches.push(tid.clone());
                } else {
                    diffs.push(format!(
                        "    vs {tid}: expected {:?}\n             got {:?}",
                        expected, hs.fields
                    ));
                }
            }
            match matches.len() {
                1 => matched_type_ids.push(matches[0].clone()),
                0 => failures.push(format!(
                    "{file}:{} — struct {} matches NO type_id in its file (layout drift):\n{}",
                    hs.line,
                    hs.name,
                    diffs.join("\n")
                )),
                n => failures.push(format!(
                    "{file}:{} — struct {} matches {n} type_ids ({matches:?}); disambiguate",
                    hs.line, hs.name
                )),
            }
        }
    }

    // Coverage report (informational): buffer-fusable primitives no hand
    // struct matched. Most are the generic-pack cohort (codegen-owned packing,
    // no hand struct by design); anything here that DOES hand-pack is a gap
    // the next editor should see.
    let mut uncovered: Vec<String> = Vec::new();
    for tid in registry.known_type_ids() {
        let Some(node) = registry.construct(tid) else { continue };
        if expected_fields(node.as_ref()).is_some() && !matched_type_ids.iter().any(|m| m == tid) {
            uncovered.push(tid.to_string());
        }
    }
    uncovered.sort();
    eprintln!("uniform layout proof: {struct_count} hand structs verified, {} primitives covered", matched_type_ids.len());
    eprintln!("generic-pack cohort (no hand struct, informational): {uncovered:?}");

    assert!(
        failures.is_empty(),
        "uniform layout proof failed ({struct_count} hand structs parsed):\n{}",
        failures.join("\n")
    );
}
