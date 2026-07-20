//! Bundled generator preset registry.
//!
//! Mirror of `node_graph::bundled_presets` (effect presets) for the
//! generator side. Each JSON generator preset is **scanned from disk at
//! startup** by [`crate::preset_loader`] (stock + optional user dirs),
//! not embedded into the binary, and exposed here as a lookup by
//! `PresetTypeId`.
//!
//! The [`GeneratorRegistry`](crate::generators::registry::GeneratorRegistry)
//! consults this table when creating a generator: if an entry matches
//! the requested type id, the registry constructs a
//! [`PresetRuntime`](crate::preset_runtime::PresetRuntime) from the JSON;
//! otherwise it falls back to the `inventory::submit!` Rust factories.
//!
//! The raw-JSON / def / type-id lookups live in the kind-agnostic
//! [`crate::node_graph::bundled_presets`] (fork #3 — one loader for both
//! kinds). This module keeps only the generator disk-bucket metadata loader
//! plus its `PresetSource` submission (the legit disk-source split) and the
//! generator-sweep tests.
//!
//! ## Add a new generator preset
//!
//! 1. Drop a JSON file at the stock generator dir `<TypeId>.json` —
//!    must reference `system.generator_input` + `system.final_output`
//!    boundary nodes (see [`crate::generators::json_graph_generator`]).
//! 2. Relaunch — the loader scans it; no rebuild required.

use manifold_core::effect_graph_def::EffectGraphDef;

use crate::preset_loader::GENERATOR_CATALOG;

/// Loader function for the core's
/// [`manifold_core::preset_definition_registry::generator::PresetSource`]
/// inventory. Walks the bundled preset table, parses each JSON document,
/// and returns the `preset_metadata` field from every entry that carries
/// one (v2 schema). Mirrors `loaded_presets_from_bundled` on the
/// effect side.
///
/// Cached at the `loaded_preset_metadata()` callsite — invoked once per
/// process. The §11 generator unification means a JSON preset's
/// `presetMetadata` block IS the canonical schema for that generator,
/// and the legacy inventory submission (if any) is overridden.
pub fn loaded_generator_presets_from_bundled()
-> Vec<manifold_core::effect_graph_def::PresetMetadata> {
    GENERATOR_CATALOG
        .load()
        .entries()
        .filter_map(|(id, json)| {
            let mut def: EffectGraphDef = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("bundled generator preset {id}: parse failed: {e}"));
            // P1 scene-panel exposure convergence: the preset-definition
            // registry seeds PresetInstance slots (via `init_defaults`), so it
            // MUST carry the same stamped scene exposures as the def cache
            // (`bundled_presets::rebuild_def_cache`). Without this, a bundled
            // scene preset (SceneStarter, the default scene) shows exposed card
            // rows whose backing instance slot never exists. Same deterministic
            // migration, applied on this parallel parse path.
            crate::node_graph::scene_exposure::migrate_scene_exposures(&mut def);
            def.preset_metadata
        })
        .collect()
}

inventory::submit! {
    manifold_core::preset_definition_registry::generator::PresetSource {
        load: loaded_generator_presets_from_bundled,
    }
}

// `bundled_generator_preset_json` and `bundled_generator_preset_type_ids`
// folded into the kind-agnostic `node_graph::bundled_presets`
// (`bundled_preset_json` / `bundled_preset_type_ids(PresetKind::Generator)`)
// — fork #3.

#[cfg(test)]
mod tests {
    use super::*;

    /// The TrivialPassthrough + Plasma presets that ship today must be
    /// discoverable through this table. The `Plasma` entry binds to
    /// `PresetTypeId::PLASMA` (the legacy id) so it supersedes the
    /// Rust factory of the same id — renaming or removing it would
    /// silently revert every existing Plasma layer to the Rust path
    /// and break the editor's cog button on those layers.
    #[test]
    fn bundled_presets_include_shipping_generators() {
        let ids: Vec<String> = crate::node_graph::bundled_preset_type_ids(
            manifold_core::preset_def::PresetKind::Generator,
        )
        .map(|t| t.as_str().to_string())
        .collect();
        assert!(
            ids.contains(&"TrivialPassthrough".to_string()),
            "TrivialPassthrough preset must ship — got {ids:?}",
        );
        assert!(
            ids.contains(&"Plasma".to_string()),
            "Plasma preset must ship under id `Plasma` to supersede the legacy Rust factory — got {ids:?}",
        );
    }

    /// Every disk-loaded JSON must be parseable. The loader does a
    /// structural JSON parse and skips malformed files; this is the
    /// deeper schema check that the bytes round-trip through serde into
    /// an `EffectGraphDef`.
    #[test]
    fn every_bundled_generator_preset_parses() {
        use manifold_core::effect_graph_def::EffectGraphDef;
        for (id, json) in GENERATOR_CATALOG.load().entries() {
            let _: EffectGraphDef = serde_json::from_str(&json).unwrap_or_else(|e| {
                panic!("bundled generator preset {id}: parse failed: {e}")
            });
        }
    }

    /// Class-level guard for the "Lissajous's clip-trigger toggle
    /// only drove mux_x, not mux_y" bug. Every binding in every
    /// bundled preset must reference an outer-card slider that
    /// actually exists — the `id` shared between [`BindingDef::id`]
    /// and [`ParamSpecDef::id`] is the rendezvous point.
    ///
    /// Why this matters as a sweep test (vs. a per-preset assertion):
    /// the bug class is "preset author adds a fan-out binding +
    /// forgets the matching outer slider, OR typos the id". The
    /// runtime degrades gracefully (warn + drop) but the symptom is
    /// silent — the inner param sits forever on the binding's
    /// `default_value`. CI catching it before merge is the only
    /// safety net that scales to N future presets.
    #[test]
    fn every_bundled_preset_binding_resolves_to_an_outer_param() {
        use manifold_core::effect_graph_def::EffectGraphDef;
        let mut violations: Vec<String> = Vec::new();
        for (preset_id, json) in GENERATOR_CATALOG.load().entries() {
            let doc: EffectGraphDef = serde_json::from_str(&json).unwrap_or_else(|e| {
                panic!("bundled preset {preset_id}: parse failed: {e}")
            });
            let Some(meta) = doc.preset_metadata.as_ref() else {
                continue; // legacy preset without metadata — no bindings to validate
            };
            let param_ids: std::collections::HashSet<&str> =
                meta.params.iter().map(|p| p.id.as_str()).collect();
            for binding in &meta.bindings {
                if !param_ids.contains(binding.id.as_str()) {
                    violations.push(format!(
                        "preset `{preset_id}`: binding id=`{}` (target {:?}) does not match \
                         any outer-card param id. Either add a `params` entry with \
                         id=`{}` or remove the binding — otherwise it will silently \
                         pin its inner target at default_value={} on every frame.",
                        binding.id, binding.target, binding.id, binding.default_value,
                    ));
                }
            }
        }
        assert!(
            violations.is_empty(),
            "Bundled preset bindings reference nonexistent outer params:\n  - {}",
            violations.join("\n  - "),
        );
    }

    #[cfg(feature = "gpu-proofs")]
    /// Sweep guard: every bundled preset must chain-build cleanly.
    /// Parse + binding-resolution pass already cover the schema; this
    /// catches the deeper failure modes that only the chain builder
    /// notices — unknown `typeId`, port-type mismatches on wires,
    /// missing required inputs, capacity-derivation cycles, output-
    /// slot-sizing failures.
    ///
    /// A new preset that lands in `assets/generator-presets/` and
    /// fails chain build would render black at runtime with just a
    /// warning in the log. This test catches it at compile time.
    #[test]
    fn every_bundled_preset_chain_builds() {
        use crate::preset_runtime::PresetRuntime;
        use crate::node_graph::PrimitiveRegistry;
        use manifold_gpu::GpuTextureFormat;
        let device = crate::test_device();
        let registry = PrimitiveRegistry::with_builtin();
        let mut failures: Vec<String> = Vec::new();
        for (preset_id, json) in GENERATOR_CATALOG.load().entries() {
            if let Err(e) = PresetRuntime::from_json_str_with_device(
                &json,
                &registry,
                device.arc(),
                1920,
                1080,
                GpuTextureFormat::Rgba16Float,
                None,
            ) {
                failures.push(format!("{preset_id}: {e}"));
            }
        }
        assert!(
            failures.is_empty(),
            "Bundled presets failed chain build:\n  - {}",
            failures.join("\n  - "),
        );
    }

    #[cfg(feature = "gpu-proofs")]
    /// Sweep guard: every bundled generator preset must successfully
    /// execute one full frame against a real Metal backend. Parse +
    /// chain-build cover the load-time validators (`into_graph` +
    /// `compile`); this catches the deeper failures that only surface
    /// at first dispatch — pipelines are created lazily inside primitive
    /// `run()` calls, so a malformed WGSL kernel, a Metal blit between
    /// mismatched texture formats (`copy_texture_to_texture` panic on
    /// cross-format `outputFormats` overrides), a workgroup-size
    /// mismatch, or an out-of-bounds binding all slip past compile and
    /// only blow up when the encoder actually records the dispatch.
    ///
    /// Failure mode caught: the "first frame grey, then app panic"
    /// symptom that's otherwise only visible at app launch on a real
    /// project load.
    ///
    /// Uses the production `Generator::render` path with `param_count =
    /// 0` so each outer-card slider falls back to the binding's
    /// `default_value` — same shape the host takes on a freshly loaded
    /// card before any user drag. Wraps the encoder dispatch +
    /// commit_and_wait in `catch_unwind` so one bad preset doesn't tear
    /// down the run; all failures are collected and reported at once.
    #[test]
    fn every_bundled_preset_executes_one_frame() {
        use crate::preset_runtime::PresetRuntime;
        use crate::preset_context::PresetContext;
        use crate::node_graph::PrimitiveRegistry;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let device = crate::test_device();
        let registry = PrimitiveRegistry::with_builtin();
        // 256x256 is enough to exercise every dispatch + copy path
        // without paying for 1080p memory traffic. The bug classes this
        // test catches (format mismatches, missing bindings, bad WGSL,
        // workgroup-size errors) reproduce at any size.
        let (w, h) = (256u32, 256u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut failures: Vec<String> = Vec::new();

        for (preset_id, json) in GENERATOR_CATALOG.load().entries() {
            let mut g = match PresetRuntime::from_json_str_with_device(
                &json, &registry, device.arc(), w, h, format, None,
            ) {
                Ok(g) => g,
                Err(e) => {
                    // Already caught by `every_bundled_preset_chain_builds`
                    // but report here too so the failure list is complete
                    // when only this test gets run in isolation.
                    failures.push(format!("{preset_id}: load failed: {e}"));
                    continue;
                }
            };

            let target = RenderTarget::new(&device, w, h, format, "first-frame-test");
            let ctx = PresetContext {
                time: 0.0,
                beat: 0.0,
                dt: 1.0 / 60.0,
                width: w,
                height: h,
                output_width: w,
                output_height: h,
                aspect: w as f32 / h as f32,
                owner_key: 0,
                is_clip_level: false,
                frame_count: 0,
                anim_progress: 0.0,
                trigger_count: 0,
            };

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut native_enc = device.create_encoder("first-frame-test");
                {
                    let mut gpu =
                        crate::gpu_encoder::GpuEncoder::new(&mut native_enc, &device);
                    g.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                native_enc.commit_and_wait_completed();
            }));

            if let Err(panic) = result {
                let msg = panic_msg(&panic);
                failures.push(format!("{preset_id}: first-frame panic: {msg}"));
            }
        }

        assert!(
            failures.is_empty(),
            "Bundled generator presets panicked on first-frame execute:\n  - {}",
            failures.join("\n  - "),
        );
    }

    /// Extract a printable message from a `catch_unwind` payload —
    /// `panic::Any` is opaque, but the standard payload shapes are
    /// `String` (from `panic!("{...}")`) and `&'static str` (from
    /// `panic!("literal")`).
    #[cfg(feature = "gpu-proofs")]
    fn panic_msg(panic: &Box<dyn std::any::Any + Send>) -> String {
        if let Some(s) = panic.downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = panic.downcast_ref::<&'static str>() {
            (*s).to_string()
        } else {
            "<non-string panic>".to_string()
        }
    }
}
