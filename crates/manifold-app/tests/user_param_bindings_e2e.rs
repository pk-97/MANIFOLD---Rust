//! Phase 3 end-to-end: V2 user-exposed parameter bindings survive
//! save/load with their associated driver and Ableton mappings.
//!
//! Lives in `manifold-app/tests/` because it depends on the renderer
//! registry being populated (Mirror effect's static param ids,
//! UVTransform inner-node param shape). Only `manifold-app` links
//! everything together.
//!
//! What this test proves:
//!
//! 1. `ToggleEffectParamExposeCommand` appends a `UserParamBinding`
//!    with the canonical id form `user.<short_handle>.<inner_param>.<n>`.
//! 2. Drivers and Ableton mappings created against that user id
//!    address the right slot via `EffectInstance.param_id_to_value_index`.
//! 3. Serializing the project to JSON and reloading preserves:
//!    - the `user_param_bindings` entry verbatim (id, label, handle,
//!      inner_param, min/max/default, convert),
//!    - the per-instance `param_values` tail slot at its expected
//!      position, with whatever value was last written,
//!    - the driver and Ableton mapping referencing the user id by
//!      name.
//! 4. After reload, the user-tail slot is still addressable by id
//!    (round-trip-safe addressing — the doc's gate-to-Phase-4 condition).
//!
//! See `docs/EFFECT_RUNTIME_UNIFICATION.md` §7.6 and §9 Phase 3.

// Force the linker to keep manifold-renderer's inventory::submit! blocks.
use manifold_renderer as _;

use manifold_core::EffectTypeId;
use manifold_core::ableton_mapping::{
    AbletonDeviceIdentity, AbletonMacroAddress, AbletonMappingStatus, AbletonParamMapping,
};
use manifold_core::effect_definition_registry;
use manifold_core::effects::{ParameterDriver, ParamConvert};
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_editing::command::Command;
use manifold_editing::commands::effects::{InnerParamMeta, ToggleEffectParamExposeCommand};

fn meta_for_uv_translate() -> InnerParamMeta {
    // UVTransform.translate is a Float param with range (-1, 1) per the
    // primitive definition. The graph-editor panel reads these from the
    // live snapshot at click time; we hardcode them here because this
    // test bypasses the panel.
    InnerParamMeta {
        label: "Translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
    }
}

#[test]
fn expose_mirror_inner_param_survives_save_reload_with_driver_and_ableton() {
    use manifold_core::project::Project;

    // Build a project with a Mirror master effect at well-aligned
    // static state. Mirror has 2 static params (amount, mode).
    let mut project = Project::default();
    let fx = effect_definition_registry::create_default(&EffectTypeId::MIRROR);
    // create_default lands param_values at registry defaults; verify.
    assert_eq!(fx.param_values.len(), 2, "Mirror has 2 static params");
    project.settings.master_effects.push(fx);
    let effect_id = project.settings.master_effects[0].id.clone();

    // 1. Expose UVTransform.translate via the command.
    let mut expose = ToggleEffectParamExposeCommand::new(
        effect_id.clone(),
        "uv_transform".to_string(),
        "translate".to_string(),
        true,
        meta_for_uv_translate(),
    );
    expose.execute(&mut project);

    let user_id = "user.uv_transform.translate.1";

    {
        let fx = &project.settings.master_effects[0];
        assert_eq!(fx.user_param_bindings.len(), 1);
        let ub = &fx.user_param_bindings[0];
        assert_eq!(ub.id, user_id);
        assert_eq!(ub.node_handle, "uv_transform");
        assert_eq!(ub.inner_param, "translate");
        // param_values gained one slot at index 2 (n_static + 0).
        assert_eq!(fx.param_values.len(), 3);
        assert_eq!(fx.param_id_to_value_index(user_id), Some(2));
    }

    // 2. Drag the slider — write a non-default value into the user-tail slot.
    {
        let fx = &mut project.settings.master_effects[0];
        let idx = fx.param_id_to_value_index(user_id).unwrap();
        fx.set_base_param(idx, 0.42);
    }

    // 3. Add a driver mapped to the user id.
    {
        let fx = &mut project.settings.master_effects[0];
        let driver = ParameterDriver::new(
            std::borrow::Cow::Owned(user_id.to_string()),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        );
        fx.drivers_mut().push(driver);
    }

    // 4. Add an Ableton macro mapping to the user id.
    {
        let fx = &mut project.settings.master_effects[0];
        let mapping = AbletonParamMapping {
            param_id: std::borrow::Cow::Owned(user_id.to_string()),
            address: AbletonMacroAddress {
                track_id: 0,
                device_id: 0,
                param_id: 7,
                device_identity: AbletonDeviceIdentity {
                    device_class_name: "TestDevice".to_string(),
                },
                track_name: String::new(),
                device_name: String::new(),
                macro_name: String::new(),
            },
            range_min: 0.0,
            range_max: 1.0,
            inverted: false,
            legacy_param_index: None,
            last_value: 0.0,
            status: AbletonMappingStatus::Dormant,
        };
        fx.ableton_mappings
            .get_or_insert_with(Vec::new)
            .push(mapping);
    }

    // 5. Serialize to JSON, then reload.
    let json = serde_json::to_string_pretty(&project).expect("serialize project");
    // Sanity: the user-id key appears in the serialized paramValues map
    // and in the userParamBindings array.
    assert!(
        json.contains("\"userParamBindings\""),
        "userParamBindings field must be emitted when non-empty"
    );
    assert!(
        json.contains("\"user.uv_transform.translate.1\": 0.42"),
        "user-tail slot value must be in the paramValues map (pretty-printed JSON): {json}"
    );

    let reloaded: Project = manifold_io::loader::load_project_from_json(&json).expect("reload");

    // 6. Verify the user binding round-trips intact.
    let fx = &reloaded.settings.master_effects[0];
    assert_eq!(fx.user_param_bindings.len(), 1, "binding survives reload");
    let ub = &fx.user_param_bindings[0];
    assert_eq!(ub.id, user_id);
    assert_eq!(ub.node_handle, "uv_transform");
    assert_eq!(ub.inner_param, "translate");
    assert_eq!(ub.label, "Translate");
    assert!((ub.min - -1.0).abs() < f32::EPSILON);
    assert!((ub.max - 1.0).abs() < f32::EPSILON);
    assert!((ub.default_value - 0.0).abs() < f32::EPSILON);
    assert!(matches!(ub.convert, ParamConvert::Float));

    // 7. The user-tail slot is still addressable by id and still holds 0.42.
    let value_idx = fx
        .param_id_to_value_index(user_id)
        .expect("user id resolves after reload");
    assert_eq!(value_idx, 2);
    assert!((fx.param_values[value_idx].value - 0.42).abs() < f32::EPSILON);
    // base_param_values matches.
    let base = fx.base_param_values.as_ref().expect("base values present");
    assert!((base[value_idx] - 0.42).abs() < f32::EPSILON);

    // 8. The driver is intact and references the user id.
    let drivers = fx.drivers.as_ref().expect("drivers present");
    assert_eq!(drivers.len(), 1);
    assert_eq!(drivers[0].param_id.as_ref(), user_id);
    assert_eq!(drivers[0].legacy_param_index, None);

    // 9. The Ableton mapping is intact.
    let mappings = fx.ableton_mappings.as_ref().expect("mappings present");
    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].param_id.as_ref(), user_id);
    assert_eq!(
        mappings[0].address.param_id, 7,
        "Ableton-side macro id (param_id on the address) survives reload"
    );
}

#[test]
fn unexpose_then_re_expose_yields_same_canonical_id() {
    // Determinism check: tick → untick → tick should yield the same
    // user_param_id, so any external references (saved fixtures,
    // shared shows) bind back cleanly.
    use manifold_core::project::Project;

    let mut project = Project::default();
    let fx = effect_definition_registry::create_default(&EffectTypeId::MIRROR);
    project.settings.master_effects.push(fx);
    let effect_id = project.settings.master_effects[0].id.clone();

    let mut expose = ToggleEffectParamExposeCommand::new(
        effect_id.clone(),
        "uv_transform".to_string(),
        "translate".to_string(),
        true,
        meta_for_uv_translate(),
    );
    expose.execute(&mut project);
    let id_first = project.settings.master_effects[0].user_param_bindings[0]
        .id
        .clone();

    let mut unexpose = ToggleEffectParamExposeCommand::new(
        effect_id.clone(),
        "uv_transform".to_string(),
        "translate".to_string(),
        false,
        meta_for_uv_translate(),
    );
    unexpose.execute(&mut project);
    assert!(
        project.settings.master_effects[0]
            .user_param_bindings
            .is_empty()
    );

    let mut expose_again = ToggleEffectParamExposeCommand::new(
        effect_id.clone(),
        "uv_transform".to_string(),
        "translate".to_string(),
        true,
        meta_for_uv_translate(),
    );
    expose_again.execute(&mut project);
    let id_second = project.settings.master_effects[0].user_param_bindings[0]
        .id
        .clone();

    assert_eq!(
        id_first, id_second,
        "deterministic id generator must produce the same id when there's no collision"
    );
}

#[test]
fn second_expose_under_same_handle_increments_n() {
    // Exposing two different inner params under the same node handle
    // must produce distinct ids — `.1` for the first, `.2` for the
    // second exposed under the same `(handle, inner_param)` pair, OR
    // `.1` for the first under each distinct `(handle, inner_param)`.
    use manifold_core::project::Project;

    let mut project = Project::default();
    let fx = effect_definition_registry::create_default(&EffectTypeId::MIRROR);
    project.settings.master_effects.push(fx);
    let effect_id = project.settings.master_effects[0].id.clone();

    let mut a = ToggleEffectParamExposeCommand::new(
        effect_id.clone(),
        "uv_transform".to_string(),
        "translate".to_string(),
        true,
        meta_for_uv_translate(),
    );
    a.execute(&mut project);

    let mut b = ToggleEffectParamExposeCommand::new(
        effect_id.clone(),
        "uv_transform".to_string(),
        "scale".to_string(),
        true,
        meta_for_uv_translate(),
    );
    b.execute(&mut project);

    let ids: Vec<String> = project.settings.master_effects[0]
        .user_param_bindings
        .iter()
        .map(|ub| ub.id.clone())
        .collect();
    assert_eq!(ids.len(), 2);
    // Distinct prefixes (different inner_param) so both land on `.1`.
    assert_eq!(ids[0], "user.uv_transform.translate.1");
    assert_eq!(ids[1], "user.uv_transform.scale.1");
}
