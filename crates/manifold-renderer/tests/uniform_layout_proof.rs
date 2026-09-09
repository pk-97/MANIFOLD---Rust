//! BUG-gx6i — uniform layout proof for the buffer-family primitives.
//!
//! Reflect the actual standalone WGSL through naga and compare its scalar
//! member offsets, types, and total span with the hand-written repr(C) fields.
//! The supported host layout is deliberately narrow: consecutive 32-bit
//! scalars with no additional representation or conditional-field attributes.
//! Unsupported source shapes fail coverage rather than silently disappearing.
//! Named non-codegen/CPU exceptions below are outside this ABI family.
//!
//! Scope: the standalone buffer path (the `dispatch_count` family). The texture
//! path's hand structs (write-gate/table family) are a follow-up.

use std::path::{Path, PathBuf};

use manifold_renderer::node_graph::freeze::codegen::standalone_for_node;
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
        fields.push(Field {
            name: p.name.as_ref().to_string(),
            ty,
        });
    }
    for d in node.derived_uniforms() {
        let (dname, dty) = d.split_once(':').unwrap_or((d, "f32"));
        if dty == "vec3" {
            for suffix in ["_x", "_y", "_z"] {
                fields.push(Field {
                    name: format!("{dname}{suffix}"),
                    ty: "f32",
                });
            }
        } else {
            let ty = match dty {
                "f32" | "i32" | "u32" => dty,
                _ => return None,
            };
            fields.push(Field {
                name: dname.to_string(),
                ty,
            });
        }
    }
    for inp in node.inputs() {
        let is_tex = matches!(inp.ty, PortType::Texture2D | PortType::Texture3D);
        if is_tex && !inp.required {
            fields.push(Field {
                name: format!("use_{}", inp.name),
                ty: "u32",
            });
        }
    }
    fields.push(Field {
        name: "dispatch_count".to_string(),
        ty: "u32",
    });
    let pad = (4 - (fields.len() % 4)) % 4;
    for i in 0..pad {
        fields.push(Field {
            name: format!("_pad{i}"),
            ty: "u32",
        });
    }
    Some(fields)
}

#[derive(Debug)]
struct HandStruct {
    name: String,
    fields: Vec<Field>,
    line: usize,
    repr_c: bool,
}

/// Parse one source file for its top-level (brace-depth-0) structs whose field
/// list contains `dispatch_count: u32`, and for every top-level
/// `type_id: "..."` literal. Nested structs (inside fn bodies / tests) are
/// ignored — the uniform mirror is always a module-level item.
fn strip_visibility(text: &str) -> &str {
    if let Some(rest) = text.strip_prefix("pub ") {
        rest
    } else if text.starts_with("pub(") {
        text.split_once(')')
            .map_or(text, |(_, rest)| rest.trim_start())
    } else {
        text
    }
}

fn parse_source(text: &str) -> (Vec<String>, Vec<HandStruct>) {
    // type_ids: whole-file scan — the literal appears inside the primitive!
    // macro (brace depth ≥ 1) and in inventory submits, never anywhere else.
    let mut type_ids = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("type_id:")
            && let Some(q1) = rest.find('"')
            && let Some(q2) = rest[q1 + 1..].find('"')
        {
            let tid = rest[q1 + 1..q1 + 1 + q2].to_string();
            if !type_ids.contains(&tid) {
                type_ids.push(tid);
            }
        }
    }
    let mut structs = Vec::new();
    let mut depth: i32 = 0;
    let mut lines = text.lines().enumerate().peekable();
    while let Some((idx, line)) = lines.next() {
        let trimmed = strip_visibility(line.trim());
        if depth == 0
            && (trimmed.starts_with("struct ") || trimmed.starts_with("pub struct "))
            && trimmed.ends_with('{')
            && !trimmed.contains('(')
        {
            let name = trimmed
                .trim_start_matches("pub ")
                .trim_start_matches("struct ")
                .trim_end_matches('{')
                .trim()
                .to_string();
            let mut fields = Vec::new();
            let mut is_dispatch_family = false;
            for (fidx, fline) in lines.by_ref() {
                let ft = fline.trim();
                if ft == "}" || ft == "};" {
                    let _ = fidx;
                    break;
                }
                if ft.is_empty() || ft.starts_with("//") {
                    continue;
                }
                // `name: ty,` — anything more exotic (arrays, generics,
                // pub fields) is outside the canonical hand-struct shape.
                let canon = ft.trim_end_matches(',');
                if let Some((fname, fty)) = canon.split_once(':') {
                    let fname = strip_visibility(fname.trim());
                    let fty = fty.trim();
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
                } else {
                    // Conditional fields and unsupported syntax cannot be
                    // treated as comments: they change the actual ABI.
                    fields.push(Field {
                        name: ft.to_string(),
                        ty: "unsupported",
                    });
                }
                // depth stays 0 inside the struct for our purposes; struct
                // literals don't appear in field position in these files.
            }
            if is_dispatch_family {
                let previous: Vec<_> = text.lines().take(idx).collect();
                let attrs: Vec<_> = previous
                    .iter()
                    .rev()
                    .map(|l| l.trim())
                    .take_while(|l| l.starts_with("#[") || l.starts_with("//") || l.is_empty())
                    .collect();
                let repr_c = attrs.contains(&"#[repr(C)]")
                    && attrs
                        .iter()
                        .all(|a| !a.starts_with("#[repr") || *a == "#[repr(C)]");
                structs.push(HandStruct {
                    name,
                    fields,
                    line: idx + 1,
                    repr_c,
                });
            }
            continue;
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

/// The generated shader is the ABI oracle. Reflect its binding-0 Params
/// struct through naga so emitter drift cannot be masked by this test.
struct ShaderLayout {
    fields: Vec<Field>,
    offsets: Vec<u32>,
    span: u32,
}

impl ShaderLayout {
    fn is_packed_scalars(&self) -> bool {
        self.offsets
            .iter()
            .enumerate()
            .all(|(i, offset)| *offset == i as u32 * 4)
            && self.span == self.fields.len() as u32 * 4
    }
}

fn shader_fields(node: &dyn EffectNode) -> Result<ShaderLayout, String> {
    let mut wgsl = standalone_for_node(node).map_err(|e| format!("codegen: {e:?}"))?;
    for (token, _) in node.wgsl_specialization() {
        wgsl = wgsl.replace(token, "1u");
    }
    reflect_shader(&wgsl)
}

fn reflect_shader(wgsl: &str) -> Result<ShaderLayout, String> {
    let module = naga::front::wgsl::parse_str(wgsl).map_err(|e| e.emit_to_string(wgsl))?;
    let global = module
        .global_variables
        .iter()
        .find(|(_, g)| {
            g.space == naga::AddressSpace::Uniform
                && g.binding
                    .as_ref()
                    .is_some_and(|b| b.group == 0 && b.binding == 0)
        })
        .ok_or_else(|| "missing Params uniform".to_string())?
        .1;
    let naga::TypeInner::Struct { members, span } = &module.types[global.ty].inner else {
        return Err("Params binding is not a struct".into());
    };
    let mut fields = Vec::with_capacity(members.len());
    let mut offsets = Vec::with_capacity(members.len());
    for m in members {
        let ty = match &module.types[m.ty].inner {
            naga::TypeInner::Scalar(s) => match (s.kind, s.width) {
                (naga::ScalarKind::Float, 4) => "f32",
                (naga::ScalarKind::Sint, 4) => "i32",
                (naga::ScalarKind::Uint, 4) => "u32",
                _ => return Err("unsupported scalar".into()),
            },
            _ => return Err(format!("non-scalar member {:?}", m.name)),
        };
        fields.push(Field {
            name: m.name.clone().ok_or("unnamed member")?,
            ty,
        });
        offsets.push(m.offset);
    }
    Ok(ShaderLayout {
        fields,
        offsets,
        span: *span,
    })
}

fn primitives_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/node_graph/primitives")
}

fn primitive_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("primitives dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            primitive_files(&path, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn hand_uniform_structs_match_codegen_layout() {
    let registry = PrimitiveRegistry::with_builtin();
    let mut failures: Vec<String> = Vec::new();
    let mut matched_type_ids: Vec<String> = Vec::new();
    let mut struct_count = 0usize;

    let mut files = Vec::new();
    primitive_files(&primitives_dir(), &mut files);
    files.sort();

    for path in &files {
        let text = std::fs::read_to_string(path).expect("readable source");
        let (type_ids, structs) = parse_source(&text);
        if structs.is_empty() {
            continue;
        }
        for hs in &structs {
            struct_count += 1;
            if !hs.repr_c {
                failures.push(format!(
                    "{}:{}: {} needs plain repr(C)",
                    path.display(),
                    hs.line,
                    hs.name
                ));
                continue;
            }
            let file = path.file_name().unwrap().to_string_lossy();
            let mut matches: Vec<String> = Vec::new();
            let mut diffs: Vec<String> = Vec::new();
            for tid in &type_ids {
                let Some(node) = registry.construct(tid) else {
                    continue;
                };
                let Some(raw_fields) = expected_fields(node.as_ref()) else {
                    continue;
                };
                let expected = match shader_fields(node.as_ref()) {
                    Ok(layout) => {
                        if !layout.is_packed_scalars() {
                            diffs.push(format!(
                                "    vs {tid}: shader Params offsets/span invalid: {:?}, span={}",
                                layout.offsets, layout.span
                            ));
                            continue;
                        }
                        let mut fields = layout.fields;
                        // Codegen prefixes reserved param names with p_. Only
                        // normalize a prefix that names a real raw field.
                        for field in &mut fields {
                            if let Some(raw) = field.name.strip_prefix("p_")
                                && !raw_fields.iter().any(|f| f.name == field.name)
                                && raw_fields.iter().any(|f| f.name == raw)
                            {
                                field.name = raw.to_string();
                            }
                        }
                        fields
                    }
                    Err(error) => {
                        diffs.push(format!("    vs {tid}: shader reflection failed: {error}"));
                        continue;
                    }
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

    let mut uncovered = Vec::new();
    for tid in registry.known_type_ids() {
        let node = registry.construct(tid).expect("registered primitive");
        if expected_fields(node.as_ref()).is_some() && !matched_type_ids.iter().any(|m| m == tid) {
            uncovered.push(tid.to_string());
        }
    }
    failures.extend(coverage_errors(&uncovered));
    eprintln!(
        "uniform layout proof: {struct_count} hand structs verified, {} primitives covered",
        matched_type_ids.len()
    );

    assert!(
        failures.is_empty(),
        "uniform layout proof failed ({struct_count} hand structs parsed):\n{}",
        failures.join("\n")
    );
}

// These nodes do not use the standalone dispatch_count ABI. Keep exceptions
// explicit: adding/removing an exception requires inspecting the run() path.
const NON_STANDALONE: &[&str] = &[
    // CPU/control/state implementations.
    "node.array_feedback",
    "node.array_filter_detections",
    "node.array_math",
    "node.combine_xy",
    "node.combine_xyzw",
    "node.connect_nearest",
    "node.edge_pairs",
    "node.grid_edges",
    "node.grid_points",
    "node.hypercube_edges",
    "node.lightning_bolt",
    "node.mesh_edges",
    "node.one_euro_filter",
    "node.platonic_solid_edges",
    "node.range",
    "node.repeat_outline",
    "node.switch_array",
    "node.track_persist",
    // Custom GPU kernels, reduction/FFI or seed-stage layouts (not codegen Params).
    "node.blob_tracker",
    "node.cylinder_wrap_field",
    "node.remove_drift_3d",
    "node.scatter_on_mesh",
    "node.spawn_from_image",
    "node.spawn_from_mesh",
    "node.spawn_particles",
    "node.torus_wrap_field",
];

fn coverage_errors(uncovered: &[String]) -> Vec<String> {
    let mut errors = Vec::new();
    for id in uncovered {
        if !NON_STANDALONE.contains(&id.as_str()) {
            errors.push(format!("missing hand-layout proof: {id}"));
        }
    }
    for id in NON_STANDALONE {
        if !uncovered.iter().any(|u| u == id) {
            errors.push(format!("stale layout exception: {id}"));
        }
    }
    errors
}

#[test]
fn missing_hand_struct_cannot_pass_as_an_exception() {
    let mut uncovered: Vec<_> = NON_STANDALONE.iter().map(|s| s.to_string()).collect();
    assert!(coverage_errors(&uncovered).is_empty());
    uncovered.push("node.scene_array".into());
    assert_eq!(
        coverage_errors(&uncovered),
        ["missing hand-layout proof: node.scene_array"]
    );
}

#[test]
fn parser_recognizes_restricted_visibility_and_requires_repr_c() {
    let source = "#[repr(C)]\npub(crate) struct Params {\n pub(crate) dispatch_count: u32,\n}\n";
    let (_, structs) = parse_source(source);
    assert_eq!(structs.len(), 1);
    assert!(structs[0].repr_c);
    let (_, structs) = parse_source(&source.replace("#[repr(C)]", "#[repr(C, align(16))]"));
    assert!(!structs[0].repr_c);
}

#[test]
fn reflection_detects_shader_alignment_size_and_type_drift() {
    let shader = |fields: &str| {
        format!("struct Params {{ {fields} }} @group(0) @binding(0) var<uniform> params: Params;")
    };
    let normal = reflect_shader(&shader("value: f32, dispatch_count: u32,")).unwrap();
    assert!(normal.is_packed_scalars());
    let aligned = reflect_shader(&shader("value: f32, @align(16) dispatch_count: u32,")).unwrap();
    assert!(!aligned.is_packed_scalars());
    let resized = reflect_shader(&shader("value: f32, @size(16) dispatch_count: u32,")).unwrap();
    assert!(!resized.is_packed_scalars());
    let retyped = reflect_shader(&shader("value: u32, dispatch_count: u32,")).unwrap();
    assert!(!layout_eq(&normal.fields, &retyped.fields));
}
