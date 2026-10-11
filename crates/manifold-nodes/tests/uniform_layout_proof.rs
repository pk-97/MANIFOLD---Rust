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

use manifold_nodes as _;
use std::path::{Path, PathBuf};

use manifold_node_engine::freeze::codegen::standalone_for_node;
use manifold_node_engine::{
    exec::effect_node::EffectNode, parameters::ParamType, persistence::PrimitiveRegistry,
    ports::PortType,
};

use crate::rust_items;

use manifold_nodes::testkit::source_roots::{primitive_source_roots, verify_wgsl_roots};

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
    repr_c: bool,
}

/// Read module-level production structs, including testkit visibility wrappers.
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
    fn scalar(ty: &syn::Type) -> &'static str {
        if let syn::Type::Path(path) = ty {
            for name in ["f32", "i32", "u32"] {
                if path.path.is_ident(name) {
                    return name;
                }
            }
        }
        "unsupported"
    }
    fn collect(item: syn::Item, structs: &mut Vec<HandStruct>) {
        match item {
            syn::Item::Macro(item) => {
                if let Some(item) =
                    rust_items::testkit_item(&item).expect("valid testkit_visible item")
                {
                    collect(item, structs);
                }
            }
            syn::Item::Struct(item) if !rust_items::test_only(&item.attrs) => {
                if !item.fields.iter().any(|f| {
                    f.ident.as_ref().is_some_and(|n| n == "dispatch_count")
                        && scalar(&f.ty) == "u32"
                }) {
                    return;
                }
                let mut repr_c = false;
                let mut supported_repr = true;
                for attr in &item.attrs {
                    if attr.path().is_ident("repr") {
                        if attr
                            .parse_args::<syn::Ident>()
                            .is_ok_and(|repr| repr == "C")
                        {
                            repr_c = true;
                        } else {
                            supported_repr = false;
                        }
                    }
                }
                structs.push(HandStruct {
                    name: item.ident.to_string(),
                    fields: item
                        .fields
                        .iter()
                        .map(|field| Field {
                            name: field
                                .ident
                                .as_ref()
                                .map(ToString::to_string)
                                .unwrap_or_default(),
                            ty: if field.attrs.iter().all(|a| a.path().is_ident("doc")) {
                                scalar(&field.ty)
                            } else {
                                "unsupported"
                            },
                        })
                        .collect(),
                    repr_c: repr_c && supported_repr && item.generics.params.is_empty(),
                });
            }
            _ => {} // Function bodies and nested test modules are not production mirrors.
        }
    }
    let mut structs = Vec::new();
    for item in syn::parse_file(text).expect("valid Rust source").items {
        collect(item, &mut structs);
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

const CFG_TEST_FIXTURES: &[&str] = &["test_multi_output_atomic_fixture.rs"];

#[test]
fn hand_uniform_structs_match_codegen_layout() {
    verify_wgsl_roots().expect("WGSL source-root inventory");
    let registry = PrimitiveRegistry::with_builtin();
    let mut failures: Vec<String> = Vec::new();
    let mut matched_type_ids: Vec<String> = Vec::new();
    let mut struct_count = 0usize;

    let mut files = Vec::new();
    for root in primitive_source_roots().expect("ABI primitive source roots") {
        primitive_files(&root, &mut files);
    }
    files.sort();

    for path in &files {
        // cfg(test) fixtures never reach this registry, so they can't be
        // constructed here. Each one proves its own Params layout in its unit tests.
        let file_name = path.file_name().unwrap().to_string_lossy();
        if CFG_TEST_FIXTURES.contains(&file_name.as_ref()) {
            continue;
        }
        let text = std::fs::read_to_string(path).expect("readable source");
        let (type_ids, structs) = parse_source(&text);
        if structs.is_empty() {
            continue;
        }
        for hs in &structs {
            struct_count += 1;
            if !hs.repr_c {
                failures.push(format!(
                    "{}: {} needs plain repr(C)",
                    path.display(),
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
                        unprefix_reserved(&mut fields, &raw_fields);
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
                    "{file} — struct {} matches NO type_id in its file (layout drift):\n{}",
                    hs.name,
                    diffs.join("\n")
                )),
                n => failures.push(format!(
                    "{file} — struct {} matches {n} type_ids ({matches:?}); disambiguate",
                    hs.name
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

#[test]
fn cut_remap_generated_params_match_shared_four_word_upload() {
    let upload = [0_u32; 4];
    let expected = vec![
        Field {
            name: "dispatch_count".into(),
            ty: "u32",
        },
        Field {
            name: "_pad0".into(),
            ty: "u32",
        },
        Field {
            name: "_pad1".into(),
            ty: "u32",
        },
        Field {
            name: "_pad2".into(),
            ty: "u32",
        },
    ];
    let registry = PrimitiveRegistry::with_builtin();
    for type_id in ["node.remap_mesh_cut", "node.remap_cut_weights"] {
        let node = registry
            .construct(type_id)
            .expect("registered cut remapper");
        let layout = shader_fields(node.as_ref()).expect("generated remapper Params");
        assert_eq!(layout.fields, expected, "Params fields for {type_id}");
        assert_eq!(
            layout.offsets,
            vec![0, 4, 8, 12],
            "Params offsets for {type_id}"
        );
        assert_eq!(layout.span, std::mem::size_of_val(&upload) as u32);
    }
}

/// Codegen prefixes reserved param names with p_. Only normalize a prefix
/// that names a real raw field.
fn unprefix_reserved(fields: &mut [Field], raw_fields: &[Field]) {
    for field in fields {
        if let Some(raw) = field.name.strip_prefix("p_")
            && !raw_fields.iter().any(|f| f.name == field.name)
            && raw_fields.iter().any(|f| f.name == raw)
        {
            field.name = raw.to_string();
        }
    }
}

/// `type_id`'s generated Params, required to be packed scalars.
fn generated_fields(registry: &PrimitiveRegistry, type_id: &str, raw: &[Field]) -> Vec<Field> {
    let node = registry.construct(type_id).expect("registered primitive");
    let layout = shader_fields(node.as_ref()).unwrap_or_else(|e| panic!("{type_id}: {e}"));
    assert!(
        layout.is_packed_scalars(),
        "{type_id}: Params offsets/span invalid: {:?}, span={}",
        layout.offsets,
        layout.span
    );
    let mut fields = layout.fields;
    unprefix_reserved(&mut fields, raw);
    fields
}

/// `SurfaceMeshPass` uploads one `RelaxUniforms` for every kernel it runs:
/// relax_surface_mesh's (also smooth_surface_mesh's) and surface_mesh_normals'.
#[test]
fn surface_mesh_pass_uniforms_match_every_kernel_it_dispatches() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../manifold-water-surface/src/primitives/relax_surface_mesh.rs");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let (_, structs) = parse_source(&text);
    let hand = structs
        .iter()
        .find(|s| s.name == "RelaxUniforms")
        .expect("RelaxUniforms is a dispatch-family hand struct");
    let registry = PrimitiveRegistry::with_builtin();
    for type_id in ["node.relax_surface_mesh", "node.surface_mesh_normals"] {
        let got = generated_fields(&registry, type_id, &hand.fields);
        assert!(
            layout_eq(&hand.fields, &got),
            "{type_id}: generated {got:?}\n  RelaxUniforms {:?}",
            hand.fields
        );
    }
}

/// `whitewater_emitter_dispatch` uploads every float param in PARAMS order,
/// then the count, zero-padded to whole 16-byte rows of its 32-word buffer.
#[test]
fn whitewater_emitter_dispatch_packs_the_generated_params() {
    let registry = PrimitiveRegistry::with_builtin();
    for type_id in [
        "node.whitewater_influence",
        "node.turbulence_emission_count",
    ] {
        let node = registry
            .construct(type_id)
            .expect("registered emitter atom");
        let mut packed: Vec<Field> = node
            .parameters()
            .iter()
            .map(|p| {
                assert!(
                    matches!(p.ty, ParamType::Float),
                    "{type_id}: the packer writes floats only, {} is not one",
                    p.name
                );
                Field {
                    name: p.name.as_ref().to_string(),
                    ty: "f32",
                }
            })
            .collect();
        packed.push(Field {
            name: "dispatch_count".into(),
            ty: "u32",
        });
        let words = packed.len().next_multiple_of(4);
        assert!(words <= 32, "{type_id}: {words} words overflow the packer");
        for pad in 0..words - packed.len() {
            packed.push(Field {
                name: format!("_pad{pad}"),
                ty: "u32",
            });
        }
        let got = generated_fields(&registry, type_id, &packed);
        assert!(
            layout_eq(&packed, &got),
            "{type_id}: generated {got:?}\n  packed {packed:?}"
        );
    }
}

// These nodes do not use the standalone dispatch_count ABI, or upload it from
// a shared packer proven above. Keep exceptions explicit: adding/removing an
// exception requires inspecting the run() path.
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
    // CPU terminal cells are uploaded directly; there is no GPU uniform ABI.
    "node.terminal_stream",
    "node.track_persist",
    // Custom GPU kernels, reduction/FFI or seed-stage layouts (not codegen Params).
    "node.blob_tracker",
    // One MPSGraph FFT call through manifold-gpu; no uniforms at all.
    "node.inverse_fft_2d",
    // CPU-origin mesh upload uses UploadUniforms, reflected against its actual
    // shader in uniform_layout_extended rather than codegen dispatch Params.
    "node.platonic_solid_mesh",
    "node.cylinder_wrap_field",
    "node.remove_drift_3d",
    "node.scatter_on_mesh",
    "node.spawn_from_image",
    "node.spawn_from_mesh",
    "node.spawn_particles",
    // Barriered multi-pass kernels (count, scan levels, scatter, total read);
    // their Params are reflected against the hand shaders in uniform_layout_extended.
    "node.running_total",
    "node.sort_particles_into_cells",
    "node.torus_wrap_field",
    // Host-borrowed Math View boundary; it has no standalone GPU Params ABI.
    "system.mesh_input",
    // Liquid state, frame ring and barriered stats reduction (the seam's and
    // GPU MPM's): cross-frame state and a multipass reduction, their custom
    // ABIs reflected by the extended custom cases.
    "node.liquid_frame",
    "node.liquid_state",
    "node.liquid_stats",
    "node.matter_frame",
    "node.matter_state",
    "node.matter_stats",
    // Block-local P2G (D6): workgroup tiles and barriers, exclusion 1.
    "node.matter_to_grid",
    // Region detection and tracking are CPU/FFI stateful boundaries. Their
    // Channels records are proven by the extended ABI test; neither node has
    // a generated standalone uniform mirror for its run() path.
    "node.detect_regions",
    "node.track_regions",
    // The two-pass reduction, barriered (its DotParams are reflected in
    // uniform_layout_extended).
    "node.dot_products",
    // Custom cut-map kernels share CutMapUniforms; their shader declaration is
    // reflected by uniform_layout_extended, while the remappers below use the
    // generated four-word dispatch ABI proof above.
    "node.cut_mesh_bands",
    "node.cut_mesh_cells",
    "node.remap_mesh_cut",
    "node.remap_cut_weights",
    // GPU FLIP's step: barriered sort, solve and particle passes over one
    // hand shader, its StepParams reflected in uniform_layout_extended.
    "node.gpu_flip_step",
    // The whitewater step: barriered spawn compaction and sort over one hand
    // shader beside its fused atoms, its HandParams reflected in
    // uniform_layout_extended.
    "node.whitewater_step",
    // The whitewater lifecycle runs FLIP's C++ on the CPU and writes its
    // outputs from there: no GPU kernel of its own, only buffer copies.
    "node.whitewater_lifecycle",
    // The water surface's barriered brick mark/scan/compact and its blob
    // bounds reduction: BrickUniforms and BoundsParams are reflected in
    // uniform_layout_extended.
    "node.lattice_bricks",
    "node.blob_bounds",
    // SurfaceMeshPass's shared RelaxUniforms, proven above for both kernels.
    "node.smooth_surface_mesh",
    "node.surface_mesh_normals",
    // The whitewater emitter packer, proven above.
    "node.whitewater_influence",
    "node.turbulence_emission_count",
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

#[test]
fn parser_finds_testkit_visible_structs_in_both_arms() {
    for name in ["testkit_visible", "manifold_core::testkit_visible"] {
        for body in [
            "#[repr(C)] struct U { dispatch_count: u32 }",
            "testkit { pub struct U { wrong: f32 } } production { #[repr(C)] struct U { dispatch_count: u32 } }",
        ] {
            let (_, structs) = parse_source(&format!("{name}! {{ {body} }}"));
            assert_eq!(structs.len(), 1);
            assert!(structs[0].repr_c);
            assert_eq!(
                structs[0].fields,
                vec![Field {
                    name: "dispatch_count".into(),
                    ty: "u32"
                }]
            );
        }
    }
}
