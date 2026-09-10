//! BUG-m3af: extended ABI proof for every primitive-owned uniform mirror.
//! Complements the existing scalar buffer proof; no GPU is needed.
mod support {
    pub mod custom_abi_cases;
    pub mod texture_abi_cases;
    pub mod uniform_abi;
}
mod texture {
    use super::support;
    use std::path::Path;

    use manifold_renderer::node_graph::PrimitiveRegistry;
    use manifold_renderer::node_graph::freeze::codegen::standalone_for_node;
    use support::texture_abi_cases::CASES;
    use support::uniform_abi::assert_wgsl_layout;

    #[test]
    fn standalone_texture_and_resolve_uniforms_match_hand_layouts() {
        let registry = PrimitiveRegistry::with_builtin();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/node_graph/primitives");
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
            let path = root.join(case.source);
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
    use std::{collections::BTreeSet, path::Path};
    use support::{custom_abi_cases, texture_abi_cases, uniform_abi::*};

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
    #[test]
    fn custom_gpu_uniforms_match_their_actual_shader_declarations() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/node_graph/primitives");
        let mut failures = Vec::new();
        for case in custom_abi_cases::CASES {
            let result = (|| {
                let source =
                    std::fs::read_to_string(root.join(case.source)).map_err(|e| e.to_string())?;
                if !source.contains(case.shader) {
                    return Err("mapped shader no longer appears in host source".into());
                }
                let shader =
                    std::fs::read_to_string(root.join(case.shader)).map_err(|e| e.to_string())?;
                let declaration = shader_declaration(&shader, case.shader_struct)?;
                assert_wgsl_layout(
                    &root.join(case.source),
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
    fn every_production_primitive_mirror_has_a_proof() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/node_graph/primitives");
        let mut expected: BTreeSet<(String, String)> = texture_abi_cases::CASES
            .iter()
            .map(|c| (c.source.into(), c.rust_struct.into()))
            .chain(
                custom_abi_cases::CASES
                    .iter()
                    .map(|c| (c.source.into(), c.rust_struct.into())),
            )
            .chain(
                INLINE_CASES
                    .iter()
                    .map(|(f, h, _, _)| ((*f).into(), (*h).into())),
            )
            .collect();
        // These two are vertex payloads, not uniforms. Their vertex descriptor owns
        // the layout. The fixture file is compiled only under cfg(test).
        let exclusions: BTreeSet<(String, String)> = [
            ("render_lines.rs".into(), "EdgeInstance".into()),
            ("render_value_overlay.rs".into(), "GlyphQuad".into()),
            (
                "test_camera_pointwise_fixture.rs".into(),
                "TestCameraPointwiseUniforms".into(),
            ),
        ]
        .into();
        let mut seen_exclusions = BTreeSet::new();
        let mut missing = Vec::new();
        let mut scalar_count = 0;
        for file in std::fs::read_dir(&root).unwrap() {
            let file = file.unwrap().path();
            if file.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let filename = file.file_name().unwrap().to_str().unwrap();
            for (name, dispatch_count) in host_structs(&file).unwrap() {
                let key = (filename.into(), name);
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
            scalar_count, 60,
            "buffer-family census changed; update its existing proof too"
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
    use manifold_renderer::node_graph::freeze::codegen::{
        standalone_for_node, standalone_for_spec,
    };
    use manifold_renderer::node_graph::primitives::{
        BlobOverlayRender, DrawConnections, DrawDots, DrawGauge, DrawMarkers, DrawTicks,
    };

    fn same<P: manifold_renderer::node_graph::primitive::Primitive + Default + 'static>() {
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
