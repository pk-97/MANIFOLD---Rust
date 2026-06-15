use manifold_io::loader;

fn fixture_path(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../tests/fixtures");
    p.push(name);
    p
}

#[test]
fn load_burn_v5_project() {
    let path = fixture_path("Burn V5.manifold");
    assert!(path.exists(), "Test fixture not found: {}", path.display());

    let project = loader::load_project(&path).expect("Failed to load Burn V5.manifold");

    // Basic project-level assertions
    assert_eq!(project.project_name, "Burn V5");

    // Settings
    assert!(
        (project.settings.bpm.0 - 138.0).abs() < 0.01,
        "BPM should be 138.0, got {}",
        project.settings.bpm
    );
    assert_eq!(project.settings.output_width, 1440);
    assert_eq!(project.settings.output_height, 2560);

    // Timeline structure
    assert_eq!(project.timeline.layers.len(), 9, "Expected 9 layers");

    // Layer names
    let layer_names: Vec<&str> = project
        .timeline
        .layers
        .iter()
        .map(|l| l.name.as_str())
        .collect();
    assert_eq!(
        layer_names,
        vec![
            "Gen 2",
            "Gen 3",
            "LISSAJOUS",
            "Gen 3",
            "Gen 4",
            "TUNNELS 2",
            "TESSERACT",
            "FIRE",
            "STOCK"
        ]
    );

    // Total clip count
    assert_eq!(
        project.timeline.total_clip_count(),
        34,
        "Expected 34 total clips"
    );
}

#[test]
fn load_burn_v5_clips_have_valid_beats() {
    let path = fixture_path("Burn V5.manifold");
    let project = loader::load_project(&path).unwrap();

    for (li, layer) in project.timeline.layers.iter().enumerate() {
        for (ci, clip) in layer.clips.iter().enumerate() {
            // IDs must not be empty
            assert!(!clip.id.is_empty(), "Clip [{li}][{ci}] has empty ID");

            // Duration must be positive
            assert!(
                clip.duration_beats > manifold_core::Beats::ZERO,
                "Clip {} [{li}][{ci}] has non-positive duration: {}",
                clip.id,
                clip.duration_beats
            );

            // Start beat should be non-negative
            assert!(
                clip.start_beat >= manifold_core::Beats::ZERO,
                "Clip {} [{li}][{ci}] has negative start beat: {}",
                clip.id,
                clip.start_beat
            );

            // end_beat should be after start_beat
            assert!(
                clip.end_beat() > clip.start_beat,
                "Clip {} [{li}][{ci}] end beat {} <= start beat {}",
                clip.id,
                clip.end_beat(),
                clip.start_beat
            );
        }
    }
}

#[test]
fn load_burn_v5_timeline_duration() {
    let path = fixture_path("Burn V5.manifold");
    let project = loader::load_project(&path).unwrap();

    let duration = project.timeline.duration_beats();
    assert!(
        duration > manifold_core::Beats::ZERO,
        "Timeline should have positive duration, got {}",
        duration
    );
}

#[test]
fn load_burn_v5_clip_lookup_works() {
    let path = fixture_path("Burn V5.manifold");
    let mut project = loader::load_project(&path).unwrap();

    // Grab a clip ID from the first layer
    let first_clip_id = project.timeline.layers[0].clips[0].id.clone();

    // O(1) lookup should find it
    let found = project.timeline.find_clip_by_id(&first_clip_id);
    assert!(
        found.is_some(),
        "Clip lookup failed for ID: {first_clip_id}"
    );
    assert_eq!(found.unwrap().id, first_clip_id);
}

#[test]
fn load_burn_v5_roundtrip_json() {
    let path = fixture_path("Burn V5.manifold");
    let project = loader::load_project(&path).unwrap();

    // Serialize back to JSON
    let json = serde_json::to_string_pretty(&project).expect("Failed to serialize project");

    // Reload from the serialized JSON
    let project2 =
        loader::load_project_from_json(&json).expect("Failed to reload from serialized JSON");

    // Basic structural equivalence
    assert_eq!(project2.project_name, project.project_name);
    assert_eq!(project2.settings.bpm, project.settings.bpm);
    assert_eq!(
        project2.timeline.layers.len(),
        project.timeline.layers.len()
    );
    assert_eq!(
        project2.timeline.total_clip_count(),
        project.timeline.total_clip_count()
    );
}

// ── Driver beat division preservation ──

#[test]
fn driver_beat_divisions_survive_load() {
    use manifold_core::types::BeatDivision;

    let path = fixture_path("Burn V5.manifold");
    let project = loader::load_project(&path).unwrap();

    // Collect all driver param ids + beat divisions after Rust load.
    // V1.1 fixture used `paramIndex: i32`; the post-load resolver
    // (Project::resolve_legacy_param_ids) translates each via the
    // registry to its stable `param_id`.
    let mut loaded: Vec<(String, String, BeatDivision)> = Vec::new();

    for (i, fx) in project.settings.master_effects.iter().enumerate() {
        if let Some(ref drivers) = fx.drivers {
            for d in drivers {
                loaded.push((
                    format!("master_fx[{i}]"),
                    d.param_id.to_string(),
                    d.beat_division,
                ));
            }
        }
    }
    for (li, layer) in project.timeline.layers.iter().enumerate() {
        if let Some(ref effects) = layer.effects {
            for (fi, fx) in effects.iter().enumerate() {
                if let Some(ref drivers) = fx.drivers {
                    for d in drivers {
                        loaded.push((
                            format!("layer[{li}].fx[{fi}]"),
                            d.param_id.to_string(),
                            d.beat_division,
                        ));
                    }
                }
            }
        }
    }

    // The legacy resolver fills `param_id` from the effect registry.
    // In the manifold-io test binary the renderer isn't linked, so
    // the registry has no effect ParamSpecs and `param_id` will be
    // empty — but the driver still loaded, beat division survived,
    // and the legacy index is gone (cleared by the resolver). In a
    // production build linking manifold-renderer (manifold-app), the
    // same load fills `param_id` from the registry.
    assert!(
        !loaded.is_empty(),
        "Burn V5 must have at least one driver post-load"
    );

    // Spot-check the count + the specific beat divisions in source order.
    let expected_divs: Vec<(&str, BeatDivision)> = vec![
        ("master_fx[0]", BeatDivision::Half),
        ("master_fx[2]", BeatDivision::TwoWhole),
        ("layer[3].fx[1]", BeatDivision::FourWhole),
        ("layer[6].fx[0]", BeatDivision::Whole),
        ("layer[6].fx[2]", BeatDivision::Whole),
        ("layer[7].fx[0]", BeatDivision::FourWhole),
        ("layer[7].fx[0]", BeatDivision::FourWhole),
        ("layer[8].fx[1]", BeatDivision::FourWhole),
    ];
    assert_eq!(loaded.len(), expected_divs.len(), "Driver count mismatch");
    for (i, ((loc, _, div), (e_loc, e_div))) in loaded.iter().zip(expected_divs.iter()).enumerate()
    {
        assert_eq!(loc, e_loc, "Location mismatch at index {i}");
        assert_eq!(
            div, e_div,
            "BeatDivision mismatch at {loc}: got {div:?}, expected {e_div:?}"
        );
    }
}

#[test]
fn driver_beat_divisions_survive_roundtrip() {
    let path = fixture_path("Burn V5.manifold");
    let project = loader::load_project(&path).unwrap();

    // Roundtrip: serialize to JSON then reload
    let json = serde_json::to_string_pretty(&project).unwrap();
    let project2 = loader::load_project_from_json(&json).unwrap();

    // Compare driver beat divisions
    for (li, (l1, l2)) in project
        .timeline
        .layers
        .iter()
        .zip(project2.timeline.layers.iter())
        .enumerate()
    {
        let fx1 = l1.effects.as_deref().unwrap_or(&[]);
        let fx2 = l2.effects.as_deref().unwrap_or(&[]);
        for (fi, (f1, f2)) in fx1.iter().zip(fx2.iter()).enumerate() {
            let d1 = f1.drivers.as_deref().unwrap_or(&[]);
            let d2 = f2.drivers.as_deref().unwrap_or(&[]);
            assert_eq!(
                d1.len(),
                d2.len(),
                "Driver count mismatch at layer[{li}].fx[{fi}]"
            );
            for (di, (a, b)) in d1.iter().zip(d2.iter()).enumerate() {
                assert_eq!(
                    a.beat_division, b.beat_division,
                    "BeatDiv roundtrip mismatch at layer[{li}].fx[{fi}].drv[{di}]: {:?} vs {:?}",
                    a.beat_division, b.beat_division
                );
            }
        }
    }
}

#[test]
fn waypoints_gen_drivers_survive_migration() {
    use manifold_core::types::BeatDivision;
    let path = fixture_path("WAYPOINTS.manifold");
    if !path.exists() {
        return;
    }

    let project = loader::load_project(&path).unwrap();

    // WAYPOINTS has legacy genDrivers that get migrated into genParams.drivers.
    // Verify they survived the V1.0.0 → V1.1.0 migration.
    let mut gen_driver_count = 0;
    let mut non_quarter_count = 0;
    for (li, layer) in project.timeline.layers.iter().enumerate() {
        if let Some(gp) = layer.gen_params()
            && let Some(drivers) = gp.drivers.as_ref()
        {
            for d in drivers {
                gen_driver_count += 1;
                if d.beat_division != BeatDivision::Quarter {
                    non_quarter_count += 1;
                }
                eprintln!(
                    "  layer[{li}].gen param={} beat_div={:?}",
                    d.param_id, d.beat_division
                );
            }
        }
    }
    eprintln!("Gen drivers: {gen_driver_count} total, {non_quarter_count} non-Quarter");
    assert!(
        gen_driver_count > 0,
        "WAYPOINTS should have generator drivers"
    );
    assert!(
        non_quarter_count > 0,
        "WAYPOINTS gen drivers should have non-Quarter beat divisions"
    );
}

// ── Additional project files ──

#[test]
fn load_burn_v4_project() {
    let path = fixture_path("Burn V4.manifold");
    if !path.exists() {
        return;
    } // Skip if fixture not available

    let project = loader::load_project(&path).expect("Failed to load Burn V4.manifold");

    assert_eq!(project.project_name, "Burn V4");
    assert!((project.settings.bpm.0 - 138.0).abs() < 0.01);
    assert_eq!(project.timeline.layers.len(), 9);
    assert_eq!(project.timeline.total_clip_count(), 34);
}

/// A generator running a per-instance graph override carries its preset id
/// twice: on the instance (`generator_type`) and in the graph's
/// `preset_metadata.id`. GraphTestsV4 was saved with those desynced — the
/// graph named its preset, but the instance reported `None`, which blanked
/// the generator card in the inspector. Post-load reconciliation must mirror
/// the graph id back onto the instance.
///
/// The fixtures are local + gitignored, so this asserts the reconciliation
/// invariant generically against whatever graph-override generator the local
/// project carries, and skips when there is none (e.g. a re-saved fixture
/// with no per-instance graph) — same posture as the path-missing guard.
#[test]
fn graphtestsv4_reconciles_desynced_generator_identity() {
    let path = fixture_path("GraphTestsV4.manifold");
    if !path.exists() {
        return;
    }

    let project = loader::load_project(&path).expect("Failed to load GraphTestsV4.manifold");

    // The desync scenario needs a generator layer carrying a per-instance
    // graph override whose metadata names a preset. Skip if the fixture no
    // longer holds one.
    let Some((gen_layer, graph_id)) = project
        .timeline
        .layers
        .iter()
        .filter(|l| l.layer_type == manifold_core::types::LayerType::Generator)
        .find_map(|l| {
            let id = l
                .generator_graph()
                .and_then(|g| g.preset_metadata.as_ref())
                .map(|m| m.id.clone())
                .filter(|id| *id != manifold_core::PresetTypeId::NONE && !id.as_str().is_empty())?;
            Some((l, id))
        })
    else {
        return;
    };

    // Post-load reconciliation: the instance's type id mirrors the graph's
    // metadata id (no longer NONE), so the inspector gate
    // (`generator_type != NONE`) surfaces the card.
    assert_eq!(*gen_layer.generator_type(), graph_id);
}

#[test]
fn load_waypoints_large_project() {
    let path = fixture_path("WAYPOINTS.manifold");
    if !path.exists() {
        return;
    }

    let project = loader::load_project(&path).expect("Failed to load WAYPOINTS.manifold");

    assert_eq!(project.project_name, "WAYPOINTS");
    assert!((project.settings.bpm.0 - 110.0).abs() < 0.01);
    assert_eq!(project.timeline.layers.len(), 9);
    // Original project had 2311 clips; 295 overlapping clips removed on load repair.
    assert_eq!(project.timeline.total_clip_count(), 2016);

    // Stress test: all clips should have valid beats and no overlaps
    for layer in &project.timeline.layers {
        for clip in &layer.clips {
            assert!(clip.duration_beats > manifold_core::Beats::ZERO);
            assert!(clip.start_beat >= manifold_core::Beats::ZERO);
        }
        assert!(
            !layer.has_overlapping_clips(),
            "Layer {:?} still has overlapping clips after load repair",
            layer.layer_id
        );
    }
}

// ── Liveschool Live Show V6 — canonical regression for steps 8-14 ──
//
// 20-minute live show with 52 layers, ~2828 clips, 155 effects across
// master + layer chains, 130 drivers, 35 envelopes, 29 Ableton mappings.
// Every addressing-site migration (steps 8-11 wire up `param_id` for
// drivers / envelopes / Ableton / macros; step 12 changes `paramValues`
// to map shape; step 14 bumps `projectVersion`) must preserve every
// count below. If a future migration regresses any of these, this test
// is the gate that catches it.

/// Walk every effect chain in a project and call `f` on each PresetInstance.
/// Covers master effects + layer effects.
fn for_each_effect<F: FnMut(&manifold_core::effects::PresetInstance)>(
    project: &manifold_core::project::Project,
    mut f: F,
) {
    for fx in &project.settings.master_effects {
        f(fx);
    }
    for layer in &project.timeline.layers {
        if let Some(ref effects) = layer.effects {
            for fx in effects {
                f(fx);
            }
        }
    }
}

fn count_drivers(project: &manifold_core::project::Project) -> usize {
    let mut n = 0;
    for_each_effect(project, |fx| {
        n += fx.drivers.as_ref().map(|d| d.len()).unwrap_or(0);
    });
    // Generator drivers live on `layer.gen_params().drivers`.
    for layer in &project.timeline.layers {
        if let Some(gp) = layer.gen_params() {
            n += gp.drivers.as_ref().map(|d| d.len()).unwrap_or(0);
        }
    }
    n
}

fn count_ableton_mappings(project: &manifold_core::project::Project) -> usize {
    let mut n = 0;
    for_each_effect(project, |fx| {
        n += fx.ableton_mappings.as_ref().map(|m| m.len()).unwrap_or(0);
    });
    for layer in &project.timeline.layers {
        if let Some(gp) = layer.gen_params() {
            n += gp.ableton_mappings.as_ref().map(|m| m.len()).unwrap_or(0);
        }
    }
    n
}

fn count_envelopes(project: &manifold_core::project::Project) -> usize {
    // Envelope-home unification: envelopes live on each effect's PresetInstance
    // (master / layer / clip) and on `layer.genParams` (generator-targeted).
    let mut n = 0;
    for_each_effect(project, |fx| {
        n += fx.envelopes.as_ref().map(|e| e.len()).unwrap_or(0);
    });
    for layer in &project.timeline.layers {
        if let Some(gp) = layer.gen_params() {
            n += gp.envelopes.as_ref().map(|e| e.len()).unwrap_or(0);
        }
    }
    n
}

fn count_effects(project: &manifold_core::project::Project) -> usize {
    let mut n = 0;
    for_each_effect(project, |_| n += 1);
    n
}

#[test]
fn load_liveschool_live_show_v6() {
    let path = fixture_path("Liveschool Live Show V6 LEDS.manifold");
    if !path.exists() {
        return;
    }

    let project =
        loader::load_project(&path).expect("Failed to load Liveschool Live Show V6 LEDS.manifold");

    // Settings — 4K output @ 150.83 BPM.
    assert!(
        (project.settings.bpm.0 - 150.83).abs() < 0.01,
        "BPM should be 150.83, got {}",
        project.settings.bpm
    );
    assert_eq!(project.settings.output_width, 3840);
    assert_eq!(project.settings.output_height, 2160);

    // Timeline — 52 layers, ~2828 clips. The clip count assertion is
    // exact: post-load repair removes overlapping clips, so this count
    // is the after-repair canonical state. If load behavior changes,
    // bump it intentionally — never silently.
    assert_eq!(project.timeline.layers.len(), 52, "expected 52 layers");
    assert_eq!(
        project.timeline.total_clip_count(),
        2828,
        "expected 2828 clips after load repair"
    );

    // Master effect count — exposes any drift in MasterEffects deserialization.
    assert_eq!(
        project.settings.master_effects.len(),
        5,
        "expected 5 master effects"
    );

    // Effect / driver / envelope / Ableton-mapping totals — these are
    // the counts every migration step (8-11, 12, 14) must preserve.
    // Drivers and Ableton mappings include both effect-targeted and
    // generator-targeted (via `layer.genParams`).
    assert_eq!(count_effects(&project), 160, "effects total drifted");
    assert_eq!(count_drivers(&project), 130, "drivers total drifted");
    // Envelope-home unification (v1.5→v1.6): the 15 layer-level envelopes
    // distribute onto their matching effect instance; 2 of them targeted a
    // `WireframeDepth` effect absent from their layer (BLACK HOLE MAIN) — those
    // were inert before (the evaluator found no target) and are dropped by the
    // migration. 13 effect envelopes + 20 generator envelopes = 33.
    assert_eq!(count_envelopes(&project), 33, "envelopes total drifted");
    assert_eq!(
        count_ableton_mappings(&project),
        29,
        "ableton mappings total drifted"
    );

    // Stress: every clip has valid beats and no layer has overlaps.
    for layer in &project.timeline.layers {
        for clip in &layer.clips {
            assert!(clip.duration_beats > manifold_core::Beats::ZERO);
            assert!(clip.start_beat >= manifold_core::Beats::ZERO);
        }
        assert!(
            !layer.has_overlapping_clips(),
            "Layer {:?} still has overlapping clips after load repair",
            layer.layer_id
        );
    }
}

#[test]
fn liveschool_roundtrip_preserves_addressing_sites() {
    // Save → reload → counts must match. This is the gate that future
    // ParamId migrations (steps 8-14) will run against. If the custom
    // Deserialize for ParameterDriver / ParamEnvelope / AbletonParamMapping
    // / paramValues drops or reshapes anything, one of the count
    // assertions below will fail.
    //
    // **Test scope:** this test runs in `manifold-io`'s test target,
    // which intentionally does NOT link `manifold-renderer`. The
    // effect/generator registries are therefore EMPTY here. Concretely:
    //
    // - Driver/envelope/Ableton mappings load with `param_id = ""`,
    //   `legacy_param_index = Some(idx)` (RegistryMissing path; the
    //   resolver preserves the parked index for next-load recovery —
    //   see `Project::resolve_legacy_param_ids`).
    // - The custom `Serialize` on those types re-emits `paramIndex`
    //   from the parked index, so the round-trip preserves addressing
    //   verbatim — this test would FAIL before the recovery-loop
    //   hardening landed.
    // - Effect/generator `paramValues` come in as V1.x `Array` shape
    //   from the on-disk fixture, never enter the Map path here, so
    //   the registry-missing branch on `into_positional` doesn't fire
    //   in this test.
    //
    // The semantic contract — that `param_id` actually resolves to the
    // intended parameter slot — is verified by
    // `manifold-app/tests/legacy_param_id_resolution.rs`, which DOES
    // link the renderer. **Do not interpret a green run of this test
    // as confirming `param_id` resolution; this test only confirms
    // shape preservation.**
    let path = fixture_path("Liveschool Live Show V6 LEDS.manifold");
    if !path.exists() {
        return;
    }

    let project = loader::load_project(&path).unwrap();
    let json = serde_json::to_string(&project).expect("serialize liveschool project");
    let project2 = loader::load_project_from_json(&json).expect("reload liveschool project");

    assert_eq!(
        project2.timeline.layers.len(),
        project.timeline.layers.len()
    );
    assert_eq!(
        project2.timeline.total_clip_count(),
        project.timeline.total_clip_count()
    );
    assert_eq!(
        project2.settings.master_effects.len(),
        project.settings.master_effects.len()
    );
    assert_eq!(count_effects(&project2), count_effects(&project));
    assert_eq!(count_drivers(&project2), count_drivers(&project));
    assert_eq!(count_envelopes(&project2), count_envelopes(&project));
    assert_eq!(
        count_ableton_mappings(&project2),
        count_ableton_mappings(&project)
    );

    // Sample-check that every driver's (paramId, beatDivision, waveform)
    // round-trips byte-equal — catches subtle reshape bugs that preserve
    // counts but mangle individual values. Covers both effect drivers
    // and generator drivers (`layer.genParams.drivers`).
    fn collect_drivers(p: &manifold_core::project::Project) -> Vec<(String, i32, i32)> {
        let mut v = Vec::new();
        for_each_effect(p, |fx| {
            if let Some(ref ds) = fx.drivers {
                for d in ds {
                    v.push((
                        d.param_id.to_string(),
                        d.beat_division as i32,
                        d.waveform as i32,
                    ));
                }
            }
        });
        for layer in &p.timeline.layers {
            if let Some(gp) = layer.gen_params()
                && let Some(ref ds) = gp.drivers
            {
                for d in ds {
                    v.push((
                        d.param_id.to_string(),
                        d.beat_division as i32,
                        d.waveform as i32,
                    ));
                }
            }
        }
        v
    }
    assert_eq!(
        collect_drivers(&project),
        collect_drivers(&project2),
        "driver shape (effect + generator) must round-trip exactly"
    );
}
