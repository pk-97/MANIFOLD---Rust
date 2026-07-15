//! End-to-end registry-resolution test for the V1.1 → V1.2 driver
//! migration (step 8 of `docs/EFFECT_RUNTIME_UNIFICATION.md` §11).
//!
//! Lives in `manifold-app/tests/` rather than `manifold-io/tests/`
//! because the post-load resolver depends on `inventory::submit!`
//! entries from `manifold-renderer/src/effects/*.rs` and
//! `manifold-renderer/src/generators/*.rs`. `manifold-app` is the
//! one crate that links all of them, so this is the only place the
//! registry is fully populated outside a running binary.
//!
//! The `manifold-io` round-trip test still covers shape preservation
//! (counts, beat divisions, byte-equal driver tuples). This test
//! covers the missing piece: that legacy `paramIndex: i32` actually
//! resolves to the correct stable `param_id` via the live registry.

// Force the linker to keep manifold-renderer's `inventory::submit!`
// blocks. Without a reference into the crate, dead-code elimination
// can drop the entire compilation unit and silently empty the
// effect / generator registries.
use manifold_renderer as _;

use manifold_io::loader;

fn fixture_path(name: &str) -> std::path::PathBuf {
    // The `.manifold` fixtures are gitignored (large personal projects), so a
    // `git worktree` checkout doesn't contain them. Resolve to the MAIN working
    // tree: `--git-common-dir` points at the primary repo's `.git`, whose parent
    // is the main checkout where the fixtures live — so these tests RUN from a
    // worktree instead of panicking with a confusing file-not-found. Falls back
    // to the crate-relative path (the main checkout, or if git isn't reachable).
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .output()
        && out.status.success()
        && let Ok(common) =
            std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).canonicalize()
        && let Some(main_root) = common.parent()
    {
        let candidate = main_root.join("tests/fixtures").join(name);
        if candidate.exists() {
            return candidate;
        }
    }
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../tests/fixtures");
    p.push(name);
    p
}

/// BUG-005: a macro mapping addresses its effect by stable `EffectId`, so with
/// two effects of the same type on one layer it drives the exact one mapped —
/// not whichever comes first by type (the old `find(effect_type)` bug).
#[test]
fn macro_drives_the_exact_effect_among_two_of_the_same_type() {
    use manifold_core::PresetTypeId;
    use manifold_core::layer::Layer;
    use manifold_core::macro_bank::{MacroBank, MacroCurve, MacroMapping, MacroMappingTarget};
    use manifold_core::preset_definition_registry;
    use manifold_core::project::Project;
    use manifold_core::types::LayerType;

    let ty = PresetTypeId::new("Bloom");
    let pid = preset_definition_registry::get(&ty)
        .param_defs
        .first()
        .map(|pd| pd.spec.id.clone())
        .expect("Bloom must have a param 0 in the live registry");

    // Two same-type effects, both seeded to a sentinel so a stray write is
    // visible. A first-match-by-type resolver would hit `fx_a` (the first).
    let mut fx_a = preset_definition_registry::create_default(&ty);
    fx_a.set_base_param(&pid, 0.25);
    let mut fx_b = preset_definition_registry::create_default(&ty);
    fx_b.set_base_param(&pid, 0.25);
    let id_a = fx_a.id.clone();
    let id_b = fx_b.id.clone();

    let mut project = Project::default();
    let mut layer = Layer::new("L".into(), LayerType::Video, 0);
    layer.effects = Some(vec![fx_a, fx_b]);
    project.timeline.layers.push(layer);

    // Map macro slot 0 to the SECOND Bloom's param.
    project.settings.macro_bank.slots[0]
        .mappings
        .push(MacroMapping {
            target: MacroMappingTarget::Effect {
                effect_id: id_b.clone(),
                param_id: std::borrow::Cow::Owned(pid.to_string()),
            },
            range_min: 0.0,
            range_max: 1.0,
            curve: MacroCurve::Linear,
            legacy_param_index: None,
            legacy_effect_addr: None,
        });

    MacroBank::apply_macro(&mut project, 0, 1.0);

    let after_a = project.find_effect_by_id(&id_a).unwrap().get_base_param(&pid);
    let after_b = project.find_effect_by_id(&id_b).unwrap().get_base_param(&pid);
    assert!(
        (after_a - 0.25).abs() < 1e-6,
        "the OTHER same-type effect must be untouched, got {after_a}"
    );
    assert!(
        (after_b - 1.0).abs() < 1e-6,
        "the mapped effect must receive the macro value, got {after_b}"
    );
}

#[test]
fn burn_v5_drivers_resolve_to_stable_param_ids() {
    let path = fixture_path("Burn V5.manifold");
    if !path.exists() {
        return; // fixtures are gitignored; skip on CI without them
    }

    let project = loader::load_project(&path).expect("load Burn V5");

    // Burn V5 master_fx[0] is WireframeDepth (legacy discriminant 29),
    // master_fx[2] is Strobe (19). Both have a driver targeting
    // paramIndex 0 → the registry's param 0 → "amount" for both.
    let mfx = &project.settings.master_effects;
    let drv0 = mfx[0]
        .drivers
        .as_ref()
        .and_then(|d| d.first())
        .expect("master_fx[0] has 1 driver");
    let drv2 = mfx[2]
        .drivers
        .as_ref()
        .and_then(|d| d.first())
        .expect("master_fx[2] has 1 driver");

    assert_eq!(
        drv0.param_id, "amount",
        "WireframeDepth paramIndex 0 must resolve to 'amount' via registry"
    );
    assert_eq!(drv0.legacy_param_index, None, "legacy index cleared");
    assert_eq!(
        drv2.param_id, "amount",
        "Strobe paramIndex 0 must resolve to 'amount' via registry"
    );
    assert_eq!(drv2.legacy_param_index, None);

    // Every driver across master + layer effects must have a non-empty
    // param_id once the renderer's registry is in scope.
    let mut walked = 0;
    for fx in mfx {
        if let Some(ref drivers) = fx.drivers {
            for d in drivers {
                walked += 1;
                assert!(
                    !d.param_id.is_empty(),
                    "Burn V5 master driver has empty param_id post-resolve"
                );
                assert_eq!(d.legacy_param_index, None);
            }
        }
    }
    for layer in &project.timeline.layers {
        if let Some(ref effects) = layer.effects {
            for fx in effects {
                if let Some(ref drivers) = fx.drivers {
                    for d in drivers {
                        walked += 1;
                        assert!(
                            !d.param_id.is_empty(),
                            "Burn V5 layer driver has empty param_id post-resolve"
                        );
                        assert_eq!(d.legacy_param_index, None);
                    }
                }
            }
        }
    }
    assert!(walked > 0, "Burn V5 must have at least one driver");
}

#[test]
fn waypoints_generator_drivers_resolve_to_stable_param_ids() {
    let path = fixture_path("WAYPOINTS.manifold");
    if !path.exists() {
        return;
    }

    let project = loader::load_project(&path).expect("load WAYPOINTS");

    // Generator drivers are the half of the addressing space that
    // only Liveschool/WAYPOINTS exercise. Verify the resolver walks
    // `layer.gen_params().drivers` AND uses the generator registry
    // (not the effect registry).
    let mut gen_drivers = 0;
    for layer in &project.timeline.layers {
        if let Some(gp) = layer.gen_params()
            && let Some(ref drivers) = gp.drivers
        {
            for d in drivers {
                gen_drivers += 1;
                assert!(
                    !d.param_id.is_empty(),
                    "WAYPOINTS generator driver has empty param_id post-resolve"
                );
                assert_eq!(d.legacy_param_index, None);
            }
        }
    }
    assert!(
        gen_drivers > 0,
        "WAYPOINTS must have generator drivers post-migration"
    );
}

#[test]
fn liveschool_envelopes_resolve_to_stable_param_ids() {
    let path = fixture_path("Liveschool Live Show V6 LEDS.manifold");
    if !path.exists() {
        return;
    }

    let project = loader::load_project(&path).expect("load Liveschool");

    // 33 envelopes total: 13 effect-targeted + 20 gen_params
    // (generator-targeted). Envelope-home unification (v1.5→v1.6) moved
    // effect envelopes off the layer onto each effect instance; 2 of the
    // original 15 layer envelopes targeted a `WireframeDepth` effect absent
    // from their layer and were dropped (inert before). Every surviving one
    // should resolve cleanly.
    fn tally_effect_envs(
        scope: &str,
        fx: &manifold_core::effects::PresetInstance,
        total: &mut i32,
        effect_envs: &mut i32,
        empty: &mut Vec<String>,
    ) {
        if let Some(ref envs) = fx.envelopes {
            for (ei, env) in envs.iter().enumerate() {
                *total += 1;
                *effect_envs += 1;
                if env.param_id.is_empty() {
                    empty.push(format!("{scope}.fx[{}].env[{ei}]", fx.id.as_str()));
                }
            }
        }
    }

    let mut total = 0;
    let mut effect_envs = 0;
    let mut gen_envs = 0;
    let mut empty: Vec<String> = Vec::new();
    for fx in &project.settings.master_effects {
        tally_effect_envs("master", fx, &mut total, &mut effect_envs, &mut empty);
    }
    for (li, layer) in project.timeline.layers.iter().enumerate() {
        if let Some(ref effects) = layer.effects {
            for fx in effects {
                tally_effect_envs(&format!("layer[{li}]"), fx, &mut total, &mut effect_envs, &mut empty);
            }
        }
        for clip in &layer.clips {
            for fx in &clip.effects {
                tally_effect_envs(&format!("layer[{li}].clip"), fx, &mut total, &mut effect_envs, &mut empty);
            }
        }
        if let Some(gp) = layer.gen_params()
            && let Some(ref envs) = gp.envelopes
        {
            for (ei, env) in envs.iter().enumerate() {
                total += 1;
                gen_envs += 1;
                if env.param_id.is_empty() {
                    empty.push(format!(
                        "layer[{li}].gen.env[{ei}] (gen={})",
                        gp.generator_type().as_str()
                    ));
                }
            }
        }
    }

    assert_eq!(total, 33, "Liveschool must have exactly 33 envelopes post-unification");
    assert_eq!(effect_envs, 13, "expected 13 effect-instance envelopes (15 - 2 orphans)");
    assert_eq!(gen_envs, 20, "expected 20 gen_params envelopes");
    assert!(
        empty.is_empty(),
        "{} envelopes failed to resolve param_id from registry: {:?}",
        empty.len(),
        empty
    );
}

#[test]
fn liveschool_ableton_mappings_resolve_to_stable_param_ids() {
    let path = fixture_path("Liveschool Live Show V6 LEDS.manifold");
    if !path.exists() {
        return;
    }

    let project = loader::load_project(&path).expect("load Liveschool");

    // 29 Ableton mappings: 12 effect-targeted + 17 generator-targeted.
    // Same kind of safety net as drivers/envelopes — every mapping
    // must come out of post-load with a non-empty `param_id` (since
    // the inspector never creates Ableton mappings against
    // out-of-range params).
    let mut effect_maps = 0;
    let mut gen_maps = 0;
    let mut empty: Vec<String> = Vec::new();
    for fx in &project.settings.master_effects {
        if let Some(ref ms) = fx.ableton_mappings {
            for (mi, m) in ms.iter().enumerate() {
                effect_maps += 1;
                if m.param_id.is_empty() {
                    empty.push(format!(
                        "master.fx[{}].abl[{}] (effect={})",
                        fx.id.as_str(),
                        mi,
                        fx.effect_type().as_str()
                    ));
                }
            }
        }
    }
    for (li, layer) in project.timeline.layers.iter().enumerate() {
        if let Some(ref effects) = layer.effects {
            for (fi, fx) in effects.iter().enumerate() {
                if let Some(ref ms) = fx.ableton_mappings {
                    for (mi, m) in ms.iter().enumerate() {
                        effect_maps += 1;
                        if m.param_id.is_empty() {
                            empty.push(format!(
                                "layer[{li}].fx[{fi}].abl[{mi}] (effect={})",
                                fx.effect_type().as_str()
                            ));
                        }
                    }
                }
            }
        }
        if let Some(gp) = layer.gen_params()
            && let Some(ref ms) = gp.ableton_mappings
        {
            for (mi, m) in ms.iter().enumerate() {
                gen_maps += 1;
                if m.param_id.is_empty() {
                    empty.push(format!(
                        "layer[{li}].gen.abl[{mi}] (gen={})",
                        gp.generator_type().as_str()
                    ));
                }
            }
        }
    }

    assert_eq!(
        effect_maps + gen_maps,
        29,
        "Liveschool must have exactly 29 Ableton mappings"
    );
    assert_eq!(effect_maps, 12, "expected 12 effect-targeted mappings");
    assert_eq!(gen_maps, 17, "expected 17 generator-targeted mappings");

    // Known parked drop: FluidSim2D's 2D disk preset is currently
    // missing the `size` param (ordinal 10) that the Rust inventory metadata
    // still declares (14 params vs the disk's 13). When the disk def wins at
    // load, every param past `clip_trigger_mode` shifts down one, so this
    // mapping — authored against the old ordinal 13 (`fill`) — falls off the
    // end and drops. Restoring `size` as a real 2D control is parked: the
    // decomposed scatter atom splats one texel per particle, so there is no
    // per-particle radius to bind it to (apparent size comes from `feather`).
    // Until that lands, allow exactly this one orphan; every OTHER mapping
    // must still resolve so a genuine regression is caught.
    const KNOWN_PARKED_DROPS: &[&str] = &["layer[4].gen.abl[3] (gen=FluidSim2D)"];
    let unexpected: Vec<&String> = empty
        .iter()
        .filter(|e| !KNOWN_PARKED_DROPS.contains(&e.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "{} Ableton mapping(s) failed to resolve param_id from registry \
         (beyond the known parked FluidSim2D `size` drop): {:?}",
        unexpected.len(),
        unexpected
    );
}

#[test]
fn liveschool_full_registry_resolution() {
    let path = fixture_path("Liveschool Live Show V6 LEDS.manifold");
    if !path.exists() {
        return;
    }

    let project = loader::load_project(&path).expect("load Liveschool");

    // Liveschool has 130 drivers across effect + generator chains.
    // Most resolve cleanly via the registry. A few are **orphan** —
    // they reference paramIndex positions that no longer exist
    // because the effect/generator's param list shrunk since the
    // project was saved. The resolver leaves those with empty
    // `param_id` (driver inert) rather than panicking. We assert:
    //
    //   1. Total driver count is 130 (no losses during load).
    //   2. The orphans are accounted for — we know which generator
    //      / paramIndex pairs they point at, so a future bug that
    //      starts dropping resolvable drivers will surface as a new
    //      entry in `orphans`, not as a silent count mismatch.
    let mut effect_drivers = 0;
    let mut gen_drivers = 0;
    let mut orphans: Vec<(String, String)> = Vec::new();

    for (mi, fx) in project.settings.master_effects.iter().enumerate() {
        if let Some(ref drivers) = fx.drivers {
            for (di, d) in drivers.iter().enumerate() {
                effect_drivers += 1;
                if d.param_id.is_empty() {
                    orphans.push((
                        format!("master[{mi}].drv[{di}]"),
                        fx.effect_type().as_str().to_string(),
                    ));
                }
            }
        }
    }
    for (li, layer) in project.timeline.layers.iter().enumerate() {
        if let Some(ref effects) = layer.effects {
            for (fi, fx) in effects.iter().enumerate() {
                if let Some(ref drivers) = fx.drivers {
                    for (di, d) in drivers.iter().enumerate() {
                        effect_drivers += 1;
                        if d.param_id.is_empty() {
                            orphans.push((
                                format!("layer[{li}].fx[{fi}].drv[{di}]"),
                                fx.effect_type().as_str().to_string(),
                            ));
                        }
                    }
                }
            }
        }
        if let Some(gp) = layer.gen_params()
            && let Some(ref drivers) = gp.drivers
        {
            for (di, d) in drivers.iter().enumerate() {
                gen_drivers += 1;
                if d.param_id.is_empty() {
                    orphans.push((
                        format!("layer[{li}].gen.drv[{di}]"),
                        gp.generator_type().as_str().to_string(),
                    ));
                }
            }
        }
    }

    assert_eq!(
        effect_drivers + gen_drivers,
        130,
        "Liveschool must have exactly 130 drivers"
    );

    // Every orphan in the current snapshot is a FluidSim3D
    // generator driver pointing at paramIndex >= 21 (the generator
    // had ~27 params at save time, was trimmed since). 6 orphans
    // total. New orphans on other types = a real regression.
    let unexpected: Vec<&(String, String)> = orphans
        .iter()
        .filter(|(_, ty)| ty != "FluidSim3D")
        .collect();
    assert!(
        unexpected.is_empty(),
        "unexpected orphan drivers (registry should resolve these): {:?}",
        unexpected
    );
    assert!(
        orphans.len() <= 10,
        "{} orphan drivers (expected ~6 from FluidSim3D); resolver may have regressed: {:?}",
        orphans.len(),
        orphans
    );
}

#[test]
fn liveschool_gen_param_values_save_as_id_keyed_map() {
    // Step 13 (superseded by PARAM_STORAGE_DESIGN.md P1): PresetInstance
    // gets the same id-keyed wire shape for both kinds. With the renderer
    // registry linked, serializing a `gen_params` block must emit `params`
    // as a Map keyed by the generator's stable param ids — the only wire
    // shape the typed (de)serialize understands now (the old positional
    // Array fallback is gone entirely, not just de-preferred).
    let path = fixture_path("Liveschool Live Show V6 LEDS.manifold");
    if !path.exists() {
        return;
    }

    let project = loader::load_project(&path).expect("load Liveschool");

    // Find the first layer with gen_params + non-empty values.
    let gp = project
        .timeline
        .layers
        .iter()
        .filter_map(|l| l.gen_params())
        .find(|gp| !gp.params.is_empty())
        .expect("Liveschool must have at least one generator with params");
    // Snapshot by id (not the whole manifest — `ParamManifest`'s `topology`
    // field makes a whole-struct comparison brittle across the two different
    // construction paths: load-then-clone vs round-trip-through-serde).
    let original_by_id: Vec<(String, f32)> = gp
        .params
        .iter()
        .map(|p| (p.id().to_string(), p.value))
        .collect();

    let json = serde_json::to_string(gp).expect("serialize PresetInstance");
    assert!(
        json.contains("\"params\":{"),
        "registry-aware Serialize must emit gen params as an id-keyed Map; got: {json}"
    );

    // A generator instance decodes through the generator-shape deserializer
    // (the same one `Layer.gen_params` uses via `deserialize_with`); the bare
    // `PresetInstance` deserialize reads the effect shape (`effectType`/`id`).
    let mut de = serde_json::Deserializer::from_str(&json);
    let back: manifold_core::effects::PresetInstance =
        manifold_core::effects::deserialize_generator_instance(&mut de)
            .expect("deserialize gen map form");
    assert_eq!(
        back.params.len(),
        original_by_id.len(),
        "Map round-trip must preserve the generator's param count"
    );
    for (id, value) in &original_by_id {
        let got = back
            .params
            .get(id)
            .unwrap_or_else(|| panic!("param {id} missing after round-trip"))
            .value;
        assert!(
            (got - value).abs() < f32::EPSILON,
            "generator param {id} drifted on round-trip: {got} vs {value}"
        );
    }
    assert_eq!(back.generator_type(), gp.generator_type());
}

#[test]
fn liveschool_param_values_save_as_id_keyed_map() {
    // Step 12 (superseded by PARAM_STORAGE_DESIGN.md P1): with
    // `manifold-renderer` linked, the registry IS populated; saving an
    // effect must emit `params` as an id-keyed Map. Loading Liveschool
    // (a real V1.1-era fixture, positional Array on disk) exercises the
    // full one-time migration path (`migrations::param_storage_v14`)
    // before this typed round-trip.
    //
    // Round-trip: load Liveschool, serialize one master effect, deserialize
    // back, assert the values survive the trip with identical positional
    // contents.
    let path = fixture_path("Liveschool Live Show V6 LEDS.manifold");
    if !path.exists() {
        return;
    }

    let project = loader::load_project(&path).expect("load Liveschool");
    assert!(
        !project.settings.master_effects.is_empty(),
        "Liveschool must have master effects to test against"
    );

    // Pick the first non-empty master effect.
    let fx = project
        .settings
        .master_effects
        .iter()
        .find(|f| !f.params.is_empty())
        .expect("Liveschool must have at least one master effect with params");
    // Snapshot by id (not the whole manifest — see the generator sibling
    // test's comment on why a whole-`ParamManifest` comparison is brittle).
    let original_by_id: Vec<(String, f32)> = fx
        .params
        .iter()
        .map(|p| (p.id().to_string(), p.value))
        .collect();

    // Serialize → must be an id-keyed Map (registry is loaded).
    let json = serde_json::to_string(fx).expect("serialize PresetInstance");
    assert!(
        json.contains("\"params\":{"),
        "registry-aware Serialize must emit params as an id-keyed Map; got: {json}"
    );

    // Deserialize back → every param must still resolve by id to the same
    // value.
    let back: manifold_core::effects::PresetInstance =
        serde_json::from_str(&json).expect("deserialize map form");
    assert_eq!(
        back.params.len(),
        original_by_id.len(),
        "Map round-trip must preserve the effect's param count"
    );
    for (id, value) in &original_by_id {
        let got = back
            .params
            .get(id)
            .unwrap_or_else(|| panic!("param {id} missing after round-trip"))
            .value;
        assert!(
            (got - value).abs() < f32::EPSILON,
            "param {id} drifted on round-trip: {got} vs {value}"
        );
    }
}

#[test]
fn liveschool_macro_mappings_resolve_to_stable_param_ids() {
    let path = fixture_path("Liveschool Live Show V6 LEDS.manifold");
    if !path.exists() {
        return;
    }

    let project = loader::load_project(&path).expect("load Liveschool");

    // Macro mappings live on `settings.macro_bank.slots[*].mappings`
    // and target effect/generator params (parameter-bearing variants)
    // or master/layer opacity (no-param variants). Step 11 introduced
    // a `param_id` field on the parameter-bearing variants; the
    // resolver must populate it from the legacy `param_index` for
    // every loaded mapping.
    use manifold_core::macro_bank::MacroMappingTarget;
    let mut total = 0;
    let mut param_total = 0;
    let mut empty: Vec<String> = Vec::new();
    for (si, slot) in project.settings.macro_bank.slots.iter().enumerate() {
        for (mi, mapping) in slot.mappings.iter().enumerate() {
            total += 1;
            assert_eq!(
                mapping.legacy_param_index, None,
                "slot[{si}].mapping[{mi}] legacy index not cleared"
            );
            match &mapping.target {
                MacroMappingTarget::MasterOpacity | MacroMappingTarget::LayerOpacity { .. } => {
                    // No param to resolve.
                }
                MacroMappingTarget::Effect { effect_id, param_id } => {
                    param_total += 1;
                    // BUG-005 migration: a legacy type-keyed macro mapping must
                    // resolve to a concrete EffectId, clear its parked legacy
                    // address, and carry a resolved param_id.
                    if effect_id.is_empty() {
                        empty.push(format!("slot[{si}].mapping[{mi}] effect id unresolved"));
                    } else if param_id.is_empty() {
                        empty.push(format!(
                            "slot[{si}].mapping[{mi}] effect {} param_id empty",
                            effect_id.as_str()
                        ));
                    }
                    assert!(
                        mapping.legacy_effect_addr.is_none(),
                        "slot[{si}].mapping[{mi}] legacy effect addr not cleared after resolve"
                    );
                }
                MacroMappingTarget::GenParam { param_id, layer_id } => {
                    param_total += 1;
                    if param_id.is_empty() {
                        empty.push(format!(
                            "slot[{si}].mapping[{mi}] gen layer={}",
                            layer_id.as_str()
                        ));
                    }
                }
            }
        }
    }

    // The fixture has macro mappings (mix of genParam and layerEffect).
    // Exact count isn't enforced (less load-bearing than drivers/envs),
    // but every parameter-bearing mapping must resolve cleanly.
    assert!(
        param_total > 0,
        "Liveschool must have at least one parameter-bearing macro mapping"
    );
    assert!(
        empty.is_empty(),
        "{} of {} macro mappings failed to resolve param_id: {:?}",
        empty.len(),
        param_total,
        empty
    );
    let _ = total;
}

#[test]
fn liveschool_mirror_mode_values_migrate_after_curation_drop() {
    // After the V2 unification dropped Mirror's
    // `ParamConvert::EnumRemap([6, 7, 8])` curation, the outer slider
    // exposes Transform.mode's full 9-option enum directly. Old saves
    // with `mode ∈ {Horiz=0, Vert=1, Both=2}` need a one-time rewrite
    // to `{FoldX=6, FoldY=7, FoldBoth=8}` to preserve the same
    // visual behavior. Liveschool V6 has 6 Mirror instances; verify
    // every one's `mode` slot lands in {6, 7, 8} after load.
    let path = fixture_path("Liveschool Live Show V6 LEDS.manifold");
    if !path.exists() {
        return;
    }
    let project = loader::load_project(&path).expect("load Liveschool");

    use manifold_core::PresetTypeId;
    let mut mirrors = Vec::new();
    for fx in &project.settings.master_effects {
        if *fx.effect_type() == PresetTypeId::MIRROR {
            mirrors.push(fx);
        }
    }
    for layer in &project.timeline.layers {
        if let Some(effects) = layer.effects.as_deref() {
            for fx in effects {
                if *fx.effect_type() == PresetTypeId::MIRROR {
                    mirrors.push(fx);
                }
            }
        }
    }
    assert!(
        !mirrors.is_empty(),
        "Liveschool fixture is expected to carry Mirror instances",
    );
    for (i, fx) in mirrors.iter().enumerate() {
        // Mirror's outer params are [amount, mode] addressed by id now;
        // "mode" holds the value under test. After migration it must be
        // 6, 7, or 8.
        let mode_value = fx.get_param("mode");
        let coerced = mode_value.round() as i32;
        assert!(
            (6..=8).contains(&coerced),
            "mirror[{i}].mode = {coerced} after load — expected 6, 7, or 8 (migrated from legacy 0/1/2)",
        );
    }
}
