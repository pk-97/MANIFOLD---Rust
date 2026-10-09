use manifold_node_engine::validate::*;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_core::effect_graph_def::EffectGraphDef;
use std::path::Path;
const ASSET_SUBDIRS: &[(&str, ValidateKind)] = &[("assets/effect-presets", ValidateKind::Effect), ("assets/generator-presets", ValidateKind::Generator)];

    /// Every bundled preset JSON on disk validates clean through
    /// `validate_def` — the same set `check_presets` walks. Kept as a
    /// disk walk (not `bundled_preset_def`/inventory) so this test
    /// exercises `validate_def` exactly the way `graph_tool validate
    /// <file.json>` will: parse-from-disk, then validate.
    #[test]
    fn every_bundled_preset_validates_clean() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let registry = PrimitiveRegistry::with_builtin();
        #[cfg(feature = "gpu-proofs")]
        let serial = manifold_gpu::testkit::test_device();
        #[cfg(feature = "gpu-proofs")]
        let device = serial.arc();
        #[cfg(not(feature = "gpu-proofs"))]
        let device = manifold_node_engine::gpu::context::test_gpu_device("validate tests");
        manifold_gpu::testkit::load_disk_shader_caches(&device);

        let mut total = 0usize;
        let mut failures: Vec<(std::path::PathBuf, ValidationReport)> = Vec::new();

        for (subdir, kind) in ASSET_SUBDIRS {
            let dir = manifest_dir.join(subdir);
            let entries = std::fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                total += 1;
                let bytes = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{}: read failed: {e}", path.display()));
                let def: EffectGraphDef = serde_json::from_str(&bytes)
                    .unwrap_or_else(|e| panic!("{}: parse failed: {e}", path.display()));
                let report = validate_def(&def, &registry, *kind, &device);
                if !report.is_valid() {
                    failures.push((path, report));
                }
            }
        }

        assert!(total > 0, "expected to find bundled preset JSON files");
        assert!(
            failures.is_empty(),
            "{} of {total} bundled presets failed validate_def:\n{}",
            failures.len(),
            failures
                .iter()
                .map(|(p, r)| format!(
                    "{}: {}",
                    p.display(),
                    r.errors
                        .iter()
                        .map(|i| i.message.clone())
                        .collect::<Vec<_>>()
                        .join("; ")
                ))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    /// Every bundled preset's card-lint WARNING count, printed for
    /// Peter's triage per the P4 gate (D8: warnings are reported
    /// verbatim, never auto-fixed or suppressed in this phase). Run
    /// with `--nocapture` to see the counts; never fails on its own —
    /// `every_bundled_preset_validates_clean` above is the pass/fail
    /// gate for errors.
    #[test]
    fn bundled_preset_card_warning_counts() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let registry = PrimitiveRegistry::with_builtin();
        #[cfg(feature = "gpu-proofs")]
        let serial = manifold_gpu::testkit::test_device();
        #[cfg(feature = "gpu-proofs")]
        let device = serial.arc();
        #[cfg(not(feature = "gpu-proofs"))]
        let device = manifold_node_engine::gpu::context::test_gpu_device("validate tests");
        manifold_gpu::testkit::load_disk_shader_caches(&device);

        for (subdir, kind) in ASSET_SUBDIRS {
            let dir = manifest_dir.join(subdir);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let bytes = std::fs::read_to_string(&path).unwrap();
                let def: EffectGraphDef = serde_json::from_str(&bytes).unwrap();
                let report = validate_def(&def, &registry, *kind, &device);
                if !report.warnings.is_empty() {
                    eprintln!(
                        "WARN-REPORT {}: {} warning(s)",
                        path.display(),
                        report.warnings.len()
                    );
                    for w in &report.warnings {
                        eprintln!("  - {}", w.message);
                    }
                }
            }
        }
    }
