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
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../tests/fixtures");
    p.push(name);
    p
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

    // 35 envelopes total: 15 layer (effect-targeted) + 20 gen_params
    // (generator-targeted). Every one of them should resolve cleanly
    // — envelopes are simpler than drivers because they don't drift
    // (envelopes can only be created via the inspector against current
    // params, while drivers can be inherited from older saves with
    // out-of-range paramIndexes).
    let mut total = 0;
    let mut layer_envs = 0;
    let mut gen_envs = 0;
    let mut empty: Vec<String> = Vec::new();
    for (li, layer) in project.timeline.layers.iter().enumerate() {
        if let Some(ref envs) = layer.envelopes {
            for (ei, env) in envs.iter().enumerate() {
                total += 1;
                layer_envs += 1;
                if env.param_id.is_empty() {
                    empty.push(format!(
                        "layer[{li}].env[{ei}] (target={})",
                        env.target_effect_type.as_str()
                    ));
                }
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

    assert_eq!(total, 35, "Liveschool must have exactly 35 envelopes");
    assert_eq!(layer_envs, 15, "expected 15 layer-effect envelopes");
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
    assert!(
        empty.is_empty(),
        "{} Ableton mappings failed to resolve param_id from registry: {:?}",
        empty.len(),
        empty
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

    // Every orphan in the current snapshot is a FluidSimulation3D
    // generator driver pointing at paramIndex >= 21 (the generator
    // had ~27 params at save time, was trimmed since). 6 orphans
    // total. New orphans on other types = a real regression.
    let unexpected: Vec<&(String, String)> = orphans
        .iter()
        .filter(|(_, ty)| ty != "FluidSimulation3D")
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
