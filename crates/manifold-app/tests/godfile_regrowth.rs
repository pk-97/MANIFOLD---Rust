//! God-file regrowth guard — the campaign register's enforcement test.
//!
//! Wave 1 D11 (`docs/UI_FUNNEL_DECOMPOSITION_DESIGN.md`) specified this test;
//! Wave 2 P2-Z built it (`docs/MODEL_COMMAND_DECOMPOSITION_DESIGN.md` INV-M6,
//! review M4 found it was never created). Per-file line ceilings for every
//! register-listed file: post-split size + slack. A failure here means a file
//! on the register is regrowing — the fix is a decomposition conversation
//! against `docs/ARCHITECTURE_DEBT.md`, never bumping the ceiling in this
//! table without a register/design update naming the reason.
//!
//! Ceiling policy: post-split `wc -l` + max(15%, 100) rounded up to 50s,
//! set at each wave's final landing. Wave-1 files still awaiting P-I/P-S get
//! interim ceilings pinned at their current size + slack; those TIGHTEN when
//! Wave 1 closes.

use std::path::Path;

// (workspace-relative path, ceiling in lines)
const CEILINGS: &[(&str, usize)] = &[
    // Wave 2 — commands/graph/ (P2-G, landed 2026-07-22; sizes incl. tests)
    ("crates/manifold-editing/src/commands/graph/mod.rs", 450),
    ("crates/manifold-editing/src/commands/graph/node_edit.rs", 2100),
    ("crates/manifold-editing/src/commands/graph/expose.rs", 2400),
    ("crates/manifold-editing/src/commands/graph/groups.rs", 900),
    ("crates/manifold-editing/src/commands/graph/scene.rs", 3650),
    ("crates/manifold-editing/src/commands/graph/modifiers.rs", 1300),
    ("crates/manifold-editing/src/commands/graph/paste.rs", 350),
    ("crates/manifold-editing/src/commands/graph/test_support.rs", 350),
    // Wave 2 — effects/ (P2-E, landed 2026-07-22)
    ("crates/manifold-core/src/effects/mod.rs", 250),
    ("crates/manifold-core/src/effects/param_defs.rs", 200),
    ("crates/manifold-core/src/effects/bindings.rs", 600),
    ("crates/manifold-core/src/effects/relight.rs", 300),
    ("crates/manifold-core/src/effects/instance.rs", 2550),
    ("crates/manifold-core/src/effects/instance_serde.rs", 1550),
    ("crates/manifold-core/src/effects/group.rs", 150),
    ("crates/manifold-core/src/effects/driver.rs", 800),
    ("crates/manifold-core/src/effects/envelope.rs", 400),
    ("crates/manifold-core/src/effects/automation.rs", 500),
    ("crates/manifold-core/src/effects/test_support.rs", 250),
    // Wave 2 — project/ (P2-P, landed 2026-07-22)
    ("crates/manifold-core/src/project/mod.rs", 350),
    ("crates/manifold-core/src/project/load_migration.rs", 1500),
    ("crates/manifold-core/src/project/presets.rs", 550),
    ("crates/manifold-core/src/project/queries.rs", 950),
    ("crates/manifold-core/src/project/validate.rs", 300),
    ("crates/manifold-core/src/project/test_support.rs", 150),
    // Wave 3 — renderer runtime decomposition (P3-Z, landed 2026-07-22;
    // RENDERER_RUNTIME_DECOMPOSITION_DESIGN.md). Ceilings = current wc -l +10%
    // rounded up to 50s, EXCEPT the two D2/D3 named ceilings pinned at +5%:
    // preset_runtime/core.rs (Peter-sanctioned ~2k) and codegen/fused.rs
    // (buffer+texture fused emission share the region model, no sub-split).
    // gltf_import/ (P3-G split + P3-D tables + P3-A ImportCtx/ObjectAssembly)
    ("crates/manifold-renderer/src/node_graph/gltf_import/mod.rs", 100),
    ("crates/manifold-renderer/src/node_graph/gltf_import/assembly.rs", 200),
    ("crates/manifold-renderer/src/node_graph/gltf_import/animation.rs", 150),
    ("crates/manifold-renderer/src/node_graph/gltf_import/materials.rs", 450),
    ("crates/manifold-renderer/src/node_graph/gltf_import/cards.rs", 250),
    ("crates/manifold-renderer/src/node_graph/gltf_import/object_group.rs", 1100),
    ("crates/manifold-renderer/src/node_graph/gltf_import/scene.rs", 800),
    ("crates/manifold-renderer/src/node_graph/gltf_import/merge.rs", 450),
    ("crates/manifold-renderer/src/node_graph/gltf_import/report.rs", 50),
    ("crates/manifold-renderer/src/node_graph/gltf_import/tests.rs", 6050),
    // freeze/codegen/ (P3-C split + P3-A StandaloneKernelSpec)
    ("crates/manifold-renderer/src/node_graph/freeze/codegen/mod.rs", 50),
    ("crates/manifold-renderer/src/node_graph/freeze/codegen/types.rs", 550),
    ("crates/manifold-renderer/src/node_graph/freeze/codegen/uniforms.rs", 150),
    ("crates/manifold-renderer/src/node_graph/freeze/codegen/entry_points.rs", 200),
    ("crates/manifold-renderer/src/node_graph/freeze/codegen/standalone.rs", 1050),
    // D2 named ceiling (+5%): fused emission is one seam, no buffer/texture sub-split
    ("crates/manifold-renderer/src/node_graph/freeze/codegen/fused.rs", 1600),
    ("crates/manifold-renderer/src/node_graph/freeze/codegen/dispatch_contract_tests.rs", 250),
    ("crates/manifold-renderer/src/node_graph/freeze/codegen/gpu_tests.rs", 3450),
    // preset_runtime/ (P3-R split + P3-A ChainBuildInputs/FrameContextInputs)
    ("crates/manifold-renderer/src/preset_runtime/mod.rs", 200),
    // D3 named ceiling (+5%): PresetRuntime is one type; core ~2k is Peter-sanctioned
    ("crates/manifold-renderer/src/preset_runtime/core.rs", 2200),
    ("crates/manifold-renderer/src/preset_runtime/build.rs", 750),
    ("crates/manifold-renderer/src/preset_runtime/errors.rs", 300),
    ("crates/manifold-renderer/src/preset_runtime/segments.rs", 200),
    ("crates/manifold-renderer/src/preset_runtime/bindings.rs", 200),
    ("crates/manifold-renderer/src/preset_runtime/instrumentation.rs", 550),
    // preset_runtime/tests/ (P3-R #[path] test modules — W3-D1)
    ("crates/manifold-renderer/src/preset_runtime/tests/multi_segment.rs", 150),
    ("crates/manifold-renderer/src/preset_runtime/tests/binding_seed.rs", 100),
    ("crates/manifold-renderer/src/preset_runtime/tests/topology_hash.rs", 300),
    ("crates/manifold-renderer/src/preset_runtime/tests/user_binding.rs", 400),
    ("crates/manifold-renderer/src/preset_runtime/tests/bug080_manifest_gate.rs", 100),
    ("crates/manifold-renderer/src/preset_runtime/tests/persistent_slot.rs", 150),
    ("crates/manifold-renderer/src/preset_runtime/tests/generator_input.rs", 500),
    ("crates/manifold-renderer/src/preset_runtime/tests/chain_error.rs", 150),
    ("crates/manifold-renderer/src/preset_runtime/tests/generator_runtime.rs", 1550),
    ("crates/manifold-renderer/src/preset_runtime/tests/chain_fusion.rs", 1450),
    ("crates/manifold-renderer/src/preset_runtime/tests/segment_prewarm.rs", 50),
    // Wave 1 register files still open (interim ceilings = current + slack;
    // tighten at Wave 1 close — P-I kills scrub fields, P-S splits panels)
    ("crates/manifold-app/src/app.rs", 4200),
    ("crates/manifold-app/src/app_render.rs", 4350),
    ("crates/manifold-ui/src/panels/mod.rs", 450),
    ("crates/manifold-ui/src/panels/param_card.rs", 8000),
    ("crates/manifold-ui/src/panels/inspector.rs", 4900),
    ("crates/manifold-ui/src/panels/param_slider_shared.rs", 3650),
    ("crates/manifold-ui/src/panels/scene_setup_panel.rs", 4150),
];

#[test]
fn no_register_listed_file_regrows() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut violations = Vec::new();
    for (rel, ceiling) in CEILINGS {
        let path = root.join(rel);
        // A missing file is fine: a later wave may have split it further.
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lines = text.lines().count();
        if lines > *ceiling {
            violations.push(format!("{rel}: {lines} lines > ceiling {ceiling}"));
        }
    }
    assert!(
        violations.is_empty(),
        "god-file regrowth detected (see docs/ARCHITECTURE_DEBT.md — do not bump ceilings \
         without a register/design update):\n{}",
        violations.join("\n")
    );
}
