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
    use crate::effects::{ParamSlot, PresetInstance, deserialize_generator_instance};
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
            string_params: &[],
        }
    }

    /// Build a generator instance with the given type and a short param array
    /// (simulating a project saved before the type grew more params).
    fn gen_with_params(gt: PresetTypeId, values: Vec<f32>) -> PresetInstance {
        let mut state = PresetInstance::new_generator(gt);
        state.param_values = values.iter().map(|v| ParamSlot::exposed(*v)).collect();
        state.base_param_values = Some(values);
        state
    }

    #[test]
    fn migrate_pads_short_param_arrays_with_defaults_preserving_existing() {
        let gt = PresetTypeId::BLACK_HOLE;
        let target_count = crate::preset_definition_registry::try_get(&gt)
            .expect("BLACK_HOLE registered")
            .param_count;
        assert!(target_count >= 4, "test assumes BLACK_HOLE has at least 4 params");

        let mut state = gen_with_params(gt.clone(), vec![1.5, 2.5, 3.5]);
        state.migrate_to_registry_length();

        assert_eq!(state.param_values.len(), target_count);
        assert_eq!(state.param_values[0].value, 1.5);
        assert_eq!(state.param_values[1].value, 2.5);
        assert_eq!(state.param_values[2].value, 3.5);

        let def = crate::preset_definition_registry::try_get(&gt).unwrap();
        for i in 3..target_count {
            assert_eq!(
                state.param_values[i].value, def.param_defs[i].default_value,
                "tail index {i} should be registry default"
            );
        }

        let base = state.base_param_values.as_ref().unwrap();
        assert_eq!(base.len(), target_count);
        assert_eq!(base[0], 1.5);
        assert_eq!(base[1], 2.5);
        assert_eq!(base[2], 3.5);
    }

    #[test]
    fn set_param_after_registry_growth_does_not_wipe_existing_values() {
        let gt = PresetTypeId::BLACK_HOLE;
        let target_count = crate::preset_definition_registry::try_get(&gt)
            .expect("BLACK_HOLE registered")
            .param_count;

        // Values inside each param's clamp range (Speed 0..5, Cam Dist 0.1..50, Tilt 0..90).
        let mut state = gen_with_params(gt, vec![2.5, 8.0, 9.0]);
        state.set_param_base(0, 2.5);

        assert_eq!(state.param_values.len(), target_count);
        assert_eq!(state.param_values[0].value, 2.5);
        assert_eq!(state.param_values[1].value, 8.0);
        assert_eq!(state.param_values[2].value, 9.0);
    }

    #[test]
    fn set_param_base_writes_through_for_json_only_generator_with_no_registry_entry() {
        let unknown_type = PresetTypeId::from_string("DoesNotExist".to_string());
        assert!(
            crate::preset_definition_registry::try_get(&unknown_type).is_none(),
            "fixture relies on this type NOT being in the registry"
        );

        let mut state = gen_with_params(unknown_type, vec![0.0, 1.0]);
        state.set_param_base(1, 0.75);

        assert_eq!(state.param_values[1].value, 0.75, "write landed on bundled slot");
        assert_eq!(state.base_param_values.as_ref().unwrap()[1], 0.75);

        state.set_param_base(2, 0.42);
        assert_eq!(state.param_values.len(), 3, "param_values auto-extended");
        assert_eq!(state.param_values[2].value, 0.42, "tail write landed");
        assert_eq!(state.base_param_values.as_ref().unwrap()[2], 0.42);
    }

    #[test]
    fn generator_serialize_round_trips_type_and_values() {
        let mut gp = PresetInstance::new_generator(PresetTypeId::BLACK_HOLE);
        gp.set_param_base(0, 1.25);
        let json = serde_json::to_string(&gp).unwrap();
        assert!(json.contains("\"generatorType\":\"BlackHole\""), "{json}");

        let mut de = serde_json::Deserializer::from_str(&json);
        let back = deserialize_generator_instance(&mut de).unwrap();
        assert_eq!(*back.generator_type(), PresetTypeId::BLACK_HOLE);
        assert!(back.is_generator());
        assert_eq!(back.get_param_base(0), 1.25);
    }
}
