//! Generator parameter behavior tests.
//!
//! The former `GeneratorParamState` type was folded into the unified
//! [`crate::effects::PresetInstance`] (Phase 3 of
//! `docs/PRESET_INSTANCE_COLLAPSE_PLAN.md`): a generator is a
//! `PresetInstance { kind: Generator }`. All the param/driver/envelope
//! behavior, serde, and registry clamping live on `PresetInstance` now. This
//! module retains the generator-specific regression tests, exercised against
//! the merged type.

#[cfg(test)]
mod tests {
    use crate::effects::{PresetInstance, deserialize_generator_instance};
    use crate::generator_registration::{GeneratorMetadata, ParamSpec};
    use crate::preset_type_id::PresetTypeId;

    // Test-only inventory submission — BLACK_HOLE isn't linked from manifold-renderer in unit tests.
    inventory::submit! {
        GeneratorMetadata {
            id: PresetTypeId::BLACK_HOLE,
            display_name: "Black Hole",
            is_line_based: false,
            available: true,
            osc_prefix: "blackHole",
            legacy_discriminant: Some(21),
            params: &[
                ParamSpec::continuous("speed", "Speed", 0.0, 5.0, 0.3, "F2", "speed"),
                ParamSpec::continuous("cam_dist", "Cam Dist", 0.1, 50.0, 20.0, "F1", "camDist"),
                ParamSpec::continuous("tilt", "Tilt", 0.0, 90.0, 15.0, "F0", "tilt"),
                ParamSpec::continuous("rotate", "Rotate", -180.0, 180.0, 0.0, "F0", "rotate"),
                ParamSpec::whole("steps", "Steps", 50.0, 500.0, 150.0, "steps"),
                ParamSpec::continuous("disk_inner", "Disk Inner", 2.0, 6.0, 3.0, "F1", "diskInner"),
                ParamSpec::continuous("disk_outer", "Disk Outer", 5.0, 20.0, 10.0, "F1", "diskOuter"),
                ParamSpec::continuous("disk_glow", "Disk Glow", 0.5, 5.0, 2.0, "F1", "diskGlow"),
                ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
                ParamSpec::continuous("stars", "Stars", 0.0, 2.0, 0.5, "F2", "stars"),
                ParamSpec::continuous("spin", "Spin", -1.0, 1.0, 0.0, "F2", "spin"),
                ParamSpec::continuous("particles", "Particles", 0.0, 1.0, 0.0, "F2", "particles"),
                ParamSpec::continuous("turbulence", "Turbulence", 0.0, 5.0, 0.5, "F2", "turbulence"),
                ParamSpec::continuous("cam_velocity", "Cam Velocity", 0.0, 0.99, 0.0, "F2", "camVelocity"),
                ParamSpec::continuous("freefall", "Freefall", 0.0, 1.0, 0.0, "F0", "freefall"),
            ],
        }
    }

    // `migrate_pads_short_param_arrays_with_defaults_preserving_existing`,
    // `set_param_after_registry_growth_does_not_wipe_existing_values`, and
    // `set_base_param_writes_through_for_json_only_generator_with_no_registry_entry`
    // were DELETED (PARAM_STORAGE_DESIGN.md D3): all three exercised the old
    // positional `param_values` auto-grow-on-write behavior
    // (`migrate_to_registry_length` / the implicit align-to-registry-length
    // triggered by an out-of-range `set_base_param` index). The id-keyed
    // manifest is fully seeded at construction/load and never lazily grows —
    // a write to an unknown id is a no-op — so there is no positional tail to
    // pad or extend anymore. `gen_with_params` (their shared positional
    // fixture builder) went with them.

    #[test]
    fn generator_serialize_round_trips_type_and_values() {
        let mut gp = PresetInstance::new_generator(PresetTypeId::BLACK_HOLE);
        gp.set_base_param("speed", 1.25);
        let json = serde_json::to_string(&gp).unwrap();
        assert!(json.contains("\"generatorType\":\"BlackHole\""), "{json}");

        let mut de = serde_json::Deserializer::from_str(&json);
        let back = deserialize_generator_instance(&mut de).unwrap();
        assert_eq!(*back.generator_type(), PresetTypeId::BLACK_HOLE);
        assert!(back.is_generator());
        assert_eq!(back.get_base_param("speed"), 1.25);
    }

    /// docs/NODE_VOCABULARY_AUDIT.md §3 test (b): a project fixture carrying
    /// an old `generatorType` value loads through the real deserializer and
    /// comes back on the current id. Exercises
    /// `type_id_migration::TYPE_ID_MIGRATIONS`' fixture entry
    /// (`__vocab_migration_test_old__` → `__vocab_migration_test_new__`),
    /// chained after `remap_legacy_string` inside
    /// `preset_type_id::deserialize_generator_type` — the same function
    /// `Layer.gen_params` (a clip's generator) routes every project load
    /// through.
    #[test]
    fn generator_type_migrates_legacy_id_on_load() {
        let json = r#"{"generatorType":"__vocab_migration_test_old__"}"#;
        let mut de = serde_json::Deserializer::from_str(json);
        let back = deserialize_generator_instance(&mut de).unwrap();
        assert_eq!(back.generator_type().as_str(), "__vocab_migration_test_new__");
    }
}
