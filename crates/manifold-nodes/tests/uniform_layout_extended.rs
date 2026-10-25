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

mod blob_v2 {
    use super::{BLOB_V2_UNIFORM_MIRRORS, region_wire_abi, support};
    use std::{
        mem::{offset_of, size_of},
        path::Path,
    };

    use manifold_node_engine::freeze::codegen::standalone_for_node;
    use manifold_node_engine::persistence::PrimitiveRegistry;
    use support::uniform_abi::shader_declaration;
    use support::uniform_abi::{assert_wgsl_layout, resolve_source_path};

    #[test]
    fn blob_v2_uniform_mirrors_match_generated_or_custom_layout() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../manifold-nodes-image/src/node_graph/primitives");
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

    use manifold_node_engine::freeze::codegen::standalone_for_node;
    use manifold_node_engine::persistence::PrimitiveRegistry;
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
    use super::support;
    use std::path::Path;
    use support::{custom_abi_cases, uniform_abi::*};

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
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../manifold-nodes-image/src/node_graph/primitives");
        let mut failures = Vec::new();
        for case in custom_abi_cases::CASES {
            let result = (|| {
                let source_path = resolve_source_path(case.source)?;
                let mut source =
                    std::fs::read_to_string(&source_path).map_err(|e| e.to_string())?;
                // A file module `x.rs` with a sibling `x/` directory is one host:
                // its submodules may hold the shader's include site.
                let submodules = source_path.with_extension("");
                if submodules.is_dir() {
                    let mut files = Vec::new();
                    source_files(&submodules, &mut files);
                    for file in files {
                        source
                            .push_str(&std::fs::read_to_string(&file).map_err(|e| e.to_string())?);
                    }
                }
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

}

#[cfg(test)]
mod dispatch_regression {
    use manifold_node_engine::freeze::codegen::{standalone_for_node, standalone_for_spec};
    use manifold_nodes_image::node_graph::primitives::{
        blob_overlay_render::BlobOverlayRender, draw_connections::DrawConnections,
        draw_dots::DrawDots, draw_gauge::DrawGauge, draw_markers::DrawMarkers,
        draw_ticks::DrawTicks,
    };

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
