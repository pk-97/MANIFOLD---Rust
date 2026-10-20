//! Bundled generator preset registry.
//!
//! Generator metadata is loaded through the kind-agnostic
//! [`crate::bundled_presets`] catalog. Each JSON generator preset is **scanned from disk at
//! startup** by [`manifold_node_engine::load::preset_loader`] (stock + optional user dirs),
//! not embedded into the binary.
//!
//! The raw JSON, parsed definitions, type ids, and migration policy all live
//! in that shared catalog module. This module keeps the generator metadata
//! inventory submission and generator-sweep tests.
//!
//! ## Add a new generator preset
//!
//! 1. Drop a JSON file at the stock generator dir `<TypeId>.json` —
//!    must reference `system.generator_input` + `system.final_output`
//!    boundary nodes (see [`manifold_node_engine::scene::boundary_nodes`]).
//! 2. Relaunch — the loader scans it; no rebuild required.

use manifold_core::preset_def::PresetKind;

/// Loader function for the core's
/// [`manifold_core::preset_definition_registry::generator::PresetSource`]
/// inventory. Walks the bundled preset table, parses each JSON document,
/// and returns the `preset_metadata` field from every entry that carries
/// one (v2 schema). Mirrors `loaded_presets_from_bundled` on the
/// effect side.
///
/// Called at startup and again during hot reload after the catalog snapshot
/// has been swapped. The section 11 generator unification means a JSON preset's
/// `presetMetadata` block is the canonical schema for that generator.
pub fn loaded_generator_presets_from_bundled()
-> Vec<manifold_core::effect_graph_def::PresetMetadata> {
    crate::bundled_presets::loaded_preset_metadata(PresetKind::Generator)
}

inventory::submit! {
    manifold_core::preset_definition_registry::generator::PresetSource {
        name: "bundled_generators",
        load: loaded_generator_presets_from_bundled,
    }
}

#[cfg(test)]
mod tests {
    use manifold_node_engine::load::preset_loader::GENERATOR_CATALOG;

    /// The Plasma preset must be discoverable through this table: the
    /// `Plasma` entry keeps the long-lived `PresetTypeId::PLASMA` shipping id.
    /// Renaming or removing it would break existing Plasma layers and the
    /// editor's cog button on those layers.
    ///
    /// (TrivialPassthrough used to be asserted here too; it moved to the
    /// test fixtures in PRESET_BROWSER_AUDITION P1, D8 — it is no longer a
    /// shipping preset.)
    #[test]
    fn bundled_presets_include_shipping_generators() {
        let ids: Vec<String> = crate::bundled_presets::bundled_preset_type_ids(
            manifold_core::preset_def::PresetKind::Generator,
        )
        .map(|t| t.as_str().to_string())
        .collect();
        assert!(
            ids.contains(&"Plasma".to_string()),
            "Plasma preset must ship under id `Plasma` — got {ids:?}",
        );
    }

    /// Every catalog entry must deserialize as an EffectGraphDef, and each
    /// binding must name an outer-card param. A missing param would leave
    /// the inner node pinned to its default value.
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
        use manifold_node_engine::runtime::PresetRuntime;
        use manifold_node_engine::persistence::PrimitiveRegistry;
        use manifold_gpu::GpuTextureFormat;
        let device = manifold_gpu::testkit::test_device();
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
    ///
    /// Every frame's pixels must also be finite: a NaN or Inf names a
    /// shader that divides by zero or takes `log(0)` on default params.
    /// Each preset also renders a first frame at a fixed non-zero time and
    /// beat (1.234, 2.5), checked the same way, so time-driven generators
    /// are read past t = 0.
    #[test]
    fn every_bundled_preset_executes_one_frame() {
        use manifold_node_engine::runtime::PresetRuntime;
        use manifold_node_engine::runtime::preset_context::PresetContext;
        use manifold_node_engine::persistence::PrimitiveRegistry;
        use manifold_node_engine::gpu::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let device = manifold_gpu::testkit::test_device();
        let registry = PrimitiveRegistry::with_builtin();
        // 256x256 is enough to exercise every dispatch + copy path
        // without paying for 1080p memory traffic. The bug classes this
        // test catches (format mismatches, missing bindings, bad WGSL,
        // workgroup-size errors) reproduce at any size.
        let (w, h) = (256u32, 256u32);
        let format = GpuTextureFormat::Rgba16Float;

        let mut failures: Vec<String> = Vec::new();
        let mut count = 0_usize;

        for (preset_id, json) in GENERATOR_CATALOG.load().entries() {
            count += 1;
            let ctx = |time: f64, beat: f64| PresetContext {
                time,
                beat,
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

            for (frame, time, beat) in [("first-frame", 0.0, 0.0), ("t=1.234 frame", 1.234, 2.5)] {
                // A fresh runtime per frame: the t=1.234 frame is a first frame
                // too. On one runtime it would follow t=0, and every simulation
                // would step through the 1.234 s gap first.
                let mut g = match PresetRuntime::from_json_str_with_device(
                    &json, &registry, device.arc(), w, h, format, None,
                ) {
                    Ok(g) => g,
                    Err(e) => {
                        // Already caught by `every_bundled_preset_chain_builds`
                        // but report here too so the failure list is complete
                        // when only this test gets run in isolation.
                        failures.push(format!("{preset_id}: load failed: {e}"));
                        break;
                    }
                };
                let target = RenderTarget::new(&device, w, h, format, "first-frame-test");
                let ctx = ctx(time, beat);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut native_enc = device.create_encoder("first-frame-test");
                    {
                        let mut gpu =
                            manifold_node_engine::gpu::gpu_encoder::GpuEncoder::new(&mut native_enc, &device);
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
                    failures.push(format!("{preset_id}: {frame} panic: {msg}"));
                    break;
                }
                let bytes = manifold_node_engine::gpu::headless_readback::readback_raw_halves(&device, &target.texture, w, h);
                if let Some(bad) = non_finite_rgba16f(&bytes) {
                    failures.push(format!("{preset_id}: {frame} {bad}"));
                }
            }
        }

        assert!(count > 0, "expected ≥1 bundled generator preset");
        assert!(
            failures.is_empty(),
            "Bundled generator presets panicked or rendered non-finite pixels on first-frame execute:\n  - {}",
            failures.join("\n  - "),
        );
    }

    /// Reinterpret an Rgba16Float byte stream as f32 channels; describe the
    /// non-finite ones, or `None` when every channel is finite.
    #[cfg(feature = "gpu-proofs")]
    fn non_finite_rgba16f(bytes: &[u8]) -> Option<String> {
        assert_eq!(bytes.len() % 2, 0, "odd byte length, expected RGBA16Float");
        let mut nans = 0_usize;
        let mut infs = 0_usize;
        let mut first_bad: Option<(usize, f32)> = None;
        for (i, chunk) in bytes.chunks_exact(2).enumerate() {
            let v = half::f16::from_bits(u16::from_le_bytes([chunk[0], chunk[1]])).to_f32();
            if v.is_nan() {
                nans += 1;
                first_bad.get_or_insert((i, v));
            } else if v.is_infinite() {
                infs += 1;
                first_bad.get_or_insert((i, v));
            }
        }
        let (idx, val) = first_bad?;
        Some(format!(
            "non-finite output — {nans} NaN(s) + {infs} Inf(s) of {} channels. First at channel {idx} = {val}.",
            bytes.len() / 2,
        ))
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
