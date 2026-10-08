//! BUG-m3af: extended ABI proof for every primitive-owned uniform mirror.
//! Complements the existing scalar buffer proof; no GPU is needed.
use manifold_nodes as _;
#[path = "support"]
mod support {
    use crate::rust_items;
    pub mod custom_abi_cases;
    pub mod texture_abi_cases;
    pub mod uniform_abi;
}

// `region_types.rs` is a wire boundary rather than a generated GPU uniform.
// Include the production declarations so this integration proof checks the
// compiler's actual repr(C) layout instead of reproducing the structs here.
#[path = "../../manifold-nodes-image/src/node_graph/primitives/region_types.rs"]
mod region_wire_abi;

const BLOB_V2_UNIFORM_MIRRORS: &[(&str, &str)] = &[
    ("resize_limit.rs", "ResizeLimitUniforms"),
    ("region_mask.rs", "RegionMaskUniforms"),
    ("rgb_distance.rs", "RgbDistanceUniforms"),
    ("mask_extrema.rs", "MaskExtremaUniforms"),
];

const BLOB_V2_WIRE_MIRRORS: &[(&str, &str)] = &[
    ("region_types.rs", "LegacyBox"),
    ("region_types.rs", "Region"),
    ("region_types.rs", "TrackRecord"),
];

mod blob_v2 {
    use super::{BLOB_V2_UNIFORM_MIRRORS, region_wire_abi, support};
    use std::{
        mem::{offset_of, size_of},
        path::Path,
    };

    use manifold_node_engine::persistence::PrimitiveRegistry;
    use manifold_node_engine::freeze::codegen::standalone_for_node;
    use support::uniform_abi::{assert_wgsl_layout, resolve_source_path};
    use support::uniform_abi::shader_declaration;

    #[test]
    fn blob_v2_uniform_mirrors_match_generated_or_custom_layout() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../manifold-nodes-image/src/node_graph/primitives");
        let registry = PrimitiveRegistry::with_builtin();
        let mut failures = Vec::new();
        for &(source, rust_struct) in BLOB_V2_UNIFORM_MIRRORS {
            let type_id = match source {
                "resize_limit.rs" => "node.resize_limit",
                "region_mask.rs" => "node.region_mask",
                "rgb_distance.rs" => "node.rgb_distance",
                "mask_extrema.rs" => "node.mask_extrema",
                _ => unreachable!("all Blob V2 mirror cases have a type id"),
            };
            let path = resolve_source_path(source).expect("Blob V2 ABI source");
            let result = if source == "mask_extrema.rs" {
                std::fs::read_to_string(root.join("shaders/mask_extrema.wgsl"))
                    .map_err(|error| error.to_string())
                    .and_then(|shader| shader_declaration(&shader, "Params"))
                    .and_then(|declaration| {
                        assert_wgsl_layout(&path, rust_struct, &declaration, "Params", &[])
                    })
            } else {
                let node = registry
                    .construct(type_id)
                    .ok_or_else(|| format!("unregistered {type_id}"));
                node.and_then(|node| {
                    let mut wgsl = standalone_for_node(node.as_ref())
                        .map_err(|error| format!("codegen: {error:?}"))?;
                    for (token, _) in node.wgsl_specialization() {
                        wgsl = wgsl.replace(token, "1u");
                    }
                    assert_wgsl_layout(&path, rust_struct, &wgsl, "Params", &[])
                })
            };
            if let Err(error) = result {
                failures.push(format!("{source} / {rust_struct}: {error}"));
            }
        }
        assert!(
            failures.is_empty(),
            "Blob V2 uniform ABI proof failed:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn blob_v2_region_wire_mirrors_have_exact_channel_layouts() {
        use region_wire_abi::{LegacyBox, Region, TrackRecord};

        assert_eq!(region_wire_abi::MAX_REGIONS, 32);
        assert_eq!(size_of::<LegacyBox>(), 16);
        assert_eq!(offset_of!(LegacyBox, x), 0);
        assert_eq!(offset_of!(LegacyBox, y), 4);
        assert_eq!(offset_of!(LegacyBox, width), 8);
        assert_eq!(offset_of!(LegacyBox, height), 12);

        assert_eq!(size_of::<Region>(), 32);
        assert_eq!(offset_of!(Region, label), 0);
        assert_eq!(offset_of!(Region, x), 4);
        assert_eq!(offset_of!(Region, y), 8);
        assert_eq!(offset_of!(Region, width), 12);
        assert_eq!(offset_of!(Region, height), 16);
        assert_eq!(offset_of!(Region, area), 20);
        assert_eq!(offset_of!(Region, cx), 24);
        assert_eq!(offset_of!(Region, cy), 28);

        assert_eq!(size_of::<TrackRecord>(), 64);
        assert_eq!(offset_of!(TrackRecord, id), 0);
        assert_eq!(offset_of!(TrackRecord, label), 4);
        assert_eq!(offset_of!(TrackRecord, observed), 8);
        assert_eq!(offset_of!(TrackRecord, age), 12);
        assert_eq!(offset_of!(TrackRecord, x), 16);
        assert_eq!(offset_of!(TrackRecord, y), 20);
        assert_eq!(offset_of!(TrackRecord, width), 24);
        assert_eq!(offset_of!(TrackRecord, height), 28);
        assert_eq!(offset_of!(TrackRecord, cx), 32);
        assert_eq!(offset_of!(TrackRecord, cy), 36);
        assert_eq!(offset_of!(TrackRecord, vx), 40);
        assert_eq!(offset_of!(TrackRecord, vy), 44);
        assert_eq!(offset_of!(TrackRecord, area), 48);
        assert_eq!(offset_of!(TrackRecord, pad0), 52);
        assert_eq!(offset_of!(TrackRecord, pad1), 56);
        assert_eq!(offset_of!(TrackRecord, pad2), 60);
    }
}

mod texture {
    use super::support;

    use manifold_node_engine::persistence::PrimitiveRegistry;
    use manifold_node_engine::freeze::codegen::standalone_for_node;
    use support::texture_abi_cases::CASES;
    use support::uniform_abi::{assert_wgsl_layout, resolve_source_path};

    #[test]
    fn standalone_texture_and_resolve_uniforms_match_hand_layouts() {
        let registry = PrimitiveRegistry::with_builtin();
        let mut failures = Vec::new();
        for case in CASES {
            let Some(node) = registry.construct(case.type_id) else {
                failures.push(format!("{}: unregistered {}", case.source, case.type_id));
                continue;
            };
            let mut wgsl = match standalone_for_node(node.as_ref()) {
                Ok(wgsl) => wgsl,
                Err(error) => {
                    failures.push(format!("{}: codegen failed: {error:?}", case.type_id));
                    continue;
                }
            };
            for (token, _) in node.wgsl_specialization() {
                wgsl = wgsl.replace(token, "1u");
            }
            let path = resolve_source_path(case.source).expect("texture ABI source");
            if let Err(error) = assert_wgsl_layout(
                &path,
                case.rust_struct,
                &wgsl,
                case.shader_struct,
                case.aliases,
            ) {
                failures.push(format!("{} / {}: {error}", case.source, case.type_id));
            }
        }
        assert!(
            failures.is_empty(),
            "texture ABI proof failed:\n{}",
            failures.join("\n")
        );
    }
}
mod custom {
    use manifold_nodes::testkit::source_roots::verify_wgsl_roots;
    use super::support;
    use std::{collections::BTreeSet, path::Path};
    use support::{
        custom_abi_cases,
        texture_abi_cases,
        uniform_abi::*,
    };

    const INLINE_CASES: &[(&str, &str, &str, &str)] = &[
        (
            "render_filled_rects.rs",
            "FilledRectsUniforms",
            "FILLED_RECTS_SHADER",
            "Uniforms",
        ),
        (
            "render_value_overlay.rs",
            "OverlayUniforms",
            "OVERLAY_SHADER",
            "Uniforms",
        ),
        (
            "multi_blend.rs",
            "MultiBlendUniforms",
            "UNIFORM_SCHEMA",
            "U",
        ),
    ];

    fn source_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("ABI source root") {
            let path = entry.expect("ABI source entry").path();
            if path.is_dir() {
                source_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    #[test]
    fn custom_gpu_uniforms_match_their_actual_shader_declarations() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../manifold-nodes-image/src/node_graph/primitives");
        let mut failures = Vec::new();
        for case in custom_abi_cases::CASES {
            let result = (|| {
                let source_path = resolve_source_path(case.source)?;
                let source = std::fs::read_to_string(&source_path).map_err(|e| e.to_string())?;
                if !source.contains(case.shader) {
                    return Err("mapped shader no longer appears in host source".into());
                }
                let shader_path = resolve_shader_path(&source_path, case.shader)?;
                let shader = std::fs::read_to_string(&shader_path).map_err(|e| e.to_string())?;
                let declaration = shader_declaration(&shader, case.shader_struct)?;
                assert_wgsl_layout(
                    &source_path,
                    case.rust_struct,
                    &declaration,
                    case.shader_struct,
                    case.aliases,
                )
            })();
            if let Err(error) = result {
                failures.push(format!("{} / {}: {error}", case.source, case.rust_struct));
            }
        }
        for &(file, host, constant, shader_name) in INLINE_CASES {
            let result = (|| {
                let shader = rust_string_constant(&root.join(file), constant)?;
                let declaration = shader_declaration(&shader, shader_name)?;
                let aliases: &[(&str, &str)] = if file == "multi_blend.rs" {
                    &[("_pad0", "_p0"), ("_pad1", "_p1"), ("_pad2", "_p2")]
                } else {
                    &[]
                };
                assert_wgsl_layout(&root.join(file), host, &declaration, shader_name, aliases)
            })();
            if let Err(error) = result {
                failures.push(format!("{file} / {host}: {error}"));
            }
        }
        assert!(
            failures.is_empty(),
            "custom ABI proof failed:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn cut_map_primitives_use_the_reflected_shared_custom_abi() {
        let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
        for type_id in custom_abi_cases::CUT_MAP_TYPE_IDS {
            assert!(
                registry.construct(type_id).is_some(),
                "cut-map primitive is not registered: {type_id}"
            );
        }
        let case = custom_abi_cases::CASES
            .iter()
            .find(|case| case.rust_struct == "CutMapUniforms")
            .expect("shared cut-map custom ABI case");
        let source_path = resolve_source_path(case.source).expect("cut-map source");
        let source = std::fs::read_to_string(&source_path).expect("cut-map source");
        assert!(
            source.contains(case.shader),
            "cut-map source must include its shader"
        );
        let shader_path = resolve_shader_path(&source_path, case.shader).expect("cut-map shader");
        let shader = std::fs::read_to_string(&shader_path).expect("cut-map shader");
        let declaration =
            shader_declaration(&shader, case.shader_struct).expect("Params declaration");
        assert_wgsl_layout(
            &source_path,
            case.rust_struct,
            &declaration,
            case.shader_struct,
            case.aliases,
        )
        .expect("shared CutMapUniforms ABI");
    }

    #[test]
    fn every_production_primitive_mirror_has_a_proof() {
        verify_wgsl_roots().expect("WGSL source-root inventory");
        let mut expected: BTreeSet<(String, String)> = texture_abi_cases::CASES
            .iter()
            .map(|c| {
                (
                    resolve_source_path(c.source)
                        .expect("texture ABI source")
                        .display()
                        .to_string(),
                    c.rust_struct.into(),
                )
            })
            .chain(
                custom_abi_cases::CASES
                    .iter()
                    .map(|c| {
                        (
                            resolve_source_path(c.source)
                                .expect("custom ABI source")
                                .display()
                                .to_string(),
                            c.rust_struct.into(),
                        )
                    }),
            )
            .chain(
                INLINE_CASES
                    .iter()
                    .map(|(f, h, _, _)| {
                        (
                            resolve_source_path(f)
                                .expect("inline ABI source")
                                .display()
                                .to_string(),
                            (*h).into(),
                        )
                    }),
            )
            .chain(
                super::BLOB_V2_UNIFORM_MIRRORS
                    .iter()
                    .map(|(f, h)| {
                        (
                            resolve_source_path(f)
                                .expect("Blob V2 ABI source")
                                .display()
                                .to_string(),
                            (*h).into(),
                        )
                    }),
            )
            .chain(
                super::BLOB_V2_WIRE_MIRRORS
                    .iter()
                    .map(|(f, h)| {
                        (
                            resolve_source_path(f)
                                .expect("Blob V2 ABI source")
                                .display()
                                .to_string(),
                            (*h).into(),
                        )
                    }),
            )
            .collect();
        // Not uniforms: two vertex payloads, whose vertex descriptor owns the
        // layout, and one Channels wire record, whose KnownItem specs own it
        // (proven in its module). The fixture files are compiled only under
        // cfg(test).
        let exclusions: BTreeSet<(String, String)> = [
            (
                resolve_source_path("render_lines.rs")
                    .expect("render_lines source")
                    .display()
                    .to_string(),
                "EdgeInstance".into(),
            ),
            (
                resolve_source_path("render_value_overlay.rs")
                    .expect("render_value_overlay source")
                    .display()
                    .to_string(),
                "GlyphQuad".into(),
            ),
            (
                resolve_source_path(
                    "../../../../manifold-node-engine/src/water/primitives/whitewater_obstacle_source.rs",
                )
                .expect("whitewater obstacle source")
                .display()
                .to_string(),
                "WhitewaterSource".into(),
            ),
            (
                resolve_source_path("test_camera_pointwise_fixture.rs")
                    .expect("camera fixture source")
                    .display()
                    .to_string(),
                "TestCameraPointwiseUniforms".into(),
            ),
            (
                resolve_source_path("test_multi_output_atomic_fixture.rs")
                    .expect("atomic fixture source")
                    .display()
                    .to_string(),
                "Uniforms".into(),
            ),
            (
                // FusedGainU is a local proof fixture for the freeze renderer,
                // not a production uniform upload or shader ABI mirror.
                resolve_source_path("proof_support.rs")
                    .expect("proof support source")
                    .display()
                    .to_string(),
                "FusedGainU".into(),
            ),
            (
                // LiquidBody and LiquidShape are storage records owned by the
                // liquid atlas upload contract, not uniform declarations.
                resolve_source_path("bodies.rs")
                    .expect("liquid bodies source")
                    .display()
                    .to_string(),
                "LiquidBody".into(),
            ),
            (
                resolve_source_path("bodies.rs")
                    .expect("liquid bodies source")
                    .display()
                    .to_string(),
                "LiquidShape".into(),
            ),
        ]
        .into();
        let mut seen_exclusions = BTreeSet::new();
        let mut missing = Vec::new();
        let mut scalar_count = 0;
        let mut files = Vec::new();
        for root in abi_source_roots().expect("ABI case source roots") {
            source_files(&root, &mut files);
        }
        for file in files {
            let file_key = file.display().to_string();
            for (name, dispatch_count) in host_structs(&file).unwrap() {
                let key = (file_key.clone(), name);
                if exclusions.contains(&key) {
                    seen_exclusions.insert(key);
                } else if expected.remove(&key) {
                } else if dispatch_count {
                    scalar_count += 1;
                }
                // Existing uniform_layout_proof verifies this family.
                else {
                    missing.push(format!("{}::{}", key.0, key.1));
                }
            }
        }
        assert_eq!(
            scalar_count, 125,
            "buffer-family census changed: a generated layout's dispatch_count mirror is proven by uniform_layout_proof; a hand kernel's struct needs a custom ABI case here and must not name its count dispatch_count"
        );
        assert_eq!(seen_exclusions, exclusions, "stale ABI census exclusion");
        assert!(
            missing.is_empty(),
            "unproved primitive mirrors: {}",
            missing.join(", ")
        );
        assert!(expected.is_empty(), "stale ABI cases: {expected:?}");
    }
}

#[cfg(test)]
mod dispatch_regression {
    use manifold_node_engine::freeze::codegen::{standalone_for_node, standalone_for_spec};
    use manifold_nodes_image::node_graph::primitives::{blob_overlay_render::BlobOverlayRender, draw_connections::DrawConnections, draw_dots::DrawDots, draw_gauge::DrawGauge, draw_markers::DrawMarkers, draw_ticks::DrawTicks};

    fn same<P: manifold_node_engine::primitive::Primitive + Default + 'static>() {
        let typed = standalone_for_spec::<P>().expect("typed standalone codegen");
        let dynamic = standalone_for_node(&P::default()).expect("dynamic standalone codegen");
        assert_eq!(dynamic, typed);
    }

    #[test]
    fn dynamic_draw_array_kernels_match_typed_codegen() {
        same::<DrawDots>();
        same::<DrawConnections>();
        same::<DrawMarkers>();
        same::<DrawTicks>();
        same::<DrawGauge>();
        let wgsl = standalone_for_node(&BlobOverlayRender::new()).expect("blob overlay codegen");
        assert!(wgsl.contains("fn cs_main"));
        assert_eq!(wgsl, standalone_for_spec::<BlobOverlayRender>().unwrap());
    }
}
